//! Public raw-mesh upload API.
//!
//! This is the canonical entry point for uploading procedural / generated geometry
//! into the renderer. The `gltf` ingestion path is one consumer of the same
//! underlying model — both lower to a `GeometrySource` and flow through
//! `register_geometry` → `add_mesh` → commit (`add_raw_mesh` is the one-shot
//! convenience that registers + adds + eagerly resolves its single geometry).
//!
//! Today's API supports static (non-skinned, non-morphed) meshes. The geometry
//! KIND (visibility vs transparency) is resolved at commit from the bound
//! material via the one `geometry_kind` fn, so `add_raw_mesh` handles opaque AND
//! transparent materials uniformly — there is no separate transparent entry point.
//!
//! ## Byte layout
//!
//! Visibility geometry (56 bytes / vertex, exploded so each triangle has its
//! own three vertex records):
//!
//! ```text
//! position(3 * f32 = 12) | triangle_index(u32 = 4) | barycentric(2 * f32 = 8)
//!   | normal(3 * f32 = 12) | tangent(4 * f32 = 16) | original_vertex_index(u32 = 4)
//! ```
//!
//! Custom attribute index (4 bytes / index, three per triangle): packed `u32`
//! triangle indices. The visibility pipeline doesn't use these directly — they
//! drive transparent-pass rendering and per-triangle attribute lookup in the
//! visibility shading pass.
//!
//! Custom attribute data: tightly-packed UVs (and optionally vertex colors), one
//! record per original (non-exploded) vertex. The per-vertex stride is the sum
//! of every declared `MeshBufferCustomVertexAttributeInfo::vertex_size()`.

use glam::{Mat4, Vec3};

use crate::{
    bounds::Aabb,
    materials::{Material, MaterialKey},
    meshes::{
        buffer_info::{
            MeshBufferCustomVertexAttributeInfo, MeshBufferGeometryMorphInfo, MeshBufferSkinInfo,
            MeshBufferVertexAttributeInfo,
        },
        mesh::Mesh,
        MeshKey,
    },
    transforms::TransformKey,
    AwsmRenderer,
};

/// Plain-data input for `AwsmRenderer::add_raw_mesh`. Mirrors `awsm_renderer_meshgen::MeshData`
/// but lives here so the renderer crate doesn't depend on `awsm-renderer-meshgen`.
#[derive(Debug, Clone, Default)]
pub struct RawMeshData {
    pub positions: Vec<[f32; 3]>,
    /// If `None`, the renderer computes per-vertex normals as the area-weighted
    /// average of incident face normals.
    pub normals: Option<Vec<[f32; 3]>>,
    /// UV sets, indexed by `TEXCOORD_n` (set `n` = `uv_sets[n]`). Empty = no UVs.
    /// Each set is packed contiguously per vertex in set order, so the derived
    /// `material_mesh_meta.uv_set_count`/`uv_sets_index` (from the attribute layout)
    /// let the WGSL `_texture_uv_per_vertex` read any set at `set_index*2` floats and
    /// custom materials read `material_uv(in, iu)` for any `i < uv_set_count`.
    /// Generalized to N (was a hardcoded `uvs` + `uvs1` pair).
    pub uv_sets: Vec<Vec<[f32; 2]>>,
    /// Optional per-vertex RGBA colors.
    pub colors: Option<Vec<[f32; 4]>>,
    pub indices: Vec<u32>,
    /// Optional authored per-vertex `TANGENT` (vec4: xyz + handedness). `Some` ⇒ the
    /// commit uses these verbatim instead of regenerating via MikkTSpace, so an
    /// imported mesh's captured geometry preserves the EXACT tangent basis a normal
    /// map was baked against across a save→reload (regenerated tangents shade
    /// differently — the dark-patch roundtrip bug). `None` ⇒ regenerate as before
    /// (procedural / edited meshes, and the player path — unchanged).
    pub tangents: Option<Vec<[f32; 4]>>,
    /// Optional skin (rig) binding — makes this a SKINNED raw mesh. The deform
    /// compute pass runs off the inserted `SkinKey` exactly like a glTF-imported
    /// skin. `None` ⇒ a static mesh (unchanged).
    pub skin: Option<RawSkin>,
    /// Optional geometry morph targets. `None` ⇒ no morphs (unchanged).
    pub morph: Option<RawMorph>,
}

