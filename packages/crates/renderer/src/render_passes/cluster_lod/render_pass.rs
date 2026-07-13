//! Cluster-LOD cut compute pass (Phase B, B.2).
//!
//! Built eagerly (like `light_culling` / `material_prep`) and gated by
//! `virtual_geometry`. Holds the cut pipeline + bind-group layout; the per-mesh
//! [`ClusterLodBuffers`] and the bind-group instance are created/recreated when a
//! cluster mesh loads. Inert (no dispatch) until a cluster mesh is present.

use awsm_renderer_core::command::compute_pass::ComputePassDescriptor;
use awsm_renderer_core::renderer::AwsmRendererWebGpu;
use glam::{Mat4, Vec3};

use crate::bind_group_layout::BindGroupLayouts;
use crate::cluster_lod::{select_cut_per_cluster, ClusterPage};
use crate::error::Result;
use crate::meshes::MeshKey;
use crate::render::RenderContext;
use crate::render_passes::cluster_lod::{
    bind_group::{ClusterCompactionBindGroups, ClusterCutBindGroups},
    buffers::ClusterLodBuffers,
    pipeline::ClusterLodPipelines,
    planner::{self, GroupGraph, PagingOp, PlannerCaps, PlannerScratch},
};
use crate::render_passes::RenderPassInitContext;

pub struct ClusterLodRenderPass {
    /// Content-lazy (axis 1): `None` until [`Self::ensure_pipelines_compiled`]
    /// runs at the first commit with a resident cluster mesh
    /// (`ensure_config_pipelines`). A `virtual_geometry` build that never
    /// loads a cluster mesh compiles zero cluster pipelines. The per-frame
    /// [`Self::dispatch_all`] no-ops while missing (the compacted draw args
    /// stay zeroed, so the cluster mesh simply isn't drawn until the commit).
    pub pipelines: Option<ClusterLodPipelines>,
    /// Bind-group prototypes (layout key + paging flag, NO bound group) cloned per
    /// mesh state in [`Self::upload_pages`]. The cut/compaction layouts are identical
    /// for every cluster mesh; only the bound buffers differ — so we capture the
    /// layout once (it needs the init ctx) and stamp a fresh bind group per mesh.
    proto_cut_bg: ClusterCutBindGroups,
    proto_compaction_bg: ClusterCompactionBindGroups,
    /// Resident cluster meshes — one per nanite asset, keyed by `render_mesh`.
    /// SEVERAL render simultaneously: each owns its page-pool buffers, cut +
    /// compaction bind groups, page count, and (under `cluster_paging`) its own
    /// per-frame paging manager. Empty until a cluster mesh loads.
    pub states: Vec<ClusterMeshState>,
}

/// One resident cluster mesh's GPU state (see [`ClusterLodRenderPass::states`]).
pub struct ClusterMeshState {
    /// The render mesh `M` (`add_raw_mesh(cm.positions, cm.indices)`) whose exploded
    /// vertex buffer this cut's compacted indirect stream draws into (M's own draw is
    /// suppressed). The state's identity key.
    pub render_mesh: MeshKey,
    /// Page count (the cut dispatch bound).
    pub cluster_count: u32,
    /// Resident triangle count this mesh charges against the global residency
    /// budget (the budget-capped resident set selected at load — `m_indices/3`,
    /// NOT the padded paging pool). Summed across states by
    /// [`ClusterLodRenderPass::resident_tris_total`] so a later mesh's load can cap
    /// itself against what's already resident (bounded total VRAM, any mesh count).
    pub resident_tris: u32,
    pub buffers: ClusterLodBuffers,
    pub bind_groups: ClusterCutBindGroups,
    pub compaction_bind_groups: ClusterCompactionBindGroups,
    /// Gap-B dynamic paging (CPU-driven). `Some` only under `cluster_paging`; holds
    /// the FULL un-clamped DAG + drives per-frame residency for THIS mesh.
    pub paging: Option<ClusterPaging>,
}

