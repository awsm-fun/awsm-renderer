//! Mesh storage and GPU buffer management.

pub mod buffer_info;
pub mod error;
pub mod geometry;
pub mod mesh;
pub mod meta;
pub mod morphs;
pub mod skin_lod;
pub mod skins;

use std::collections::HashMap;

use awsm_renderer_core::buffers::{BufferDescriptor, BufferUsage};
use awsm_renderer_core::renderer::AwsmRendererWebGpu;
use glam::Mat4;
use slotmap::{new_key_type, DenseSlotMap, SecondaryMap};

use crate::bind_groups::{BindGroupCreate, BindGroups};
use crate::bounds::Aabb;
use crate::buffer::dynamic_storage::DynamicStorageBuffer;
use crate::instances::Instances;
use crate::materials::Materials;
use crate::meshes::buffer_info::MeshBufferVertexInfo;
use crate::transforms::{Transform, TransformKey, Transforms};
use crate::{AwsmRenderer, AwsmRendererLogging};
use buffer_info::{MeshBufferInfoKey, MeshBufferInfos};
use meta::{MeshMeta, MESH_META_INITIAL_CAPACITY};
use skins::{SkinKey, Skins};

use error::{AwsmMeshError, Result};
use mesh::{BillboardMode, Mesh};
use morphs::{GeometryMorphKey, MaterialMorphKey, Morphs};

impl AwsmRenderer {
    /// Duplicates a mesh into an existing transform key and mirrors transparent pass pipeline state.
    pub fn duplicate_mesh_with_transform(
        &mut self,
        mesh_key: MeshKey,
        new_transform_key: TransformKey,
    ) -> crate::error::Result<MeshKey> {
        let new_mesh_key = self.meshes.duplicate_with_transform(
            mesh_key,
            new_transform_key,
            &self.materials,
            &self.transforms,
        )?;

        self.render_passes
            .material_transparent
            .pipelines
            .clone_render_pipeline_key(mesh_key, new_mesh_key);

        self.sync_spatial_for_mesh(new_mesh_key);

        Ok(new_mesh_key)
    }

    /// Clones a mesh and its current transform under the same parent.
    pub fn clone_mesh(&mut self, mesh_key: MeshKey) -> crate::error::Result<MeshKey> {
        let transform_key = self.meshes.get(mesh_key)?.transform_key;
        let local_transform = self.transforms.get_local(transform_key)?.clone();
        let parent_transform = self.transforms.get_parent(transform_key).ok();
        let new_transform_key = self.transforms.insert(local_transform, parent_transform);

        self.duplicate_mesh_with_transform(mesh_key, new_transform_key)
    }

    /// Duplicates all meshes that share a transform, returning the new transform and mesh keys.
    ///
    /// Transparent pass pipeline mappings are copied per duplicated mesh.
    pub fn duplicate_meshes_by_transform_key(
        &mut self,
        transform_key: TransformKey,
    ) -> crate::error::Result<(TransformKey, Vec<MeshKey>)> {
        let source_mesh_keys = self
            .meshes
            .keys_by_transform_key(transform_key)
            .cloned()
            .ok_or(AwsmMeshError::TransformHasNoMeshes(transform_key))?;

        let (new_transform_key, new_mesh_keys) = self.meshes.duplicate_by_transform_key(
            transform_key,
            &self.materials,
            &mut self.transforms,
        )?;

        for (source_mesh_key, new_mesh_key) in source_mesh_keys
            .into_iter()
            .zip(new_mesh_keys.iter().copied())
        {
            self.render_passes
                .material_transparent
                .pipelines
                .clone_render_pipeline_key(source_mesh_key, new_mesh_key);
            self.sync_spatial_for_mesh(new_mesh_key);
        }

        Ok((new_transform_key, new_mesh_keys))
    }

    /// Sets mesh visibility state.
    pub fn set_mesh_hidden(&mut self, mesh_key: MeshKey, hidden: bool) -> crate::error::Result<()> {
        let mesh = self.meshes.get_mut(mesh_key)?;
        mesh.hidden = hidden;
        self.sync_spatial_for_mesh(mesh_key);
        Ok(())
    }

    /// Routes the mesh through the HUD render pass so it draws on top of
    /// world geometry. Used by editor overlay primitives (gizmos, point
    /// handles) that need to remain visible regardless of occluding meshes.
    pub fn set_mesh_hud(&mut self, mesh_key: MeshKey, hud: bool) -> crate::error::Result<()> {
        let mesh = self.meshes.get_mut(mesh_key)?;
        mesh.hud = hud;
        if hud {
            // T2.6: first HUD usage flips the sticky flag so the next
            // `RenderTextures::views` allocates the HUD depth
            // attachment. Stays true for the renderer's lifetime —
            // a single HUD transition is cheap and the alternative
            // would be re-shrinking on every HUD toggle.
            self.meshes.mark_hud_used();
        }
        self.sync_spatial_for_mesh(mesh_key);
        Ok(())
    }

    /// Reassign the material a mesh references. The previous material is left
    /// in the materials map for reuse; callers may remove it via the
    /// `materials` API if they're sure nothing else references it.
    ///
    /// Refreshes the mesh's metadata in the meta buffer so the visibility-
    /// buffer compute pass picks up the new material on the next frame.
    pub fn set_mesh_material(
        &mut self,
        mesh_key: MeshKey,
        new_material_key: crate::materials::MaterialKey,
    ) -> crate::error::Result<()> {
        let mesh = self.meshes.get_mut(mesh_key)?;
        mesh.material_key = new_material_key;
        self.meshes
            .refresh_meta_for_mesh_public(mesh_key, &self.materials, &self.transforms)?;
        Ok(())
    }

    /// Sets (or clears) the cheap-material variant for a mesh. The
    /// cheap variant takes over the mesh's shading whenever last-frame
    /// coverage drops below the threshold (per-mesh
    /// `cheap_material_pixel_threshold`, falling back to the
    /// renderer's `default_cheap_material_pixel_threshold`).
    ///
    /// Constraint (validated here): the cheap material MUST share the
    /// authored material's [`crate::materials::MaterialShaderId`] AND its
    /// [`is_transparency_pass`](crate::materials::Material::is_transparency_pass) classification. The per-frame routing
    /// in `Meshes::refresh_cheap_material_routing` only swaps the
    /// GPU-side `material_offset` — it doesn't migrate the mesh
    /// between the opaque / transparent renderable pools or rebuild
    /// the opaque-compute pipeline key. A mismatched pair would
    /// either silently no-op the cheap path (same-pass, different
    /// shader_id → wrong compute kernel) or, worse, run a transparent
    /// material through the opaque pipeline → layout mismatch /
    /// garbage shading. Returns `IncompatibleCheapMaterial` rather
    /// than swallowing the mistake.
    ///
    /// Pass `cheap_material_key = None` to clear an existing cheap
    /// variant. The next frame's `refresh_cheap_material_routing`
    /// re-patches the mesh's `material_offset` back to the authored
    /// material.
    pub fn set_mesh_cheap_material(
        &mut self,
        mesh_key: MeshKey,
        cheap_material_key: Option<crate::materials::MaterialKey>,
        cheap_material_pixel_threshold: Option<u32>,
    ) -> crate::error::Result<()> {
        let authored_material = {
            let mesh = self.meshes.get(mesh_key)?;
            mesh.material_key
        };
        if let Some(cheap) = cheap_material_key {
            let authored_shader = self.materials.shader_id(authored_material);
            let cheap_shader = self.materials.shader_id(cheap);
            if authored_shader != cheap_shader {
                return Err(crate::meshes::AwsmMeshError::IncompatibleCheapMaterial {
                    authored: authored_material,
                    cheap,
                    reason: format!(
                        "shader_id mismatch (authored {authored_shader:?} vs cheap {cheap_shader:?}) — \
                         the per-frame routing only swaps material_offset; cross-shader cheap variants \
                         need a separate pipeline + render pool migration that isn't wired."
                    ),
                }
                .into());
            }
            let authored_blend = self.materials.is_transparency_pass(authored_material);
            let cheap_blend = self.materials.is_transparency_pass(cheap);
            if authored_blend != cheap_blend {
                return Err(crate::meshes::AwsmMeshError::IncompatibleCheapMaterial {
                    authored: authored_material,
                    cheap,
                    reason: format!(
                        "transparency-pass classification mismatch (authored opaque?={} vs cheap opaque?={}) — \
                         a cheap variant on the opposite pass would land in the wrong renderable list.",
                        !authored_blend, !cheap_blend
                    ),
                }
                .into());
            }
        }
        let mesh = self.meshes.get_mut(mesh_key)?;
        mesh.cheap_material_key = cheap_material_key;
        mesh.cheap_material_pixel_threshold = cheap_material_pixel_threshold;
        Ok(())
    }

    /// Removes all meshes under a transform and clears any pass-local mesh state.
    pub fn remove_meshes_by_transform_key(&mut self, transform_key: TransformKey) -> Vec<MeshKey> {
        let mesh_keys = self
            .meshes
            .keys_by_transform_key(transform_key)
            .cloned()
            .unwrap_or_default();

        if mesh_keys.is_empty() {
            return mesh_keys;
        }

        self.meshes.remove_by_transform_key(transform_key);

        for mesh_key in &mesh_keys {
            self.render_passes
                .material_transparent
                .pipelines
                .remove_render_pipeline_key(*mesh_key);
            self.drop_spatial_for_mesh(*mesh_key);
        }

        mesh_keys
    }

    /// Removes one mesh and clears any pass-local mesh state.
    pub fn remove_mesh(&mut self, mesh_key: MeshKey) -> bool {
        let removed = self.meshes.remove(mesh_key).is_some();

        if removed {
            self.render_passes
                .material_transparent
                .pipelines
                .remove_render_pipeline_key(mesh_key);
            self.drop_spatial_for_mesh(mesh_key);
        }

        removed
    }

    /// Splits a mesh out to a new transform key.
    pub fn split_mesh(&mut self, mesh_key: MeshKey) -> crate::error::Result<TransformKey> {
        let new_transform_key =
            self.meshes
                .split_mesh(mesh_key, &mut self.transforms, &self.materials)?;
        self.sync_spatial_for_mesh(mesh_key);
        Ok(new_transform_key)
    }

    /// Splits all meshes under a transform into new transform keys.
    pub fn split_meshes_by_transform_key(
        &mut self,
        transform_key: TransformKey,
    ) -> crate::error::Result<Vec<(MeshKey, TransformKey)>> {
        let result = self.meshes.split_meshes_by_transform_key(
            transform_key,
            &mut self.transforms,
            &self.materials,
        )?;
        for (mesh_key, _) in &result {
            self.sync_spatial_for_mesh(*mesh_key);
        }
        Ok(result)
    }

    /// Joins meshes under a shared transform, optionally overriding the transform.
    pub fn join_meshes(
        &mut self,
        mesh_keys: &[MeshKey],
        transform_override: Option<Transform>,
    ) -> crate::error::Result<(TransformKey, Vec<MeshKey>)> {
        let (new_transform_key, moved) = self.meshes.join_meshes(
            mesh_keys,
            &mut self.transforms,
            &self.materials,
            transform_override,
        )?;
        for mesh_key in &moved {
            self.sync_spatial_for_mesh(*mesh_key);
        }
        Ok((new_transform_key, moved))
    }

    /// Enables GPU instancing for an opaque mesh — sync because the
    /// transparent pipeline rebuild is unnecessary when the mesh doesn't
    /// flow through the transparent pass. Use `enable_mesh_instancing` for
    /// meshes that may also render via the transparent pipeline.
    pub fn enable_mesh_instancing_opaque(
        &mut self,
        mesh_key: MeshKey,
        transforms: &[Transform],
    ) -> crate::error::Result<()> {
        let transform_key = self.meshes.get(mesh_key)?.transform_key;
        if transforms.is_empty() {
            return Err(AwsmMeshError::InstancingMissingTransforms(mesh_key).into());
        }
        {
            let mesh = self.meshes.get_mut(mesh_key)?;
            if mesh.instanced {
                return Err(AwsmMeshError::InstancingAlreadyEnabled(mesh_key).into());
            }
            mesh.instanced = true;
        }
        self.instances.transform_insert(transform_key, transforms)?;
        Ok(())
    }

