//! RCAS sharpening after the TAA resolve (03 §9, plan phase 4).
//!
//! FSR 1's robust contrast-adaptive sharpening on `taa_out`, at post size,
//! into a texture of its own (a 5-tap filter cannot work in place). Bloom,
//! effects and display then read [`TaaRcas::out_view`] instead of `taa_out`.
//!
//! Lazy and zero cost when off: the pass exists only while TAA is on AND
//! `TaaConfig::sharpen > 0`. Turning sharpening on or off is therefore
//! structural (the consumers' bind groups switch source); changing the
//! strength while it is on is a 16-byte uniform write.
//!
//! Self-contained like the opaque mipgen: one static shader
//! (`shader/taa_wgsl/rcas.wgsl`), compiled with its own pipeline, no shader
//! cache key.
//!
//! Wiring: built / dropped with `sharpen` by `AwsmRenderer::sync_taa_rcas`
//! (from `set_anti_aliasing`), sized and fed its strength in
//! `prepare_taa_frame`, its group rebuilt with the resolve's
//! (`FunctionToCall::Taa`), recorded right after the resolve, and BEFORE the
//! HUD-after-TAA pass, which draws into this output so the HUD is never
//! sharpened. Consumers read `TaaRenderPass::final_view`.

use std::borrow::Cow;

use awsm_renderer_core::{
    bind_groups::{
        BindGroupDescriptor, BindGroupEntry, BindGroupLayoutDescriptor, BindGroupLayoutEntry,
        BindGroupLayoutResource, BindGroupResource, BufferBindingLayout, BufferBindingType,
        StorageTextureAccess, StorageTextureBindingLayout, TextureBindingLayout,
    },
    buffers::{BufferBinding, BufferDescriptor, BufferUsage},
    command::compute_pass::ComputePassDescriptor,
    error::AwsmCoreError,
    pipeline::{
        layout::{PipelineLayoutDescriptor, PipelineLayoutKind},
        ComputePipelineDescriptor, ProgrammableStage,
    },
    renderer::AwsmRendererWebGpu,
    shaders::{ShaderModuleDescriptor, ShaderModuleExt},
    texture::{Extent3d, TextureDescriptor, TextureSampleType, TextureUsage, TextureViewDimension},
};

use crate::{
    bind_groups::AwsmBindGroupError, buffer::mapped_uploader::MappedUploader, error::Result,
    render::RenderContext, render_passes::taa::texture::TAA_COLOR_FORMAT,
};

/// The shader (plain WGSL, no template).
pub const RCAS_SHADER_SOURCE: &str = include_str!("shader/taa_wgsl/rcas.wgsl");

/// Frames a replaced output texture is kept before `destroy()` (the TAA
/// textures' rule: past the frames a submit can stay in flight).
const DESTROY_DELAY_FRAMES: u32 = 3;

/// `RcasParams` in `rcas.wgsl`: strength, reactive_valid + 2 pad floats.
const PARAMS_BYTE_SIZE: usize = 16;

pub struct TaaRcas {
    pipeline: web_sys::GpuComputePipeline,
    layout: web_sys::GpuBindGroupLayout,
    params_buffer: web_sys::GpuBuffer,
    params_uploader: std::cell::RefCell<MappedUploader>,
    /// The strength last uploaded (`None` before the first upload).
    strength: std::cell::Cell<Option<f32>>,
    /// Whether this frame's reactive mask is valid (the resolve's
    /// `reactive_valid`): only then are HUD pass-through pixels (mask 1) told
    /// apart and left unsharpened.
    reactive_valid: std::cell::Cell<bool>,
    /// Sharpened output at post size.
    out: Option<RcasTarget>,
    retired: Vec<(web_sys::GpuTexture, u32)>,
    bind_group: Option<web_sys::GpuBindGroup>,
}

struct RcasTarget {
    texture: web_sys::GpuTexture,
    view: web_sys::GpuTextureView,
    size: (u32, u32),
}

