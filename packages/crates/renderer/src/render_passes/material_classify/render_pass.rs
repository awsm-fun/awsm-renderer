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

/// Material classify pass bind groups and the compiled-pipeline cache.
///
/// The classify shader is keyed on the live bucket layout, so the
/// compiled pipeline is looked up per-frame from [`Self::pipeline_cache`]
/// (keyed on the registry's `(dispatch_hash, msaa)`). The cache is the
/// single source of truth: `ensure_scene_pipelines` (run by
/// `prewarm_pipelines` at boot, and on every bucket-layout / MSAA change)
/// installs the pipeline for the active config — including the empty
/// first-party-only layout — before the first frame renders. There is no
/// separate "first-party" key: every consumer awaits
/// `wait_for_pipelines_ready` (→ `prewarm`) before rendering, so the cache
/// is always populated for the active config.
pub struct MaterialClassifyRenderPass {
    pub bind_groups: MaterialClassifyBindGroups,
    /// `(dispatch_hash, msaa) → compiled pipeline`. Populated by
    /// `ensure_scene_pipelines` (via `prewarm_pipelines`). RefCell so the
    /// dispatch path can take a snapshot without `&mut self`.
    pub pipeline_cache: RefCell<HashMap<(u64, Option<u32>), ComputePipelineKey>>,
}

impl MaterialClassifyRenderPass {
    /// Number of cached dynamic classify pipeline keys, keyed by
    /// `(dispatch_hash, msaa)` (leak/observability diagnostics — see `memory_stats`).
    pub fn dynamic_cache_len(&self) -> usize {
        self.pipeline_cache.borrow().len()
    }

    /// Prune `dynamic_pipeline_cache` entries whose `dispatch_hash` no longer
    /// matches `current_dispatch_hash` — a bucket-SET change orphaned them and
    /// the dispatch path (keyed on the live `dispatch_hash`) will never look them
    /// up again. Returns the dropped pool keys so the caller frees them from the
    /// shared compute-pipeline pool. Without this the cache (and the pool it
    /// references) grew unbounded on every registry edit → GPU OOM ("aw snap").
    /// Pruning only non-current entries is dangle-free: nothing dispatches them.
    /// Part of the pipeline-leak fix.
    pub fn prune_dynamic_pipeline_cache(
        &mut self,
        current_dispatch_hash: u64,
    ) -> Vec<ComputePipelineKey> {
        let mut dropped = Vec::new();
        self.pipeline_cache
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
        // Warm the compute-pipeline pool with the active-config classify so the
        // first `ensure_scene_pipelines` is a pool hit; the resulting key is
        // installed into `pipeline_cache` by that ensure (run via
        // `prewarm_pipelines`) before the first frame, so we don't store it here.
        let first_party_entries = crate::dynamic_materials::first_party_bucket_entries();
        MaterialClassifyPipelines::warm_pool(ctx, &bind_groups, &first_party_entries).await?;
        Ok(Self {
            bind_groups,
            pipeline_cache: RefCell::new(HashMap::new()),
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
        // The compiled classify pipeline for the live `(dispatch_hash, msaa)`.
        // `pipeline_cache` is the single source of truth, kept current for the
        // active config by `ensure_scene_pipelines` (run via `prewarm_pipelines`
        // at boot + on every bucket-layout / MSAA change). A miss only happens in
        // the brief window after a config change before its recompile lands; we
        // skip the dispatch that frame (the next frame, once installed, runs it).
        let key = (ctx.dynamic_materials.dispatch_hash_cached(), msaa);
        let pipeline_key_opt = self.pipeline_cache.borrow().get(&key).copied();
        let Some(pipeline_key) = pipeline_key_opt else {
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