/// Gap-B dynamic-paging manager (CPU-driven design — see NORTHSTAR-GAPS step 3).
///
/// At our cluster counts (≤~80k for a 5–10M-tri asset) the CPU runs the LOD cut
/// itself each frame against the FULL un-clamped DAG and diffs the desired set
/// against current residency — no GPU feedback/readback. This struct holds that
/// persistent state plus pooled scratch (no per-frame heap allocation).
///
/// The per-frame decision logic itself is the pure
/// [`planner`](crate::render_passes::cluster_lod::planner) (unit-tested
/// natively); this struct owns its inputs (residency bookkeeping + the
/// init-time [`GroupGraph`]) and applies the planned ops through the GPU-write
/// paths in [`ClusterLodRenderPass::stream_paging`].
pub struct ClusterPaging {
    /// The full DAG's un-clamped cluster pages (`lod_error`/`parent_error` are the
    /// bake's real interval — NOT the resident frontier's clamped `0`/`MAX`). The
    /// CPU cut's input. Each page's `first_index`/`index_count` index into
    /// [`Self::indices`] (the original `cm.indices`).
    pub pages: Vec<ClusterPage>,
    /// Reused scratch for the per-frame desired cut (cluster ids). Cleared+refilled
    /// each frame ⇒ no per-frame allocation.
    desired: Vec<u32>,
    /// Frames the paging update has run (diagnostics + LRU timestamps).
    frame: u64,
    /// Last desired-count we logged — log only on change, so the on-device console
    /// shows the cut tracking the camera without per-frame spam.
    last_desired_logged: usize,

    // ── CPU geometry, to build a streamed slot's exploded bytes ──
    /// Original unique-vertex positions (`cm.positions`); a slot's exploded verts
    /// gather these by `indices[page.first_index + k]`.
    positions: Vec<[f32; 3]>,
    /// Original unique-vertex normals (`cm.normals`); empty ⇒ the streamer defaults.
    normals: Vec<[f32; 3]>,
    /// Original triangle index buffer (`cm.indices`) the pages' spans address.
    indices: Vec<u32>,

    // ── residency bookkeeping in FULL-DAG cluster space (the page-pool state) ──
    /// `resident[cluster_id]` = its page-pool slot, or `-1` (absent). Length =
    /// `pages.len()`.
    resident: Vec<i32>,
    /// `slot_cluster[slot]` = the full-DAG cluster currently in that slot, or `-1`
    /// (free). Length = `pool_slots`.
    slot_cluster: Vec<i32>,
    /// `slot_last_used[slot]` = the `frame` the slot was last in the desired cut
    /// (the planner's LRU / coldness key). Length = `pool_slots`.
    slot_last_used: Vec<u64>,
    /// Fixed page-pool capacity (slots) — the VRAM bound.
    pool_slots: usize,

    // ── the planner (20b-iv-b-2c) ──
    /// DAG simplification-group graph, reconstructed once at init from the
    /// pages' bit-exact shared group spheres (no bake-format change).
    graph: GroupGraph,
    /// Per-frame op caps + the rule-d cold horizon.
    caps: PlannerCaps,
    /// Pooled planner scratch (no per-frame allocation).
    scratch: PlannerScratch,
    /// The planned ops, refilled each frame (pooled).
    ops: Vec<PagingOp>,

    // ── pooled per-frame scratch (no per-frame heap allocation) ──
    /// One slot's exploded visibility bytes (`PAGE_VERTS*56`).
    slot_bytes_scratch: Vec<u8>,
    /// One slot's triangle-order corner indices (`PAGE_VERTS`) + slot-relative
    /// source indices, reused per stream.
    corner_scratch: Vec<u32>,
    src_idx_scratch: Vec<u32>,
    /// Pooled byte staging for the per-slot GPU writes (page entry + source-indices
    /// span), reused every stream so the buffer-write helpers don't allocate.
    page_bytes_scratch: Vec<u8>,
    src_bytes_scratch: Vec<u8>,
    /// Exploded verts per page-pool slot (= `CLUSTER_PAGE_VERTS`); the slot byte math.
    page_verts: usize,
}

