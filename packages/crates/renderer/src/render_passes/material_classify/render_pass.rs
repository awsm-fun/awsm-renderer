//! Material classify render pass execution. Produces per-`shader_id`
//! tile buckets + indirect-dispatch args consumed by the opaque
//! material pipelines.

use std::cell::RefCell;
use std::collections::HashMap;

use awsm_renderer_core::command::compute_pass::ComputePassDescriptor;

use crate::{
    error::Result,
    pipelines::compute_pipeline::ComputePipelineKey,
    render::RenderContext,
    render_passes::{
        material_classify::{
            bind_group::MaterialClassifyBindGroups, pipeline::MaterialClassifyPipelines,
        },
        RenderPassInitContext,
    },
};

/// Material classify pass bind groups and pipelines.
///
/// The base `pipelines` field holds the first-party-only pipeline keys
/// compiled at builder time. When dynamic materials register, a
/// different bucket-entry list is needed; the new pipeline gets
/// compiled via [`crate::AwsmRenderer::prewarm_pipelines`] and looked
/// up here at dispatch time via `dynamic_pipeline_cache`.
pub struct MaterialClassifyRenderPass {
    pub bind_groups: MaterialClassifyBindGroups,
    pub pipelines: MaterialClassifyPipelines,
    /// `(dispatch_hash, msaa) → compiled pipeline`. Populated by
    /// `prewarm_pipelines`. RefCell so the dispatch path can take a
    /// snapshot without `&mut self`.
    ///
    /// Previously keyed on `(Vec<BucketEntry>, Option<u32>)` — the
    /// dispatch path allocated a fresh `Vec` every frame to probe.
    /// Now the key is `(u64, Option<u32>)` (the registry's cached
    /// `dispatch_hash`); the per-frame probe stays alloc-free.
    pub dynamic_pipeline_cache: RefCell<HashMap<(u64, Option<u32>), ComputePipelineKey>>,
}

impl MaterialClassifyRenderPass {
    /// Number of cached dynamic classify pipeline keys, keyed by
    /// `(dispatch_hash, msaa)` (leak/observability diagnostics — see `memory_stats`).
    pub fn dynamic_cache_len(&self) -> usize {
        self.dynamic_pipeline_cache.borrow().len()
    }

    /// Prune `dynamic_pipeline_cache` entries whose `dispatch_hash` no longer
    /// matches `current_dispatch_hash` — a bucket-SET change orphaned them and
    /// the dispatch path (keyed on the live `dispatch_hash`) will never look them
    /// up again. Returns the dropped pool keys so the caller frees them from the
    /// shared compute-pipeline pool. Without this the cache (and the pool it
    /// references) grew unbounded on every registry edit → GPU OOM ("aw snap").
    /// Pruning only non-current entries is dangle-free: nothing dispatches them.
    /// Part of the pipeline-leak fix; see docs/plans/mesh-pipeline-overhaul.md.
    pub fn prune_dynamic_pipeline_cache(
        &mut self,
        current_dispatch_hash: u64,
    ) -> Vec<ComputePipelineKey> {
        let mut dropped = Vec::new();
        self.dynamic_pipeline_cache
            .borrow_mut()
            .retain(|(hash, _msaa), key| {
                if *hash == current_dispatch_hash {
                    true
                } else {
                    dropped.push(*key);
                    false
                }
            });
        dropped
    }

    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let bind_groups = MaterialClassifyBindGroups::new(ctx).await?;
        let first_party_entries = crate::dynamic_materials::first_party_bucket_entries();
        let pipelines =
            MaterialClassifyPipelines::new(ctx, &bind_groups, &first_party_entries).await?;
        Ok(Self {
            bind_groups,
            pipelines,
            dynamic_pipeline_cache: RefCell::new(HashMap::new()),
        })
    }

    /// Dispatches the classify shader: one workgroup per 8×8 tile of
    /// the visibility buffer. Per-workgroup atomic-or builds a bucket
    /// mask, then thread 0 atomically appends the tile to each
    /// bucket bit it touched.
    ///
    /// When dynamic materials are registered, the dispatch uses a
    /// pre-compiled "dynamic-aware" pipeline keyed on the current
    /// bucket_entries. `prewarm_pipelines` populates this cache;
    /// without prewarm the dispatch falls back to the first-party-only
    /// pipeline (which still classifies first-party shader_ids
    /// correctly, but dynamic-id pixels won't enter any bucket and
    /// won't be shaded).
    pub fn render(&self, ctx: &RenderContext) -> Result<()> {
        let compute_pass = ctx.command_encoder.begin_compute_pass(Some(
            &ComputePassDescriptor::new(Some("Material Classify Pass")).into(),
        ));

        let msaa = ctx.anti_aliasing.msaa_sample_count;
        // First-party fallback for the active MSAA. Lazy-pool: this
        // is `None` if the user changed MSAA mid-session without
        // calling `set_anti_aliasing` first. The match below skips
        // dispatch in that case (a no-op classify produces an empty
        // bucket — the opaque/transparent passes' "skip if no work"
        // paths handle the empty result correctly).
        let first_party_key = if msaa.is_some() {
            self.pipelines.multisampled_pipeline_key
        } else {
            self.pipelines.singlesampled_pipeline_key
        };
        let pipeline_key_opt = if !ctx.dynamic_materials.is_empty() {
            // `(dispatch_hash, msaa)` keyed lookup — both halves are
            // `Copy`, so the probe is alloc-free on the hot path
            // (vs the previous `Vec<BucketEntry>` clone-and-hash).
            let key = (ctx.dynamic_materials.dispatch_hash_cached(), msaa);
            self.dynamic_pipeline_cache
                .borrow()
                .get(&key)
                .copied()
                .or(first_party_key)
        } else {
            first_party_key
        };
        let Some(pipeline_key) = pipeline_key_opt else {
            // No compiled variant for the current MSAA — skip
            // dispatch. Caller should have awaited
            // `AwsmRenderer::set_anti_aliasing` before changing the
            // mode if they wanted classify to run this frame.
            compute_pass.end();
            return Ok(());
        };

        compute_pass.set_pipeline(ctx.pipelines.compute.get(pipeline_key)?);
        compute_pass.set_bind_group(0, self.bind_groups.get_bind_group()?, None)?;

        let workgroups_x = ctx.render_texture_views.width.div_ceil(8);
        let workgroups_y = ctx.render_texture_views.height.div_ceil(8);
        compute_pass.dispatch_workgroups(workgroups_x, Some(workgroups_y), Some(1));

        compute_pass.end();
        Ok(())
    }
}
