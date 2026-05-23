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
    /// `MeshKey` that participates in the visibility-buffer opaque pass.
    ///
    /// Sync; opaque-only. Use [`AwsmRenderer::add_raw_mesh_transparent`] for
    /// materials whose alpha mode routes through the transparent pass (the
    /// transparent path needs an async transparent-pipeline-key registration
    /// for the per-mesh attributes, so it can't be sync).
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
            return Err(crate::error::AwsmError::Mesh(
                crate::meshes::error::AwsmMeshError::MeshListEmpty,
            ));
        }

        // Inherit the material's double-sided flag — the gltf path does
        // the same upstream, but procedural meshes never had a packed
        // material attached at mesh-creation time, so this was silently
        // hardcoded to single-sided. Without it, toggling "Double-sided"
        // in the material inspector has no effect on a plane / sweep /
        // sprite mesh's cull mode.
        let double_sided = self
            .materials
            .get(material_key)
            .map(Material::double_sided)
            .unwrap_or(false);

        let mesh = Mesh::new(
            transform_key,
            material_key,
            double_sided,
            false,
            false,
            false,
        );

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

        self.sync_spatial_for_mesh(mesh_key);

        Ok(mesh_key)
    }

    /// Async variant of [`AwsmRenderer::add_raw_mesh`] that supports the
    /// transparent pass. Builds a 40-byte transparency vertex pack
    /// (position(12) + normal(12) + tangent(16); per-vertex, *not*
    /// exploded — matches the glTF transparent path) and registers the
    /// per-mesh transparent pipeline key after insert.
    ///
    /// For opaque materials this delegates to the sync `add_raw_mesh` —
    /// the async wrapper is a no-op cost in that case.
    pub async fn add_raw_mesh_transparent(
        &mut self,
        mut data: RawMeshData,
        transform_key: TransformKey,
        material_key: MaterialKey,
    ) -> crate::error::Result<MeshKey> {
        if !self.materials.is_transparency_pass(material_key) {
            return self.add_raw_mesh(data, transform_key, material_key);
        }
        if data.positions.is_empty() || data.indices.len() % 3 != 0 {
            return Err(crate::error::AwsmError::Mesh(
                crate::meshes::error::AwsmMeshError::MeshListEmpty,
            ));
        }

        data.ensure_normals();
        let aabb = data.aabb();
        let vertex_count = data.vertex_count();
        let triangle_count = data.triangle_count();
        let exploded_count = triangle_count * 3;

        // ── Visibility geometry (56 B / exploded vertex) — same layout as
        // the opaque path; the transparent pipeline doesn't consume this but
        // the buffer_info expects both visibility + transparency offsets.
        let mut visibility_bytes: Vec<u8> = Vec::with_capacity(
            exploded_count * MeshBufferVertexInfo::VISIBILITY_GEOMETRY_BYTE_SIZE,
        );
        let normals = data.normals.as_ref().expect("ensure_normals filled this");
        const BARYCENTRICS: [[f32; 2]; 3] = [[1.0, 0.0], [0.0, 1.0], [0.0, 0.0]];
        for (triangle_index, tri) in data.indices.chunks_exact(3).enumerate() {
            for (corner, &vertex_index) in tri.iter().enumerate() {
                let v_idx = vertex_index as usize;
                let pos = data.positions[v_idx];
                let normal = normals[v_idx];
                let bary = BARYCENTRICS[corner];
                visibility_bytes.extend_from_slice(&pos[0].to_le_bytes());
                visibility_bytes.extend_from_slice(&pos[1].to_le_bytes());
                visibility_bytes.extend_from_slice(&pos[2].to_le_bytes());
                visibility_bytes.extend_from_slice(&(triangle_index as u32).to_le_bytes());
                visibility_bytes.extend_from_slice(&bary[0].to_le_bytes());
                visibility_bytes.extend_from_slice(&bary[1].to_le_bytes());
                visibility_bytes.extend_from_slice(&normal[0].to_le_bytes());
                visibility_bytes.extend_from_slice(&normal[1].to_le_bytes());
                visibility_bytes.extend_from_slice(&normal[2].to_le_bytes());
                visibility_bytes.extend_from_slice(&0.0_f32.to_le_bytes());
                visibility_bytes.extend_from_slice(&0.0_f32.to_le_bytes());
                visibility_bytes.extend_from_slice(&0.0_f32.to_le_bytes());
                visibility_bytes.extend_from_slice(&1.0_f32.to_le_bytes());
                visibility_bytes.extend_from_slice(&vertex_index.to_le_bytes());
            }
        }

        // ── Transparency geometry (40 B / vertex, per-original-vertex,
        // *not* exploded). Matches `gltf/buffers/mesh/transparency.rs`.
        let mut transparency_bytes: Vec<u8> = Vec::with_capacity(
            vertex_count * MeshBufferVertexInfo::TRANSPARENCY_GEOMETRY_BYTE_SIZE,
        );
        for (v_idx, normal) in normals.iter().enumerate().take(vertex_count) {
            let pos = data.positions[v_idx];
            let normal = *normal;
            transparency_bytes.extend_from_slice(&pos[0].to_le_bytes());
            transparency_bytes.extend_from_slice(&pos[1].to_le_bytes());
            transparency_bytes.extend_from_slice(&pos[2].to_le_bytes());
            transparency_bytes.extend_from_slice(&normal[0].to_le_bytes());
            transparency_bytes.extend_from_slice(&normal[1].to_le_bytes());
            transparency_bytes.extend_from_slice(&normal[2].to_le_bytes());
            // Synthetic tangent [0,0,0,1] matches the gltf default-tangent
            // fallback when meshes don't ship per-vertex tangents.
            transparency_bytes.extend_from_slice(&0.0_f32.to_le_bytes());
            transparency_bytes.extend_from_slice(&0.0_f32.to_le_bytes());
            transparency_bytes.extend_from_slice(&0.0_f32.to_le_bytes());
            transparency_bytes.extend_from_slice(&1.0_f32.to_le_bytes());
        }

        // Attribute index (per-triangle u32s — same as opaque).
        let mut attribute_index_bytes: Vec<u8> = Vec::with_capacity(triangle_count * 12);
        for tri in data.indices.chunks_exact(3) {
            attribute_index_bytes.extend_from_slice(&tri[0].to_le_bytes());
            attribute_index_bytes.extend_from_slice(&tri[1].to_le_bytes());
            attribute_index_bytes.extend_from_slice(&tri[2].to_le_bytes());
        }

        // Custom attributes (UVs + colors if supplied). Same packing as opaque.
        let mut custom_attributes: Vec<MeshBufferVertexAttributeInfo> = Vec::new();
        let mut custom_attribute_bytes: Vec<u8> = Vec::new();
        let has_uvs = data.uvs.is_some();
        let has_colors = data.colors.is_some();
        if has_uvs {
            custom_attributes.push(MeshBufferVertexAttributeInfo::Custom(
                MeshBufferCustomVertexAttributeInfo::TexCoords {
                    index: 0,
                    data_size: 4,
                    component_len: 2,
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
            transparency_geometry_vertex: Some(MeshBufferVertexInfo {
                count: vertex_count,
            }),
            triangles: triangle_info,
            geometry_morph: None,
            material_morph: None,
            skin: None,
        };
        let buffer_info_key = self.meshes.buffer_infos.insert(buffer_info);

        // See note on the opaque path's add_raw_mesh — same propagation.
        let double_sided = self
            .materials
            .get(material_key)
            .map(Material::double_sided)
            .unwrap_or(false);
        let mesh = Mesh::new(
            transform_key,
            material_key,
            double_sided,
            false,
            false,
            false,
        );
        let mesh_key = self.meshes.insert_public(
            mesh,
            &self.materials,
            &self.transforms,
            buffer_info_key,
            Some(&visibility_bytes),
            Some(&transparency_bytes),
            &custom_attribute_bytes,
            &attribute_index_bytes,
            aabb,
            None,
            None,
            None,
        )?;

        self.sync_spatial_for_mesh(mesh_key);

        // Register the per-mesh transparent pipeline key so the transparent
        // pass has a draw pipeline for this geometry. Mirrors what
        // `enable_mesh_instancing` does for instanced transparent meshes.
        let mesh_ref = self.meshes.get(mesh_key)?;
        let has_transmission = self.materials.has_transmission(mesh_ref.material_key);
        self.render_passes
            .material_transparent
            .pipelines
            .set_render_pipeline_key(
                &self.gpu,
                mesh_ref,
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
                has_transmission,
            )
            .await?;

        Ok(mesh_key)
    }
}