    /// Enables GPU instancing for a mesh with explicit instance transforms.
    pub async fn enable_mesh_instancing(
        &mut self,
        mesh_key: MeshKey,
        transforms: &[Transform],
    ) -> crate::error::Result<()> {
        let buffer_info_key = self.meshes.buffer_info_key(mesh_key)?;
        let transform_key = self.meshes.get(mesh_key)?.transform_key;
        if transforms.is_empty() {
            return Err(AwsmMeshError::InstancingMissingTransforms(mesh_key).into());
        }
        {
            let mesh = self.meshes.get_mut(mesh_key)?;
            if mesh.instanced {
                return Err(AwsmMeshError::InstancingAlreadyEnabled(mesh_key).into());
            }
            mesh.instanced = true;
        }

        self.instances.transform_insert(transform_key, transforms)?;

        let mesh = self.meshes.get(mesh_key)?;
        // Only transparent-pass meshes get a transparent pipeline (see the
        // matching guard in `add_raw_mesh`): an opaque dynamic material's
        // author WGSL targets the opaque contract and can't compile against
        // the transparent fragment.
        if !self.materials.is_transparency_pass(mesh.material_key) {
            return Ok(());
        }
        let writes_depth = self.materials.transparent_writes_depth(mesh.material_key);
        let (mat_base, mat_pbr_features) = self.materials.transparent_variant(mesh.material_key);
        let dynamic_shader_id = matches!(mat_base, crate::dynamic_materials::ShadingBase::Custom)
            .then(|| self.materials.shader_id(mesh.material_key));
        let dynamic_shader =
            dynamic_shader_id.and_then(|id| self.dynamic_materials.shader_info_for(id));
        self.render_passes
            .material_transparent
            .pipelines
            .set_render_pipeline_key(
                &self.gpu,
                mesh,
                mesh_key,
                buffer_info_key,
                &mut self.shaders,
                &mut self.pipelines,
                &self.render_passes.material_transparent.bind_groups,
                &self.pipeline_layouts,
                &self.meshes.buffer_infos,
                &self.anti_aliasing,
                &self.textures,
                &self.render_textures.formats,
                writes_depth,
                mat_base,
                mat_pbr_features,
                dynamic_shader_id,
                dynamic_shader,
            )
            .await?;

        Ok(())
    }

    /// Replaces all instance transforms for an instanced mesh.
    pub fn set_mesh_instances(
        &mut self,
        mesh_key: MeshKey,
        transforms: &[Transform],
    ) -> crate::error::Result<()> {
        if transforms.is_empty() {
            return Err(AwsmMeshError::InstancingMissingTransforms(mesh_key).into());
        }
        let mesh = self.meshes.get(mesh_key)?;
        if !mesh.instanced {
            return Err(AwsmMeshError::InstancingNotEnabled(mesh_key).into());
        }

        // In-place when the count is unchanged (the per-frame particle path
        // allocates nothing); insert on shape changes.
        self.instances
            .transform_write_all(mesh.transform_key, transforms)?;

        Ok(())
    }

    /// Sets the per-mesh camera-facing billboard mode and refreshes geometry
    /// meta so the next frame's vertex shader picks up the new mode.
    pub fn set_mesh_billboard_mode(
        &mut self,
        mesh_key: MeshKey,
        mode: BillboardMode,
    ) -> crate::error::Result<()> {
        if let Ok(mesh) = self.meshes.get_mut(mesh_key) {
            mesh.billboard_mode = mode;
        } else {
            return Err(AwsmMeshError::MeshNotFound(mesh_key).into());
        }
        self.meshes
            .refresh_meta_for_mesh_public(mesh_key, &self.materials, &self.transforms)?;
        Ok(())
    }

    /// Writes per-instance attributes (color + alpha + size) for every mesh
    /// sharing the given transform key, and refreshes those meshes' geometry
    /// meta so the shading pass picks up the new `instance_attr_base`.
    ///
    /// The number of `attrs` must match the number of transforms previously
    /// written via `set_mesh_instances` / `transform_insert`. Mismatches
    /// (including the case where no transforms exist yet for the key)
    /// return `AwsmMeshError::InstanceAttrCountMismatch` — silently
    /// accepting a shorter slice would leave the shader reading past the
    /// logical attr range into zero-fill / neighbor allocations and tint
    /// the trailing instances with garbage.
    pub fn set_mesh_instance_attrs(
        &mut self,
        transform_key: TransformKey,
        attrs: &[crate::instances::InstanceAttr],
    ) -> crate::error::Result<()> {
        let transforms = self
            .instances
            .transform_instance_count(transform_key)
            .unwrap_or(0);
        if transforms != attrs.len() {
            return Err(AwsmMeshError::InstanceAttrCountMismatch {
                transform_key,
                attrs: attrs.len(),
                transforms,
            }
            .into());
        }
        self.instances.attribute_write_all(transform_key, attrs)?;

        let base = self
            .instances
            .attribute_buffer_offset(transform_key)
            .map(|off| (off / crate::instances::InstanceAttr::BYTE_SIZE) as u32)
            .unwrap_or(u32::MAX);

        let mesh_keys: Vec<MeshKey> = self
            .meshes
            .keys_by_transform_key(transform_key)
            .cloned()
            .unwrap_or_default();

        for mesh_key in mesh_keys {
            if let Ok(mesh) = self.meshes.get_mut(mesh_key) {
                mesh.instance_attr_base = base;
            }
            self.meshes.refresh_meta_for_mesh_public(
                mesh_key,
                &self.materials,
                &self.transforms,
            )?;
        }

        Ok(())
    }

    /// Appends a single instance transform to an instanced mesh.
    pub fn append_mesh_instance(
        &mut self,
        mesh_key: MeshKey,
        transform: Transform,
    ) -> crate::error::Result<usize> {
        let start_index = self.append_mesh_instances(mesh_key, &[transform])?;
        Ok(start_index)
    }

    /// Appends instance transforms to an instanced mesh. Keeps any
    /// already-bound per-instance attributes extended in lockstep with
    /// default `InstanceAttr` entries so the shading pass's
    /// `instance_attrs[base + instance_index]` lookup never reads past
    /// the logical slice.
    pub fn append_mesh_instances(
        &mut self,
        mesh_key: MeshKey,
        transforms: &[Transform],
    ) -> crate::error::Result<usize> {
        if transforms.is_empty() {
            return Err(AwsmMeshError::InstancingMissingTransforms(mesh_key).into());
        }

        let mesh = self.meshes.get(mesh_key)?;
        if !mesh.instanced {
            return Err(AwsmMeshError::InstancingNotEnabled(mesh_key).into());
        }
        let transform_key = mesh.transform_key;
        if self
            .instances
            .transform_instance_count(transform_key)
            .is_none()
        {
            return Err(AwsmMeshError::InstancingMissingTransforms(mesh_key).into());
        }

        let start_index = self.instances.transform_extend(transform_key, transforms)?;
        self.instances
            .attribute_extend_with_default(transform_key, transforms.len())?;
        Ok(start_index)
    }

    /// Reserves additional instance slots for an instanced mesh. Mirrors
    /// `append_mesh_instances` for attrs: if attrs are already bound,
    /// extend with defaults so the invariant holds even when reserved
    /// slots are written via `attribute_update` directly.
    pub fn reserve_mesh_instances(
        &mut self,
        mesh_key: MeshKey,
        additional: usize,
    ) -> crate::error::Result<usize> {
        let mesh = self.meshes.get(mesh_key)?;
        if !mesh.instanced {
            return Err(AwsmMeshError::InstancingNotEnabled(mesh_key).into());
        }
        let transform_key = mesh.transform_key;
        if self
            .instances
            .transform_instance_count(transform_key)
            .is_none()
        {
            return Err(AwsmMeshError::InstancingMissingTransforms(mesh_key).into());
        }

        let start_index = self
            .instances
            .transform_reserve(transform_key, additional)?;
        self.instances
            .attribute_extend_with_default(transform_key, additional)?;
        Ok(start_index)
    }
}

/// Shared mesh resource data and buffer offsets.
#[derive(Debug, Clone)]
pub struct MeshResource {
    pub buffer_info_key: MeshBufferInfoKey,
    pub visibility_geometry_data_offset: Option<usize>,
    pub transparency_geometry_data_offset: Option<usize>,
    pub custom_attribute_data_offset: usize,
    pub custom_attribute_index_offset: usize,
    pub aabb: Option<Aabb>,
    pub geometry_morph_key: Option<GeometryMorphKey>,
    pub material_morph_key: Option<MaterialMorphKey>,
    pub skin_key: Option<SkinKey>,
    pub refcount: usize,
}

/// Mesh list with shared resources and GPU buffers.
pub struct Meshes {
    list: DenseSlotMap<MeshKey, Mesh>,
    resources: DenseSlotMap<MeshResourceKey, MeshResource>,
    /// Registered geometry sources (§1 ②). CPU-only retained source, held from
    /// `register_geometry` until its first `commit_load` packs+uploads the kinds
    /// its bound materials need, then dropped. The transaction's geometry-dedup
    /// unit — many meshes share one [`GeometryKey`]. (Populated/consumed by later
    /// steps; for now the registry exists in parallel to the legacy `insert` path.)
    geometries: DenseSlotMap<geometry::GeometryKey, geometry::GeometrySource>,
    /// Which geometry each pending/committed mesh binds to (§1 ③). Set by
    /// `add_mesh`; read by `resolve_geometry` to wire the shared resource.
    mesh_to_geometry: SecondaryMap<MeshKey, geometry::GeometryKey>,
    /// Reverse: the meshes bound to each registered geometry, so `resolve_geometry`
    /// can union their materials' kinds + wire them all to the one shared resource.
    geometry_to_meshes: SecondaryMap<geometry::GeometryKey, Vec<MeshKey>>,
    mesh_to_resource: SecondaryMap<MeshKey, MeshResourceKey>,
    transform_to_meshes: SecondaryMap<TransformKey, Vec<MeshKey>>,
    // Merged geometry pool: one allocation per mesh holds
    // [visibility_data || custom_attribute_index || custom_attribute_data]
    // contiguously. Per-mesh sub-offsets live in `MeshResource`. Bound
    // once as `visibility_data` on the opaque compute pass and reused as
    // a vertex/index buffer by the geometry + transparent passes.
    mesh_geometry_pool_buffers: DynamicStorageBuffer<MeshResourceKey>,
    mesh_geometry_pool_gpu_buffer: web_sys::GpuBuffer,
    mesh_geometry_pool_dirty: bool,
    // visibility geometry index buffers (position, triangle-id, barycentric, etc.)
    visibility_geometry_index_buffers: DynamicStorageBuffer<MeshResourceKey>,
    visibility_geometry_index_gpu_buffer: web_sys::GpuBuffer,
    visibility_geometry_index_dirty: bool,
    // transparency geometry data buffers (position, etc.)
    transparency_geometry_data_buffers: DynamicStorageBuffer<MeshResourceKey>,
    transparency_geometry_data_gpu_buffer: web_sys::GpuBuffer,
    transparency_geometry_data_dirty: bool,
    mesh_geometry_pool_uploader: crate::buffer::mapped_uploader::MappedUploader,
    visibility_geometry_index_uploader: crate::buffer::mapped_uploader::MappedUploader,
    transparency_geometry_data_uploader: crate::buffer::mapped_uploader::MappedUploader,
    // buffer infos
    pub buffer_infos: MeshBufferInfos,
    // meta
    pub meta: MeshMeta,
    // morphs and skins
    pub morphs: Morphs,
    pub skins: Skins,
    /// Last-frame effective `MaterialKey` per mesh — the value
    /// `Mesh::effective_material_key` resolved to. Used by
    /// `refresh_cheap_material_routing` to detect coverage-cross-threshold
    /// transitions and patch `MaterialMeshMeta.material_offset` only on
    /// the meshes that actually crossed; steady-state writes are O(0)
    /// even when every mesh has a cheap variant authored.
    last_effective_material: SecondaryMap<MeshKey, crate::materials::MaterialKey>,
    /// Per-skin "frames-since-last-frame-with-coverage > 0" counter,
    /// driving the coverage-skin-skip grace period in `update_world`.
    ///
    /// Tracks: while ANY consumer mesh of a skin had non-zero coverage
    /// last frame, the counter resets to 0. When every consumer hit
    /// zero coverage, the counter increments. The skip gate only
    /// fires once the counter clears the grace threshold AND no
    /// consumer mesh sits inside the camera frustum (the BVH override).
    ///
    /// Default 0 = never-observed-skin / fresh-insertion = "still in
    /// grace period" so the very first frame after a skin's meshes
    /// materialise, skinning runs normally (no rest-pose pop).
    skin_zero_coverage_grace: SecondaryMap<SkinKey, u32>,
    /// Scratch inverted-index `skin_key → Vec<MeshKey>` reused across
    /// `update_world` invocations. The outer HashMap is cleared (not
    /// dropped) at the start of each frame so the bucket-storage Vec
    /// allocations stick around — steady-state per-frame cost drops
    /// to "rebucket meshes that have a skin", with zero heap traffic
    /// once the map's capacity has stabilised. A persistent inverted
    /// index maintained on mesh insert/remove would be marginally
    /// faster still, but every mesh-mutation path would have to
    /// remember to keep it in sync; reuse-and-clear gets us the bulk
    /// of the win with none of the maintenance burden.
    skin_consumers_scratch: HashMap<SkinKey, Vec<MeshKey>>,
    /// T2.6 sticky flag — set to `true` the first time any mesh
    /// transitions to the HUD render group, and never cleared. The
    /// HUD depth texture is allocated only when this flag is true,
    /// saving a full-screen Depth32/Depth24 attachment on builds
    /// that never use HUD overlays (the library / game default).
    has_seen_hud: bool,
    /// Monotonic counter bumped every time a mesh enters the HUD render
    /// group (post-insert `set_mesh_hud(true)` or insert-with-`hud`).
    /// The per-frame transparent/HUD pipeline-resolve kick in `render()`
    /// folds this into its dirty signal so a freshly-inserted HUD
    /// overlay (editor gizmo, in-game HUD primitive) gets its transparent
    /// pipeline variant resolved on the next frame — even when the
    /// texture-pool shape hasn't changed. Stays at 0 for builds that
    /// never use HUD, so the resolve kick early-outs and they pay
    /// nothing.
    hud_revision: u64,
}
impl Meshes {
    // Initial sizes assume ~1000 vertices per mesh
    // but this is just an allocation, can be divided many ways
    const INDICES_INITIAL_SIZE: usize = MESH_META_INITIAL_CAPACITY * 3 * 1000;

