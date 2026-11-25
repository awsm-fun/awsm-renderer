//! Depth of field before the TAA resolve (`TaaDofOrder::BeforeResolve`,
//! docs/TAA.md §3.12).
//!
//! The effects pass's DoF (`effects_wgsl/helpers/dof.wgsl`, the same code)
//! on this frame's HDR render (`composite`, render size, jittered) into a
//! render-size target of its own; the resolve reads that instead of
//! `composite` and accumulates the blurred image. The effects pass then
//! compiles without DoF and keeps haze and bloom. Bloom's `CurrentFrame`
//! source keeps reading `composite` itself (pre-DoF, the no-TAA order).
//!
//! Lazy and zero cost otherwise: the pass exists only while TAA is on, DoF is
//! on and the order is `BeforeResolve` (`AwsmRenderer::sync_taa_dof_pre`).
//! Self-contained like RCAS: its own template-rendered shader and pipelines,
//! no shader cache key. TAA never runs with MSAA, so the depth it reads is
//! single-sampled.
//!
//! Wiring: built / dropped by `sync_taa_dof_pre` (from `set_anti_aliasing`,
//! `set_post_processing` and the TAA pass's construction), sized in
//! `prepare_taa_frame`, its group rebuilt with the resolve's
//! (`FunctionToCall::Taa`, which also points the resolve's binding 2 at
//! [`TaaDofPre::out_view`]), recorded right before the resolve.

use std::borrow::Cow;

use askama::Template;
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
    render::RenderContext, render_passes::taa::texture::TAA_COLOR_FORMAT, shaders::AwsmShaderError,
};

/// Frames a replaced texture / buffer is kept before `destroy()` (the TAA
/// textures' rule: past the frames a submit can stay in flight).
const DESTROY_DELAY_FRAMES: u32 = 3;

/// `DofPreParams` in `dof_pre.wgsl`: max_blur + 3 pad floats.
const PARAMS_BYTE_SIZE: usize = 16;

/// The effects pass's widest disc, output pixels (`DOF_MAX_BLUR` in
/// `helpers/dof.wgsl`).
pub const DOF_MAX_BLUR_OUTPUT_PX: f32 = 20.0;

/// The pre-resolve DoF shader (`taa_wgsl/dof_pre.wgsl`). The flags the shared
/// includes read are fixed: no SMAA, haze or MSAA exist on this path, and the
/// CoC is in render pixels (not the effects pass's `taa_post`).
#[derive(Template, Debug)]
#[template(path = "taa_wgsl/dof_pre.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateTaaDofPre {
    pub reverse_z: bool,
    pub multisampled_geometry: bool,
    pub taa_post: bool,
    pub smaa_anti_alias: bool,
    pub atmosphere: bool,
    pub atmosphere_volumetric: bool,
    pub dof_pre: bool,
}

impl ShaderTemplateTaaDofPre {
    pub fn new(reverse_z: bool) -> Self {
        Self {
            reverse_z,
            multisampled_geometry: false,
            taa_post: false,
            smaa_anti_alias: false,
            atmosphere: false,
            atmosphere_volumetric: false,
            dof_pre: true,
        }
    }

    pub fn into_source(self) -> std::result::Result<String, AwsmShaderError> {
        self.render().map_err(AwsmShaderError::from)
    }
}

/// `dof_data`'s size for a `width` x `height` gather (mirrors
/// `dof_data_byte_size` in effects/bind_group.rs and the layout in dof.wgsl).
fn dof_data_byte_size(width: u32, height: u32) -> usize {
    let tiles = width.div_ceil(8) as usize * height.div_ceil(8) as usize;
    let pixels = width.div_ceil(2) as usize * height as usize;
    (tiles + pixels) * 4
}

/// The widest disc in render pixels: the effects pass's 20 output pixels at
/// this render / output ratio, so the largest blur matches `AfterResolve` (and
/// the no-TAA reference) at any render scale.
pub fn max_blur_render_px(render_height: u32, post_height: u32) -> f32 {
    DOF_MAX_BLUR_OUTPUT_PX * render_height.max(1) as f32 / post_height.max(1) as f32
}

pub struct TaaDofPre {
    prepare_pipeline: web_sys::GpuComputePipeline,
    main_pipeline: web_sys::GpuComputePipeline,
    layout: web_sys::GpuBindGroupLayout,
    params_buffer: web_sys::GpuBuffer,
    params_uploader: std::cell::RefCell<MappedUploader>,
    max_blur: std::cell::Cell<Option<f32>>,
    /// The depth convention the shader was compiled for.
    pub reverse_z: bool,
    out: Option<DofPreTarget>,
    retired: Vec<(web_sys::GpuTexture, Option<web_sys::GpuBuffer>, u32)>,
    bind_group: Option<web_sys::GpuBindGroup>,
}

