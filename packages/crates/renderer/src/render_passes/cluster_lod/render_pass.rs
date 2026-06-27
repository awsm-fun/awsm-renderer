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
};
use crate::render_passes::RenderPassInitContext;

pub struct ClusterLodRenderPass {
    pub pipelines: ClusterLodPipelines,
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
/// **Step 20a (this slice):** holds the full pages + computes the per-frame
/// desired cut + logs how it tracks the camera. No geometry streaming yet (that
/// needs the exploded slot-write API, step 20b), so the drawn frontier — and thus
/// the render — is unchanged this slice.
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

    // ── CPU geometry, to build a streamed slot's exploded bytes (consumed in 20b-iv) ──
    /// Original unique-vertex positions (`cm.positions`); a slot's exploded verts
    /// gather these by `indices[page.first_index + k]`.
    #[allow(dead_code)] // read by the per-frame streamer (step 20b-iv)
    positions: Vec<[f32; 3]>,
    /// Original unique-vertex normals (`cm.normals`); empty ⇒ the streamer defaults.
    #[allow(dead_code)] // read by the per-frame streamer (step 20b-iv)
    normals: Vec<[f32; 3]>,
    /// Original triangle index buffer (`cm.indices`) the pages' spans address.
    #[allow(dead_code)] // read by the per-frame streamer (step 20b-iv)
    indices: Vec<u32>,

    // ── residency bookkeeping in FULL-DAG cluster space (the page-pool state) ──
    /// `resident[cluster_id]` = its page-pool slot, or `-1` (absent). Length =
    /// `pages.len()`.
    #[allow(dead_code)] // mutated by the per-frame streamer (step 20b-iv)
    resident: Vec<i32>,
    /// `slot_cluster[slot]` = the full-DAG cluster currently in that slot, or `-1`
    /// (free). Length = `pool_slots`.
    #[allow(dead_code)] // mutated by the per-frame streamer (step 20b-iv)
    slot_cluster: Vec<i32>,
    /// `slot_last_used[slot]` = the `frame` the slot was last in the desired cut
    /// (LRU eviction key). Length = `pool_slots`.
    #[allow(dead_code)] // mutated by the per-frame streamer (step 20b-iv)
    slot_last_used: Vec<u64>,
    /// Fixed page-pool capacity (slots) — the VRAM bound.
    #[allow(dead_code)] // read by the per-frame streamer (step 20b-iv)
    pool_slots: usize,

    // ── pooled per-frame scratch (no per-frame heap allocation in 20b-iv) ──
    /// One slot's exploded visibility bytes (`PAGE_VERTS*56`).
    #[allow(dead_code)] // reused by the per-frame streamer (step 20b-iv)
    slot_bytes_scratch: Vec<u8>,
    /// One slot's triangle-order corner indices (`PAGE_VERTS`) + slot-relative
    /// source indices, reused per stream.
    corner_scratch: Vec<u32>,
    src_idx_scratch: Vec<u32>,
    /// Pooled byte staging for the per-slot GPU writes (page entry + source-indices
    /// span), reused every stream so the buffer-write helpers don't allocate.
    page_bytes_scratch: Vec<u8>,
    src_bytes_scratch: Vec<u8>,
    /// `desired_flag[cluster_id]` = is this cluster in the current frame's desired
    /// cut. Pooled membership test for the eviction sweep (length = full DAG). Set
    /// from `desired` at the top of `stream_paging`, cleared at the end — so it is
    /// `false` everywhere between frames (no per-frame alloc, no per-frame clear of
    /// the whole vector).
    desired_flag: Vec<bool>,
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
            slot_bytes_scratch: Vec::new(),
            corner_scratch: Vec::new(),
            src_idx_scratch: Vec::new(),
            page_bytes_scratch: Vec::new(),
            src_bytes_scratch: Vec::new(),
            desired_flag: vec![false; pages_len],
            page_verts,
        }
    }
}

impl ClusterLodRenderPass {
    /// Builds the bind-group layout + cut compute pipeline. **Creating the
    /// pipeline validates `cluster_cut.wgsl` on-device** (the GPU driver compiles
    /// it here) — the first on-GPU checkpoint for the per-cluster cut.
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let proto_cut_bg = ClusterCutBindGroups::new(ctx)?;
        let proto_compaction_bg = ClusterCompactionBindGroups::new(ctx)?;
        let pipelines = ClusterLodPipelines::new(ctx, &proto_cut_bg, &proto_compaction_bg).await?;
        Ok(Self {
            pipelines,
            proto_cut_bg,
            proto_compaction_bg,
            states: Vec::new(),
        })
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