    const VISIBILITY_GEOMETRY_INITIAL_SIZE: usize =
        Self::INDICES_INITIAL_SIZE * MeshBufferVertexInfo::VISIBILITY_GEOMETRY_BYTE_SIZE;

    const TRANSPARENCY_GEOMETRY_INITIAL_SIZE: usize =
        Self::INDICES_INITIAL_SIZE * MeshBufferVertexInfo::TRANSPARENCY_GEOMETRY_BYTE_SIZE;

    // Attribute data is much smaller - only custom attributes (UVs, colors, joints, weights).
    // Estimate: 2 UV sets (8 bytes each) = 16 bytes per vertex as a reasonable starting point.
    // For textureless models this will be 0, but buffer will grow as needed.
    const ATTRIBUTE_DATA_INITIAL_SIZE: usize = Self::INDICES_INITIAL_SIZE * 16;

    // Merged pool capacity = vis_data + attr_index + attr_data
    // (visibility-byte stride 56 + index stride 4 + attr stride 16).
    const MESH_GEOMETRY_POOL_INITIAL_SIZE: usize = Self::VISIBILITY_GEOMETRY_INITIAL_SIZE
        + Self::INDICES_INITIAL_SIZE
        + Self::ATTRIBUTE_DATA_INITIAL_SIZE;

    /// Creates mesh storage and GPU buffers.
    pub fn new(gpu: &AwsmRendererWebGpu) -> Result<Self> {
        Ok(Self {
            list: DenseSlotMap::with_key(),
            resources: DenseSlotMap::with_key(),
            geometries: DenseSlotMap::with_key(),
            mesh_to_geometry: SecondaryMap::new(),
            geometry_to_meshes: SecondaryMap::new(),
            mesh_to_resource: SecondaryMap::new(),
            transform_to_meshes: SecondaryMap::new(),
            buffer_infos: MeshBufferInfos::new(),
            // Merged geometry pool: vis_data + attr_index + attr_data per mesh.
            mesh_geometry_pool_buffers: DynamicStorageBuffer::new(
                Self::MESH_GEOMETRY_POOL_INITIAL_SIZE,
                Some("MeshGeometryPool".to_string()),
            ),
            mesh_geometry_pool_gpu_buffer: gpu.create_buffer(
                &BufferDescriptor::new(
                    Some("MeshGeometryPool"),
                    Self::MESH_GEOMETRY_POOL_INITIAL_SIZE,
                    BufferUsage::new()
                        .with_copy_dst()
                        .with_vertex()
                        .with_storage()
                        .with_index(),
                )
                .into(),
            )?,
            mesh_geometry_pool_dirty: true,
            // visibility index
            visibility_geometry_index_buffers: DynamicStorageBuffer::new(
                Self::INDICES_INITIAL_SIZE,
                Some("MeshVisibilityIndex".to_string()),
            ),
            visibility_geometry_index_gpu_buffer: gpu.create_buffer(
                &BufferDescriptor::new(
                    Some("MeshVisibilityIndex"),
                    Self::INDICES_INITIAL_SIZE,
                    BufferUsage::new().with_copy_dst().with_index(),
                )
                .into(),
            )?,
            visibility_geometry_index_dirty: true,
            // transparency geometry
            transparency_geometry_data_buffers: DynamicStorageBuffer::new(
                Self::TRANSPARENCY_GEOMETRY_INITIAL_SIZE,
                Some("MeshTransparencyData".to_string()),
            ),
            transparency_geometry_data_gpu_buffer: gpu.create_buffer(
                &BufferDescriptor::new(
                    Some("MeshTransparencyData"),
                    Self::TRANSPARENCY_GEOMETRY_INITIAL_SIZE,
                    BufferUsage::new().with_copy_dst().with_vertex(),
                )
                .into(),
            )?,
            transparency_geometry_data_dirty: true,
            mesh_geometry_pool_uploader: crate::buffer::mapped_uploader::MappedUploader::new(
                "MeshGeometryPool",
            ),
            visibility_geometry_index_uploader: crate::buffer::mapped_uploader::MappedUploader::new(
                "MeshVisibilityIndex",
            ),
            transparency_geometry_data_uploader:
                crate::buffer::mapped_uploader::MappedUploader::new("MeshTransparencyData"),
            meta: MeshMeta::new(gpu)?,
            // attribute morphs and skins
            morphs: Morphs::new(gpu)?,
            skins: Skins::new(gpu)?,
            last_effective_material: SecondaryMap::new(),
            skin_zero_coverage_grace: SecondaryMap::new(),
            skin_consumers_scratch: HashMap::new(),
            has_seen_hud: false,
            hud_revision: 0,
        })
    }

    /// Has any mesh ever been routed through the HUD render group?
    /// Sticky-true; used by `RenderTextures::views` to defer
    /// allocation of the HUD depth attachment until a HUD renderable
    /// actually exists. Builds that never insert a HUD mesh (the
    /// library / game default) save a full-screen depth attachment.
    pub fn has_seen_hud(&self) -> bool {
        self.has_seen_hud
    }

    /// Revision counter that bumps whenever a mesh enters the HUD render
    /// group. The `render()` transparent/HUD resolve kick watches this
    /// (together with the texture-pool shape) to decide when to
    /// re-resolve HUD meshes' transparent pipeline variants.
    pub fn hud_revision(&self) -> u64 {
        self.hud_revision
    }

    /// Internal: stickily mark that HUD rendering is now in use, and
    /// bump the HUD revision so the per-frame resolve kick re-checks.
    /// Called from the public `set_mesh_hud(.., true)` and any other
    /// insertion path that places a mesh into the HUD group from
    /// scratch.
    pub(crate) fn mark_hud_used(&mut self) {
        self.has_seen_hud = true;
        self.hud_revision = self.hud_revision.wrapping_add(1);
    }

    /// Walk every mesh with a `cheap_material_key` authored and patch
    /// its `MaterialMeshMeta.material_offset` to point at the
    /// *effective* material for this frame (cheap when coverage is
    /// below threshold, authored otherwise). Idempotent — the
    /// `last_effective_material` sidecar tracks the last patched
    /// value, so meshes whose effective key didn't change generate no
    /// GPU writes.
    ///
    /// Safety: the cheap-material compatibility constraint (same
    /// `MaterialShaderId` AND same `is_transparency_pass()`
    /// classification) is enforced by
    /// [`AwsmRenderer::set_mesh_cheap_material`] at authoring time —
    /// it returns `AwsmMeshError::IncompatibleCheapMaterial` on any
    /// pair that would violate it. This per-frame refresh therefore
    /// only swaps `material_offset`, not pipeline keys or pass-list
    /// membership. Without that constraint a cross-pass cheap
    /// material would land in the wrong renderable list relative to
    /// the meta data the shader reads and either silently no-op the
    /// cheap path or, worse, route a transparent material through
    /// the opaque pipeline. There is no separate validation helper —
    /// the public setter IS the validation entrypoint.
    ///
    /// Called once per frame from `AwsmRenderer::render` after
    /// `coverage.ingest` and before `meshes.meta.write_gpu`.
    pub fn refresh_cheap_material_routing(
        &mut self,
        materials: &crate::materials::Materials,
        coverage: &crate::coverage::MeshCoverage,
        default_threshold: u32,
    ) -> Result<()> {
        // Two-pass to avoid the immutable-borrow-of-self vs
        // mutable-borrow-of-self.meta conflict — gather updates first,
        // then apply.
        let mut updates: Vec<(MeshKey, u32, crate::materials::MaterialKey)> = Vec::new();
        for (mesh_key, mesh) in self.list.iter() {
            if mesh.cheap_material_key.is_none() {
                continue;
            }
            let effective = mesh.effective_material_key(mesh_key, coverage, default_threshold);
            let last = self.last_effective_material.get(mesh_key).copied();
            if last == Some(effective) {
                continue;
            }
            // Resolve to GPU offset now (still inside the immutable
            // borrow) so the update step doesn't need `materials`
            // access — keeps the mutation set tiny.
            let offset = materials.buffer_offset(effective)? as u32;
            updates.push((mesh_key, offset, effective));
        }
        for (mesh_key, offset, effective) in updates {
            // Only cache the patched value when the meta-buffer write
            // actually went through. `set_material_offset` returns
            // `false` when the mesh has no material-meta slot (either
            // never registered or removed between the gather and apply
            // phase — possible if a sync action races this routine).
            // Updating `last_effective_material` unconditionally would
            // suppress every future patch for that mesh: the next
            // gather pass sees `last == effective` and skips it, so the
            // GPU `material_offset` stays at whatever it was when the
            // slot was last alive (frequently a stale, just-recycled
            // index).
            if self.meta.set_material_offset(mesh_key, offset) {
                self.last_effective_material.insert(mesh_key, effective);
            } else {
                tracing::debug!(
                    "refresh_cheap_material_routing: mesh {mesh_key:?} has no material-meta slot; \
                     skipping cache update so the next gather pass retries"
                );
            }
        }
        Ok(())
    }

    /// Mapped-ring upload telemetry for mesh-side per-frame buffers.
    /// Aggregates the three internal uploaders (geometry pool,
    /// visibility index, transparency data) into one rollup.
    pub fn upload_stats(&self) -> crate::buffer::mapped_staging_ring::UploadStats {
        let mut s = self.mesh_geometry_pool_uploader.stats();
        let b = self.visibility_geometry_index_uploader.stats();
        let c = self.transparency_geometry_data_uploader.stats();
        s.peak_ring_depth_used = s
            .peak_ring_depth_used
            .max(b.peak_ring_depth_used)
            .max(c.peak_ring_depth_used);
        s.fallback_count += b.fallback_count + c.fallback_count;
        s.map_async_wait_ms += b.map_async_wait_ms + c.map_async_wait_ms;
        s.bytes_uploaded_via_ring += b.bytes_uploaded_via_ring + c.bytes_uploaded_via_ring;
        s.bytes_uploaded_via_fallback +=
            b.bytes_uploaded_via_fallback + c.bytes_uploaded_via_fallback;
        s.bytes_uploaded_via_writebuffer +=
            b.bytes_uploaded_via_writebuffer + c.bytes_uploaded_via_writebuffer;
        s.resize_count += b.resize_count + c.resize_count;
        s
    }

