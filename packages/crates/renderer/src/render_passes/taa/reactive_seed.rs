//! TAA reactive seed (plan 03 §5.2): one fullscreen draw at the start of the
//! world transparent pass that writes `k_material` into the reactive mask for
//! opaque pixels whose material is flagged `temporal: reactive` (the per-mesh
//! material meta carries the flag). See `taa_wgsl/reactive_seed.wgsl`.
//!
//! Built with the TAA pass (so nothing exists without TAA) and drawn only on
//! frames that need it: a live material is flagged
//! (`Materials::has_temporal_reactive`), a HUD is drawn with the scene, or
//! SSR runs with `TaaTuning::reactive_ssr > 0`. The last uses the `ssr`
//! variant, which also binds the SSR reflection descriptor (4) and adds
//! `reflectivity * k_ssr` (03 §5.3); the plain variant never reads it.
//!
//! Bind group: 0 camera uniform, 1 `visibility_data` (uint, single-sampled:
//! TAA excludes MSAA), 2 material mesh meta (read-only storage, as words),
//! 3 `hud_depth` (a 1x1 never-read placeholder until a HUD exists; not the
//! scene depth, which is this pass's depth attachment). Also flags the HUD drawn with the scene fully reactive (03 §5.4).
//! Rebuilt with the resolve's groups (`FunctionToCall::Taa`), which also runs
//! on `MaterialMeshMetaResize`.

use std::borrow::Cow;

use awsm_renderer_core::bind_groups::{
    BindGroupDescriptor, BindGroupEntry, BindGroupLayoutResource, BindGroupResource,
    BufferBindingLayout, BufferBindingType, TextureBindingLayout,
};
use awsm_renderer_core::buffers::BufferBinding;
use awsm_renderer_core::command::render_pass::RenderPassEncoder;
use awsm_renderer_core::compare::CompareFunction;
use awsm_renderer_core::pipeline::depth_stencil::DepthStencilState;
use awsm_renderer_core::pipeline::fragment::{
    BlendComponent, BlendFactor, BlendOperation, BlendState, ColorTargetState,
};
use awsm_renderer_core::pipeline::primitive::PrimitiveState;
use awsm_renderer_core::texture::{
    Extent3d, TextureDescriptor, TextureSampleType, TextureUsage, TextureViewDimension,
};

use crate::bind_group_layout::{
    BindGroupLayoutCacheKey, BindGroupLayoutCacheKeyEntry, BindGroupLayoutKey,
};
use crate::bind_groups::{AwsmBindGroupError, BindGroupRecreateContext};
use crate::error::Result;
use crate::pipeline_layouts::PipelineLayoutCacheKey;
use crate::pipelines::render_pipeline::{RenderPipelineCacheKey, RenderPipelineKey};
use crate::render::RenderContext;
use crate::render_passes::taa::shader::cache_key::ShaderCacheKeyTaaReactiveSeed;
use crate::render_passes::RenderPassInitContext;
use crate::shaders::ShaderCacheKey;

/// One seed pipeline + its group (the plain and the `ssr` variants differ
/// only by binding 4).
struct SeedVariant {
    layout_key: BindGroupLayoutKey,
    pipeline: RenderPipelineKey,
    bind_group: Option<web_sys::GpuBindGroup>,
}

pub struct TaaReactiveSeed {
    plain: SeedVariant,
    ssr: SeedVariant,
    /// Bound at 3 while no `hud_depth` exists.
    placeholder_depth: web_sys::GpuTexture,
    placeholder_depth_view: web_sys::GpuTextureView,
}

