//! Public raw-mesh upload API.
//!
//! This is the canonical entry point for uploading procedural / generated geometry
//! into the renderer. The `gltf` ingestion path is one consumer of the same
//! underlying mesh buffers — `add_raw_mesh` packs an in-memory `RawMeshData`
//! into the same byte layouts the gltf populate path produces, and inserts via
//! the same `Meshes::insert` entry point.
//!
//! Today's API supports static (non-skinned, non-morphed) opaque meshes. The
//! visibility-buffer compute pipeline handles them uniformly with gltf-loaded
//! meshes. Transparent-pass routing is supported when the caller passes a
//! transparent material.
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

use glam::Vec3;

use crate::{
    bounds::Aabb,
    materials::{Material, MaterialKey},
    meshes::{
        buffer_info::{MeshBufferCustomVertexAttributeInfo, MeshBufferVertexAttributeInfo},
        mesh::Mesh,
        MeshKey,
    },
    transforms::TransformKey,
    AwsmRenderer,
};

/// Plain-data input for `AwsmRenderer::add_raw_mesh`. Mirrors `awsm_meshgen::MeshData`
/// but lives here so the renderer crate doesn't depend on `awsm-meshgen`.
#[derive(Debug, Clone, Default)]
pub struct RawMeshData {
    pub positions: Vec<[f32; 3]>,
    /// If `None`, the renderer computes per-vertex normals as the area-weighted
    /// average of incident face normals.
    pub normals: Option<Vec<[f32; 3]>>,
    /// Optional UV-set 0.
    pub uvs: Option<Vec<[f32; 2]>>,
    /// Optional UV-set 1 (`TEXCOORD_1`). Packed contiguously right after set 0 so
    /// `material_mesh_meta.uv_sets_index` points at set 0 and set 1 reads at
    /// `+2` floats (matching the WGSL `_texture_uv_per_vertex` `set_index*2`
    /// layout + the glTF populate path). Only meaningful when `uvs` is also set;
    /// `uv_set_count` becomes 2 so custom materials can read `material_uv(in,1u)`.
    pub uvs1: Option<Vec<[f32; 2]>>,
    /// Optional per-vertex RGBA colors.
    pub colors: Option<Vec<[f32; 4]>>,
    pub indices: Vec<u32>,
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
        let has_uvs = self.uvs.is_some();
        let has_uvs1 = has_uvs && self.uvs1.is_some();
        let has_colors = self.colors.is_some();
        if has_uvs {
            vertex_attributes.push(MeshBufferVertexAttributeInfo::Custom(
                MeshBufferCustomVertexAttributeInfo::TexCoords {
                    index: 0,
                    data_size: 4,
                    component_len: 2,
                },
            ));
        }
        if has_uvs1 {
            vertex_attributes.push(MeshBufferVertexAttributeInfo::Custom(
                MeshBufferCustomVertexAttributeInfo::TexCoords {
                    index: 1,
                    data_size: 4,
                    component_len: 2,
                },
            ));
        }
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
            if let Some(uvs) = self.uvs.as_ref() {
                let uv = uvs[v];
                custom_attribute_bytes.extend_from_slice(&uv[0].to_le_bytes());
                custom_attribute_bytes.extend_from_slice(&uv[1].to_le_bytes());
            }
            if has_uvs1 {
                let uv1 = self.uvs1.as_ref().unwrap()[v];
                custom_attribute_bytes.extend_from_slice(&uv1[0].to_le_bytes());
                custom_attribute_bytes.extend_from_slice(&uv1[1].to_le_bytes());
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
            uvs0: self.uvs,
            // Raw meshes don't author tangents — generated at commit if a normal-map
            // material is bound.
            tangents: None,
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

// (MikkTSpace tangent generation moved to the shared `awsm-tangents` crate.)

/// Per-mesh options for [`AwsmRenderer::add_mesh`] beyond the geometry / material /
/// transform — the instance flags `Mesh::new` takes. `double_sided` is NOT here: it's
/// derived from the bound material (as the glTF path does).
#[derive(Debug, Clone, Copy, Default)]
pub struct AddMeshOpts {
    pub instanced: bool,
    pub hud: bool,
    pub hidden: bool,
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
        let double_sided = self
            .materials
            .get(material_key)
            .map(Material::double_sided)
            .unwrap_or(false);
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
        data: RawMeshData,
        transform_key: TransformKey,
        material_key: MaterialKey,
    ) -> crate::error::Result<MeshKey> {
        if data.positions.is_empty() || data.indices.len() % 3 != 0 {
            return Err(crate::error::AwsmError::Mesh(
                crate::meshes::error::AwsmMeshError::MeshListEmpty,
            ));
        }
        let geometry = self.register_geometry(
            data.into_geometry_source(awsm_renderer_core::pipeline::primitive::FrontFace::Ccw),
        );
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