    /// Register a [`geometry::GeometrySource`] (§1 ②) — CPU-only, NO GPU upload.
    /// Returns a [`geometry::GeometryKey`] that meshes bind to via `add_mesh`; the
    /// per-pass representations are packed+uploaded at the next `commit_load` from
    /// the union of bound materials, then the source is dropped. Many meshes may
    /// share one key (geometry dedup).
    pub fn register_geometry(&mut self, source: geometry::GeometrySource) -> geometry::GeometryKey {
        self.geometries.insert(source)
    }

    /// The retained source for a registered geometry, while it's still present
    /// (i.e. before its first commit consumes + frees it). `None` after that, or
    /// for an unknown key.
    pub fn geometry_source(&self, key: geometry::GeometryKey) -> Option<&geometry::GeometrySource> {
        self.geometries.get(key)
    }

    /// Number of registered geometries still holding source (drives the
    /// `UploadingGeometry` progress count).
    pub fn geometry_count(&self) -> usize {
        self.geometries.len()
    }

    /// Bind an already-built [`Mesh`] to a registered geometry (the deferred
    /// "append" — `add_mesh`'s storage half). Inserts the mesh (sync MeshKey),
    /// fills its world AABB from the geometry source, and records the binding maps.
    /// NO GPU upload, NO resource, NO meta — those happen at the next
    /// `resolve_geometry` (commit). Returns the MeshKey.
    pub(crate) fn bind_mesh(
        &mut self,
        mut mesh: Mesh,
        geometry_key: geometry::GeometryKey,
    ) -> Result<MeshKey> {
        let source = self
            .geometries
            .get(geometry_key)
            .ok_or(AwsmMeshError::GeometryNotFound(geometry_key))?;
        if mesh.world_aabb.is_none() {
            mesh.world_aabb = source.aabb.clone();
        }
        let mesh_key = self.list.insert(mesh);
        self.mesh_to_geometry.insert(mesh_key, geometry_key);
        self.geometry_to_meshes
            .entry(geometry_key)
            .unwrap()
            .or_default()
            .push(mesh_key);
        Ok(mesh_key)
    }

    /// THE geometry commit (`commit_load` phase 0, §2): for every registered
    /// geometry, derive + upload the representation(s) the union of its bound
    /// materials needs — ONCE each — into a single shared resource, wire every
    /// bound mesh to it, then FREE the source. Returns the wired mesh keys (for the
    /// caller to sync into the spatial index). Idempotent on an empty registry.
    ///
    /// Reuses the existing `insert_resource` (upload) + `wire_instance` (per-mesh)
    /// plumbing — the only new logic is the union-of-kinds + pack-from-source. A
    /// geometry registered but never bound is simply dropped (source freed).
    pub(crate) fn resolve_geometry(
        &mut self,
        materials: &Materials,
        transforms: &Transforms,
    ) -> Result<Vec<MeshKey>> {
        let geometry_keys: Vec<geometry::GeometryKey> = self.geometries.keys().collect();
        let mut wired = Vec::new();
        for gkey in geometry_keys {
            wired.extend(self.resolve_one(gkey, materials, transforms)?);
        }
        Ok(wired)
    }

    /// Resolve a SINGLE registered geometry: pack the representation(s) the union
    /// of its bound materials needs (once each), upload into one shared resource,
    /// wire every bound mesh, and free the source. Returns the wired mesh keys
    /// (empty if the geometry is unknown or unbound). Shared by the commit-time
    /// `resolve_geometry` (all pending) and the eager `add_raw_mesh` (its one
    /// geometry — so a one-off raw mesh draws immediately, sync, without a commit).
    pub(crate) fn resolve_one(
        &mut self,
        gkey: geometry::GeometryKey,
        materials: &Materials,
        transforms: &Transforms,
    ) -> Result<Vec<MeshKey>> {
        use crate::meshes::geometry::{geometry_kind, GeometryKind};

        // Take the source OUT (frees it — §1 ②) so the uploads below can borrow
        // `&mut self` without aliasing the registry.
        let Some(source) = self.geometries.remove(gkey) else {
            return Ok(Vec::new());
        };
        let bound = self.geometry_to_meshes.remove(gkey).unwrap_or_default();
        if bound.is_empty() {
            return Ok(Vec::new()); // registered but never bound — nothing to build.
        }
        let mut wired = Vec::new();

        {
            // Union the kinds + tangent-need over the bound meshes' materials.
            let mut want_visibility = false;
            let mut want_transparency = false;
            let mut want_tangents = false;
            for &mk in &bound {
                let Some(mesh) = self.list.get(mk) else {
                    continue;
                };
                let is_hud = mesh.hud;
                let Ok(material) = materials.get(mesh.material_key) else {
                    continue;
                };
                match geometry_kind(material, is_hud) {
                    GeometryKind::Visibility => want_visibility = true,
                    GeometryKind::Transparency => want_transparency = true,
                    GeometryKind::Both => {
                        want_visibility = true;
                        want_transparency = true;
                    }
                }
                if crate::raw_mesh::material_wants_tangents(material) {
                    want_tangents = true;
                }
            }

            // Tangents derived ONCE here (commit-time, gated — see §6 step 2).
            let tangents = if want_tangents {
                source.uvs0.as_ref().and_then(|uvs| {
                    awsm_tangents::generate_tangents(
                        &source.positions,
                        &source.normals,
                        uvs,
                        &source.indices,
                    )
                })
            } else {
                None
            };

            // Pack exactly the needed representation(s), once each.
            let visibility_bytes = want_visibility.then(|| {
                crate::mesh_pack::pack_visibility_bytes(
                    &source.positions,
                    &source.normals,
                    tangents.as_deref(),
                    &source.indices,
                    source.front_face,
                )
            });
            let transparency_bytes = want_transparency.then(|| {
                crate::mesh_pack::pack_transparency_bytes(
                    &source.positions,
                    &source.normals,
                    tangents.as_deref(),
                    source.vertex_count(),
                )
            });

            // Rebuild the layout descriptor from the source (vis/transp Some/None
            // matches what we packed; triangles from the source attributes).
            let triangle_count = source.triangle_count();
            let buffer_info = buffer_info::MeshBufferInfo {
                visibility_geometry_vertex: visibility_bytes.as_ref().map(|_| {
                    MeshBufferVertexInfo {
                        count: triangle_count * 3,
                    }
                }),
                transparency_geometry_vertex: transparency_bytes.as_ref().map(|_| {
                    MeshBufferVertexInfo {
                        count: source.vertex_count(),
                    }
                }),
                triangles: buffer_info::MeshBufferTriangleInfo {
                    count: triangle_count,
                    vertex_attribute_indices: buffer_info::MeshBufferAttributeIndexInfo {
                        count: triangle_count * 3,
                    },
                    vertex_attributes: source.vertex_attributes.clone(),
                    vertex_attributes_size: source.custom_attribute_bytes.len(),
                    triangle_data: buffer_info::MeshBufferTriangleDataInfo {
                        size_per_triangle: 12,
                        total_size: triangle_count * 12,
                    },
                },
                // Morph/skin layout travels with the geometry source (deltas are
                // kind-independent). `None` for the raw path; the glTF decoder fills
                // these when it produces morphed/skinned geometry (§6 step 5).
                geometry_morph: source.geometry_morph_info.clone(),
                material_morph: source.material_morph_info.clone(),
                skin: source.skin_info.clone(),
            };
            let buffer_info_key = self.buffer_infos.insert(buffer_info);

            // ONE shared upload for this geometry; refcount = number of bound meshes.
            let resource_key = self.insert_resource(
                buffer_info_key,
                visibility_bytes.as_deref(),
                transparency_bytes.as_deref(),
                &source.custom_attribute_bytes,
                &source.attribute_index_bytes,
                source.aabb.clone(),
                source.geometry_morph_key,
                source.material_morph_key,
                source.skin_key,
            )?;
            if let Some(resource) = self.resources.get_mut(resource_key) {
                resource.refcount = bound.len();
            }

            // Wire every bound mesh to the shared resource (flags + meta).
            for &mk in &bound {
                self.wire_instance(mk, resource_key, materials, transforms)?;
                self.mesh_to_geometry.remove(mk);
                wired.push(mk);
            }
        }

        Ok(wired)
    }

