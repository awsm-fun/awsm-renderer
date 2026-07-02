//! Transform hierarchy and GPU upload.

use glam::{Mat4, Quat, Vec3};
use thiserror::Error;

use std::{
    collections::{HashMap, HashSet},
    sync::LazyLock,
};

use awsm_renderer_core::{
    buffers::{BufferDescriptor, BufferUsage},
    error::AwsmCoreError,
    pipeline::primitive::FrontFace,
    renderer::AwsmRendererWebGpu,
};
use slotmap::{new_key_type, SecondaryMap, SlotMap};

use crate::{
    bind_groups::{BindGroupCreate, BindGroups},
    buffer::dynamic_uniform::DynamicUniformBuffer,
    buffer::mapped_uploader::MappedUploader,
    buffer::shared_arena::{SharedArena, SlotBinding},
    meshes::skins::AwsmSkinError,
    AwsmRenderer, AwsmRendererLogging,
};

/// Semantic stride of a transform in the shared sim-state arena: one world
/// `Mat4` (decision D2). The render side packs this to the 112-byte GPU
/// layout (model + inverse-transpose normal) on its dirty descent; the sim
/// worker only ever writes these 64 semantic bytes.
pub const TRANSFORM_ARENA_STRIDE: usize = 64;

/// Pack a world `Mat4` into the 112-byte GPU transform entry: the model
/// matrix (64 B) followed by the inverse-transpose normal matrix as a
/// `mat3x3<f32>` with vec3 columns padded to vec4 (48 B, 12 B useful per
/// column). This is the **single** packing routine — both the
/// single-threaded hierarchy walk and the arena-backed descent call it, so
/// the two paths are byte-identical by construction (pinned by
/// `packed_bytes_match_reference`).
pub fn pack_world_transform(world: &Mat4) -> [u8; Transforms::BYTE_SIZE] {
    let mut packed = [0u8; Transforms::BYTE_SIZE];

    // Model matrix: 64 bytes (4 columns × 16 B).
    let model_values = world.to_cols_array();
    let model_bytes = unsafe { std::slice::from_raw_parts(model_values.as_ptr() as *const u8, 64) };
    packed[0..64].copy_from_slice(model_bytes);

    // Normal matrix: 9 floats as 3 columns × (vec3 + 4-byte pad). Padding
    // floats stay zero; the shader's `mat3x3<f32>` ctor reads only the 9
    // useful values.
    let normal_matrix = glam::Mat3::from_mat4(world.inverse().transpose());
    let nm = normal_matrix.to_cols_array();
    for col in 0..3usize {
        let col_off = Transforms::NORMAL_OFFSET + col * 16;
        let src = col * 3;
        let col_bytes =
            unsafe { std::slice::from_raw_parts(nm[src..src + 3].as_ptr() as *const u8, 12) };
        packed[col_off..col_off + 12].copy_from_slice(col_bytes);
    }
    packed
}

