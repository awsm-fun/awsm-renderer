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
    materials::MaterialKey,
    meshes::{
        buffer_info::{
            MeshBufferAttributeIndexInfo, MeshBufferCustomVertexAttributeInfo, MeshBufferInfo,
            MeshBufferTriangleDataInfo, MeshBufferTriangleInfo, MeshBufferVertexAttributeInfo,
            MeshBufferVertexInfo,
        },
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
}

impl AwsmRenderer {
    /// Upload a raw `RawMeshData` + material into the renderer and return a
    /// `MeshKey` that participates in the standard render passes (visibility
    /// buffer for opaque, fragment pass for transparent).
    ///
    /// The returned mesh is **not** instanced. To draw multiple copies, either
    /// call `add_raw_mesh` repeatedly or `enable_mesh_instancing` after creation.
    pub fn add_raw_mesh(
        &mut self,
        mut data: RawMeshData,
        transform_key: TransformKey,
        material_key: MaterialKey,
    ) -> crate::error::Result<MeshKey> {
        if data.positions.is_empty() {
            return Err(crate::error::AwsmError::Mesh(
                crate::meshes::error::AwsmMeshError::MeshListEmpty,
            ));
        }
        if data.indices.len() % 3 != 0 {
            return Err(crate::error::AwsmError::Mesh(
                crate::meshes::error::AwsmMeshError::MeshListEmpty,
            ));
        }

        data.ensure_normals();
        let aabb = data.aabb();

        let vertex_count = data.vertex_count();
        let triangle_count = data.triangle_count();
        let exploded_count = triangle_count * 3;

        // ── Visibility geometry (56 bytes / exploded vertex) ───────────────
        const VIS_BYTES_PER_VERTEX: usize = MeshBufferVertexInfo::VISIBILITY_GEOMETRY_BYTE_SIZE;
        let mut visibility_bytes: Vec<u8> =
            Vec::with_capacity(exploded_count * VIS_BYTES_PER_VERTEX);
        let normals = data.normals.as_ref().expect("ensure_normals filled this");

        // Same barycentric pattern the gltf path uses (see
        // `gltf/buffers/mesh/visibility.rs`).
        const BARYCENTRICS: [[f32; 2]; 3] = [[1.0, 0.0], [0.0, 1.0], [0.0, 0.0]];

        for (triangle_index, tri) in data.indices.chunks_exact(3).enumerate() {
            for (corner, &vertex_index) in tri.iter().enumerate() {
                let v_idx = vertex_index as usize;
                let pos = data.positions[v_idx];
                let normal = normals[v_idx];
                let bary = BARYCENTRICS[corner];

                // position (12)
                visibility_bytes.extend_from_slice(&pos[0].to_le_bytes());
                visibility_bytes.extend_from_slice(&pos[1].to_le_bytes());
                visibility_bytes.extend_from_slice(&pos[2].to_le_bytes());
                // triangle_index (4)
                visibility_bytes.extend_from_slice(&(triangle_index as u32).to_le_bytes());
                // barycentric (8)
                visibility_bytes.extend_from_slice(&bary[0].to_le_bytes());
                visibility_bytes.extend_from_slice(&bary[1].to_le_bytes());
                // normal (12)
                visibility_bytes.extend_from_slice(&normal[0].to_le_bytes());
                visibility_bytes.extend_from_slice(&normal[1].to_le_bytes());
                visibility_bytes.extend_from_slice(&normal[2].to_le_bytes());
                // tangent (16) — synthetic [0,0,0,1] when caller didn't supply one.
                // Matches gltf populate's default-tangent fallback.
                visibility_bytes.extend_from_slice(&0.0_f32.to_le_bytes());
                visibility_bytes.extend_from_slice(&0.0_f32.to_le_bytes());
                visibility_bytes.extend_from_slice(&0.0_f32.to_le_bytes());
                visibility_bytes.extend_from_slice(&1.0_f32.to_le_bytes());
                // original_vertex_index (4)
                visibility_bytes.extend_from_slice(&vertex_index.to_le_bytes());
            }
        }

        // ── Custom attribute index (12 bytes per triangle = 3 * u32) ──────
        let mut attribute_index_bytes: Vec<u8> = Vec::with_capacity(triangle_count * 12);
        for tri in data.indices.chunks_exact(3) {
            attribute_index_bytes.extend_from_slice(&tri[0].to_le_bytes());
            attribute_index_bytes.extend_from_slice(&tri[1].to_le_bytes());
            attribute_index_bytes.extend_from_slice(&tri[2].to_le_bytes());
        }

        // ── Custom attribute data (UVs + optional colors, AoS) ─────────────
        let mut custom_attributes: Vec<MeshBufferVertexAttributeInfo> = Vec::new();
        let mut custom_attribute_bytes: Vec<u8> = Vec::new();

        let has_uvs = data.uvs.is_some();
        let has_colors = data.colors.is_some();

        if has_uvs {
            custom_attributes.push(MeshBufferVertexAttributeInfo::Custom(
                MeshBufferCustomVertexAttributeInfo::TexCoords {
                    index: 0,
                    data_size: 4,     // f32 → 4 bytes per component
                    component_len: 2, // u, v
                },
            ));
        }
        if has_colors {
            custom_attributes.push(MeshBufferVertexAttributeInfo::Custom(
                MeshBufferCustomVertexAttributeInfo::Colors {
                    index: 0,
                    data_size: 4,
                    component_len: 4,
                },
            ));
        }

        // Pack as array-of-structs: per vertex, write each declared attribute in
        // declaration order. Matches `pack_vertex_attributes` in gltf::buffers.
        for v in 0..vertex_count {
            if let Some(uvs) = data.uvs.as_ref() {
                let uv = uvs[v];
                custom_attribute_bytes.extend_from_slice(&uv[0].to_le_bytes());
                custom_attribute_bytes.extend_from_slice(&uv[1].to_le_bytes());
            }
            if let Some(colors) = data.colors.as_ref() {
                let c = colors[v];
                custom_attribute_bytes.extend_from_slice(&c[0].to_le_bytes());
                custom_attribute_bytes.extend_from_slice(&c[1].to_le_bytes());
                custom_attribute_bytes.extend_from_slice(&c[2].to_le_bytes());
                custom_attribute_bytes.extend_from_slice(&c[3].to_le_bytes());
            }
        }

        // ── Build MeshBufferInfo describing the layouts ────────────────────
        let triangle_info = MeshBufferTriangleInfo {
            count: triangle_count,
            vertex_attribute_indices: MeshBufferAttributeIndexInfo {
                count: triangle_count * 3,
            },
            vertex_attributes: custom_attributes,
            vertex_attributes_size: custom_attribute_bytes.len(),
            triangle_data: MeshBufferTriangleDataInfo {
                size_per_triangle: 12,
                total_size: triangle_count * 12,
            },
        };

        let buffer_info = MeshBufferInfo {
            visibility_geometry_vertex: Some(MeshBufferVertexInfo {
                count: exploded_count,
            }),
            // Transparency vertices come from a separate write; for opaque meshes
            // this stays None. Transparent raw meshes go through a later path.
            transparency_geometry_vertex: None,
            triangles: triangle_info,
            geometry_morph: None,
            material_morph: None,
            skin: None,
        };

        let buffer_info_key = self.meshes.buffer_infos.insert(buffer_info);

        let is_transparent = self.materials.is_transparency_pass(material_key);
        if is_transparent {
            // Transparent path also needs a transparency-vertex-bytes write so
            // the fragment shader has positions / normals / tangents. v1 of
            // raw-mesh API ships opaque-only; transparent raw meshes are TODO.
            return Err(crate::error::AwsmError::Mesh(
                crate::meshes::error::AwsmMeshError::MeshListEmpty,
            ));
        }

        let mesh = Mesh::new(transform_key, material_key, false, false, false, false);

        let mesh_key = self.meshes.insert_public(
            mesh,
            &self.materials,
            &self.transforms,
            buffer_info_key,
            Some(&visibility_bytes),
            None,
            &custom_attribute_bytes,
            &attribute_index_bytes,
            aabb,
            None,
            None,
            None,
        )?;

        Ok(mesh_key)
    }
}