    /// Public wrapper around `insert` for the raw-mesh path. Same semantics —
    /// see `raw_mesh::AwsmRenderer::add_raw_mesh` for the canonical caller.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_public(
        &mut self,
        mesh: Mesh,
        materials: &Materials,
        transforms: &Transforms,
        buffer_info_key: MeshBufferInfoKey,
        visibility_geometry_data: Option<&[u8]>,
        transparency_geometry_data: Option<&[u8]>,
        attribute_data: &[u8],
        attribute_index: &[u8],
        aabb: Option<Aabb>,
        geometry_morph_key: Option<GeometryMorphKey>,
        material_morph_key: Option<MaterialMorphKey>,
        skin_key: Option<SkinKey>,
    ) -> Result<MeshKey> {
        self.insert(
            mesh,
            materials,
            transforms,
            buffer_info_key,
            visibility_geometry_data,
            transparency_geometry_data,
            attribute_data,
            attribute_index,
            aabb,
            geometry_morph_key,
            material_morph_key,
            skin_key,
        )
    }

    /// Inserts a mesh and its backing resource data, returning a mesh key.
    ///
    /// Pub so external ingestion crates (e.g. `awsm-renderer-gltf`) can
    /// upload meshes through the same path glTF historically used.
    #[allow(clippy::too_many_arguments)]
    pub fn insert(
        &mut self,
        mesh: Mesh,
        materials: &Materials,
        transforms: &Transforms,
        buffer_info_key: MeshBufferInfoKey,
        visibility_geometry_data: Option<&[u8]>,
        transparency_geometry_data: Option<&[u8]>,
        attribute_data: &[u8],
        attribute_index: &[u8],
        aabb: Option<Aabb>,
        geometry_morph_key: Option<GeometryMorphKey>,
        material_morph_key: Option<MaterialMorphKey>,
        skin_key: Option<SkinKey>,
    ) -> Result<MeshKey> {
        // (A mesh's pass-routing flags `has_visibility_geometry` /
        // `has_transparency_geometry` are DERIVED in `wire_instance` below from
        // the resource's actual representation offsets — the single choke point
        // shared by this legacy path AND the geometry-transaction `resolve_geometry`
        // path. They can never disagree with the buffers because the resource's
        // offsets ARE their source of truth.)
        let resource_key = self.insert_resource(
            buffer_info_key,
            visibility_geometry_data,
            transparency_geometry_data,
            attribute_data,
            attribute_index,
            aabb,
            geometry_morph_key,
            material_morph_key,
            skin_key,
        )?;

        self.insert_instance(mesh, resource_key, materials, transforms)
    }

    fn insert_resource(
        &mut self,
        buffer_info_key: MeshBufferInfoKey,
        visibility_geometry_data: Option<&[u8]>,
        transparency_geometry_data: Option<&[u8]>,
        attribute_data: &[u8],
        attribute_index: &[u8],
        aabb: Option<Aabb>,
        geometry_morph_key: Option<GeometryMorphKey>,
        material_morph_key: Option<MaterialMorphKey>,
        skin_key: Option<SkinKey>,
    ) -> Result<MeshResourceKey> {
        let buffer_info = self.buffer_infos.get(buffer_info_key)?;

        // Pre-validate geometry buffer info before any mutation.
        if visibility_geometry_data.is_some() && buffer_info.visibility_geometry_vertex.is_none() {
            return Err(AwsmMeshError::VisibilityGeometryBufferInfoNotFound(
                buffer_info_key,
            ));
        }

        let resource_key = self.resources.insert(MeshResource {
            buffer_info_key,
            visibility_geometry_data_offset: None,
            transparency_geometry_data_offset: None,
            custom_attribute_data_offset: 0,
            custom_attribute_index_offset: 0,
            aabb,
            geometry_morph_key,
            material_morph_key,
            skin_key,
            refcount: 1,
        });

        // Perform all fallible buffer updates in one pass so we can roll back on error.
        // The merged geometry pool holds [vis_data || attr_index || attr_data] per mesh
        // in one allocation; per-section offsets are computed from the section sizes.
        let vis_data_len = visibility_geometry_data.map(|d| d.len()).unwrap_or(0);
        let attr_index_len = attribute_index.len();
        let offsets_result: Result<(Option<usize>, Option<usize>, usize, usize)> = (|| {
            if let Some(geometry_data) = visibility_geometry_data {
                let vertex_info = buffer_info
                    .visibility_geometry_vertex
                    .as_ref()
                    .expect("visibility_geometry_vertex presence pre-validated");
                let mut geometry_index = Vec::new();
                for i in 0..vertex_info.count {
                    geometry_index.extend_from_slice(&(i as u32).to_le_bytes());
                }
                self.visibility_geometry_index_buffers
                    .update(resource_key, &geometry_index)
                    .map_err(|e| {
                        AwsmMeshError::BufferCapacityOverflow(format!(
                            "visibility geometry index: {e}"
                        ))
                    })?;
                self.visibility_geometry_index_dirty = true;

                let mut combined =
                    Vec::with_capacity(geometry_data.len() + attr_index_len + attribute_data.len());
                combined.extend_from_slice(geometry_data);
                combined.extend_from_slice(attribute_index);
                combined.extend_from_slice(attribute_data);
                let base = self
                    .mesh_geometry_pool_buffers
                    .update(resource_key, &combined)
                    .map_err(|e| {
                        AwsmMeshError::BufferCapacityOverflow(format!("mesh geometry pool: {e}"))
                    })?;
                self.mesh_geometry_pool_dirty = true;

                let visibility_offset = Some(base);
                let custom_attribute_indices_offset = base + vis_data_len;
                let custom_attribute_data_offset = base + vis_data_len + attr_index_len;

                let transparency_offset = match transparency_geometry_data {
                    Some(geometry_data) => {
                        let offset = self
                            .transparency_geometry_data_buffers
                            .update(resource_key, geometry_data)
                            .map_err(|e| {
                                AwsmMeshError::BufferCapacityOverflow(format!(
                                    "transparency geometry data: {e}"
                                ))
                            })?;
                        self.transparency_geometry_data_dirty = true;
                        Some(offset)
                    }
                    None => None,
                };

                Ok((
                    visibility_offset,
                    transparency_offset,
                    custom_attribute_indices_offset,
                    custom_attribute_data_offset,
                ))
            } else {
                let mut combined = Vec::with_capacity(attr_index_len + attribute_data.len());
                combined.extend_from_slice(attribute_index);
                combined.extend_from_slice(attribute_data);
                let base = self
                    .mesh_geometry_pool_buffers
                    .update(resource_key, &combined)
                    .map_err(|e| {
                        AwsmMeshError::BufferCapacityOverflow(format!("mesh geometry pool: {e}"))
                    })?;
                self.mesh_geometry_pool_dirty = true;

                let custom_attribute_indices_offset = base;
                let custom_attribute_data_offset = base + attr_index_len;

                let transparency_offset = match transparency_geometry_data {
                    Some(geometry_data) => {
                        let offset = self
                            .transparency_geometry_data_buffers
                            .update(resource_key, geometry_data)
                            .map_err(|e| {
                                AwsmMeshError::BufferCapacityOverflow(format!(
                                    "transparency geometry data: {e}"
                                ))
                            })?;
                        self.transparency_geometry_data_dirty = true;
                        Some(offset)
                    }
                    None => None,
                };

                Ok((
                    None,
                    transparency_offset,
                    custom_attribute_indices_offset,
                    custom_attribute_data_offset,
                ))
            }
        })();

        let (
            visibility_geometry_data_offset,
            transparency_geometry_data_offset,
            custom_attribute_indices_offset,
            custom_attribute_data_offset,
        ) = match offsets_result {
            Ok(offsets) => offsets,
            Err(e) => {
                // Roll back any partial buffer allocations and the resource entry itself.
                self.visibility_geometry_index_buffers.remove(resource_key);
                self.mesh_geometry_pool_buffers.remove(resource_key);
                self.transparency_geometry_data_buffers.remove(resource_key);
                self.resources.remove(resource_key);
                return Err(e);
            }
        };

        // KEEP THIS AROUND FOR DEBUGGING
        // Very helpful - shows all the non-position vertex attributes and triangle indices
        // tracing::info!(
        //     "attribute indices: {:?}",
        //     buffer_info
        //         .triangles
        //         .vertex_attribute_indices
        //         .debug_to_vec(attribute_index)
        // );
        // for attr in buffer_info.triangles.vertex_attributes.iter() {
        //     tracing::info!(
        //         "attribute data {:?}: {:?}",
        //         attr,
        //         buffer_info
        //             .triangles
        //             .debug_get_attribute_vec_f32(attr, attribute_data)
        //     );
        // }

        // for attr in buffer_info.triangles.vertex_attributes.iter() {
        //     match attr {
        //         crate::mesh::MeshBufferVertexAttributeInfo::Custom(
        //             crate::mesh::MeshBufferCustomVertexAttributeInfo::Colors { .. },
        //         ) => {
        //             tracing::info!(
        //                 "attribute data {:?}: {:?}",
        //                 attr,
        //                 buffer_info
        //                     .triangles
        //                     .debug_get_attribute_vec_f32(attr, attribute_data)
        //             );
        //         }
        //         _ => {}
        //     }
        // }

        if let Some(resource) = self.resources.get_mut(resource_key) {
            resource.visibility_geometry_data_offset = visibility_geometry_data_offset;
            resource.transparency_geometry_data_offset = transparency_geometry_data_offset;
            resource.custom_attribute_data_offset = custom_attribute_data_offset;
            resource.custom_attribute_index_offset = custom_attribute_indices_offset;
        }

        Ok(resource_key)
    }

    fn insert_instance(
        &mut self,
        mesh: Mesh,
        resource_key: MeshResourceKey,
        materials: &Materials,
        transforms: &Transforms,
    ) -> Result<MeshKey> {
        let mesh_key = self.list.insert(mesh);
        self.wire_instance(mesh_key, resource_key, materials, transforms)?;
        Ok(mesh_key)
    }

    /// Wires an ALREADY-inserted mesh (in `self.list`) to its shared GPU
    /// `resource_key`: derives the pass-routing flags from the resource's
    /// representation offsets, fills the world AABB, registers the HUD/transform
    /// bookkeeping, and writes the per-mesh meta. The single choke point shared by
    /// the legacy `insert` path (via `insert_instance`) AND the geometry-transaction
    /// `resolve_geometry`, which binds N meshes to one resource. Bumps no refcount —
    /// the caller owns that (each `insert` resource starts at 1; `resolve_geometry`
    /// sets it to the bound-mesh count).
    fn wire_instance(
        &mut self,
        mesh_key: MeshKey,
        resource_key: MeshResourceKey,
        materials: &Materials,
        transforms: &Transforms,
    ) -> Result<()> {
        let (
            resource_aabb,
            buffer_info_key,
            visibility_geometry_data_offset,
            transparency_geometry_data_offset,
            custom_attribute_index_offset,
            custom_attribute_data_offset,
            geometry_morph_key,
            material_morph_key,
            skin_key,
        ) = {
            let resource = self
                .resources
                .get(resource_key)
                .ok_or(AwsmMeshError::ResourceNotFound(resource_key))?;
            (
                resource.aabb.clone(),
                resource.buffer_info_key,
                resource.visibility_geometry_data_offset,
                resource.transparency_geometry_data_offset,
                resource.custom_attribute_index_offset,
                resource.custom_attribute_data_offset,
                resource.geometry_morph_key,
                resource.material_morph_key,
                resource.skin_key,
            )
        };

        let transform_key = {
            let mesh = self
                .list
                .get_mut(mesh_key)
                .ok_or(AwsmMeshError::MeshNotFound(mesh_key))?;
            // Flags DERIVED from the resource's actual representation offsets — the
            // routing flags can never disagree with the uploaded buffers.
            mesh.has_visibility_geometry = visibility_geometry_data_offset.is_some();
            mesh.has_transparency_geometry = transparency_geometry_data_offset.is_some();
            if mesh.world_aabb.is_none() {
                mesh.world_aabb = resource_aabb;
            }
            // T2.6: catch the insert-with-`hud: true` path too — either route into
            // the HUD render group trips the sticky flag so the HUD depth attachment
            // lands by the next render frame.
            if mesh.hud {
                self.has_seen_hud = true;
                self.hud_revision = self.hud_revision.wrapping_add(1);
            }
            mesh.transform_key
        };

        self.mesh_to_resource.insert(mesh_key, resource_key);
        self.transform_to_meshes
            .entry(transform_key)
            .unwrap()
            .or_default()
            .push(mesh_key);

        let mesh = self
            .list
            .get(mesh_key)
            .ok_or(AwsmMeshError::MeshNotFound(mesh_key))?
            .clone();
        let buffer_info = self.buffer_infos.get(buffer_info_key)?;
        self.meta.insert(
            mesh_key,
            &mesh,
            buffer_info,
            visibility_geometry_data_offset,
            custom_attribute_index_offset,
            custom_attribute_data_offset,
            geometry_morph_key,
            material_morph_key,
            skin_key,
            materials,
            transforms,
            &self.morphs,
            &self.skins,
        )?;

        Ok(())
    }

    /// Duplicates a mesh instance and assigns a new transform key.
    ///
    /// This low-level API only duplicates mesh storage state. Callers that need pass-specific
    /// renderer mappings (for example transparent material pipeline keys) should use
    /// `AwsmRenderer::duplicate_mesh_with_transform`.
    pub(crate) fn duplicate_with_transform(
        &mut self,
        mesh_key: MeshKey,
        new_transform_key: TransformKey,
        materials: &Materials,
        transforms: &Transforms,
    ) -> Result<MeshKey> {
        let mesh = self.get(mesh_key)?.clone();
        let resource_key = self.resource_key(mesh_key)?;
        let resource_aabb = {
            let resource = self
                .resources
                .get_mut(resource_key)
                .ok_or(AwsmMeshError::ResourceNotFound(resource_key))?;
            resource.refcount += 1;
            resource.aabb.clone()
        };

        // Pre-transform the AABB into the new transform's world space.
        // `update_world` only refreshes meshes whose transform key is
        // currently dirty — but a duplicated mesh is often re-parented
        // under a transform whose dirty flag has long since cleared,
        // so without this the world_aabb stays at the local-space AABB
        // and consumers (frustum culling, selection bboxes, gizmo
        // centering) see an unrotated, unscaled box.
        let world_aabb = match (
            resource_aabb.as_ref(),
            transforms.get_world(new_transform_key).ok(),
        ) {
            (Some(aabb), Some(world_mat)) => Some(aabb.transformed(world_mat)),
            (Some(aabb), None) => Some(aabb.clone()),
            (None, _) => None,
        };

        let mut new_mesh = mesh.clone();
        new_mesh.transform_key = new_transform_key;
        new_mesh.world_aabb = world_aabb;

        self.insert_instance(new_mesh, resource_key, materials, transforms)
    }

    /// Duplicates all meshes under a transform into a new transform key.
    pub(crate) fn duplicate_by_transform_key(
        &mut self,
        transform_key: TransformKey,
        materials: &Materials,
        transforms: &mut Transforms,
    ) -> Result<(TransformKey, Vec<MeshKey>)> {
        let mesh_keys = self
            .transform_to_meshes
            .get(transform_key)
            .cloned()
            .ok_or(AwsmMeshError::TransformHasNoMeshes(transform_key))?;

        if mesh_keys.is_empty() {
            return Err(AwsmMeshError::TransformHasNoMeshes(transform_key));
        }

        for mesh_key in &mesh_keys {
            if self.get(*mesh_key)?.instanced {
                return Err(AwsmMeshError::InstancedMeshUnsupported(*mesh_key));
            }
        }

        let new_transform_key = transforms.duplicate(transform_key)?;

        let mut new_mesh_keys = Vec::with_capacity(mesh_keys.len());
        for mesh_key in mesh_keys {
            let new_mesh_key =
                self.duplicate_with_transform(mesh_key, new_transform_key, materials, transforms)?;
            new_mesh_keys.push(new_mesh_key);
        }

        Ok((new_transform_key, new_mesh_keys))
    }

    /// Splits a mesh into a new transform key so it can move independently.
    pub(crate) fn split_mesh(
        &mut self,
        mesh_key: MeshKey,
        transforms: &mut Transforms,
        materials: &Materials,
    ) -> Result<TransformKey> {
        let old_transform_key = self.get(mesh_key)?.transform_key;
        if self.get(mesh_key)?.instanced {
            return Err(AwsmMeshError::InstancedMeshUnsupported(mesh_key));
        }

        let new_transform_key = transforms.duplicate(old_transform_key)?;

        self.update_mesh_transform(
            mesh_key,
            old_transform_key,
            new_transform_key,
            materials,
            transforms,
        )?;

        Ok(new_transform_key)
    }

    /// Splits all meshes under a transform into independent transforms.
    pub(crate) fn split_meshes_by_transform_key(
        &mut self,
        transform_key: TransformKey,
        transforms: &mut Transforms,
        materials: &Materials,
    ) -> Result<Vec<(MeshKey, TransformKey)>> {
        let mesh_keys = self
            .transform_to_meshes
            .get(transform_key)
            .cloned()
            .ok_or(AwsmMeshError::TransformHasNoMeshes(transform_key))?;

        if mesh_keys.is_empty() {
            return Err(AwsmMeshError::TransformHasNoMeshes(transform_key));
        }

        let mut out = Vec::with_capacity(mesh_keys.len());
        for mesh_key in mesh_keys {
            let new_transform_key = self.split_mesh(mesh_key, transforms, materials)?;
            out.push((mesh_key, new_transform_key));
        }

        Ok(out)
    }

    /// Joins multiple meshes under a single transform key.
    pub(crate) fn join_meshes(
        &mut self,
        mesh_keys: &[MeshKey],
        transforms: &mut Transforms,
        materials: &Materials,
        transform_override: Option<Transform>,
    ) -> Result<(TransformKey, Vec<MeshKey>)> {
        if mesh_keys.is_empty() {
            return Err(AwsmMeshError::MeshListEmpty);
        }

        for mesh_key in mesh_keys {
            if self.get(*mesh_key)?.instanced {
                return Err(AwsmMeshError::InstancedMeshUnsupported(*mesh_key));
            }
        }

        let mut common_parent = None;
        for (index, mesh_key) in mesh_keys.iter().enumerate() {
            let mesh = self.get(*mesh_key)?;
            let parent = transforms.get_parent(mesh.transform_key).ok();
            if index == 0 {
                common_parent = parent;
            } else if common_parent != parent {
                common_parent = None;
                break;
            }
        }

        let new_local = match transform_override {
            Some(transform) => transform,
            None => {
                let mut center_sum = glam::Vec3::ZERO;
                for mesh_key in mesh_keys {
                    let mesh = self.get(*mesh_key)?;
                    let center = mesh
                        .world_aabb
                        .as_ref()
                        .map(|aabb| aabb.center())
                        .or_else(|| {
                            transforms
                                .get_world(mesh.transform_key)
                                .ok()
                                .map(|m| m.w_axis.truncate())
                        })
                        .unwrap_or(glam::Vec3::ZERO);
                    center_sum += center;
                }
                let centroid_world = center_sum / mesh_keys.len() as f32;
                let local_translation = match common_parent {
                    Some(parent_key) => transforms
                        .get_world(parent_key)
                        .ok()
                        .map(|m| m.inverse().transform_point3(centroid_world))
                        .unwrap_or(centroid_world),
                    None => centroid_world,
                };
                Transform::IDENTITY.with_translation(local_translation)
            }
        };

        let new_transform_key = transforms.insert(new_local, common_parent);

        let moved = mesh_keys.to_vec();
        for mesh_key in &moved {
            let old_transform_key = self.get(*mesh_key)?.transform_key;
            self.update_mesh_transform(
                *mesh_key,
                old_transform_key,
                new_transform_key,
                materials,
                transforms,
            )?;
        }

        Ok((new_transform_key, moved))
    }

    /// Updates world-space AABBs for meshes affected by dirty transforms or instances.
    ///
    /// Returns every mesh key whose `world_aabb` was potentially refreshed
    /// this call. The caller (currently `AwsmRenderer::update_transforms`)
    /// uses the list to mirror the new AABBs into the spatial index.
    pub fn update_world(
        &mut self,
        dirty_transforms: HashMap<TransformKey, Mat4>,
        dirty_instances: &std::collections::HashSet<TransformKey>,
        transforms: &Transforms,
        instances: &Instances,
        frame_index: u64,
        // Coverage data is consulted at gate time. Empty = consumers
        // fall through to their conservative defaults (always update),
        // so the parameter is harmless when the GPU coverage pass
        // isn't wired yet on the producer side.
        coverage: &crate::coverage::MeshCoverage,
        // Current camera frustum, if any. The coverage-driven
        // skin-skip uses this as a "BVH-visible override": a skin
        // whose consumer meshes' world AABBs are all *out of frustum*
        // is genuinely off-screen; if any AABB is in-frustum the
        // skin is likely about to (or already) disocclude, so we
        // continue animating to dodge rest-pose pop-in. `None` is
        // treated conservatively (assume in-frustum, never skip
        // via coverage) — used by first-frame paths that don't
        // have a camera matrix yet.
        frustum: Option<&crate::frustum::Frustum>,
    ) -> Vec<MeshKey> {
        let mut update_keys = std::collections::HashSet::new();
        update_keys.extend(dirty_transforms.keys().copied());
        update_keys.extend(dirty_instances.iter().copied());

        let mut touched = Vec::new();

        // This doesn't mark anything as dirty, it just updates the world AABB for frustum culling and depth sorting
        for transform_key in update_keys {
            let world_mat = dirty_transforms
                .get(&transform_key)
                .copied()
                .or_else(|| transforms.get_world(transform_key).ok().copied());

            let world_mat = match world_mat {
                Some(mat) => mat,
                None => continue,
            };

            if let Some(mesh_keys) = self.transform_to_meshes.get(transform_key) {
                for mesh_key in mesh_keys {
                    let resource_aabb = self
                        .resource(*mesh_key)
                        .ok()
                        .and_then(|resource| resource.aabb.clone());

                    let world_aabb = match resource_aabb {
                        Some(aabb) => {
                            let mesh = match self.list.get(*mesh_key) {
                                Some(mesh) => mesh,
                                None => continue,
                            };

                            if mesh.instanced {
                                match instances.transform_list(mesh.transform_key) {
                                    Some(transforms_list) if !transforms_list.is_empty() => {
                                        let first = world_mat * transforms_list[0].to_matrix();
                                        let mut combined = aabb.transformed(&first);
                                        for transform in &transforms_list[1..] {
                                            let world = world_mat * transform.to_matrix();
                                            let transformed = aabb.transformed(&world);
                                            combined.extend(&transformed);
                                        }
                                        Some(combined)
                                    }
                                    _ => None,
                                }
                            } else {
                                Some(aabb.transformed(&world_mat))
                            }
                        }
                        None => None,
                    };

                    if let Some(mesh) = self.list.get_mut(*mesh_key) {
                        mesh.world_aabb = world_aabb;
                    }
                    touched.push(*mesh_key);
                }
            }
        }

        // Skin-skip gate. Two layers compose:
        //
        //   1. `skin_update_period` cadence — purely period-based, no
        //      coverage / frustum input. A `period = 4` skin only
        //      updates on frames whose `frame_index % 4 == 0`. Drives
        //      the distance-LOD skinning policy.
        //
        //   2. Coverage-driven skip with grace period + BVH override.
        //      Layer (1) gates *cadence*; this gates *visibility*. A
        //      skin every one of whose consumer meshes had coverage = 0
        //      last frame AND whose AABBs all sit outside the camera
        //      frustum is genuinely off-screen → safe to skip.
        //
        //      The grace period (`SKIN_COVERAGE_GRACE_FRAMES`) protects
        //      multi-primitive characters (e.g. BrainStem's 59
        //      primitives sharing one skeleton) where one submesh
        //      briefly self-occludes another for a frame or two. Without
        //      grace, that submesh freezes in its last-skinned pose;
        //      when the occluder moves and reveals it, it pops into
        //      view in rest pose. The grace counter lets the skin
        //      keep animating for the first ~2 frames of zero coverage
        //      so a brief self-occlusion doesn't propagate.
        //
        //      The BVH override (`frustum.intersects_aabb`) catches the
        //      complementary case: a skin re-entering the frustum is
        //      about to disocclude, so we resume animation immediately
        //      regardless of last-frame coverage.
        //
        // `cheap_material_pixel_threshold` (the other coverage consumer)
        // doesn't suffer from rest-pose persistence — it just picks
        // a cheaper shader on the next frame.
        const SKIN_COVERAGE_GRACE_FRAMES: u32 = 2;
        let mut skip_skins: std::collections::HashSet<SkinKey> = std::collections::HashSet::new();
        // Build the inverted index skin_key → Vec<MeshKey> once so the
        // per-skin BVH / coverage walk is O(meshes) total instead of
        // O(skins × meshes). Empty entries are fine (a skin with no
        // consumer meshes can't show pop-in by definition).
        //
        // `skin_consumers_scratch` is a persistent field on `Meshes`
        // that's `clear()`-ed (not dropped) here so the outer HashMap
        // and each bucket Vec keep their capacities frame-to-frame.
        // Steady-state heap traffic drops to ~zero once the map sizes
        // up; we still pay one rebucket walk per frame, which is the
        // O(meshes) cost the original comment promised. The
        // disjoint-field destructure scopes the mutable borrow to
        // this block; downstream code then re-borrows the scratch
        // immutably via `&self.skin_consumers_scratch`.
        {
            let Self {
                list,
                mesh_to_resource,
                resources,
                skin_consumers_scratch,
                ..
            } = self;
            for bucket in skin_consumers_scratch.values_mut() {
                bucket.clear();
            }
            for (mesh_key, _mesh) in list.iter() {
                let Some(resource_key) = mesh_to_resource.get(mesh_key).copied() else {
                    continue;
                };
                let Some(resource) = resources.get(resource_key) else {
                    continue;
                };
                if let Some(skin_key) = resource.skin_key {
                    skin_consumers_scratch
                        .entry(skin_key)
                        .or_default()
                        .push(mesh_key);
                }
            }
        }
        let skin_consumers: &HashMap<SkinKey, Vec<MeshKey>> = &self.skin_consumers_scratch;

        if frame_index > 0 {
            let skin_keys: Vec<SkinKey> = self.skins.iter_skin_keys().collect();
            for skin_key in skin_keys {
                // Layer 1: cadence gate.
                if !self.skin_should_update_this_frame(skin_key, frame_index) {
                    skip_skins.insert(skin_key);
                    continue;
                }

                // Layer 2: coverage + BVH + grace.
                let consumers = skin_consumers
                    .get(&skin_key)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);

                // A skin with no live consumers — its meshes were
                // removed but the skin slot lingers. No pop-in
                // possible, but no work to do either; the cadence
                // gate already lets it run if `period` allows, and
                // the skip below would also fire (no visible consumers,
                // no in-frustum consumers, grace counter expired
                // immediately since there's nothing to keep the
                // counter at 0). The default branch handles it
                // cleanly so we don't need a special case here.
                let any_visible_last_frame = consumers
                    .iter()
                    .any(|mk| !coverage.is_below_threshold(*mk, 1));

                let any_in_frustum = match frustum {
                    None => true, // conservative: assume in-frustum
                    Some(f) => consumers.iter().any(|mk| {
                        self.list
                            .get(*mk)
                            .and_then(|m| m.world_aabb.as_ref())
                            .map(|aabb| f.intersects_aabb(aabb))
                            .unwrap_or(true)
                    }),
                };

                // Grace counter: reset to 0 the moment ANY consumer
                // showed coverage last frame; otherwise increment
                // (saturating so it never wraps).
                let grace = if any_visible_last_frame {
                    0
                } else {
                    self.skin_zero_coverage_grace
                        .get(skin_key)
                        .copied()
                        .unwrap_or(0)
                        .saturating_add(1)
                };
                self.skin_zero_coverage_grace.insert(skin_key, grace);

                // Skip only when (a) the BVH agrees the skin is
                // off-screen AND (b) coverage has been zero for long
                // enough that a brief self-occlusion is unlikely to
                // be the cause AND (c) no consumer showed coverage
                // last frame.
                if !any_visible_last_frame && !any_in_frustum && grace > SKIN_COVERAGE_GRACE_FRAMES
                {
                    skip_skins.insert(skin_key);
                }
            }
        }

        // This does update the GPU as dirty, bit skins manage their own GPU dirty state
        self.skins
            .update_transforms(dirty_transforms, transforms, |skin_key| {
                !skip_skins.contains(&skin_key)
            });

        touched
    }

    fn update_mesh_transform(
        &mut self,
        mesh_key: MeshKey,
        old_transform_key: TransformKey,
        new_transform_key: TransformKey,
        materials: &Materials,
        transforms: &Transforms,
    ) -> Result<()> {
        let resource_aabb = self.resource(mesh_key).ok().and_then(|r| r.aabb.clone());

        // Same reason as `duplicate_with_transform`: pre-transform into
        // world space rather than leave the AABB local — the new
        // transform key may not be dirty when the next `update_world`
        // runs.
        let world_aabb = match (
            resource_aabb.as_ref(),
            transforms.get_world(new_transform_key).ok(),
        ) {
            (Some(aabb), Some(world_mat)) => Some(aabb.transformed(world_mat)),
            (Some(aabb), None) => Some(aabb.clone()),
            (None, _) => None,
        };

        if let Some(mesh) = self.list.get_mut(mesh_key) {
            mesh.transform_key = new_transform_key;
            mesh.world_aabb = world_aabb;
        }

        if let Some(meshes) = self.transform_to_meshes.get_mut(old_transform_key) {
            meshes.retain(|&key| key != mesh_key);
        }
        if let Some(meshes) = self.transform_to_meshes.get(old_transform_key) {
            if meshes.is_empty() {
                self.transform_to_meshes.remove(old_transform_key);
            }
        }

        if let Some(meshes) = self.transform_to_meshes.get_mut(new_transform_key) {
            meshes.push(mesh_key);
        } else {
            self.transform_to_meshes
                .insert(new_transform_key, vec![mesh_key]);
        }

        self.refresh_meta_for_mesh(mesh_key, materials, transforms)?;

        Ok(())
    }

    /// Public wrapper around `refresh_meta_for_mesh` for the `set_mesh_material`
    /// path on `AwsmRenderer`.
    pub fn refresh_meta_for_mesh_public(
        &mut self,
        mesh_key: MeshKey,
        materials: &Materials,
        transforms: &Transforms,
    ) -> Result<()> {
        self.refresh_meta_for_mesh(mesh_key, materials, transforms)
    }

    fn refresh_meta_for_mesh(
        &mut self,
        mesh_key: MeshKey,
        materials: &Materials,
        transforms: &Transforms,
    ) -> Result<()> {
        let mesh = self
            .list
            .get(mesh_key)
            .ok_or(AwsmMeshError::MeshNotFound(mesh_key))?;

        let (
            buffer_info_key,
            visibility_geometry_data_offset,
            custom_attribute_index_offset,
            custom_attribute_data_offset,
            geometry_morph_key,
            material_morph_key,
            skin_key,
        ) = {
            let resource = self.resource(mesh_key)?;
            (
                resource.buffer_info_key,
                resource.visibility_geometry_data_offset,
                resource.custom_attribute_index_offset,
                resource.custom_attribute_data_offset,
                resource.geometry_morph_key,
                resource.material_morph_key,
                resource.skin_key,
            )
        };

        let buffer_info = self.buffer_infos.get(buffer_info_key)?;

        self.meta.insert(
            mesh_key,
            mesh,
            buffer_info,
            visibility_geometry_data_offset,
            custom_attribute_index_offset,
            custom_attribute_data_offset,
            geometry_morph_key,
            material_morph_key,
            skin_key,
            materials,
            transforms,
            &self.morphs,
            &self.skins,
        )?;

        Ok(())
    }

    /// Returns mesh keys associated with a transform key.
    pub fn keys_by_transform_key(&self, transform_key: TransformKey) -> Option<&Vec<MeshKey>> {
        self.transform_to_meshes.get(transform_key)
    }

    /// Whether this mesh is skinned (its shared resource carries a `SkinKey`).
    /// Skinned meshes can't be naively duplicated/hidden by editor tooling:
    /// the per-frame joint-matrix update is driven through the live mesh, so a
    /// duplicate (or a hidden original) loses its skinning. Callers use this to
    /// leave skinned meshes rendering in place.
    pub fn mesh_is_skinned(&self, mesh_key: MeshKey) -> bool {
        self.resource_key(mesh_key)
            .ok()
            .and_then(|rk| self.resources.get(rk))
            .map(|r| r.skin_key.is_some())
            .unwrap_or(false)
    }

    /// The number of triangles in this mesh's geometry, if known. Editor tooling
    /// uses this to report a selection's real triangle count.
    pub fn mesh_triangle_count(&self, mesh_key: MeshKey) -> Option<usize> {
        let resource_key = self.resource_key(mesh_key).ok()?;
        let resource = self.resources.get(resource_key)?;
        let buffer_info = self.buffer_infos.get(resource.buffer_info_key).ok()?;
        Some(buffer_info.triangles.count)
    }

    /// Iterates over all mesh keys.
    pub fn keys(&self) -> impl Iterator<Item = MeshKey> + '_ {
        self.list.keys()
    }

    /// Returns the resource key for a mesh.
    pub fn resource_key(&self, mesh_key: MeshKey) -> Result<MeshResourceKey> {
        self.mesh_to_resource
            .get(mesh_key)
            .copied()
            .ok_or(AwsmMeshError::MeshNotFound(mesh_key))
    }

    /// Returns the buffer info key for a mesh.
    pub fn buffer_info_key(&self, mesh_key: MeshKey) -> Result<MeshBufferInfoKey> {
        Ok(self.resource(mesh_key)?.buffer_info_key)
    }

    /// Returns the buffer info for a mesh.
    pub fn buffer_info(&self, mesh_key: MeshKey) -> Result<&buffer_info::MeshBufferInfo> {
        let buffer_info_key = self.buffer_info_key(mesh_key)?;
        self.buffer_infos.get(buffer_info_key)
    }

    /// Returns the mesh resource referenced by a mesh key.
    pub fn resource(&self, mesh_key: MeshKey) -> Result<&MeshResource> {
        let resource_key = self.resource_key(mesh_key)?;
        self.resources
            .get(resource_key)
            .ok_or(AwsmMeshError::ResourceNotFound(resource_key))
    }

    /// Convenience accessor for the optional `SkinKey` on a mesh resource.
    /// Returns `None` if the mesh has no resource or no skin. Used by the
    /// spatial-index auto-flagger to route skinned meshes through the
    /// dynamic sidecar.
    pub fn mesh_skin_key(&self, mesh_key: MeshKey) -> Option<Option<SkinKey>> {
        self.resource(mesh_key).ok().map(|r| r.skin_key)
    }

    /// Convenience accessor for the optional `GeometryMorphKey` on a mesh
    /// resource. Returns `None` if the mesh has no resource or no geometry
    /// morph targets. Used by the editor's animation bridge to resolve a
    /// morph-weight animation track (which names a node) to the renderer
    /// morph-weight set it drives.
    pub fn geometry_morph_key_for_mesh(&self, mesh_key: MeshKey) -> Option<GeometryMorphKey> {
        self.resource(mesh_key)
            .ok()
            .and_then(|r| r.geometry_morph_key)
    }

    /// Material-morph counterpart of [`Self::geometry_morph_key_for_mesh`] —
    /// `None` if the mesh has no resource or no material (UV/color) morph
    /// targets. Used by the editor's live `SetMorphWeight` path so a weight
    /// poke drives BOTH morph buffers, exactly like a morph animation track.
    pub fn material_morph_key_for_mesh(&self, mesh_key: MeshKey) -> Option<MaterialMorphKey> {
        self.resource(mesh_key)
            .ok()
            .and_then(|r| r.material_morph_key)
    }

    /// Smallest `skin_update_period` across every mesh that references
    /// `skin_key`. Used by the per-frame skinning-LOD gate: a skin is
    /// updated this frame if ANY of its consumer meshes wants the
    /// update, which is the conservative choice for shared skeletons.
    /// Returns `1` if no meshes reference the skin (forces an update
    /// if anything dirties the joints).
    pub fn skin_smallest_period(&self, skin_key: SkinKey) -> u8 {
        let mut min_period: u8 = u8::MAX;
        for (mesh_key, mesh) in self.iter() {
            let same_skin = self
                .resource(mesh_key)
                .ok()
                .and_then(|r| r.skin_key)
                .map(|k| k == skin_key)
                .unwrap_or(false);
            if !same_skin {
                continue;
            }
            min_period = min_period.min(mesh.skin_update_period.max(1));
        }
        if min_period == u8::MAX {
            1
        } else {
            min_period
        }
    }

    /// Coverage gate for skinning skip. Returns true if
    /// EVERY mesh that references `skin_key` had zero pixels last frame.
    /// One non-zero consumer is enough to keep the skin updating.
    pub fn skin_all_consumers_zero_coverage(
        &self,
        skin_key: SkinKey,
        coverage: &crate::coverage::MeshCoverage,
    ) -> bool {
        let mut had_any_consumer = false;
        for (mesh_key, _mesh) in self.iter() {
            let same_skin = self
                .resource(mesh_key)
                .ok()
                .and_then(|r| r.skin_key)
                .map(|k| k == skin_key)
                .unwrap_or(false);
            if !same_skin {
                continue;
            }
            had_any_consumer = true;
            if coverage.is_visible_last_frame(mesh_key) {
                return false;
            }
        }
        // If no consumers exist, the skin isn't actually rendered —
        // skipping it is fine.
        had_any_consumer
    }

    /// Whether the skin should run its per-joint matrix refresh on this
    /// frame, given the renderer-wide `frame_index`. A skin updates if
    /// `frame_index % min_period == 0`. Always updates on the first
    /// frame after a load (frame_index == 0) so the initial pose lands.
    pub fn skin_should_update_this_frame(&self, skin_key: SkinKey, frame_index: u64) -> bool {
        let period = self.skin_smallest_period(skin_key).max(1) as u64;
        if period == 1 || frame_index == 0 {
            return true;
        }
        frame_index % period == 0
    }

    /// Returns the merged geometry pool GPU buffer. All three per-mesh
    /// sections — visibility, attribute indices, attribute data —
    /// live in this one buffer; per-mesh sub-offsets in `MeshResource`
    /// (visibility/custom_attribute_index/custom_attribute_data_offset)
    /// say where each section starts.
    pub fn mesh_geometry_pool_gpu_buffer(&self) -> &web_sys::GpuBuffer {
        &self.mesh_geometry_pool_gpu_buffer
    }

    /// Returns the merged geometry pool GPU buffer used by the opaque
    /// compute pass for visibility-data reads. Same handle as
    /// [`Self::mesh_geometry_pool_gpu_buffer`] — `visibility_data` in
    /// WGSL is now a view over the pool.
    pub fn visibility_geometry_data_gpu_buffer(&self) -> &web_sys::GpuBuffer {
        &self.mesh_geometry_pool_gpu_buffer
    }
    /// Returns the offset into the merged geometry pool where this mesh's
    /// visibility-data section starts.
    pub fn visibility_geometry_data_buffer_offset(&self, key: MeshKey) -> Result<usize> {
        let resource_key = self.resource_key(key)?;
        self.resources
            .get(resource_key)
            .and_then(|r| r.visibility_geometry_data_offset)
            .ok_or(AwsmMeshError::VisibilityGeometryBufferNotFound(key))
    }

    /// Returns the GPU buffer for visibility geometry indices.
    pub fn visibility_geometry_index_gpu_buffer(&self) -> &web_sys::GpuBuffer {
        &self.visibility_geometry_index_gpu_buffer
    }
    /// Returns the offset into visibility geometry indices for a mesh.
    pub fn visibility_geometry_index_buffer_offset(&self, key: MeshKey) -> Result<usize> {
        let resource_key = self.resource_key(key)?;
        self.visibility_geometry_index_buffers
            .offset(resource_key)
            .ok_or(AwsmMeshError::VisibilityGeometryBufferNotFound(key))
    }

    /// Returns the merged geometry pool — custom attribute data is a
    /// section inside it.
    pub fn custom_attribute_data_gpu_buffer(&self) -> &web_sys::GpuBuffer {
        &self.mesh_geometry_pool_gpu_buffer
    }
    /// Returns the offset into the pool where this mesh's custom-attribute
    /// data section starts.
    pub fn custom_attribute_data_buffer_offset(&self, key: MeshKey) -> Result<usize> {
        let resource_key = self.resource_key(key)?;
        self.resources
            .get(resource_key)
            .map(|r| r.custom_attribute_data_offset)
            .ok_or(AwsmMeshError::CustomAttributeBufferNotFound(key))
    }

    /// Returns the GPU buffer for transparency geometry vertex data.
    pub fn transparency_geometry_data_gpu_buffer(&self) -> &web_sys::GpuBuffer {
        &self.transparency_geometry_data_gpu_buffer
    }
    /// Returns the offset into transparency geometry data for a mesh.
    pub fn transparency_geometry_data_buffer_offset(&self, key: MeshKey) -> Result<usize> {
        let resource_key = self.resource_key(key)?;
        self.transparency_geometry_data_buffers
            .offset(resource_key)
            .ok_or(AwsmMeshError::TransparencyGeometryBufferNotFound(key))
    }
    /// Returns the merged geometry pool used as the transparent draw's
    /// index buffer.
    pub fn transparency_geometry_index_gpu_buffer(&self) -> &web_sys::GpuBuffer {
        &self.mesh_geometry_pool_gpu_buffer
    }
    /// Returns the offset into the pool where this mesh's attribute-index
    /// section starts — reused as the transparent path's index-buffer
    /// offset.
    pub fn transparency_geometry_index_buffer_offset(&self, key: MeshKey) -> Result<usize> {
        let resource_key = self.resource_key(key)?;
        self.resources
            .get(resource_key)
            .map(|r| r.custom_attribute_index_offset)
            .ok_or(AwsmMeshError::CustomAttributeBufferNotFound(key))
    }

    /// Returns the merged geometry pool — custom attribute indices are a
    /// section inside it.
    pub fn custom_attribute_index_gpu_buffer(&self) -> &web_sys::GpuBuffer {
        &self.mesh_geometry_pool_gpu_buffer
    }
    /// Returns the offset into the pool where this mesh's custom-attribute
    /// index section starts.
    pub fn custom_attribute_index_buffer_offset(&self, key: MeshKey) -> Result<usize> {
        let resource_key = self.resource_key(key)?;
        self.resources
            .get(resource_key)
            .map(|r| r.custom_attribute_index_offset)
            .ok_or(AwsmMeshError::CustomAttributeBufferNotFound(key))
    }

    /// Total number of `Mesh` entries (including hidden / non-renderable).
    /// Used as an upper bound when sizing per-mesh GPU buffers before the
    /// per-frame renderables list is collected.
    pub fn len(&self) -> usize {
        self.list.len()
    }

    /// True when there are no `Mesh` entries.
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Iterates over meshes and their keys.
    pub fn iter(&self) -> impl Iterator<Item = (MeshKey, &Mesh)> {
        self.list.iter()
    }

    /// Walk every mesh key and apply `gate_fn(mesh_key) -> u32` to
    /// `MeshMeta::set_shadow_receiver_gate`. Exists so the caller
    /// doesn't have to materialise a `Vec<MeshKey>` per frame just to
    /// step around the `&self.list` vs `&mut self.meta` split borrow
    /// — both fields are disjoint sub-borrows of `self`, so we do the
    /// split here and walk `self.list.keys()` in place. The cached
    /// last-frame-gate inside `MeshMeta::set_shadow_receiver_gate`
    /// keeps the per-call cost effectively `Cell::get + compare` on
    /// unchanged meshes; per-frame allocation drops to zero.
    pub fn update_shadow_receiver_gates<F: FnMut(MeshKey) -> u32>(&mut self, mut gate_fn: F) {
        for mesh_key in self.list.keys() {
            let gate = gate_fn(mesh_key);
            self.meta.set_shadow_receiver_gate(mesh_key, gate);
        }
    }

    /// Returns a mesh by key.
    pub fn get(&self, mesh_key: MeshKey) -> Result<&Mesh> {
        self.list
            .get(mesh_key)
            .ok_or(AwsmMeshError::MeshNotFound(mesh_key))
    }

    /// Returns a mutable mesh by key.
    pub(crate) fn get_mut(&mut self, mesh_key: MeshKey) -> Result<&mut Mesh> {
        self.list
            .get_mut(mesh_key)
            .ok_or(AwsmMeshError::MeshNotFound(mesh_key))
    }

    /// Removes all meshes that share the given transform key.
    pub(crate) fn remove_by_transform_key(
        &mut self,
        transform_key: TransformKey,
    ) -> Option<Vec<Mesh>> {
        if let Some(mesh_keys) = self.transform_to_meshes.get(transform_key).cloned() {
            let mut removed_meshes = Vec::with_capacity(mesh_keys.capacity());
            for mesh_key in mesh_keys.iter() {
                if let Some(mesh) = self.remove(*mesh_key) {
                    removed_meshes.push(mesh);
                }
            }
            Some(removed_meshes)
        } else {
            None
        }
    }
    /// Removes a mesh by key and returns it if found.
    pub(crate) fn remove(&mut self, mesh_key: MeshKey) -> Option<Mesh> {
        if let Some(mesh) = self.list.remove(mesh_key) {
            self.meta.remove(mesh_key);
            // Drop the cheap-material LOD cache entry so a recycled
            // MeshKey can't inherit a stale "effective_material was X"
            // hit (which would suppress the first frame's patch).
            self.last_effective_material.remove(mesh_key);

            if let Some(meshes) = self.transform_to_meshes.get_mut(mesh.transform_key) {
                meshes.retain(|&key| key != mesh_key)
            }

            if let Some(resource_key) = self.mesh_to_resource.remove(mesh_key) {
                let should_remove_resource = match self.resources.get_mut(resource_key) {
                    Some(resource) => {
                        if resource.refcount > 1 {
                            resource.refcount -= 1;
                            false
                        } else {
                            true
                        }
                    }
                    None => false,
                };

                if should_remove_resource {
                    if let Some(resource) = self.resources.remove(resource_key) {
                        self.mesh_geometry_pool_buffers.remove(resource_key);
                        self.visibility_geometry_index_buffers.remove(resource_key);
                        self.transparency_geometry_data_buffers.remove(resource_key);

                        self.mesh_geometry_pool_dirty = true;
                        self.visibility_geometry_index_dirty = true;
                        self.transparency_geometry_data_dirty = true;

                        if self.buffer_infos.remove(resource.buffer_info_key).is_some() {
                            self.mesh_geometry_pool_dirty = true;
                            self.visibility_geometry_index_dirty = true;
                            self.transparency_geometry_data_dirty = true;
                        }

                        if let Some(morph_key) = resource.geometry_morph_key {
                            self.morphs.geometry.remove(morph_key);
                        }

                        if let Some(morph_key) = resource.material_morph_key {
                            self.morphs.material.remove(morph_key);
                        }

                        if let Some(skin_key) = resource.skin_key {
                            self.skins.remove(skin_key, None);
                            // Drop the grace-period cache entry so a
                            // recycled SkinKey can't inherit a stale
                            // "out-of-frustum for N frames" counter.
                            self.skin_zero_coverage_grace.remove(skin_key);
                        }
                    }
                }
            }

            Some(mesh)
        } else {
            None
        }
    }

    /// Writes dirty mesh buffers to the GPU and updates bind groups.
    pub fn write_gpu(
        &mut self,
        logging: &AwsmRendererLogging,
        gpu: &AwsmRendererWebGpu,
        bind_groups: &mut BindGroups,
    ) -> Result<()> {
        let any_dirty = self.mesh_geometry_pool_dirty
            || self.visibility_geometry_index_dirty
            || self.transparency_geometry_data_dirty;

        if any_dirty {
            let _maybe_span_guard = if logging.render_timings.sub_frame() {
                Some(tracing::span!(tracing::Level::INFO, "Mesh GPU write").entered())
            } else {
                None
            };

            if self.mesh_geometry_pool_dirty {
                let mut resized = false;
                if let Some(new_size) = self.mesh_geometry_pool_buffers.take_gpu_needs_resize() {
                    self.mesh_geometry_pool_gpu_buffer = gpu.create_buffer(
                        &BufferDescriptor::new(
                            Some("MeshGeometryPool"),
                            new_size,
                            BufferUsage::new()
                                .with_copy_dst()
                                .with_vertex()
                                .with_storage()
                                .with_index(),
                        )
                        .into(),
                    )?;
                    bind_groups.mark_create(BindGroupCreate::MeshGeometryPoolResize);
                    resized = true;
                }
                if resized {
                    self.mesh_geometry_pool_buffers.clear_dirty_ranges();
                    gpu.write_buffer(
                        &self.mesh_geometry_pool_gpu_buffer,
                        None,
                        self.mesh_geometry_pool_buffers.raw_slice(),
                        None,
                        None,
                    )?;
                } else {
                    let ranges = self.mesh_geometry_pool_buffers.take_dirty_ranges();
                    self.mesh_geometry_pool_uploader.write_dirty_ranges(
                        gpu,
                        &self.mesh_geometry_pool_gpu_buffer,
                        self.mesh_geometry_pool_buffers.raw_slice().len(),
                        self.mesh_geometry_pool_buffers.raw_slice(),
                        &ranges,
                    )?;
                }
            }

            if self.visibility_geometry_index_dirty {
                let mut resized = false;
                if let Some(new_size) = self
                    .visibility_geometry_index_buffers
                    .take_gpu_needs_resize()
                {
                    self.visibility_geometry_index_gpu_buffer = gpu.create_buffer(
                        &BufferDescriptor::new(
                            Some("MeshVisibilityIndex"),
                            new_size,
                            BufferUsage::new().with_copy_dst().with_index(),
                        )
                        .into(),
                    )?;
                    resized = true;
                }
                if resized {
                    self.visibility_geometry_index_buffers.clear_dirty_ranges();
                    gpu.write_buffer(
                        &self.visibility_geometry_index_gpu_buffer,
                        None,
                        self.visibility_geometry_index_buffers.raw_slice(),
                        None,
                        None,
                    )?;
                } else {
                    let ranges = self.visibility_geometry_index_buffers.take_dirty_ranges();
                    self.visibility_geometry_index_uploader.write_dirty_ranges(
                        gpu,
                        &self.visibility_geometry_index_gpu_buffer,
                        self.visibility_geometry_index_buffers.raw_slice().len(),
                        self.visibility_geometry_index_buffers.raw_slice(),
                        &ranges,
                    )?;
                }
            }

            if self.transparency_geometry_data_dirty {
                let mut resized = false;
                if let Some(new_size) = self
                    .transparency_geometry_data_buffers
                    .take_gpu_needs_resize()
                {
                    self.transparency_geometry_data_gpu_buffer = gpu.create_buffer(
                        &BufferDescriptor::new(
                            Some("MeshTransparencyGeometryData"),
                            new_size,
                            BufferUsage::new()
                                .with_copy_dst()
                                .with_vertex()
                                .with_storage(),
                        )
                        .into(),
                    )?;
                    resized = true;
                }
                if resized {
                    self.transparency_geometry_data_buffers.clear_dirty_ranges();
                    gpu.write_buffer(
                        &self.transparency_geometry_data_gpu_buffer,
                        None,
                        self.transparency_geometry_data_buffers.raw_slice(),
                        None,
                        None,
                    )?;
                } else {
                    let ranges = self.transparency_geometry_data_buffers.take_dirty_ranges();
                    self.transparency_geometry_data_uploader
                        .write_dirty_ranges(
                            gpu,
                            &self.transparency_geometry_data_gpu_buffer,
                            self.transparency_geometry_data_buffers.raw_slice().len(),
                            self.transparency_geometry_data_buffers.raw_slice(),
                            &ranges,
                        )?;
                }
            }

            self.mesh_geometry_pool_dirty = false;
            self.visibility_geometry_index_dirty = false;
            self.transparency_geometry_data_dirty = false;
        }

        Ok(())
    }
}

impl Drop for Meshes {
    fn drop(&mut self) {
        self.mesh_geometry_pool_gpu_buffer.destroy();
        self.visibility_geometry_index_gpu_buffer.destroy();
        self.transparency_geometry_data_gpu_buffer.destroy();
    }
}

new_key_type! {
    /// Opaque key for mesh instances.
    pub struct MeshKey;
    /// Opaque key for shared mesh resources.
    pub struct MeshResourceKey;
}