impl AwsmRenderer {
    /// Updates world transforms and mesh bounds from dirty transforms,
    /// plus every per-frame spatial / light-bucket / coverage hook that
    /// has to run once per render. Both `update_all()` and the editor's
    /// custom render loop call this — keep all per-frame renderer-owned
    /// bookkeeping here so the two paths stay in lockstep.
    ///
    /// Mirrors every refreshed `world_aabb` into the spatial index so the
    /// per-view frustum queries see the latest geometry positions on the
    /// same frame they're recomputed.
    pub fn update_transforms(&mut self) {
        // Bump the renderer-wide frame counter first so all per-frame
        // consumers see the same index across the frame.
        self.frame_index = self.frame_index.wrapping_add(1);

        self.transforms.update_world();
        let dirty_transforms = self.transforms.take_dirty_meshes();
        let dirty_instances = self.instances.take_dirty_transforms();

        // §B static-shadow cache: did any *shadow-caster* mesh move this frame?
        // Filter the dirty transforms to meshes that actually cast shadows
        // (`cast_shadows && !hud && !hidden`) so the editor's per-frame HUD churn
        // (gizmos / light icons / skeleton overlays — all non-caster) does NOT
        // disable the cache. The dirty set is small, the reverse lookup is O(1), so
        // this is cheap; OR-accumulated on `Shadows` across the multiple
        // `update_transforms` calls per frame, read + reset by `take_shadow_static`.
        let caster_moved = dirty_transforms
            .keys()
            .chain(dirty_instances.iter())
            .any(|tk| {
                self.meshes.keys_by_transform_key(*tk).is_some_and(|keys| {
                    keys.iter().any(|mk| {
                        self.meshes
                            .get(*mk)
                            .is_ok_and(|m| m.cast_shadows && !m.hud && !m.hidden)
                    })
                })
            });
        if caster_moved {
            self.shadows.note_shadow_caster_moved();
        }

        // Propagate animated node transforms to any lights bound to them
        // (e.g. glTF point lights on animated firefly nodes) before the
        // light-bucket rebuild below reads their world AABBs. Consumes a
        // borrow of `dirty_transforms`, which is moved into
        // `meshes.update_world` immediately after.
        self.lights.update_from_transforms(&dirty_transforms);
        // Build a frustum from the last-known camera matrices so
        // `Meshes::update_world` can run the coverage-driven
        // skin-skip's BVH-visible override. `None` on the very
        // first frame before `update_camera` has run; the skin-skip
        // logic treats `None` conservatively (assume every consumer
        // is in-frustum, so never skip via coverage).
        let frustum = self
            .camera
            .last_matrices
            .as_ref()
            .map(|m| crate::frustum::Frustum::from_view_projection(m.view_projection()));
        let touched = self.meshes.update_world(
            dirty_transforms,
            &dirty_instances,
            &self.transforms,
            &self.instances,
            self.frame_index,
            &self.coverage,
            frustum.as_ref(),
        );
        for mesh_key in touched {
            self.sync_spatial_for_mesh(mesh_key);
        }

        // Per-frame BVH maintenance (refit + incremental rebalance; a
        // no-op on idle frames). Must run between the transform sync above
        // and ANY tree query below — deferred leaf updates propagate here.
        // Span so the cost shows up in the browser Performance API
        // (`?trace=sub-frame`).
        {
            let _maybe_span_guard = if self.logging.render_timings.sub_frame() {
                Some(tracing::span!(tracing::Level::INFO, "SceneSpatial Maintain").entered())
            } else {
                None
            };
            self.scene_spatial.maintain();
        }

        // Per-frame per-light → per-mesh bucket rebuild — cheap (one
        // `query_envelope` per active punctual light).
        self.light_buckets
            .rebuild(&self.lights, &self.scene_spatial);

        // Per-mesh "any shadow-caster reaches me" flag.
        let shadows_ref = &self.shadows;
        self.light_buckets
            .mark_shadow_receivers(&self.lights, |key| {
                shadows_ref
                    .light_params(key)
                    .map(|p| p.cast)
                    .unwrap_or(false)
            });

        // Project the bucket result onto each mesh's `shadow_receiver_gate`
        // u32 inside `MaterialMeshMeta` so `apply_lighting*` in
        // `lights.wgsl` can skip shadow sampling for meshes no caster
        // reaches this frame. The patch path inside
        // `MeshMeta::set_shadow_receiver_gate` caches the last-frame
        // value and short-circuits unchanged writes, so the dirty-range
        // set stays sparse on a steady-state stress scene.
        {
            let _maybe_span_guard = if self.logging.render_timings.sub_frame() {
                Some(tracing::span!(tracing::Level::INFO, "Shadow Receiver Gate").entered())
            } else {
                None
            };
            // `Meshes::update_shadow_receiver_gates` walks the mesh
            // key set in-place — no per-frame `Vec<MeshKey>` alloc.
            // The split borrow (immutable `light_buckets` + mutable
            // `meshes`) holds because both are disjoint fields on `self`.
            let light_buckets = &self.light_buckets;
            self.meshes.update_shadow_receiver_gates(|mesh_key| {
                if light_buckets.is_shadow_receiver(mesh_key) {
                    1
                } else {
                    0
                }
            });
        }

        #[cfg(debug_assertions)]
        {
            // Count only RESOLVED meshes (those that have a geometry resource).
            // `scene_spatial` is fed exclusively by `sync_spatial_for_mesh`, which
            // runs at `resolve_geometry` (commit). A mesh BOUND but not yet resolved
            // legitimately carries a `world_aabb` — `bind_mesh` stamps it from the
            // geometry source so the mesh stays OUT of `collect_renderables`'
            // `world_aabb.is_none()` conservative-draw fallback (it must not draw
            // before its GPU resource exists) — yet it is correctly absent from the
            // spatial index until commit. The editor's deferred glTF import releases
            // the renderer lock between `populate_gltf` (deferred `add_mesh`) and the
            // materialise-time `commit_load`, so a render frame runs in that window;
            // gating on "resolved" excludes that legitimate transient state while
            // still catching a genuinely missing sync hook on any resolved mesh.
            let with_aabb = self
                .meshes
                .iter()
                .filter(|(k, m)| m.world_aabb.is_some() && self.meshes.resource_key(*k).is_ok())
                .count();
            let spatial_count = self.scene_spatial.len();
            debug_assert!(
                with_aabb == spatial_count,
                "scene_spatial leaf count ({spatial_count}) diverged from RESOLVED meshes with world_aabb ({with_aabb}) — sync hook missing on a mutation path"
            );
        }
    }
}

/// Transform hierarchy with GPU buffers.
pub struct Transforms {
    locals: SlotMap<TransformKey, Transform>,
    world_matrices: SecondaryMap<TransformKey, glam::Mat4>,
    children: SecondaryMap<TransformKey, Vec<TransformKey>>,
    parents: SecondaryMap<TransformKey, TransformKey>,
    // These are the transforms that are dirtied from the outside
    // e.g. may be set multiples times by the user or randomly in the hierarchy
    dirties: HashSet<TransformKey>,
    // While we calculate the dirties, we can know if meshes need to be updated
    // this is set internally
    // not every transform here is definitely a mesh, just in potential
    dirty_meshes: Vec<TransformKey>,
    gpu_dirty: bool,
    pub root_node: TransformKey,
    buffer: DynamicUniformBuffer<TransformKey>,
    pub(crate) gpu_buffer: web_sys::GpuBuffer,
    /// Mapped-ring upload companion (Phase 2.1). Lazy-initialised on
    /// first write_gpu call; sized to mirror `gpu_buffer`.
    uploader: MappedUploader,