struct DofPreTarget {
    texture: web_sys::GpuTexture,
    view: web_sys::GpuTextureView,
    dof_data: web_sys::GpuBuffer,
    size: (u32, u32),
}

impl TaaDofPre {
    /// Renders and compiles the shader and builds both pipelines. The target
    /// is created by the first [`Self::ensure_size`].
    pub async fn new(gpu: &AwsmRendererWebGpu, reverse_z: bool) -> Result<Self> {
        let source = ShaderTemplateTaaDofPre::new(reverse_z).into_source()?;
        let shader = gpu.compile_shader(
            &ShaderModuleDescriptor::new(&source, Some("TAA DoF before resolve")).into(),
        );
        shader.validate_shader().await?;
        let compute_only = |binding: u32, resource| {
            BindGroupLayoutEntry::new(binding, resource).with_visibility_compute()
        };
        let layout = gpu.create_bind_group_layout(
            &BindGroupLayoutDescriptor::new(Some("TAA DoF pre layout"))
                .with_entries(vec![
                    // 0 composite (render size)
                    compute_only(
                        0,
                        BindGroupLayoutResource::Texture(
                            TextureBindingLayout::new()
                                .with_sample_type(TextureSampleType::UnfilterableFloat)
                                .with_view_dimension(TextureViewDimension::N2d)
                                .with_multisampled(false),
                        ),
                    ),
                    // 1 camera
                    compute_only(
                        1,
                        BindGroupLayoutResource::Buffer(
                            BufferBindingLayout::new()
                                .with_binding_type(BufferBindingType::Uniform),
                        ),
                    ),
                    // 2 depth (render size, single-sampled under TAA)
                    compute_only(
                        2,
                        BindGroupLayoutResource::Texture(
                            TextureBindingLayout::new()
                                .with_sample_type(TextureSampleType::Depth)
                                .with_view_dimension(TextureViewDimension::N2d)
                                .with_multisampled(false),
                        ),
                    ),
                    // 3 out
                    compute_only(
                        3,
                        BindGroupLayoutResource::StorageTexture(
                            StorageTextureBindingLayout::new(TAA_COLOR_FORMAT)
                                .with_view_dimension(TextureViewDimension::N2d)
                                .with_access(StorageTextureAccess::WriteOnly),
                        ),
                    ),
                    // 4 dof_data
                    compute_only(
                        4,
                        BindGroupLayoutResource::Buffer(
                            BufferBindingLayout::new()
                                .with_binding_type(BufferBindingType::Storage),
                        ),
                    ),
                    // 5 params
                    compute_only(
                        5,
                        BindGroupLayoutResource::Buffer(
                            BufferBindingLayout::new()
                                .with_binding_type(BufferBindingType::Uniform),
                        ),
                    ),
                ])
                .into(),
        )?;
        let pipeline_layout = gpu.create_pipeline_layout(
            &PipelineLayoutDescriptor::new(
                Some("TAA DoF pre pipeline layout"),
                vec![layout.clone()],
            )
            .into(),
        );
        let prepare_pipeline = gpu
            .create_compute_pipeline(
                &ComputePipelineDescriptor::new(
                    ProgrammableStage::new(&shader, Some("dof_prepare")),
                    PipelineLayoutKind::Custom(&pipeline_layout),
                    Some("TAA DoF pre prepare"),
                )
                .into(),
            )
            .await?;
        let main_pipeline = gpu
            .create_compute_pipeline(
                &ComputePipelineDescriptor::new(
                    ProgrammableStage::new(&shader, Some("main")),
                    PipelineLayoutKind::Custom(&pipeline_layout),
                    Some("TAA DoF pre"),
                )
                .into(),
            )
            .await?;
        let params_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("TAA DoF pre params"),
                PARAMS_BYTE_SIZE,
                BufferUsage::new().with_uniform().with_copy_dst(),
            )
            .into(),
        )?;
        Ok(Self {
            prepare_pipeline,
            main_pipeline,
            layout,
            params_buffer,
            params_uploader: std::cell::RefCell::new(MappedUploader::new("TAA DoF pre params")),
            max_blur: std::cell::Cell::new(None),
            reverse_z,
            out: None,
            retired: Vec::new(),
            bind_group: None,
        })
    }

    /// Once per frame (from `prepare_taa_frame`): destroys retired resources
    /// whose delay is over, (re)creates the target and the CoC buffer for
    /// `render_size`, and uploads the widest disc for this render / output
    /// ratio. Returns `true` when the target is new: this pass's group and
    /// the resolve's must be rebuilt.
    pub fn ensure_size(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        render_size: (u32, u32),
        post_size: (u32, u32),
    ) -> Result<bool> {
        self.retired.retain_mut(|(texture, buffer, frames_left)| {
            if *frames_left == 0 {
                texture.destroy();
                if let Some(buffer) = buffer.take() {
                    buffer.destroy();
                }
                false
            } else {
                *frames_left -= 1;
                true
            }
        });
        let max_blur = max_blur_render_px(render_size.1, post_size.1);
        if self.max_blur.get() != Some(max_blur) {
            self.max_blur.set(Some(max_blur));
            let mut bytes = [0u8; PARAMS_BYTE_SIZE];
            bytes[0..4].copy_from_slice(&max_blur.to_ne_bytes());
            self.params_uploader.borrow_mut().write_dirty_ranges(
                gpu,
                &self.params_buffer,
                PARAMS_BYTE_SIZE,
                &bytes,
                &[(0, PARAMS_BYTE_SIZE)],
            )?;
        }
        let size = (render_size.0.max(1), render_size.1.max(1));
        if self.out.as_ref().is_some_and(|o| o.size == size) {
            return Ok(false);
        }
        let texture = gpu.create_texture(
            &TextureDescriptor::new(
                TAA_COLOR_FORMAT,
                Extent3d::new(size.0, Some(size.1), Some(1)),
                // Storage-written here, texture-read by the resolve.
                TextureUsage::new()
                    .with_storage_binding()
                    .with_texture_binding(),
            )
            .with_label("TAA DoF pre out")
            .into(),
        )?;
        let view = texture.create_view().map_err(|e| {
            AwsmCoreError::create_texture_view(format!("TAA DoF pre out: {e:?}").into())
        })?;
        let dof_data = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("TAA DoF pre data"),
                dof_data_byte_size(size.0, size.1),
                BufferUsage::new().with_storage(),
            )
            .into(),
        )?;
        if let Some(old) = self.out.replace(DofPreTarget {
            texture,
            view,
            dof_data,
            size,
        }) {
            self.retired
                .push((old.texture, Some(old.dof_data), DESTROY_DELAY_FRAMES));
        }
        self.bind_group = None;
        Ok(true)
    }

    /// The blurred render-size image, once [`Self::ensure_size`] ran: what the
    /// resolve reads (binding 2) instead of `composite`.
    pub fn out_view(&self) -> Option<&web_sys::GpuTextureView> {
        self.out.as_ref().map(|o| &o.view)
    }

    /// Rebuild the bind group against `composite`, depth and the camera.
    pub fn recreate(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        composite: &web_sys::GpuTextureView,
        depth: &web_sys::GpuTextureView,
        camera: &web_sys::GpuBuffer,
    ) -> Result<()> {
        let out = self
            .out
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("TAA DoF pre out".to_string()))?;
        let entries = vec![
            BindGroupEntry::new(0, BindGroupResource::TextureView(Cow::Borrowed(composite))),
            BindGroupEntry::new(1, BindGroupResource::Buffer(BufferBinding::new(camera))),
            BindGroupEntry::new(2, BindGroupResource::TextureView(Cow::Borrowed(depth))),
            BindGroupEntry::new(3, BindGroupResource::TextureView(Cow::Borrowed(&out.view))),
            BindGroupEntry::new(
                4,
                BindGroupResource::Buffer(BufferBinding::new(&out.dof_data)),
            ),
            BindGroupEntry::new(
                5,
                BindGroupResource::Buffer(BufferBinding::new(&self.params_buffer)),
            ),
        ];
        let descriptor = BindGroupDescriptor::new(&self.layout, Some("TAA DoF pre"), entries);
        self.bind_group = Some(gpu.create_bind_group(&descriptor.into()));
        Ok(())
    }

    /// Blur `composite` into the target: the CoC dispatch, then the gather,
    /// in one compute pass (WebGPU orders their use of the CoC buffer).
    /// Recorded right before the resolve.
    pub fn render(&self, ctx: &RenderContext) -> Result<()> {
        let missing = || AwsmBindGroupError::NotFound("TAA DoF pre".to_string());
        let out = self.out.as_ref().ok_or_else(missing)?;
        let bind_group = self.bind_group.as_ref().ok_or_else(missing)?;
        let compute_pass = ctx.command_encoder.begin_compute_pass(Some(
            &ComputePassDescriptor::new(Some("TAA DoF pre"))
                .with_timestamp_writes_opt(
                    ctx.gpu_timestamps
                        .and_then(|t| t.writes_for_compute("TaaDofPre")),
                )
                .into(),
        ));
        compute_pass.set_bind_group(0, bind_group, None)?;
        let groups = (out.size.0.div_ceil(8), out.size.1.div_ceil(8));
        compute_pass.set_pipeline(&self.prepare_pipeline);
        compute_pass.dispatch_workgroups(groups.0, Some(groups.1), Some(1));
        compute_pass.set_pipeline(&self.main_pipeline);
        compute_pass.dispatch_workgroups(groups.0, Some(groups.1), Some(1));
        compute_pass.end();
        Ok(())
    }

    /// Release the GPU resources (order, DoF or TAA turned off; between
    /// frames, like the TAA pass's own `destroy`).
    pub fn destroy(self) {
        if let Some(out) = self.out {
            out.texture.destroy();
            out.dof_data.destroy();
        }
        for (texture, buffer, _) in self.retired {
            texture.destroy();
            if let Some(buffer) = buffer {
                buffer.destroy();
            }
        }
        self.params_buffer.destroy();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validate(source: &str) -> naga::Module {
        let module = match naga::front::wgsl::parse_str(source) {
            Ok(m) => m,
            Err(e) => panic!("dof_pre: parse failed:\n{}", e.emit_to_string(source)),
        };
        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        if let Err(e) = validator.validate(&module) {
            panic!("dof_pre: validation failed:\n{}", e.emit_to_string(source));
        }
        module
    }

    #[test]
    fn dof_pre_shader_validates_for_both_depth_conventions() {
        for reverse_z in [false, true] {
            let source = ShaderTemplateTaaDofPre::new(reverse_z)
                .into_source()
                .expect("renders");
            let module = validate(&source);
            for entry in ["main", "dof_prepare"] {
                assert!(
                    module.entry_points.iter().any(|ep| ep.name == entry),
                    "entry point {entry}"
                );
            }
            // Render pixels: depth read directly, CoC from the render size,
            // the widest disc from the uniform.
            assert!(!source.contains("fn load_depth_post"));
            assert!(source.contains("let depth = load_depth(vec2<i32>(p));"));
            assert!(source.contains("camera.viewport_size.y"));
            assert!(source
                .contains("clamp(coc_pixels, -dof_pre_params.max_blur, dof_pre_params.max_blur)"));
            // The rgba16float store is rounded like the resolve's.
            assert!(source
                .contains("textureStore(dof_out, coords, vec4<f32>(taa_round_f16(rgb), 1.0));"));
            assert_eq!(source.matches("textureStore(").count(), 1);
        }
    }

    #[test]
    fn widest_disc_is_twenty_output_pixels() {
        assert_eq!(max_blur_render_px(1080, 1080), 20.0);
        assert!((max_blur_render_px(724, 1080) - 20.0 * 724.0 / 1080.0).abs() < 1e-4);
        // Never a zero-size division.
        assert!(max_blur_render_px(0, 0).is_finite());
    }

    #[test]
    fn coc_buffer_matches_the_effects_layout() {
        // One word per 8x8 tile, then two pixels to a word, row by row.
        assert_eq!(dof_data_byte_size(8, 8), (1 + 4 * 8) * 4);
        assert_eq!(dof_data_byte_size(9, 1), (2 + 5) * 4);
    }

    /// The resolve reads its input's `.rgb` only, and bloom's `CurrentFrame`
    /// source keeps reading `composite` itself (pre-DoF, the no-TAA order),
    /// never this pass's output.
    #[test]
    fn the_resolve_reads_rgb_and_bloom_stays_pre_dof() {
        let resolve = include_str!("shader/taa_wgsl/resolve.wgsl");
        for line in resolve
            .lines()
            .filter(|l| l.contains("textureLoad(color_tex"))
        {
            assert!(line.contains(", 0).rgb"), "{line}");
        }
        let bind_groups = include_str!("../../bind_groups.rs");
        let bloom_arm = bind_groups
            .split("FunctionToCall::Bloom =>")
            .nth(1)
            .and_then(|s| s.split("FunctionToCall::").next())
            .expect("bloom arm");
        assert!(
            !bloom_arm.contains("dof_pre"),
            "bloom never reads the pre-resolve DoF"
        );
        let bloom_group = include_str!("../bloom/bind_group.rs");
        assert!(!bloom_group.contains("dof_pre"));
    }
}