    /// Per-frame Gap-B dynamic paging (CPU-driven; step 20b-iv-b-2b). No-op unless
    /// paging is armed + a cluster render mesh + buffers exist.
    ///
    /// Computes the camera-adaptive complete antichain (`select_cut_per_cluster` over
    /// the full DAG), then STREAMS the desired clusters that aren't resident yet into
    /// FREE page-pool slots — writing the slot's exploded visibility verts, its GPU
    /// page (clamped always-draw), its slot-aligned source indices, and its residency
    /// entry. Free-slots-only ⇒ it only ADDS coverage, never removes it ⇒ crack-free
    /// (the coarser ancestor stays resident+drawn until its region is fully refined;
    /// at most a transient z-fight overlap, never a hole). Bounded: refinement caps at
    /// `pool_slots` (coarser where it doesn't fit). LRU eviction (to recycle slots that
    /// leave the antichain) is the next layer (20b-iv-b-2c). All scratch is pooled.
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

            let data_buf = meshes.visibility_geometry_data_gpu_buffer();
            let data_off = meshes.visibility_geometry_data_buffer_offset(render_mesh)?;
            let pv = p.page_verts;
            const STRIDE: usize = 56; // visibility vertex bytes
            const MAX_LOADS: usize = 96; // cap streams/frame so a camera jump doesn't hitch

            // Mark this frame's desired cut for the membership test the eviction sweep
            // (below) needs. Cleared again at the end of the frame so `desired_flag` is
            // all-false between frames — no per-frame alloc, no full-vector clear.
            for &c in &p.desired {
                p.desired_flag[c as usize] = true;
            }

            let mut next_free = 0usize; // free-slot scan cursor (monotone within a frame)
            let mut streamed = 0usize;
            // True once every desired cluster is resident — only then is it crack-free to
            // evict the resident-but-no-longer-desired slots (resident becomes EXACTLY the
            // antichain `desired`). While loads are still pending we keep the coarser
            // ancestors resident (transient overlap = z-fight, never holes).
            let mut all_desired_resident = true;
            let mut i = 0usize;
            while i < p.desired.len() {
                let cluster = p.desired[i] as usize;
                i += 1;
                if p.resident[cluster] >= 0 {
                    p.slot_last_used[p.resident[cluster] as usize] = p.frame; // keep warm
                    continue;
                }
                if streamed >= MAX_LOADS {
                    all_desired_resident = false; // capped this frame ⇒ more to stream next
                    continue;
                }
                // Find a FREE slot (stream-into-free-before-evict ⇒ crack-free).
                while next_free < p.pool_slots && p.slot_cluster[next_free] >= 0 {
                    next_free += 1;
                }
                if next_free >= p.pool_slots {
                    all_desired_resident = false; // pool full — bounded partial refinement
                    break;
                }
                let slot = next_free;
                next_free += 1;

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
                // GPU resident is SLOT-indexed: mark this slot drawable (value = slot).
                buffers.write_resident_entry(gpu, slot, slot as i32)?;
                p.resident[cluster] = slot as i32;
                p.slot_cluster[slot] = cluster as i32;
                p.slot_last_used[slot] = p.frame;
                streamed += 1;
            }

            // Eviction sweep — only when the whole desired cut is resident, so dropping
            // the no-longer-desired slots leaves EXACTLY the crack-free antichain. This is
            // what makes the draw FALL on zoom-out and recycles slots within the bounded
            // pool. Capped per frame; pages stay always-draw so a free slot simply isn't
            // selected by the cut (resident < 0).
            let mut evicted = 0usize;
            if all_desired_resident {
                for slot in 0..p.pool_slots {
                    if evicted >= MAX_LOADS {
                        break;
                    }
                    let c = p.slot_cluster[slot];
                    if c >= 0 && !p.desired_flag[c as usize] {
                        buffers.write_resident_entry(gpu, slot, -1)?;
                        p.resident[c as usize] = -1;
                        p.slot_cluster[slot] = -1;
                        evicted += 1;
                    }
                }
            }

            // Clear this frame's desired marks (keep `desired_flag` all-false between
            // frames without a full-vector reset).
            for &c in &p.desired {
                p.desired_flag[c as usize] = false;
            }

            let desired = p.desired.len();
            if streamed > 0 || evicted > 0 || desired != p.last_desired_logged {
                p.last_desired_logged = desired;
                tracing::info!(
                    "cluster paging (Gap B, frame {}): desired={desired} (full DAG={}, pool={}), \
                 streamed {streamed}, evicted {evicted} [20b-iv-b-2b]",
                    p.frame,
                    p.pages.len(),
                    p.pool_slots,
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
                cp.set_pipeline(ctx.pipelines.compute.get(self.pipelines.cut)?);
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
                cp.set_pipeline(ctx.pipelines.compute.get(self.pipelines.compaction)?);
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