    /// Shared sim-state mode (`docs/PLAYER-GUIDE.md §9`, M2). When
    /// `Some`, world matrices are stored as semantic 64-byte values in a
    /// shared-memory [`SharedArena`] (foreign-writable by a physics worker)
    /// and the 112-byte GPU layout is produced on the render-side dirty
    /// descent ([`Transforms::descend_pack_arena`]). `None` on the
    /// single-threaded build → the classic in-place 112-byte pack, untouched.
    arena: Option<TransformArena>,

    /// Reused across descents to carry each updated slot's (key, world
    /// matrix) without a per-frame heap allocation (shared mode only). The
    /// world matrix drives BOTH the 112-byte GPU pack AND the CPU-side bounds
    /// refresh (decision A / H3).
    arena_pack_scratch: Vec<(TransformKey, Mat4)>,

    /// Stats from the most recent arena descent (shared mode) — exposed for
    /// the stress/hot-path proof (work tracks movers, not total slots).
    last_descend: TransformDescendStats,
}

/// Per-frame arena-descent stats (shared mode).
#[derive(Debug, Default, Clone, Copy)]
pub struct TransformDescendStats {
    /// Slots that took a fresh value (≈ number of movers) this frame.
    pub updated: usize,
    /// Slots that read torn (reused last value) this frame.
    pub torn: usize,
    /// Dirty chunks descended this frame.
    pub chunks: usize,
}

/// Arena-backed semantic transform store + key↔slot mapping (shared mode).
struct TransformArena {
    arena: SharedArena,
    key_to_slot: SecondaryMap<TransformKey, usize>,
    slot_to_key: Vec<Option<TransformKey>>,
}

static BUFFER_USAGE: LazyLock<BufferUsage> =
    LazyLock::new(|| BufferUsage::new().with_storage().with_copy_dst());

impl Transforms {
    /// Number of live transform slots. Observability (leak/soak checks):
    /// a steadily climbing count on an idle scene means something is
    /// inserting transforms per frame without removing them.
    pub fn len(&self) -> usize {
        self.locals.len()
    }

    /// True when no transforms exist.
    pub fn is_empty(&self) -> bool {
        self.locals.is_empty()
    }

    /// Initial transform slot capacity.
    pub const INITIAL_CAPACITY: usize = 32; // 32 elements is a good starting point
    /// Byte size of a packed transform entry (Option E — model + normal
    /// in one struct).
    ///
    /// Layout:
    ///   - bytes  0.. 64: `model_world: mat4x4<f32>` (4 columns × 16 B)
    ///   - bytes 64..112: `normal_world: mat3x3<f32>` (3 columns × 16 B,
    ///     vec3 cols padded to vec4 per WGSL rule)
    ///
    /// Stride is 112 bytes — already 16-aligned so no further padding.
    /// The CPU side writes 9 useful f32s for the normal matrix; the
    /// remaining 12 bytes (3 padding f32s, one per column) stay zeroed
    /// and the shader's `mat3x3<f32>` constructor only reads the 9
    /// useful values. Costs 12 B per transform vs the previous split
    /// design (was 64 B + 36 B), but saves one storage-buffer binding
    /// because both are read from the same `var<storage> transforms`
    /// declaration.
    pub const BYTE_SIZE: usize = 112;
    /// Offset of the normal matrix inside a packed transform entry.
    pub const NORMAL_OFFSET: usize = 64;