/// Geometry + initial residency seed for the paging manager (step 20b-iii). Keeps
/// [`ClusterLodRenderPass::init_paging`] from growing an unwieldy argument list.
pub struct ClusterPagingInit {
    /// Full-DAG un-clamped pages (`first_index`/`index_count` into `indices`).
    pub pages: Vec<ClusterPage>,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
    /// `slot_cluster[slot]` = the full-DAG cluster id initially uploaded into that
    /// slot (the load-time frontier, in slot order). Its length is the pool size.
    pub slot_cluster: Vec<i32>,
    /// Exploded verts per slot (`CLUSTER_PAGE_VERTS` from the loader).
    pub page_verts: usize,
}

impl ClusterPaging {
    fn new(init: ClusterPagingInit) -> Self {
        let ClusterPagingInit {
            pages,
            positions,
            normals,
            indices,
            slot_cluster,
            page_verts,
        } = init;
        let pool_slots = slot_cluster.len();
        let pages_len = pages.len();
        // Invert slot_cluster → resident (full-DAG cluster space).
        let mut resident = vec![-1i32; pages_len];
        for (slot, &cid) in slot_cluster.iter().enumerate() {
            if cid >= 0 && (cid as usize) < resident.len() {
                resident[cid as usize] = slot as i32;
            }
        }
        // Reconstruct the DAG's simplification-group graph from the pages'
        // bit-exact shared group spheres (once; the planner's rules c/d walk it
        // every frame without allocating).
        let graph = GroupGraph::build(&pages);
        let gs = graph.stats();
        tracing::debug!(
            "cluster paging: group graph — {} groups over {} clusters ({} roots, {} leaves, \
             {} parentless groups) [20b-iv-b-2c]",
            gs.groups,
            pages_len,
            gs.roots,
            gs.leaves,
            gs.parentless_groups,
        );
        if gs.unmatched_nonleaf > 0 {
            // Would mean the bake stopped writing group spheres by copy — the
            // affected clusters simply never retire per-group (rules c-down/d
            // skip them); rendering stays correct.
            tracing::warn!(
                "cluster paging: {} non-leaf clusters matched no group (bit-exact key miss)",
                gs.unmatched_nonleaf
            );
        }
        Self {
            pages,
            desired: Vec::new(),
            frame: 0,
            last_desired_logged: usize::MAX,
            positions,
            normals,
            indices,
            resident,
            slot_cluster,
            slot_last_used: vec![0u64; pool_slots],
            pool_slots,
            graph,
            caps: PlannerCaps::default(),
            scratch: PlannerScratch::new(pages_len),
            ops: Vec::new(),
            slot_bytes_scratch: Vec::new(),
            corner_scratch: Vec::new(),
            src_idx_scratch: Vec::new(),
            page_bytes_scratch: Vec::new(),
            src_bytes_scratch: Vec::new(),
            page_verts,
        }
    }
}

