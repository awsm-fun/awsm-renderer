//! TAA deforming-mesh velocity overlay (32 B/sample fallback, docs/TAA.md
//! §3.10).
//!
//! On the prep-reconstruction path the prep pass writes every pixel's vector
//! from depth and the rigid object matrices, which cannot see skinning,
//! morphs, vertex hooks or per-instance motion. This pass then re-draws ONLY
//! the meshes that deformed this frame (plus vertex-hook meshes, every
//! frame) with the geometry vertex stage's current and previous evaluation
//! and stores the exact per-fragment vector over the prep's, from the
//! fragment stage (`textureStore` into the same `rg32float` storage
//! texture). A fragment writes only where it is the surface the geometry pass
//! kept (same triangle, mesh meta and instance id in the visibility and
//! barycentric targets), so each pixel has one writer whatever the depth
//! precision of the two pipelines.
//!
//! Zero cost when off: built only while TAA runs on the fallback path with a
//! storage-buffer limit that fits ([`crate::anti_alias::TaaFallbackOverlay`]);
//! skipped on frames where no such mesh is visible.

pub mod render_pass;
pub mod shader;

use crate::bind_groups::BindGroupCreate;
use crate::error::Result;
use crate::render_passes::RenderPassInitContext;
use crate::AwsmRenderer;

impl AwsmRenderer {
    /// Build the velocity overlay while TAA runs on the fallback path with a
    /// storage-buffer limit that fits it, drop it otherwise. Called by
    /// `set_anti_aliasing` (and the builder). Rebuilt when the covered set
    /// (the limit override) or the debug check changes.
    pub(crate) async fn sync_taa_overlay_pass(&mut self) -> Result<()> {
        let tier = self
            .render_textures
            .formats
            .taa_fallback_overlay(&self.anti_aliasing);
        let debug = self.taa_overlay_debug_check;
        let keep = self
            .render_passes
            .taa_overlay
            .as_ref()
            .is_some_and(|p| tier.enabled() && p.tier() == tier && p.debug() == debug);
        if keep {
            return Ok(());
        }
        if let Some(pass) = self.render_passes.taa_overlay.take() {
            self.pipelines.render.release_reserved(pass.pipelines());
            pass.destroy();
        }
        if !tier.enabled() {
            return Ok(());
        }
        let custom_vertex: Vec<_> = if tier.vertex_hooks() {
            self.dynamic_materials
                .iter()
                .map(|(id, _)| id)
                .collect::<Vec<_>>()
                .into_iter()
                .filter_map(|id| {
                    self.dynamic_materials
                        .vertex_shader_info_for(id)
                        .map(|info| (id, info))
                })
                .collect()
        } else {
            Vec::new()
        };
        let mut ctx = RenderPassInitContext {
            gpu: &self.gpu,
            bind_group_layouts: &mut self.bind_group_layouts,
            pipeline_layouts: &mut self.pipeline_layouts,
            pipelines: &mut self.pipelines,
            shaders: &mut self.shaders,
            render_texture_formats: &mut self.render_textures.formats,
            textures: &mut self.textures,
            features: &self.features,
            anti_aliasing: &self.anti_aliasing,
            post_processing: &self.post_processing,
            prep_config: &self.prep_config,
            max_edge_budget: self
                .material_edge_buffers
                .as_ref()
                .map(|b| b.max_edge_budget)
                .unwrap_or(
                    crate::render_passes::material_opaque::edge_buffers::DEFAULT_MAX_EDGE_BUDGET_DESKTOP,
                ),
        };
        let geometry = &self.render_passes.geometry;
        let mut pass = render_pass::TaaOverlayRenderPass::new(
            &mut ctx,
            &geometry.bind_groups,
            &geometry.masked_bind_group,
            tier,
            debug,
        )
        .await?;
        for (shader_id, info) in &custom_vertex {
            pass.ensure_custom_vertex_variant(
                &mut ctx,
                &geometry.masked_bind_group,
                *shader_id,
                info,
            )
            .await?;
        }
        self.render_passes.taa_overlay = Some(pass);
        // Group 1's TAA variant (current + previous transforms) exists only
        // once its layout was asked for, which the overlay just did; and the
        // overlay's own group 2.
        self.bind_groups
            .mark_create(BindGroupCreate::MotionHistoryBuffers);
        self.bind_groups.mark_create(BindGroupCreate::TaaResources);
        Ok(())
    }

    /// TAA test hook: build the fallback's velocity overlay as its debug
    /// check variant, which counts per mesh the pixels it wrote and how many
    /// of them a depth-EQUAL or the production depth compare would have lost
    /// ([`Self::debug_read_taa_overlay_check`]). Takes effect at the next
    /// `set_anti_aliasing`. Not for shipping (an extra storage buffer, atomics
    /// and a depth read per fragment).
    #[doc(hidden)]
    pub fn set_taa_debug_overlay_check(&mut self, on: bool) {
        self.taa_overlay_debug_check = on;
    }

    /// TAA test hook: the overlay debug check of the LAST rendered frame,
    /// one entry per mesh the overlay drew (see
    /// [`render_pass::TaaOverlayMeshCheck`]). Errors unless the overlay was
    /// built with [`Self::set_taa_debug_overlay_check`]. The copy is
    /// submitted now; only the map is awaited.
    #[doc(hidden)]
    pub fn debug_read_taa_overlay_check(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<render_pass::TaaOverlayMeshCheck>>>>,
    > {
        match self.render_passes.taa_overlay.as_ref() {
            Some(pass) => pass.debug_read_check(&self.gpu, &self.meshes.meta),
            None => Box::pin(async {
                Err(crate::error::AwsmError::from(
                    crate::bind_groups::AwsmBindGroupError::NotFound(
                        "TAA overlay: not on the fallback path with an overlay".to_string(),
                    ),
                ))
            }),
        }
    }

    /// Cost measurement: draw the velocity overlay `n` times per frame (1 =
    /// normal); `(T(n) - T(1)) / (n - 1)` is one overlay pass.
    #[doc(hidden)]
    pub fn set_taa_debug_overlay_repeat(&mut self, n: u32) {
        if let Some(pass) = self.render_passes.taa_overlay.as_ref() {
            pass.set_debug_repeat(n);
        }
    }

    /// The meshes the velocity overlay redraws this frame (fallback only;
    /// visible or not). Test hook.
    #[doc(hidden)]
    pub fn taa_overlay_draw_count(&self) -> usize {
        self.meshes.meta.non_rigid_overlay_draws()
    }
}