    /// Creates transform storage and GPU buffers.
    pub fn new(gpu: &AwsmRendererWebGpu) -> Result<Self> {
        let gpu_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("Transforms"),
                Transforms::INITIAL_CAPACITY * Transforms::BYTE_SIZE,
                *BUFFER_USAGE,
            )
            .into(),
        )?;

        let buffer = DynamicUniformBuffer::new(
            Self::INITIAL_CAPACITY,
            Self::BYTE_SIZE,
            None,
            Some("Transforms".to_string()),
        );

        let mut locals = SlotMap::with_capacity_and_key(Self::INITIAL_CAPACITY);
        let mut world_matrices = SecondaryMap::with_capacity(Self::INITIAL_CAPACITY);
        let mut children = SecondaryMap::new();

        let root_node = locals.insert(Transform::default());
        world_matrices.insert(root_node, glam::Mat4::IDENTITY);
        children.insert(root_node, Vec::new());

        Ok(Self {
            locals,
            world_matrices,
            children,
            parents: SecondaryMap::new(),
            dirties: HashSet::new(),
            dirty_meshes: Vec::with_capacity(Self::INITIAL_CAPACITY),
            gpu_dirty: true,
            root_node,
            buffer,
            gpu_buffer,
            uploader: MappedUploader::new("Transforms"),
            arena: None,
            arena_pack_scratch: Vec::new(),
            last_descend: TransformDescendStats::default(),
        })
    }

    /// Switch this transform store into **shared sim-state mode**
    /// (`docs/PLAYER-GUIDE.md §9`, M2/M3): back world matrices with a
    /// shared-memory [`SharedArena`] of semantic 64-byte values, foreign-
    /// writable by a physics worker. Existing transforms are migrated into
    /// the arena (a slot allocated + current world matrix written) and all
    /// future world updates flow through the arena → 112-byte pack on
    /// descent.
    ///
    /// Idempotent-ish: only meaningful once, at setup, before the hot loop.
    /// The single-threaded editor / model-viewer never call this, so their
    /// path is byte-for-byte unchanged.
    pub fn enable_shared_arena(&mut self) {
        if self.arena.is_some() {
            return;
        }
        // ~1M-slot ceiling (1024 slots × 1024 chunks) at 64 B/slot.
        let mut arena = SharedArena::new(TRANSFORM_ARENA_STRIDE, 1024, 1024);
        let mut key_to_slot = SecondaryMap::new();
        let mut slot_to_key = Vec::new();
        // Migrate every existing transform (deterministic key order via the
        // slotmap iteration) and seed its current world matrix.
        let keys: Vec<TransformKey> = self.locals.keys().collect();
        for key in keys {
            let slot = arena.allocate();
            key_to_slot.insert(key, slot);
            if slot_to_key.len() <= slot {
                slot_to_key.resize(slot + 1, None);
            }
            slot_to_key[slot] = Some(key);
            let world = self
                .world_matrices
                .get(key)
                .copied()
                .unwrap_or(Mat4::IDENTITY);
            let bytes = world.to_cols_array();
            let raw = unsafe { std::slice::from_raw_parts(bytes.as_ptr() as *const u8, 64) };
            arena.write_value(slot, raw);
        }
        self.arena = Some(TransformArena {
            arena,
            key_to_slot,
            slot_to_key,
        });
        // Force a full descend+pack+upload on the next frame.
        self.gpu_dirty = true;
    }

    /// `true` when the shared sim-state arena is active.
    pub fn is_shared(&self) -> bool {
        self.arena.is_some()
    }

    /// Stats from the most recent `update_world` arena descent (shared
    /// mode). `updated` tracks the number of movers, not the total slot
    /// count — the hot-path scalability property (M3 stress proof).
    pub fn last_descend_stats(&self) -> TransformDescendStats {
        self.last_descend
    }

    /// Base address of the arena's chunk dirty bitmap (for a foreign
    /// writer). `None` outside shared mode.
    pub fn arena_dirty_words_addr(&self) -> Option<usize> {
        self.arena.as_ref().map(|a| a.arena.dirty_words_addr())
    }

    /// Raw [`SlotBinding`] a physics worker uses to write `key`'s world
    /// matrix into shared memory. `None` if not shared / key unbound.
    pub fn arena_slot_binding(&self, key: TransformKey) -> Option<SlotBinding> {
        let a = self.arena.as_ref()?;
        let slot = *a.key_to_slot.get(key)?;
        Some(a.arena.slot_binding(slot))
    }

    /// Mapped-ring upload telemetry for this subsystem.
    pub fn upload_stats(&self) -> crate::buffer::mapped_staging_ring::UploadStats {
        self.uploader.stats()
    }

    /// Inserts a transform and returns its key.
    pub fn insert(&mut self, transform: Transform, parent: Option<TransformKey>) -> TransformKey {
        let world_matrix = transform.to_matrix();

        let key = self.locals.insert(transform);

        self.world_matrices.insert(key, world_matrix);
        self.children.insert(key, Vec::new());
        self.dirties.insert(key);

        self.buffer.update(key, &[0; Self::BYTE_SIZE]);

        // Shared mode: bind the key to an arena slot now (topology is
        // owner-only). The world matrix is written on the next
        // `update_world` descent (the key is in `dirties`).
        if let Some(a) = self.arena.as_mut() {
            let slot = a.arena.allocate();
            a.key_to_slot.insert(key, slot);
            if a.slot_to_key.len() <= slot {
                a.slot_to_key.resize(slot + 1, None);
            }
            a.slot_to_key[slot] = Some(key);
        }

        self.set_parent(key, parent);

        key
    }

    /// Duplicates a transform and returns the new key.
    pub fn duplicate(&mut self, key: TransformKey) -> Result<TransformKey> {
        let local_transform = self.get_local(key)?.clone();
        let parent_transform = self.get_parent(key).ok();
        Ok(self.insert(local_transform, parent_transform))
    }

    /// Removes a transform and its buffers.
    pub fn remove(&mut self, key: TransformKey) {
        if key == self.root_node {
            return;
        }

        // happens separately so that we can remove the node from the parent's children list
        self.unset_parent(key);

        self.locals.remove(key);
        self.world_matrices.remove(key);
        self.children.remove(key);
        self.dirties.remove(&key);
        self.buffer.remove(key);

        if let Some(a) = self.arena.as_mut() {
            if let Some(slot) = a.key_to_slot.remove(key) {
                a.arena.free(slot);
                if slot < a.slot_to_key.len() {
                    a.slot_to_key[slot] = None;
                }
            }
        }

        self.gpu_dirty = true;
    }

    // This is the only way to modify the matrices (since it must manage the dirty flags)
    // world transforms are updated by calling update()
    /// Sets the local transform for a key.
    pub fn set_local(&mut self, key: TransformKey, transform: Transform) -> Result<()> {
        if key == self.root_node {
            return Err(AwsmTransformError::CannotModifyRootNode);
        }
        match self.locals.get_mut(key) {
            Some(existing) => {
                *existing = transform;
                self.dirties.insert(key);
                Ok(())
            }
            None => Err(AwsmTransformError::LocalNotFound(key)),
        }
    }

    // if parent is None then the parent is the root node
    /// Sets the parent of a transform.
    pub fn set_parent(&mut self, child: TransformKey, parent: Option<TransformKey>) {
        if child == self.root_node {
            return;
        }

        let parent = parent.unwrap_or(self.root_node);

        if let Some(existing_parent) = self.parents.get(child) {
            if *existing_parent == parent {
                return;
            } else {
                self.unset_parent(child);
            }
        }

        // safe because all transforms have children vec when created
        self.children.get_mut(parent).unwrap().push(child);

        self.parents.insert(child, parent);
    }

    /// Returns the parent key for a transform.
    pub fn get_parent(&self, child: TransformKey) -> Result<TransformKey> {
        if child == self.root_node {
            return Err(AwsmTransformError::CannotGetParentOfRootNode);
        }

        self.parents
            .get(child)
            .copied()
            .ok_or(AwsmTransformError::CannotGetParent(child))
    }

    /// Returns the local transform for a key.
    pub fn get_local(&self, key: TransformKey) -> Result<&Transform> {
        self.locals
            .get(key)
            .ok_or(AwsmTransformError::LocalNotFound(key))
    }

    /// Returns the world matrix for a key.
    pub fn get_world(&self, key: TransformKey) -> Result<&glam::Mat4> {
        self.world_matrices
            .get(key)
            .ok_or(AwsmTransformError::WorldNotFound(key))
    }

    /// Returns the children of a transform.
    pub fn get_children(&self, key: TransformKey) -> Option<&[TransformKey]> {
        self.children.get(key).map(Vec::as_slice)
    }

    // This is the only way to update the world matrices
    // it does *not* write to the GPU, so it can be called relatively frequently for physics etc.
    pub(crate) fn update_world(&mut self) {
        self.gpu_dirty = self.gpu_dirty || !self.dirties.is_empty();

        self.update_inner_recursively(self.root_node, false);

        self.dirties.clear();

        // Shared mode: descend the arena (picking up this thread's writes
        // above *and* any foreign physics-worker writes) and pack each
        // changed slot 64 B → 112 B into the GPU mirror.
        if self.arena.is_some() {
            self.descend_pack_arena();
        }
    }

    /// Shared mode: read changed semantic 64-byte world matrices out of the
    /// arena (torn-read-safe) and pack them into the 112-byte GPU mirror
    /// buffer. Pack cost is proportional to the dirty count (decision A).
    ///
    /// H8 — the per-frame "copy at the pack step", settled empirically. The
    /// plan flagged GPU-upload-from-shared-memory as an open question (would
    /// `queue.writeBuffer` reject a `SharedArrayBuffer`-backed view?). MEASURED
    /// in Chrome: `writeBuffer` ACCEPTS shared-backed views (and a mapped range
    /// can be written from one) — corroborated by the fact that this whole
    /// threaded renderer already uploads from the (shared) wasm heap every
    /// frame and renders correctly. So the upload is NOT forced off shared
    /// memory by the platform.
    ///
    /// The work here is therefore necessary computation, not a removable copy:
    /// for each MOVED transform we (1) snapshot its 64 B model matrix out of
    /// shared memory torn-read-safe (seqlock), (2) PACK to the 112 B GPU layout
    /// — which computes the inverse-transpose normal matrix
    /// ([`pack_world_transform`]) — into the CPU mirror, (3) upload via the
    /// mapped ring. Each step earns its keep (tear safety / derived-data
    /// compute / upload); none is a redundant memcpy, and all cost is ∝ movers.
    /// The mirror is the single source the ST and MT paths share, so there is
    /// no shared-only divergent upload path to maintain.
    fn descend_pack_arena(&mut self) {
        let mut scratch = std::mem::take(&mut self.arena_pack_scratch);
        scratch.clear();
        {
            let Some(a) = self.arena.as_mut() else {
                self.arena_pack_scratch = scratch;
                return;
            };
            let result = a.arena.descend();
            self.last_descend = TransformDescendStats {
                updated: result.updated,
                torn: result.torn,
                chunks: result.chunks,
            };
            let stride = TRANSFORM_ARENA_STRIDE;
            for (off, len) in &result.ranges {
                let start = off / stride;
                let count = len / stride;
                for slot in start..start + count {
                    if let Some(Some(key)) = a.slot_to_key.get(slot).copied() {
                        let m = &a.arena.mirror()[slot * stride..slot * stride + stride];
                        let mut cols = [0f32; 16];
                        // SAFETY: `m` is exactly 64 bytes = 16 f32 columns.
                        unsafe {
                            std::ptr::copy_nonoverlapping(
                                m.as_ptr(),
                                cols.as_mut_ptr() as *mut u8,
                                64,
                            );
                        }
                        let world = Mat4::from_cols_array(&cols);
                        scratch.push((key, world));
                    }
                }
            }
        }
        if !scratch.is_empty() {
            for (key, world) in scratch.iter().copied() {
                // GPU pack (model + inverse-transpose normal).
                self.buffer.update(key, &pack_world_transform(&world));
                // H3: refresh the CPU-side world matrix + mark the node dirty
                // for the bounds/spatial/culling pass, so a physics-driven
                // transform's `world_aabb` (frustum culling, shadows, picking)
                // tracks its real position — not a stale one. `update_world`'s
                // hierarchy walk skips these nodes (they're not in `dirties`),
                // so this descent is the ONLY place their bounds get refreshed.
                self.world_matrices.insert(key, world);
                self.dirty_meshes.push(key);
            }
            self.gpu_dirty = true;
        }
        self.arena_pack_scratch = scratch;
    }

    // This *does* write to the gpu, should be called only once per frame
    // just write the entire buffer in one fell swoop
    /// Writes dirty transform data to the GPU.
    pub fn write_gpu(
        &mut self,
        logging: &AwsmRendererLogging,
        gpu: &AwsmRendererWebGpu,
        bind_groups: &mut BindGroups,
    ) -> Result<()> {
        if self.gpu_dirty {
            let _maybe_span_guard = if logging.render_timings.sub_frame() {
                Some(tracing::span!(tracing::Level::INFO, "Transform GPU write").entered())
            } else {
                None
            };

            let mut transform_resized = false;
            if let Some(new_size) = self.buffer.take_gpu_needs_resize() {
                self.gpu_buffer = gpu.create_buffer(
                    &BufferDescriptor::new(Some("Transforms"), new_size, *BUFFER_USAGE).into(),
                )?;

                bind_groups.mark_create(BindGroupCreate::TransformsResize);
                transform_resized = true;
            }

            if transform_resized {
                // Post-resize: dest buffer is uninitialised; do a full
                // overwrite via writeBuffer (the bind-group rebuild
                // makes upload latency irrelevant here).
                self.buffer.clear_dirty_ranges();
                gpu.write_buffer(&self.gpu_buffer, None, self.buffer.raw_slice(), None, None)?;
            } else {
                let transform_ranges = self.buffer.take_dirty_ranges();
                self.uploader.write_dirty_ranges(
                    gpu,
                    &self.gpu_buffer,
                    self.buffer.raw_slice().len(),
                    self.buffer.raw_slice(),
                    &transform_ranges,
                )?;
            }

            self.gpu_dirty = false;
        }
        Ok(())
    }

    /// Takes and clears the list of dirty mesh transforms.
    pub fn take_dirty_meshes(&mut self) -> HashMap<TransformKey, Mat4> {
        self.dirty_meshes
            .drain(..)
            .map(|key| {
                // this for sure exists since we just drained the key
                let world_matrix = self.world_matrices.get(key).copied().unwrap();
                (key, world_matrix)
            })
            .collect()
    }

    /// Returns the GPU buffer offset for a transform.
    pub fn buffer_offset(&self, key: TransformKey) -> Result<usize> {
        self.buffer
            .offset(key)
            .ok_or(AwsmTransformError::TransformBufferSlotMissing(key))
    }

    /// Returns the GPU buffer offset for the packed normal matrix
    /// inside a transform slot. Equal to `buffer_offset(key) +
    /// NORMAL_OFFSET`. Kept as a separate accessor so callers don't
    /// have to know the internal layout — and so the WGSL side, which
    /// indexes `transforms[transform_offset / BYTE_SIZE].normal_world`,
    /// can be re-routed if the struct shape ever changes.
    pub fn normals_buffer_offset(&self, key: TransformKey) -> Result<usize> {
        Ok(self.buffer_offset(key)? + Self::NORMAL_OFFSET)
    }

    /// Returns a reference to world matrices.
    pub fn world_matrices_ref(&self) -> &SecondaryMap<TransformKey, glam::Mat4> {
        &self.world_matrices
    }

    // should only be used for debugging really
    /// Returns a tree of transforms for debugging.
    pub fn get_tree(&self) -> TransformTreeNode {
        fn build_node(transforms: &Transforms, key: TransformKey) -> TransformTreeNode {
            let children = transforms.children.get(key).unwrap();

            let child_nodes = children
                .iter()
                .map(|&child_key| build_node(transforms, child_key))
                .collect();

            TransformTreeNode {
                key,
                children: child_nodes,
            }
        }

        build_node(self, self.root_node)
    }

    // internal-only function
    // See: https://gameprogrammingpatterns.com/dirty-flag.html
    // the overall idea is we walk the tree and skip over nodes that are not dirty
    // whenever we encounter a dirty node, we must also mark all of its children dirty
    // finally, for each dirty node, its world transform is its parent's world transform
    // multiplied by its local transform
    // or in other words, it's the local transform, offset by its parent in world space
    //
    // we also update the CPU-side buffer as needed so it will be ready for the GPU
    fn update_inner_recursively(&mut self, key: TransformKey, dirty_tracker: bool) -> bool {
        let dirty = self.dirties.contains(&key) | dirty_tracker;

        if dirty {
            let local_matrix = self.locals[key].to_matrix();

            let world_matrix = match self.parents.get(key) {
                Some(parent) => {
                    let parent_matrix = self.world_matrices[*parent];
                    parent_matrix.mul_mat4(&local_matrix)
                }
                None => local_matrix,
            };

            self.world_matrices[key] = world_matrix;

            if let Some(a) = self.arena.as_mut() {
                // Shared mode: store the semantic 64-byte world matrix in
                // the arena. The 112-byte GPU pack happens on the descent
                // (`descend_pack_arena`), sourced from the arena so a
                // foreign physics worker can feed the same slots (M3).
                if let Some(&slot) = a.key_to_slot.get(key) {
                    let cols = world_matrix.to_cols_array();
                    let raw = unsafe { std::slice::from_raw_parts(cols.as_ptr() as *const u8, 64) };
                    a.arena.write_value(slot, raw);
                }
            } else {
                // Single-threaded path: pack model + normal directly into
                // the 112-byte GPU mirror (byte-identical to the shared
                // path's pack — both call `pack_world_transform`).
                let packed = pack_world_transform(&world_matrix);
                self.buffer.update(key, &packed);
            }

            self.dirty_meshes.push(key);
        }

        // safety: can't keep a mutable reference to self while it has a borrow of the iterator
        // TODO: maybe split this function into a pure function that takes the deps?
        let children = self.children[key].clone();
        for child in children {
            self.update_inner_recursively(child, dirty);
        }

        dirty
    }

    // internal-only function - leaves the node dangling
    // after this call, the node should either be immediately removed or reparented
    fn unset_parent(&mut self, child: TransformKey) {
        if let Some(parent) = self.parents.remove(child) {
            if let Some(children) = self.children.get_mut(parent) {
                children.retain(|&c| c != child);
            }
        }
    }
}

