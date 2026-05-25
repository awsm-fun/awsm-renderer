use awsm_renderer_core::renderer::AwsmRendererWebGpu;
use slotmap::SlotMap;

use crate::{
    bind_group_layout::BindGroupLayouts,
    error::Result,
    pipeline_layouts::PipelineLayouts,
    pipelines::{render_pipeline::RenderPipelineKey, Pipelines},
    render::RenderContext,
    render_textures::RenderTextureFormats,
    shaders::Shaders,
};

use super::pipelines::{LinePipelines, LinePipelinesDescriptors, LineVariantKey};
use super::types::{GpuLineSegment, LineEntry, LineKey, LINE_UNIFORM_BYTES};

/// Renderer-side state owning the four line pipelines and every registered line strip.
pub struct LineRenderer {
    pub(super) pipelines: LinePipelines,
    pub(super) entries: SlotMap<LineKey, LineEntry>,
    /// Scratch packing buffer reused across `add_line` / `update_line`
    /// calls. The `pack_into` helper clears + extends in place so
    /// per-call allocation cost goes to zero in the steady state.
    /// Held on the renderer (not on each call site) so editor
    /// overlays that re-pack many small line strips per frame
    /// (collider wireframes, point handles, selection outlines)
    /// don't bounce the allocator.
    pub(super) pack_buf: Vec<GpuLineSegment>,
}

/// Pre-resolved layouts + 4 pipeline cache keys for the line renderer.
/// Returned by [`LineRenderer::build_descriptors`] and consumed by
/// [`LineRenderer::from_resolved`] after the cross-system tail pool
/// resolves the keys via one batched `RenderPipelines::ensure_keys`.
pub struct LineRendererDescriptors {
    pub(super) inner: LinePipelinesDescriptors,
}

impl LineRendererDescriptors {
    /// Slice of cache keys to fold into the cross-system render-pipeline
    /// pool. 4 entries, in `LinePipelines::VARIANT_KEYS` order.
    pub fn pipeline_cache_keys(
        &self,
    ) -> &[crate::pipelines::render_pipeline::RenderPipelineCacheKey] {
        &self.inner.pipeline_cache_keys
    }
}

impl LineRenderer {
    /// Loads the four pipeline variants and creates an empty line
    /// registry. Thin wrapper over [`Self::build_descriptors`] +
    /// [`Self::from_resolved`]. The pooled startup path bypasses this
    /// and calls the two halves directly so LineRenderer's 4 pipeline
    /// compiles share the cross-system tail `RenderPipelines::ensure_keys`.
    pub async fn load(
        gpu: &AwsmRendererWebGpu,
        bind_group_layouts: &mut BindGroupLayouts,
        pipeline_layouts: &mut PipelineLayouts,
        pipelines: &mut Pipelines,
        shaders: &mut Shaders,
        formats: &RenderTextureFormats,
    ) -> Result<Self> {
        let descs =
            Self::build_descriptors(gpu, bind_group_layouts, pipeline_layouts, shaders, formats)
                .await?;
        let resolved = pipelines
            .render
            .ensure_keys(
                gpu,
                shaders,
                pipeline_layouts,
                descs.inner.pipeline_cache_keys.clone(),
            )
            .await?;
        Ok(Self::from_resolved(descs, resolved))
    }

    /// Builds layouts + 4 pipeline cache keys. Cache-hit on the line
    /// shader (pre-warmed by `RenderPasses::new`); otherwise sync.
    pub async fn build_descriptors(
        gpu: &AwsmRendererWebGpu,
        bind_group_layouts: &mut BindGroupLayouts,
        pipeline_layouts: &mut PipelineLayouts,
        shaders: &mut Shaders,
        formats: &RenderTextureFormats,
    ) -> Result<LineRendererDescriptors> {
        let inner = LinePipelines::build_descriptors(
            gpu,
            bind_group_layouts,
            pipeline_layouts,
            shaders,
            formats,
        )
        .await?;
        Ok(LineRendererDescriptors { inner })
    }

    /// Folds resolved pipeline keys back into the typed `LineRenderer`.
    pub fn from_resolved(descs: LineRendererDescriptors, resolved: Vec<RenderPipelineKey>) -> Self {
        Self {
            pipelines: LinePipelines::from_resolved(descs.inner, resolved),
            entries: SlotMap::with_key(),
            pack_buf: Vec::new(),
        }
    }

    /// Returns the number of registered lines.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true if there are no registered lines.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl LineRenderer {
    /// Executes the line render pass: re-writes each line's uniform buffer
    /// with the current viewport size + width, then draws all registered lines
    /// against the world-space transparent target. Safe to call with zero
    /// registered lines (it returns early).
    pub fn render(&self, ctx: &RenderContext) -> Result<()> {
        if self.entries.is_empty() {
            return Ok(());
        }
        let msaa = ctx.anti_aliasing.has_msaa_checked()?;
        let viewport_w = ctx.render_texture_views.width as f32;
        let viewport_h = ctx.render_texture_views.height as f32;

        let render_pass = ctx.begin_world_transparent_pass(Some("Line Render Pass"))?;
        let mut current_variant: Option<LineVariantKey> = None;

        for entry in self.entries.values() {
            if entry.segment_count == 0 {
                continue;
            }
            let mut uniform_bytes = [0u8; LINE_UNIFORM_BYTES];
            uniform_bytes[0..4].copy_from_slice(&entry.width_px.to_le_bytes());
            uniform_bytes[4..8].copy_from_slice(&viewport_w.to_le_bytes());
            uniform_bytes[8..12].copy_from_slice(&viewport_h.to_le_bytes());
            entry.uniform_uploader.lock().unwrap().write_dirty_ranges(
                ctx.gpu,
                &entry.uniform_buffer,
                LINE_UNIFORM_BYTES,
                &uniform_bytes[..],
                &[(0, LINE_UNIFORM_BYTES)],
            )?;

            let variant = LineVariantKey {
                depth_test_always: entry.depth_test_always,
                msaa,
            };
            if current_variant != Some(variant) {
                let pipeline_key = self.pipelines.get(variant);
                render_pass.set_pipeline(ctx.pipelines.render.get(pipeline_key)?);
                current_variant = Some(variant);
            }
            render_pass.set_bind_group(0, &entry.bind_group, None)?;
            // 4 vertices per instance (triangle strip quad), N-1 instances.
            // Web GPU instanced non-indexed draw.
            render_pass.draw_with_instance_count(4, entry.segment_count);
        }
        render_pass.end();
        Ok(())
    }
}