impl ClusterLodRenderPass {
    /// Builds the bind-group layout prototypes only — cheap layout
    /// registrations, no Dawn compile. The cut + compaction pipelines are
    /// content-lazy: [`Self::ensure_pipelines_compiled`] builds them at the
    /// first commit with a resident cluster mesh, which is also where
    /// `cluster_cut.wgsl` gets its on-device validation.
    pub fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let proto_cut_bg = ClusterCutBindGroups::new(ctx)?;
        let proto_compaction_bg = ClusterCompactionBindGroups::new(ctx)?;
        Ok(Self {
            pipelines: None,
            proto_cut_bg,
            proto_compaction_bg,
            states: Vec::new(),
        })
    }

    /// Compile the cut + compaction pipelines if a cluster mesh is resident
    /// and they aren't compiled yet. **Creating the cut pipeline validates
    /// `cluster_cut.wgsl` on-device** (the GPU driver compiles it here) — the
    /// first on-GPU checkpoint for the per-cluster cut. Called from
    /// `ensure_config_pipelines` (every `commit_load`), so the scene-loader's
    /// `upload_cluster_pages` → `commit_load` transaction lands with the
    /// pipelines GPU-resident before the first committed frame. Idempotent +
    /// cheap when warm (cache-keyed). Returns whether a compile ran.
    pub async fn ensure_pipelines_compiled(
        &mut self,
        ctx: &mut RenderPassInitContext<'_>,
    ) -> Result<bool> {
        if self.pipelines.is_some() || self.states.is_empty() {
            return Ok(false);
        }
        self.pipelines = Some(
            ClusterLodPipelines::new(ctx, &self.proto_cut_bg, &self.proto_compaction_bg).await?,
        );
        Ok(true)
    }

    /// The resident state for a render mesh, if loaded.
    pub fn state(&self, render_mesh: MeshKey) -> Option<&ClusterMeshState> {
        self.states.iter().find(|s| s.render_mesh == render_mesh)
    }

    fn state_mut(&mut self, render_mesh: MeshKey) -> Option<&mut ClusterMeshState> {
        self.states
            .iter_mut()
            .find(|s| s.render_mesh == render_mesh)
    }

    /// Total resident triangles across every loaded cluster mesh — what a new
    /// mesh's load caps itself against so the SUM stays within the global residency
    /// budget regardless of mesh count.
    pub fn resident_tris_total(&self) -> usize {
        self.states.iter().map(|s| s.resident_tris as usize).sum()
    }

    /// Drop a cluster mesh's GPU state (e.g. when its node is removed).
    pub fn remove_mesh(&mut self, render_mesh: MeshKey) {
        self.states.retain(|s| s.render_mesh != render_mesh);
    }

    /// Install the Gap-B paging manager with the full DAG + CPU geometry + the
    /// initial residency seed (called at mesh load, only under `cluster_paging`).
    /// Idempotent per mesh: replaces any prior state. The drawn set is still
    /// whatever [`Self::upload_pages`] uploaded (the load-time frontier in slots);
    /// this arms the per-frame CPU cut (step 20a) and seeds the page-pool residency
    /// bookkeeping + CPU geometry the per-frame streamer (step 20b-iv) consumes.
    pub fn init_paging(&mut self, render_mesh: MeshKey, init: ClusterPagingInit) {
        if let Some(state) = self.state_mut(render_mesh) {
            state.paging = Some(ClusterPaging::new(init));
        } else {
            tracing::warn!(
                "init_paging: no cluster state for the render mesh (call upload_pages first)"
            );
        }
    }

    /// Per-frame Gap-B dynamic paging (CPU-driven; step 20b-iv-b-2c). No-op unless
    /// paging is armed + a cluster render mesh + buffers exist.
    ///
    /// Computes the camera-adaptive complete antichain (`select_cut_per_cluster`
    /// over the full DAG), hands it to the pure [`planner`] (which streams
    /// desired∧non-resident clusters into free slots, retires per-group-redundant
    /// residents, and — under pool pressure — coarsens cold groups by write-over,
    /// see the planner docs for the crack-free argument), then applies the planned
    /// ops through the GPU-write paths: a `Load` writes the slot's exploded
    /// visibility verts, its GPU page (clamped always-draw), its slot-aligned
    /// source indices, and its residency entry; an `Evict` clears the residency
    /// entry (the cut skips the slot). All scratch is pooled — zero steady-state
    /// heap allocation.
    pub fn stream_paging(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        meshes: &crate::meshes::Meshes,
        cam_pos: Vec3,
        tan_half_fov_y: f32,
        viewport_h: f32,
        pixel_budget: f32,
    ) -> Result<()> {
        for state in &mut self.states {
            let render_mesh = state.render_mesh;
            let buffers = &state.buffers;
            let Some(p) = state.paging.as_mut() else {
                continue;
            };
            p.frame += 1;
            select_cut_per_cluster(
                &p.pages,
                &Mat4::IDENTITY,
                cam_pos,
                tan_half_fov_y,
                viewport_h,
                pixel_budget,
                &mut p.desired,
            );

            // Plan this frame's ops (pure CPU — the residency bookkeeping is
            // updated in place; `p.ops` receives the GPU work, in order).
            let stats = planner::plan(
                &p.desired,
                p.frame,
                &p.graph,
                &mut p.resident,
                &mut p.slot_cluster,
                &mut p.slot_last_used,
                &p.caps,
                &mut p.scratch,
                &mut p.ops,
            );

            // Apply the ops via the slot-write paths. All of a frame's writes land
            // before the next submit, so the batch is atomic to the draw — the
            // planner's cover invariant holds at every drawn frame.
            let data_buf = meshes.visibility_geometry_data_gpu_buffer();
            let data_off = meshes.visibility_geometry_data_buffer_offset(render_mesh)?;
            let pv = p.page_verts;
            const STRIDE: usize = 56; // visibility vertex bytes
            for op in &p.ops {
                match *op {
                    PagingOp::Load { cluster, slot } => {
                        let cluster = cluster as usize;
                        let slot = slot as usize;
                        let page = p.pages[cluster];
                        let ic = (page.index_count as usize).min(pv);
                        let f = page.first_index as usize;
                        // Slot corner indices (triangle order), padded to a full slot.
                        p.corner_scratch.clear();
                        for k in 0..pv {
                            let v = if k < ic {
                                p.indices[f + k]
                            } else if ic > 0 {
                                p.indices[f]
                            } else {
                                0
                            };
                            p.corner_scratch.push(v);
                        }
                        crate::mesh_pack::pack_visibility_slot_bytes(
                            &p.positions,
                            &p.normals,
                            &p.corner_scratch,
                            slot,
                            pv,
                            awsm_renderer_core::pipeline::primitive::FrontFace::Ccw,
                            &mut p.slot_bytes_scratch,
                        );
                        gpu.write_buffer(
                            data_buf,
                            Some(crate::renderer::cluster_slot_data_offset(
                                data_off,
                                slot,
                                pv * STRIDE,
                            )),
                            p.slot_bytes_scratch.as_slice(),
                            None,
                            None,
                        )?;
                        // The slot's GPU page: clamp always-draw, slot-aligned source span.
                        let mut gp = page;
                        gp.lod_error = 0.0;
                        gp.parent_error = f32::MAX;
                        gp.first_index = (slot * pv) as u32;
                        gp.index_count = ic as u32;
                        buffers.write_page_entry(gpu, slot, &gp, &mut p.page_bytes_scratch)?;
                        p.src_idx_scratch.clear();
                        for k in 0..ic {
                            p.src_idx_scratch.push((slot * pv + k) as u32);
                        }
                        buffers.write_source_indices_span(
                            gpu,
                            (slot * pv) as u32,
                            &p.src_idx_scratch,
                            &mut p.src_bytes_scratch,
                        )?;
                        // GPU resident is SLOT-indexed: mark this slot drawable
                        // (value = slot). A write-over load (rule d) simply
                        // re-marks the slot it replaces.
                        buffers.write_resident_entry(gpu, slot, slot as i32)?;
                    }
                    PagingOp::Evict { slot } => {
                        buffers.write_resident_entry(gpu, slot as usize, -1)?;
                    }
                }
            }

            let desired = p.desired.len();
            if stats.streamed > 0
                || stats.evicted > 0
                || stats.coarsened > 0
                || desired != p.last_desired_logged
            {
                p.last_desired_logged = desired;
                tracing::info!(
                    "cluster paging (Gap B, frame {}): desired={desired} (full DAG={}, pool={}), \
                 streamed {}, evicted {}, coarsened={} groups [20b-iv-b-2c]",
                    p.frame,
                    p.pages.len(),
                    p.pool_slots,
                    stats.streamed,
                    stats.evicted,
                    stats.coarsened,
                );
            }
        }
        Ok(())
    }

    /// Upload a cluster mesh's pages (once, at mesh load): (re)allocate the
    /// buffers to hold `pages`, write them, and rebuild the bind group against
    /// the new buffers. Idempotent per mesh.
    pub fn upload_pages(
        &mut self,
        render_mesh: MeshKey,
        gpu: &AwsmRendererWebGpu,
        layouts: &BindGroupLayouts,
        pages: &[ClusterPage],
        indices: &[u32],
        resident_tris: u32,
    ) -> Result<()> {
        let count = pages.len() as u32;
        let index_count = indices.len() as u32;
        // Find-or-create this mesh's state (clone the bind-group prototypes into
        // locals first so we don't borrow `self` while pushing to `self.states`).
        if self.state(render_mesh).is_none() {
            let bind_groups = self.proto_cut_bg.clone();
            let compaction_bind_groups = self.proto_compaction_bg.clone();
            let buffers = ClusterLodBuffers::with_capacity(gpu, count.max(1), index_count.max(3))?;
            self.states.push(ClusterMeshState {
                render_mesh,
                cluster_count: 0,
                resident_tris: 0,
                buffers,
                bind_groups,
                compaction_bind_groups,
                paging: None,
            });
        }
        let state = self.state_mut(render_mesh).unwrap();
        state.buffers.ensure_capacity(gpu, count, index_count)?;
        state.buffers.write_pages(gpu, pages)?;
        state.buffers.write_source_indices(gpu, indices)?;
        state.cluster_count = count;
        state.resident_tris = resident_tris;
        state.bind_groups.recreate(gpu, layouts, &state.buffers)?;
        state
            .compaction_bind_groups
            .recreate(gpu, layouts, &state.buffers)?;
        Ok(())
    }

    /// Upload the Gap-B residency table (`cluster_id → slot`). Must be called after
    /// [`Self::upload_pages`] (the buffers must exist). No-op if no cluster mesh is
    /// loaded. Only the `cluster_paging` path calls this.
    pub fn upload_resident(
        &mut self,
        render_mesh: MeshKey,
        gpu: &AwsmRendererWebGpu,
        layouts: &BindGroupLayouts,
        resident: &[i32],
    ) -> Result<()> {
        let Some(state) = self.state_mut(render_mesh) else {
            return Ok(());
        };
        state.buffers.write_resident(gpu, resident)?;
        // The paging cut bind group has a `resident` entry that could only be
        // bound once the table existed — (re)build it now that it does.
        state.bind_groups.recreate(gpu, layouts, &state.buffers)?;
        Ok(())
    }

    /// Dispatch the per-cluster cut + compaction for EVERY resident cluster mesh.
    /// For each state: write the per-frame params, run the `cut` compute over
    /// `ceil(cluster_count/64)` workgroups (writes 0/1 per cluster into
    /// `selected`), then reset the indirect args and run the compaction (packs the
    /// selected clusters' index pages into `compacted_indices` + bumps
    /// `draw_args.index_count`). After this each state's `draw_args` drives one
    /// `drawIndexedIndirect(compacted_indices)` from the geometry pass's cluster
    /// draw override. MUST run before the geometry pass (it reads the results this
    /// frame). No-op if no cluster mesh is loaded. (Instance world is identity for
    /// now — the per-instance world is the follow-up; the camera + viewport are
    /// live.)
    ///
    /// `first_instance` is per state: the render mesh M's meta slot, so the
    /// indirect draw's vertex shader resolves M's material meta
    /// (`geometry_mesh_metas[instance_index]`).
    ///
    /// Returns the readback kick — one `(readback_buffer, cluster_count)` per
    /// resident cluster mesh — when the cadence (frame 5, then every 30) fires and
    /// no readback is in flight. The caller copies each `draw_args` → that mesh's
    /// readback inside the encoder and maps them all after submit, summing the drawn
    /// index counts (diagnostics; logs the total drawn cut + mesh count on change).
    /// One entry per mesh keeps the totals correct with several resident meshes.
    pub fn dispatch_all(
        &self,
        ctx: &RenderContext,
        readback: &std::sync::Mutex<crate::renderer::ClusterCutReadback>,
        cam_pos: Vec3,
        tan_half_fov_y: f32,
        viewport_h: f32,
        pixel_budget: f32,
    ) -> Result<Option<Vec<(web_sys::GpuBuffer, u32)>>> {
        // Content-lazy: pipelines compile at the commit that loaded the first
        // cluster mesh. Between `upload_pages` and that commit, skip — the
        // zeroed `draw_args` mean the cluster mesh isn't drawn yet, matching
        // the load transaction's "not drawn until commit" contract.
        let Some(pipelines) = self.pipelines.as_ref() else {
            return Ok(None);
        };
        for state in &self.states {
            if state.cluster_count == 0 {
                continue;
            }
            // first_instance = the render mesh M's meta slot.
            let first_instance = ctx
                .meshes
                .meta
                .geometry_buffer_offset(state.render_mesh)
                .ok()
                .map(|off| {
                    off as u32
                        / crate::meshes::meta::geometry_meta::GEOMETRY_MESH_META_BYTE_ALIGNMENT
                            as u32
                })
                .unwrap_or(0);

            state.buffers.write_params(
                ctx.gpu,
                &Mat4::IDENTITY,
                cam_pos,
                tan_half_fov_y,
                viewport_h,
                pixel_budget,
                1.0,
                state.cluster_count,
            )?;
            {
                let cp = ctx.command_encoder.begin_compute_pass(Some(
                    &ComputePassDescriptor::new(Some("Cluster Cut")).into(),
                ));
                cp.set_pipeline(ctx.pipelines.compute.get(pipelines.cut)?);
                cp.set_bind_group(0, state.bind_groups.get_bind_group()?, None)?;
                cp.dispatch_workgroups(
                    ClusterLodBuffers::dispatch_groups(state.cluster_count),
                    Some(1),
                    Some(1),
                );
                cp.end();
            }
            // queue.writeBuffer is ordered before the submitted compute pass.
            state.buffers.init_draw_args(ctx.gpu, first_instance)?;
            {
                let cp = ctx.command_encoder.begin_compute_pass(Some(
                    &ComputePassDescriptor::new(Some("Cluster Compaction")).into(),
                ));
                cp.set_pipeline(ctx.pipelines.compute.get(pipelines.compaction)?);
                cp.set_bind_group(0, state.compaction_bind_groups.get_bind_group()?, None)?;
                cp.dispatch_workgroups(
                    ClusterLodBuffers::dispatch_groups(state.cluster_count),
                    Some(1),
                    Some(1),
                );
                cp.end();
            }
        }

        // Readback verification of draw_args.index_count across EVERY resident
        // mesh. Re-fires on a cadence (frame 5, then every 30) so the drawn cut is
        // observable as the camera/scene change; the async handler sums + logs on
        // change. One kick entry per mesh ⇒ correct totals with several meshes.
        let resident = || self.states.iter().filter(|s| s.cluster_count > 0);
        if resident().next().is_none() {
            return Ok(None);
        }
        let want = {
            let mut st = readback.lock().unwrap();
            st.frames += 1;
            !st.inflight && (st.frames == 5 || st.frames % 30 == 0)
        };
        if want {
            let mut kicks = Vec::new();
            for state in resident() {
                ctx.command_encoder.copy_buffer_to_buffer(
                    &state.buffers.draw_args_buffer,
                    0,
                    &state.buffers.readback_buffer,
                    0,
                    4,
                )?;
                kicks.push((state.buffers.readback_buffer.clone(), state.cluster_count));
            }
            return Ok(Some(kicks));
        }
        Ok(None)
    }
}