/// Tree node for transform hierarchy debugging.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TransformTreeNode {
    pub key: TransformKey,
    pub children: Vec<TransformTreeNode>,
}

/// Transform with translation, rotation, and scale.
#[derive(Clone, Debug)]
pub struct Transform {
    pub translation: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
}

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Transform {
    /// Identity transform.
    pub const IDENTITY: Self = Self {
        translation: Vec3::ZERO,
        rotation: Quat::IDENTITY,
        scale: Vec3::ONE,
    };

    /// Sets translation.
    pub fn with_translation(mut self, translation: Vec3) -> Self {
        self.translation = translation;
        self
    }
    /// Sets rotation.
    pub fn with_rotation(mut self, rotation: Quat) -> Self {
        self.rotation = rotation;
        self
    }
    /// Sets scale.
    pub fn with_scale(mut self, scale: Vec3) -> Self {
        self.scale = scale;
        self
    }

    /// Converts the transform to a matrix.
    pub fn to_matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
    }

    /// Returns the winding order implied by the transform.
    pub fn winding_order(&self) -> FrontFace {
        /*
        Staying consistent with gltf spec: "When a mesh primitive uses any triangle-based topology (i.e., triangles, triangle strip, or triangle fan),
        the determinant of the node’s global transform defines the winding order of that primitive.
        If the determinant is a positive value, the winding order triangle faces is counterclockwise;
        in the opposite case, the winding order is clockwise.
        */
        if self.to_matrix().determinant() > 0.0 {
            FrontFace::Ccw
        } else {
            FrontFace::Cw
        }
    }
}