/// Skin (rig) data for a [`RawMeshData`]. The fields match
/// [`crate::meshes::skins::Skins::insert`] + the glTF decode's skin output
/// (`skin_joint_index_weight_bytes` + joints + inverse-bind matrices), so the
/// editor's per-node skinned capture (Phase 2) can supply the same shapes the
/// importer already produces.
#[derive(Debug, Clone)]
pub struct RawSkin {
    /// The skeleton's joint transforms (editor scene nodes / glTF joint nodes).
    pub joints: Vec<TransformKey>,
    /// Per-joint inverse-bind matrix, parallel to `joints`.
    pub inverse_bind_matrices: Vec<Mat4>,
    /// Number of skin sets (JOINTS_0/WEIGHTS_0, …); 4 joint influences per set.
    pub set_count: usize,
    /// Per-vertex packed joint indices + weights, the exact byte layout
    /// `Skins::insert` consumes (`original_vertices * set_count * (vec4<u32> idx +
    /// vec4<f32> weight)`).
    pub index_weights: Vec<u8>,
}

/// Geometry morph-target data for a [`RawMeshData`]. Fields match
/// [`crate::meshes::morphs`]' `insert_raw` (the same call the glTF decode uses).
#[derive(Debug, Clone)]
pub struct RawMorph {
    /// Layout descriptor (targets count, per-vertex stride, total values size).
    pub info: MeshBufferGeometryMorphInfo,
    /// Default per-target weights, as little-endian `f32` bytes.
    pub weights: Vec<u8>,
    /// Packed per-target vertex deltas (position [+ normal + tangent]).
    pub values: Vec<u8>,
}

impl RawMeshData {
    pub fn vertex_count(&self) -> usize {
        self.positions.len()
    }

    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    fn ensure_normals(&mut self) {
        if self.normals.is_some() {
            return;
        }
        let mut acc = vec![Vec3::ZERO; self.positions.len()];
        let positions: Vec<Vec3> = self
            .positions
            .iter()
            .map(|p| Vec3::from_array(*p))
            .collect();
        for tri in self.indices.chunks_exact(3) {
            let i0 = tri[0] as usize;
            let i1 = tri[1] as usize;
            let i2 = tri[2] as usize;
            let a = positions[i0];
            let b = positions[i1];
            let c = positions[i2];
            let n = (b - a).cross(c - a);
            acc[i0] += n;
            acc[i1] += n;
            acc[i2] += n;
        }
        self.normals = Some(
            acc.into_iter()
                .map(|n| n.normalize_or_zero().to_array())
                .collect(),
        );
    }

    fn aabb(&self) -> Option<Aabb> {
        if self.positions.is_empty() {
            return None;
        }
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for p in &self.positions {
            let v = Vec3::from_array(*p);
            min = min.min(v);
            max = max.max(v);
        }
        Some(Aabb { min, max })
    }