impl TaaRcas {
    /// Compiles the shader and builds the pipeline. The output texture is
    /// created by the first [`Self::ensure_size`].
    pub async fn new(gpu: &AwsmRendererWebGpu) -> Result<Self> {
        let shader = gpu.compile_shader(
            &ShaderModuleDescriptor::new(RCAS_SHADER_SOURCE, Some("TAA RCAS shader")).into(),
        );
        shader.validate_shader().await?;
        let compute_only = |binding: u32, resource| {
            BindGroupLayoutEntry::new(binding, resource).with_visibility_compute()
        };
        let layout = gpu.create_bind_group_layout(
            &BindGroupLayoutDescriptor::new(Some("TAA RCAS layout"))
                .with_entries(vec![
                    compute_only(
                        0,
                        BindGroupLayoutResource::Buffer(
                            BufferBindingLayout::new()
                                .with_binding_type(BufferBindingType::Uniform),
                        ),
                    ),
                    compute_only(
                        1,
                        BindGroupLayoutResource::Texture(
                            TextureBindingLayout::new()
                                .with_sample_type(TextureSampleType::UnfilterableFloat)
                                .with_view_dimension(TextureViewDimension::N2d)
                                .with_multisampled(false),
                        ),
                    ),
                    compute_only(
                        2,
                        BindGroupLayoutResource::StorageTexture(
                            StorageTextureBindingLayout::new(TAA_COLOR_FORMAT)
                                .with_view_dimension(TextureViewDimension::N2d)
                                .with_access(StorageTextureAccess::WriteOnly),
                        ),
                    ),
                    // The TAA reactive mask (render size): HUD pass-through
                    // pixels are 1.0 there and are not sharpened.
                    compute_only(
                        3,
                        BindGroupLayoutResource::Texture(
                            TextureBindingLayout::new()
                                .with_sample_type(TextureSampleType::Float)
                                .with_view_dimension(TextureViewDimension::N2d)
                                .with_multisampled(false),
                        ),
                    ),
                ])
                .into(),
        )?;
        let pipeline_layout = gpu.create_pipeline_layout(
            &PipelineLayoutDescriptor::new(Some("TAA RCAS pipeline layout"), vec![layout.clone()])
                .into(),
        );
        let pipeline = gpu
            .create_compute_pipeline(
                &ComputePipelineDescriptor::new(
                    ProgrammableStage::new(&shader, None),
                    PipelineLayoutKind::Custom(&pipeline_layout),
                    Some("TAA RCAS"),
                )
                .into(),
            )
            .await?;
        let params_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("TAA RCAS params"),
                PARAMS_BYTE_SIZE,
                BufferUsage::new().with_uniform().with_copy_dst(),
            )
            .into(),
        )?;
        Ok(Self {
            pipeline,
            layout,
            params_buffer,
            params_uploader: std::cell::RefCell::new(MappedUploader::new("TAA RCAS params")),
            strength: std::cell::Cell::new(None),
            reactive_valid: std::cell::Cell::new(false),
            out: None,
            retired: Vec::new(),
            bind_group: None,
        })
    }

    /// Once per frame (from `prepare_taa_frame`): destroys retired textures
    /// whose delay is over, and (re)creates the output for `post_size`.
    /// Returns `true` when the output is new: its consumers (bloom, effects)
    /// and this pass's bind group must be rebuilt.
    pub fn ensure_size(&mut self, gpu: &AwsmRendererWebGpu, post_size: (u32, u32)) -> Result<bool> {
        self.retired.retain_mut(|(texture, frames_left)| {
            if *frames_left == 0 {
                texture.destroy();
                false
            } else {
                *frames_left -= 1;
                true
            }
        });
        let size = (post_size.0.max(1), post_size.1.max(1));
        if self.out.as_ref().is_some_and(|o| o.size == size) {
            return Ok(false);
        }
        let texture = gpu.create_texture(
            &TextureDescriptor::new(
                TAA_COLOR_FORMAT,
                Extent3d::new(size.0, Some(size.1), Some(1)),
                // Storage-written here, texture-read by bloom / effects, and
                // the HUD-after-TAA pass draws into it.
                TextureUsage::new()
                    .with_storage_binding()
                    .with_texture_binding()
                    .with_render_attachment(),
            )
            .with_label("TAA RCAS out")
            .into(),
        )?;
        let view = texture.create_view().map_err(|e| {
            AwsmCoreError::create_texture_view(format!("TAA RCAS out: {e:?}").into())
        })?;
        if let Some(old) = self.out.replace(RcasTarget {
            texture,
            view,
            size,
        }) {
            self.retired.push((old.texture, DESTROY_DELAY_FRAMES));
        }
        self.bind_group = None;
        Ok(true)
    }

    /// Uploads the strength (`TaaConfig::sharpen`, clamped to 0..=1) when it
    /// changed. Live: no rebuild.
    pub fn write_strength(&self, gpu: &AwsmRendererWebGpu, strength: f32) -> Result<()> {
        let strength = if strength.is_finite() {
            strength.clamp(0.0, 1.0)
        } else {
            0.0
        };
        if self.strength.get() == Some(strength) {
            return Ok(());
        }
        self.strength.set(Some(strength));
        self.upload(gpu)
    }

    /// The frame's reactive-mask validity (the resolve's `reactive_valid`),
    /// uploaded when it changed.
    pub fn set_reactive_valid(&self, gpu: &AwsmRendererWebGpu, valid: bool) -> Result<()> {
        if self.reactive_valid.get() == valid || self.strength.get().is_none() {
            self.reactive_valid.set(valid);
            return Ok(());
        }
        self.reactive_valid.set(valid);
        self.upload(gpu)
    }

    fn upload(&self, gpu: &AwsmRendererWebGpu) -> Result<()> {
        let mut bytes = [0u8; PARAMS_BYTE_SIZE];
        bytes[0..4].copy_from_slice(&self.strength.get().unwrap_or(0.0).to_ne_bytes());
        let valid = if self.reactive_valid.get() {
            1.0f32
        } else {
            0.0
        };
        bytes[4..8].copy_from_slice(&valid.to_ne_bytes());
        self.params_uploader.borrow_mut().write_dirty_ranges(
            gpu,
            &self.params_buffer,
            PARAMS_BYTE_SIZE,
            &bytes,
            &[(0, PARAMS_BYTE_SIZE)],
        )?;
        Ok(())
    }

    /// The sharpened image (post size), once [`Self::ensure_size`] ran: what
    /// bloom / effects read instead of `taa_out` while sharpening is on.
    pub fn out_view(&self) -> Option<&web_sys::GpuTextureView> {
        self.out.as_ref().map(|o| &o.view)
    }

    /// Rebuild the bind group against `taa_out` (the input), the output and
    /// the reactive mask.
    pub fn recreate(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        taa_out: &web_sys::GpuTextureView,
        reactive: &web_sys::GpuTextureView,
    ) -> Result<()> {
        let out = self
            .out
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("TAA RCAS out".to_string()))?;
        let entries = vec![
            BindGroupEntry::new(
                0,
                BindGroupResource::Buffer(BufferBinding::new(&self.params_buffer)),
            ),
            BindGroupEntry::new(1, BindGroupResource::TextureView(Cow::Borrowed(taa_out))),
            BindGroupEntry::new(2, BindGroupResource::TextureView(Cow::Borrowed(&out.view))),
            BindGroupEntry::new(3, BindGroupResource::TextureView(Cow::Borrowed(reactive))),
        ];
        let descriptor = BindGroupDescriptor::new(&self.layout, Some("TAA RCAS"), entries);
        self.bind_group = Some(gpu.create_bind_group(&descriptor.into()));
        Ok(())
    }

    /// Sharpen `taa_out` into the output: one dispatch at post size, recorded
    /// right after the resolve.
    pub fn render(&self, ctx: &RenderContext) -> Result<()> {
        let missing = || AwsmBindGroupError::NotFound("TAA RCAS".to_string());
        let out = self.out.as_ref().ok_or_else(missing)?;
        let bind_group = self.bind_group.as_ref().ok_or_else(missing)?;
        let compute_pass = ctx.command_encoder.begin_compute_pass(Some(
            &ComputePassDescriptor::new(Some("TAA RCAS"))
                .with_timestamp_writes_opt(
                    ctx.gpu_timestamps
                        .and_then(|t| t.writes_for_compute("TaaRcas")),
                )
                .into(),
        ));
        compute_pass.set_pipeline(&self.pipeline);
        compute_pass.set_bind_group(0, bind_group, None)?;
        compute_pass.dispatch_workgroups(
            out.size.0.div_ceil(8),
            Some(out.size.1.div_ceil(8)),
            Some(1),
        );
        compute_pass.end();
        Ok(())
    }

    /// Release the GPU textures (sharpening or TAA turned off; between frames,
    /// like the TAA pass's own `destroy`).
    pub fn destroy(self) {
        if let Some(out) = self.out {
            out.texture.destroy();
        }
        for (texture, _) in self.retired {
            texture.destroy();
        }
        self.params_buffer.destroy();
    }
}