impl TaaReactiveSeed {
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let plain = Self::variant(ctx, false).await?;
        let ssr = Self::variant(ctx, true).await?;
        let placeholder_depth = ctx
            .gpu
            .create_texture(
                &TextureDescriptor::new(
                    ctx.render_texture_formats.depth,
                    Extent3d::new(1, Some(1), Some(1)),
                    TextureUsage::new().with_texture_binding(),
                )
                .with_label("TAA Reactive Seed placeholder depth")
                .into(),
            )
            .map_err(crate::render_textures::AwsmRenderTextureError::CreateTexture)?;
        let placeholder_depth_view = placeholder_depth.create_view().map_err(|e| {
            crate::render_textures::AwsmRenderTextureError::CreateTextureView(format!(
                "reactive seed placeholder depth: {e:?}"
            ))
        })?;
        Ok(Self {
            plain,
            ssr,
            placeholder_depth,
            placeholder_depth_view,
        })
    }

    async fn variant(ctx: &mut RenderPassInitContext<'_>, ssr: bool) -> Result<SeedVariant> {
        let layout_key = ctx
            .bind_group_layouts
            .get_key(ctx.gpu, layout_cache_key(ssr))?;
        let shader_cache_key = ShaderCacheKey::from(ShaderCacheKeyTaaReactiveSeed { ssr });
        ctx.shaders
            .ensure_keys(ctx.gpu, vec![shader_cache_key.clone()])
            .await?;
        let shader = ctx.shaders.get_key(ctx.gpu, shader_cache_key).await?;
        let layout = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![layout_key]),
        )?;
        let formats = &*ctx.render_texture_formats;
        // Colour target 0 is the pass's colour attachment: left untouched.
        // (Blend `0 * src + 1 * dst` rather than a zero write mask: the core
        // `ColorTargetState` -> web_sys conversion skips `set_write_mask` for a
        // mask of 0, so it silently becomes the default ALL. Pre-existing
        // renderer-core footgun, listed by the lead.)
        let keep = BlendComponent::new()
            .with_src_factor(BlendFactor::Zero)
            .with_dst_factor(BlendFactor::One)
            .with_operation(BlendOperation::Add);
        let color =
            ColorTargetState::new(formats.color).with_blend(BlendState::new(keep.clone(), keep));
        let reactive =
            crate::render_passes::material_transparent::pipeline::reactive_target(formats);
        let cache_key = RenderPipelineCacheKey::new(shader, layout)
            .with_primitive(PrimitiveState::new())
            .with_depth_stencil(
                DepthStencilState::new(formats.depth)
                    .with_depth_write_enabled(false)
                    .with_depth_compare(CompareFunction::Always),
            )
            .with_push_fragment_targets(vec![color, reactive]);
        let keys = ctx
            .pipelines
            .render
            .ensure_keys(ctx.gpu, ctx.shaders, ctx.pipeline_layouts, vec![cache_key])
            .await?;
        Ok(SeedVariant {
            layout_key,
            pipeline: keys[0],
            bind_group: None,
        })
    }

    /// Every pipeline the seed holds.
    pub(crate) fn pipelines(&self) -> Vec<RenderPipelineKey> {
        vec![self.plain.pipeline, self.ssr.pipeline]
    }

    pub fn recreate(&mut self, ctx: &BindGroupRecreateContext<'_>) -> Result<()> {
        for (variant, ssr) in [(&mut self.plain, false), (&mut self.ssr, true)] {
            let layout = ctx.bind_group_layouts.get(variant.layout_key)?;
            let mut entries = vec![
                BindGroupEntry::new(
                    0,
                    BindGroupResource::Buffer(BufferBinding::new(&ctx.camera.gpu_buffer)),
                ),
                BindGroupEntry::new(
                    1,
                    BindGroupResource::TextureView(Cow::Borrowed(
                        &ctx.render_texture_views.visibility_data,
                    )),
                ),
                BindGroupEntry::new(
                    2,
                    BindGroupResource::Buffer(BufferBinding::new(
                        ctx.meshes.meta.material_gpu_buffer(),
                    )),
                ),
                BindGroupEntry::new(
                    3,
                    BindGroupResource::TextureView(Cow::Borrowed(
                        ctx.render_texture_views
                            .hud_depth
                            .as_ref()
                            .unwrap_or(&self.placeholder_depth_view),
                    )),
                ),
            ];
            if ssr {
                entries.push(BindGroupEntry::new(
                    4,
                    BindGroupResource::TextureView(Cow::Borrowed(
                        &ctx.render_texture_views.reflection_descriptor,
                    )),
                ));
            }
            let descriptor = BindGroupDescriptor::new(layout, Some("TAA Reactive Seed"), entries);
            variant.bind_group = Some(ctx.gpu.create_bind_group(&descriptor.into()));
        }
        Ok(())
    }

    /// Draws the seed into `render_pass` (a world transparent pass with the
    /// reactive attachment). `hud`: the HUD geometry pass ran this frame, so
    /// `hud_depth` marks the HUD's pixels. `k_ssr`: `Some` (> 0) when SSR ran
    /// this frame and reflections add reactivity (the `ssr` variant).
    pub fn draw(
        &self,
        ctx: &RenderContext,
        render_pass: &RenderPassEncoder,
        hud: bool,
        k_ssr: Option<f32>,
    ) -> Result<()> {
        let variant = if k_ssr.is_some() {
            &self.ssr
        } else {
            &self.plain
        };
        let bind_group = variant
            .bind_group
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("TAA Reactive Seed".to_string()))?;
        render_pass.set_pipeline(ctx.pipelines.render.get(variant.pipeline)?);
        render_pass.set_bind_group(0, bind_group, None)?;
        render_pass.draw_with_instance_count_and_first_vertex_and_first_instance(
            3,
            1,
            0,
            seed_instance(hud, ctx.features.depth().clear_value() == 0.0, k_ssr),
        );
        Ok(())
    }

    /// Release the placeholder texture (TAA turned off).
    pub fn destroy(self) {
        self.placeholder_depth.destroy();
    }
}