    /// Lower this raw mesh into a [`GeometrySource`](crate::meshes::geometry::GeometrySource)
    /// for the load transaction (`register_geometry` → `add_mesh`). Does the
    /// pass-INDEPENDENT work — computes normals, the AABB, the custom-attribute
    /// layout + bytes (UVs/colors), and the per-triangle attribute-index bytes. The
    /// per-pass visibility/transparency reps + tangents are derived at commit from
    /// the retained positions/normals/UV0/indices (tangents gated on the bound
    /// material), so this carries NO kind decision — that's `geometry_kind` at commit.
    pub(crate) fn into_geometry_source(
        mut self,
        front_face: awsm_renderer_core::pipeline::primitive::FrontFace,
    ) -> crate::meshes::geometry::GeometrySource {
        self.ensure_normals();
        let aabb = self.aabb();
        let vertex_count = self.vertex_count();
        let triangle_count = self.triangle_count();

        // Per-triangle attribute indices (3 × u32 per triangle).
        let mut attribute_index_bytes: Vec<u8> = Vec::with_capacity(triangle_count * 12);
        for tri in self.indices.chunks_exact(3) {
            attribute_index_bytes.extend_from_slice(&tri[0].to_le_bytes());
            attribute_index_bytes.extend_from_slice(&tri[1].to_le_bytes());
            attribute_index_bytes.extend_from_slice(&tri[2].to_le_bytes());
        }

        // Custom attributes (UVs + optional 2nd UV set + optional colors), AoS,
        // one record per original vertex — pass-independent.
        let mut vertex_attributes: Vec<MeshBufferVertexAttributeInfo> = Vec::new();
        let mut custom_attribute_bytes: Vec<u8> = Vec::new();
        // Every UV set the mesh carries → one TEXCOORD_n attribute (the meta derives
        // uv_set_count/uv_sets_index from these). A set is only meaningful packed in
        // order, so a gap (set N present but N-1 absent) never arises — `uv_sets` is
        // dense by construction.
        for (index, _) in self.uv_sets.iter().enumerate() {
            vertex_attributes.push(MeshBufferVertexAttributeInfo::Custom(
                MeshBufferCustomVertexAttributeInfo::TexCoords {
                    index: index as u32,
                    data_size: 4,
                    component_len: 2,
                },
            ));
        }
        let has_colors = self.colors.is_some();
        if has_colors {
            vertex_attributes.push(MeshBufferVertexAttributeInfo::Custom(
                MeshBufferCustomVertexAttributeInfo::Colors {
                    index: 0,
                    data_size: 4,
                    component_len: 4,
                },
            ));
        }
        for v in 0..vertex_count {
            // UV sets in order (set 0, 1, …) — matches the attribute push above.
            for set in &self.uv_sets {
                let uv = set[v];
                custom_attribute_bytes.extend_from_slice(&uv[0].to_le_bytes());
                custom_attribute_bytes.extend_from_slice(&uv[1].to_le_bytes());
            }
            if let Some(colors) = self.colors.as_ref() {
                let c = colors[v];
                for comp in c {
                    custom_attribute_bytes.extend_from_slice(&comp.to_le_bytes());
                }
            }
        }

        crate::meshes::geometry::GeometrySource {
            normals: self.normals.expect("ensure_normals filled this"),
            positions: self.positions,
            uvs0: self.uv_sets.into_iter().next(),
            // Authored tangents (e.g. an imported mesh's captured glTF TANGENT) are
            // used verbatim; `None` ⇒ generated at commit if a normal-map material is
            // bound (procedural / edited meshes, and the player path).
            tangents: self.tangents,
            indices: self.indices,
            front_face,
            vertex_attributes,
            custom_attribute_bytes,
            attribute_index_bytes,
            aabb,
            geometry_morph_key: None,
            geometry_morph_info: None,
            material_morph_key: None,
            material_morph_info: None,
            skin_key: None,
            skin_info: None,
        }
    }
}

/// True when a material samples a normal map (base or clearcoat) and therefore
/// needs a real tangent basis. Mirrors `renderer-gltf`'s `ensure_tangents`
/// gating so the raw-mesh path generates tangents in exactly the cases the
/// gltf path does.
pub(crate) fn material_wants_tangents(mat: &Material) -> bool {
    match mat {
        Material::Pbr(m) => {
            m.normal_tex.is_some() || m.clearcoat.as_ref().is_some_and(|c| c.normal_tex.is_some())
        }
        _ => false,
    }
}

// (MikkTSpace tangent generation moved to the shared `awsm-renderer-tangents` crate.)

/// Per-mesh options for [`AwsmRenderer::add_mesh`] beyond the geometry / material /
/// transform — the instance flags `Mesh::new` takes.
#[derive(Debug, Clone, Copy, Default)]
pub struct AddMeshOpts {
    pub instanced: bool,
    pub hud: bool,
    pub hidden: bool,
    /// Double-sided override. `None` ⇒ derive from the bound material (the default,
    /// what the raw path uses). `Some(v)` ⇒ force this value — the glTF path uses
    /// this to apply its `should_force_single_sided_for_opaque_thin_shell` heuristic
    /// (which the bound material alone can't express).
    pub double_sided: Option<bool>,
}

impl AwsmRenderer {
    /// Register a [`GeometrySource`](crate::meshes::geometry::GeometrySource) — the
    /// load transaction's geometry "declare". CPU-only; returns a `GeometryKey` to
    /// bind meshes to via [`Self::add_mesh`]. The per-pass GPU representations are
    /// derived at the next `commit_load` from the union of bound materials, then the
    /// source is freed. Convenience wrapper over `self.meshes.register_geometry`.
    pub fn register_geometry(
        &mut self,
        source: crate::meshes::geometry::GeometrySource,
    ) -> crate::meshes::geometry::GeometryKey {
        self.meshes.register_geometry(source)
    }