impl From<&Mat4> for Transform {
    fn from(matrix: &Mat4) -> Self {
        let (scale, rotation, translation) = matrix.to_scale_rotation_translation();
        Self {
            translation,
            rotation,
            scale,
        }
    }
}

impl From<Mat4> for Transform {
    fn from(matrix: Mat4) -> Self {
        Self::from(&matrix)
    }
}

impl From<&Transform> for Mat4 {
    fn from(transform: &Transform) -> Self {
        Mat4::from_scale_rotation_translation(
            transform.scale,
            transform.rotation,
            transform.translation,
        )
    }
}

impl From<Transform> for Mat4 {
    fn from(transform: Transform) -> Self {
        Mat4::from(&transform)
    }
}

new_key_type! {
    /// Opaque key for transforms.
    pub struct TransformKey;
}

/// Result type for transform operations.
pub type Result<T> = std::result::Result<T, AwsmTransformError>;

/// Transform-related errors.
#[derive(Error, Debug)]
pub enum AwsmTransformError {
    #[error("[transform] local transform does not exist {0:?}")]
    LocalNotFound(TransformKey),

    #[error("[transform] world transform does not exist {0:?}")]
    WorldNotFound(TransformKey),

    #[error("[transform] cannot modify root node")]
    CannotModifyRootNode,