/// The draw's first instance, the seed's per-draw constants (see the
/// shader): bits 0-1 the HUD mode (0 none, 1 forward Z, 2 reverse Z), bits
/// 2.. `k_ssr` in 1/1024 steps.
pub(crate) fn seed_instance(hud: bool, reverse_z: bool, k_ssr: Option<f32>) -> u32 {
    let hud_mode = match (hud, reverse_z) {
        (false, _) => 0,
        (true, false) => 1,
        (true, true) => 2,
    };
    let k = (k_ssr.unwrap_or(0.0).clamp(0.0, 1.0) * 1024.0).round() as u32;
    hud_mode | (k << 2)
}

fn layout_cache_key(ssr: bool) -> BindGroupLayoutCacheKey {
    let fragment_only = |resource: BindGroupLayoutResource| BindGroupLayoutCacheKeyEntry {
        resource,
        visibility_vertex: false,
        visibility_fragment: true,
        visibility_compute: false,
    };
    let mut entries = vec![
        fragment_only(BindGroupLayoutResource::Buffer(
            BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
        )),
        fragment_only(BindGroupLayoutResource::Texture(
            TextureBindingLayout::new()
                .with_view_dimension(TextureViewDimension::N2d)
                .with_sample_type(TextureSampleType::Uint)
                .with_multisampled(false),
        )),
        fragment_only(BindGroupLayoutResource::Buffer(
            BufferBindingLayout::new().with_binding_type(BufferBindingType::ReadOnlyStorage),
        )),
        fragment_only(BindGroupLayoutResource::Texture(
            TextureBindingLayout::new()
                .with_view_dimension(TextureViewDimension::N2d)
                .with_sample_type(TextureSampleType::Depth)
                .with_multisampled(false),
        )),
    ];
    if ssr {
        // 4: the SSR reflection descriptor (rgba8unorm, render size).
        entries.push(fragment_only(BindGroupLayoutResource::Texture(
            TextureBindingLayout::new()
                .with_view_dimension(TextureViewDimension::N2d)
                .with_sample_type(TextureSampleType::Float)
                .with_multisampled(false),
        )));
    }
    BindGroupLayoutCacheKey { entries }
}

#[cfg(test)]
mod tests {
    use super::seed_instance;

    #[test]
    fn seed_instance_packs_hud_mode_and_k_ssr() {
        assert_eq!(seed_instance(false, false, None), 0);
        assert_eq!(seed_instance(true, false, None), 1);
        assert_eq!(seed_instance(true, true, None), 2);
        assert_eq!(seed_instance(false, false, Some(0.25)) >> 2, 256);
        assert_eq!(seed_instance(true, true, Some(0.5)), 2 | (512 << 2));
    }
}