    /// Assign a material + transform to a registered geometry → a drawable mesh (the
    /// load transaction's "append"). Mints the `MeshKey` SYNCHRONOUSLY but uploads
    /// NOTHING: the mesh draws nothing until the next `commit_load` resolves its
    /// geometry. Many `add_mesh` calls may share one `GeometryKey` (dedup — each
    /// needed kind uploads once across all of them).
    pub fn add_mesh(
        &mut self,
        geometry: crate::meshes::geometry::GeometryKey,
        material_key: MaterialKey,
        transform_key: TransformKey,
        opts: AddMeshOpts,
    ) -> crate::error::Result<MeshKey> {
        let double_sided = opts.double_sided.unwrap_or_else(|| {
            self.materials
                .get(material_key)
                .map(Material::double_sided)
                .unwrap_or(false)
        });
        let mesh = Mesh::new(
            transform_key,
            material_key,
            double_sided,
            opts.instanced,
            opts.hud,
            opts.hidden,
        );
        Ok(self.meshes.bind_mesh(mesh, geometry)?)
    }

    /// Upload a raw `RawMeshData` + material — the one-shot raw-mesh convenience.
    /// Sugar over the geometry transaction: `register_geometry` + `add_mesh` + an
    /// EAGER resolve of just this geometry, so the mesh uploads + draws immediately
    /// (sync, no `commit_load` needed — matching today's behavior). The geometry
    /// kind (visibility vs transparency) is decided from the bound material via the
    /// one `geometry_kind` path, so this handles opaque AND transparent materials
    /// uniformly — there is no separate transparent entry point. (The deferred
    /// `register_geometry` + `add_mesh` + `commit_load` path is for batched/deduped
    /// content like glTF, where many meshes share one geometry across one commit.)
    ///
    /// The returned mesh is **not** instanced. To draw multiple copies, either call
    /// `add_raw_mesh` repeatedly or `enable_mesh_instancing` after creation.
    pub fn add_raw_mesh(
        &mut self,
        mut data: RawMeshData,
        transform_key: TransformKey,
        material_key: MaterialKey,
    ) -> crate::error::Result<MeshKey> {
        if data.positions.is_empty() || data.indices.len() % 3 != 0 {
            return Err(crate::error::AwsmError::Mesh(
                crate::meshes::error::AwsmMeshError::MeshListEmpty,
            ));
        }
        // Insert optional skin / morph into the shared stores BEFORE building the
        // source, so the GeometrySource carries the keys + layout the deferred
        // resolve reattaches (the same path the glTF decode uses). `None` ⇒ a plain
        // static mesh, exactly as before (default-equals-today).
        let skin = data.skin.take();
        let morph = data.morph.take();
        let skin_bits = match skin {
            Some(s) => {
                let info = MeshBufferSkinInfo {
                    set_count: s.set_count,
                    index_weights_size: s.index_weights.len(),
                };
                let key = self.meshes.skins.insert(
                    s.joints,
                    &s.inverse_bind_matrices,
                    s.set_count,
                    &s.index_weights,
                )?;
                Some((key, info))
            }
            None => None,
        };
        let morph_bits = match morph {
            Some(m) => {
                let key = self.meshes.morphs.geometry.insert_raw(
                    m.info.clone(),
                    &m.weights,
                    &m.values,
                )?;
                Some((key, m.info))
            }
            None => None,
        };

        let mut source =
            data.into_geometry_source(awsm_renderer_core::pipeline::primitive::FrontFace::Ccw);
        if let Some((key, info)) = skin_bits {
            source.skin_key = Some(key);
            source.skin_info = Some(info);
        }
        if let Some((key, info)) = morph_bits {
            source.geometry_morph_key = Some(key);
            source.geometry_morph_info = Some(info);
        }
        let geometry = self.register_geometry(source);
        let mesh_key = self.add_mesh(
            geometry,
            material_key,
            transform_key,
            AddMeshOpts::default(),
        )?;
        // Eager resolve THIS geometry (only) — packs its rep from the bound
        // material's kind, uploads once, wires the mesh, frees the source. Sync; the
        // mesh is drawable this frame, so existing sync callers (gizmos, handles,
        // particles) need no commit (default-equals-today).
        let wired = self
            .meshes
            .resolve_one(geometry, &self.materials, &self.transforms)?;
        for mesh_key in wired {
            self.sync_spatial_for_mesh(mesh_key);
        }
        Ok(mesh_key)
    }
}