    #[error("[transform] buffer slot missing {0:?}")]
    TransformBufferSlotMissing(TransformKey),

    #[error("[transform] cannot get parent of root node")]
    CannotGetParentOfRootNode,

    #[error("[transform] cannot get parent for {0:?}")]
    CannotGetParent(TransformKey),

    #[error("[transform] {0:?}")]
    Core(#[from] AwsmCoreError),

    #[error("[transform] {0:?}")]
    Skin(#[from] AwsmSkinError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::shared_arena::SharedArena;
    use glam::{Quat, Vec3};

    fn sample_worlds() -> Vec<Mat4> {
        vec![
            Mat4::IDENTITY,
            Mat4::from_translation(Vec3::new(1.0, -2.0, 3.5)),
            Mat4::from_scale_rotation_translation(
                Vec3::new(2.0, 0.5, 1.25),
                Quat::from_rotation_y(0.7) * Quat::from_rotation_x(-0.3),
                Vec3::new(-4.0, 5.0, 6.0),
            ),
            Mat4::from_scale(Vec3::new(-1.0, 1.0, 1.0)), // mirrored (determinant < 0)
        ]
    }

    /// The packed 112-byte entry equals the reference layout: model matrix
    /// in bytes 0..64, inverse-transpose normal matrix as three padded
    /// vec3 columns at NORMAL_OFFSET.
    #[test]
    fn packed_bytes_match_reference() {
        for world in sample_worlds() {
            let packed = pack_world_transform(&world);

            // Model bytes.
            let model = world.to_cols_array();
            let model_bytes =
                unsafe { std::slice::from_raw_parts(model.as_ptr() as *const u8, 64) };
            assert_eq!(&packed[0..64], model_bytes, "model bytes mismatch");

            // Normal bytes (3 columns × 12 useful bytes, 4 pad each).
            let nm = glam::Mat3::from_mat4(world.inverse().transpose()).to_cols_array();
            for col in 0..3usize {
                let off = Transforms::NORMAL_OFFSET + col * 16;
                let src = col * 3;
                let want = unsafe {
                    std::slice::from_raw_parts(nm[src..src + 3].as_ptr() as *const u8, 12)
                };
                assert_eq!(&packed[off..off + 12], want, "normal col {col} mismatch");
                // Padding float stays zero.
                assert_eq!(&packed[off + 12..off + 16], &[0u8; 4], "normal pad {col}");
            }
        }
    }

    /// H3: a world `Mat4` round-trips through the arena byte-for-byte, so any
    /// `world_aabb` the bounds pass derives from the arena-sourced matrix (it
    /// transforms the geometry's local AABB by exactly this matrix) equals the
    /// single-threaded compute. This is the basis for sim-owned culling /
    /// shadow / pick correctness.
    #[test]
    fn arena_world_matrix_roundtrips_for_bounds() {
        let worlds = sample_worlds();
        let mut arena = SharedArena::new(TRANSFORM_ARENA_STRIDE, 8, 8);
        let mut slots = Vec::new();
        for world in &worlds {
            let slot = arena.allocate();
            slots.push(slot);
            let cols = world.to_cols_array();
            let raw = unsafe { std::slice::from_raw_parts(cols.as_ptr() as *const u8, 64) };
            arena.write_value(slot, raw);
        }
        let r = arena.descend();
        assert_eq!(r.updated, worlds.len());
        for (i, world) in worlds.iter().enumerate() {
            let m = &arena.mirror()
                [slots[i] * TRANSFORM_ARENA_STRIDE..(slots[i] + 1) * TRANSFORM_ARENA_STRIDE];
            let mut cols = [0f32; 16];
            unsafe {
                std::ptr::copy_nonoverlapping(m.as_ptr(), cols.as_mut_ptr() as *mut u8, 64);
            }
            let from_arena = Mat4::from_cols_array(&cols);
            assert_eq!(
                from_arena.to_cols_array(),
                world.to_cols_array(),
                "arena world matrix differs from source for world {i}"
            );
            // A representative local-AABB corner transforms identically.
            let corner = glam::Vec3::new(0.5, -0.5, 0.5);
            assert_eq!(
                from_arena.transform_point3(corner),
                world.transform_point3(corner),
                "arena-derived AABB corner differs for world {i}"
            );
        }
    }

    /// The arena descent → pack pipeline (M2's shared path) produces the
    /// SAME 112 bytes as the direct single-threaded pack. This is the
    /// gate's "packed bytes equal current packing", proven without a GPU:
    /// write semantic 64-byte world matrices into the arena, descend, read
    /// the mirror back, pack, compare.
    #[test]
    fn arena_descend_pack_matches_direct_pack() {
        let worlds = sample_worlds();
        let mut arena = SharedArena::new(TRANSFORM_ARENA_STRIDE, 8, 8);
        let mut slots = Vec::new();
        for world in &worlds {
            let slot = arena.allocate();
            slots.push(slot);
            let cols = world.to_cols_array();
            let raw = unsafe { std::slice::from_raw_parts(cols.as_ptr() as *const u8, 64) };
            arena.write_value(slot, raw);
        }

        let result = arena.descend();
        assert_eq!(result.torn, 0);
        assert_eq!(result.updated, worlds.len());

        for (i, world) in worlds.iter().enumerate() {
            let slot = slots[i];
            let m =
                &arena.mirror()[slot * TRANSFORM_ARENA_STRIDE..(slot + 1) * TRANSFORM_ARENA_STRIDE];
            let mut cols = [0f32; 16];
            unsafe {
                std::ptr::copy_nonoverlapping(m.as_ptr(), cols.as_mut_ptr() as *mut u8, 64);
            }
            let from_arena = pack_world_transform(&Mat4::from_cols_array(&cols));
            let direct = pack_world_transform(world);
            assert_eq!(
                from_arena, direct,
                "arena pack != direct pack for world {i}"
            );
        }
    }
}