#[cfg(test)]
mod tests {
    //! The shader validates (naga) and keeps the Apple GPU rules, and a CPU
    //! mirror of its per-pixel maths pins RCAS's behaviour. Keep the mirror in
    //! step with `rcas.wgsl` by hand: it is what the assertions are about.
    use super::RCAS_SHADER_SOURCE;

    #[test]
    fn rcas_shader_validates_and_keeps_the_apple_rules() {
        let module = match naga::front::wgsl::parse_str(RCAS_SHADER_SOURCE) {
            Ok(m) => m,
            Err(e) => panic!(
                "rcas: parse failed:\n{}",
                e.emit_to_string(RCAS_SHADER_SOURCE)
            ),
        };
        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        if let Err(e) = validator.validate(&module) {
            panic!(
                "rcas: validation failed:\n{}",
                e.emit_to_string(RCAS_SHADER_SOURCE)
            );
        }
        assert!(module.entry_points.iter().any(|ep| ep.name == "cs_main"));
        // Code only (comments name the rules).
        let code: String = RCAS_SHADER_SOURCE
            .lines()
            .map(|l| l.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");
        for banned in ["switch", "for (", "for(", "loop {", "while ("] {
            assert!(!code.contains(banned), "rcas must not contain `{banned}`");
        }
        let functions = module
            .functions
            .iter()
            .map(|(_, f)| f)
            .chain(module.entry_points.iter().map(|ep| &ep.function));
        for function in functions {
            for (_, local) in function.local_variables.iter() {
                assert!(
                    !matches!(module.types[local.ty].inner, naga::TypeInner::Array { .. }),
                    "rcas declares a per-invocation array"
                );
            }
        }
        // Five taps, the FSR limit, and the working space.
        assert_eq!(code.matches("= tap(p").count(), 5, "five taps");
        assert!(code.contains("const RCAS_LIMIT: f32 = 0.1875;"));
        assert!(code.contains("return c / (1.0 + max3(c));"));
        // HUD pass-through pixels are copied unsharpened, recognised by the
        // resolve's own threshold, and only when the mask is valid.
        let hud = "const HUD_REACTIVE: f32 = 0.999;";
        assert!(code.contains(hud));
        assert!(include_str!("shader/taa_wgsl/resolve.wgsl").contains(hud));
        assert!(code.contains("if (params.reactive_valid > 0.5) {"));
        assert!(code.contains("textureLoad(reactive_tex, r_p, 0).r >= HUD_REACTIVE"));
    }

    const LIMIT: f32 = 0.1875;
    const T_MAX: f32 = 0.9995;

    fn max3(c: [f32; 3]) -> f32 {
        c[0].max(c[1]).max(c[2])
    }
    fn tonemap(c: [f32; 3]) -> [f32; 3] {
        let k = 1.0 / (1.0 + max3(c));
        c.map(|v| (v * k).min(T_MAX))
    }
    fn untonemap(t: [f32; 3]) -> [f32; 3] {
        let k = 1.0 / (1.0 - max3(t).min(T_MAX));
        t.map(|v| v * k)
    }

    /// CPU mirror of `cs_main` for one pixel: `[b, d, e, f, h]` linear HDR.
    fn rcas(taps: [[f32; 3]; 5], strength: f32) -> [f32; 3] {
        let [b, d, e, f, h] = taps.map(tonemap);
        let mut lobe_max = f32::NEG_INFINITY;
        for i in 0..3 {
            let mn4 = b[i].min(d[i]).min(f[i]).min(h[i]);
            let mx4 = b[i].max(d[i]).max(f[i]).max(h[i]);
            let hit_min = mn4.min(e[i]) / (4.0 * mx4).max(1e-6);
            let hit_max = (1.0 - mx4.max(e[i])) / (4.0 * mn4 - 4.0).min(-1e-6);
            lobe_max = lobe_max.max((-hit_min).max(hit_max));
        }
        let lobe = (-LIMIT).max(lobe_max.min(0.0)) * strength.clamp(0.0, 1.0);
        let t: [f32; 3] = std::array::from_fn(|i| {
            ((lobe * (b[i] + d[i] + f[i] + h[i]) + e[i]) / (4.0 * lobe + 1.0)).clamp(0.0, T_MAX)
        });
        untonemap(t)
    }

    fn close(a: [f32; 3], b: [f32; 3], eps: f32) -> bool {
        a.iter()
            .zip(b)
            .all(|(x, y)| (x - y).abs() <= eps * y.abs().max(1.0))
    }

    #[test]
    fn flat_regions_and_zero_strength_are_unchanged() {
        let c = [0.3, 0.6, 2.5];
        assert!(close(rcas([c; 5], 1.0), c, 1e-4));
        let taps = [[0.1; 3], [0.9; 3], [0.4; 3], [0.2; 3], [3.0; 3]];
        assert!(close(rcas(taps, 0.0), taps[2], 1e-4));
    }

    #[test]
    fn an_edge_is_sharpened_and_stays_inside_its_neighbourhood() {
        // Centre between a dark left and a bright right neighbour: sharpening
        // pushes it away from the ring's mean (the ring mean here is 0.5) but
        // never outside [min, max] of the five taps.
        let dark = [0.1f32; 3];
        let bright = [0.9f32; 3];
        let centre = [0.6f32; 3];
        let taps = [centre, dark, centre, bright, centre];
        let out = rcas(taps, 1.0);
        assert!(out[0] > centre[0], "sharpened: {out:?}");
        assert!(out[0] <= bright[0] + 1e-4, "stays in range: {out:?}");
        // More strength, more sharpening.
        assert!(rcas(taps, 0.25)[0] < out[0]);
    }

    #[test]
    fn hdr_and_degenerate_rings_stay_finite() {
        // All black (FSR's 1 / (4·mx4) zero), all at the working-space
        // ceiling (its 1 / (4·mn4 − 4) zero), and one huge highlight.
        for taps in [
            [[0.0; 3]; 5],
            [[1.0e4; 3]; 5],
            [[0.2; 3], [0.2; 3], [6.0e4; 3], [0.2; 3], [0.2; 3]],
        ] {
            let out = rcas(taps, 1.0);
            assert!(
                out.iter().all(|v| v.is_finite() && *v >= 0.0),
                "{taps:?} → {out:?}"
            );
        }
    }
}
