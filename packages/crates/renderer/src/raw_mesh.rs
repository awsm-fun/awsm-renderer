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

    /// Per-vertex MikkTSpace tangents (`vec4`: xyz direction + handedness `w`),
    /// one entry per original (non-exploded) vertex, or `None` when the mesh
    /// lacks the normals/UVs MikkTSpace needs (or generation fails).
    ///
    /// This mirrors the gltf populate path's `ensure_tangents` so a captured /
    /// imported mesh gets the SAME tangent basis a `populate_gltf` load would —
    /// without it the geometry packs the synthetic `[0,0,0,1]` fallback, which
    /// degenerates normal mapping (flat, washed-out normal-mapped surfaces —
    /// e.g. an imported model's normal-mapped metal/glass looks wrong vs the
    /// model-viewer's `populate_gltf` render). Callers only invoke this when the
    /// material actually samples a normal map (see `material_wants_tangents`),
    /// matching populate's gating so non-normal-mapped meshes pay nothing.
    fn compute_tangents(&self) -> Option<Vec<[f32; 4]>> {
        let normals = self.normals.as_ref()?;
        let uvs = self.uvs.as_ref()?;
        awsm_tangents::generate_tangents(&self.positions, normals, uvs, &self.indices)
    }
}

/// True when a material samples a normal map (base or clearcoat) and therefore
/// needs a real tangent basis. Mirrors `renderer-gltf`'s `ensure_tangents`
/// gating so the raw-mesh path generates tangents in exactly the cases the
/// gltf path does.
fn material_wants_tangents(mat: &Material) -> bool {
    match mat {
        Material::Pbr(m) => {
            m.normal_tex.is_some() || m.clearcoat.as_ref().is_some_and(|c| c.normal_tex.is_some())
        }
        _ => false,
    }
}

// (MikkTSpace tangent generation moved to the shared `awsm-tangents` crate.)

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

        // ── Visibility geometry (56 bytes / exploded vertex) — packed by the
        // shared `mesh_pack` packer (the canonical byte layout). Real MikkTSpace
        // tangents when the material samples a normal map (the gltf populate path
        // does the same via `ensure_tangents`); otherwise the packer writes the
        // synthetic [0,0,0,1] fallback — a surface with no normal map never reads
        // the tangent, so we skip the generation cost.
        let normals = data.normals.as_ref().expect("ensure_normals filled this");
        let tangents = self
            .materials
            .get(material_key)
            .is_ok_and(material_wants_tangents)
            .then(|| data.compute_tangents())
            .flatten();
        let visibility_bytes = crate::mesh_pack::pack_visibility_bytes(
            &data.positions,
            normals,
            tangents.as_deref(),
            &data.indices,
            awsm_renderer_core::pipeline::primitive::FrontFace::Ccw,
        );

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

        // A 2nd UV set is only valid alongside set 0 (it's contiguous after it).
        let has_uvs1 = has_uvs && data.uvs1.is_some();
        if has_uvs {
            custom_attributes.push(MeshBufferVertexAttributeInfo::Custom(
                MeshBufferCustomVertexAttributeInfo::TexCoords {
                    index: 0,
                    data_size: 4,     // f32 → 4 bytes per component
                    component_len: 2, // u, v
                },
            ));
        }
        // TEXCOORD_1 — declared right after set 0 (before colors) so the UV sets
        // are contiguous (uv_sets_index → set 0, set 1 at +2 floats).
        if has_uvs1 {
            custom_attributes.push(MeshBufferVertexAttributeInfo::Custom(
                MeshBufferCustomVertexAttributeInfo::TexCoords {
                    index: 1,
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

        // Pack as array-of-structs: per vertex, write each declared attribute in
        // declaration order. Matches `pack_vertex_attributes` in gltf::buffers.
        for v in 0..vertex_count {
            if let Some(uvs) = data.uvs.as_ref() {
                let uv = uvs[v];
                custom_attribute_bytes.extend_from_slice(&uv[0].to_le_bytes());
                custom_attribute_bytes.extend_from_slice(&uv[1].to_le_bytes());
            }
            if has_uvs1 {
                let uv1 = data.uvs1.as_ref().unwrap()[v];
                custom_attribute_bytes.extend_from_slice(&uv1[0].to_le_bytes());
                custom_attribute_bytes.extend_from_slice(&uv1[1].to_le_bytes());
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
        let normals = data.normals.as_ref().expect("ensure_normals filled this");

        // Real MikkTSpace tangents when the material samples a normal map — same
        // gating + basis as the gltf populate path (see opaque `add_raw_mesh`).
        let tangents = self
            .materials
            .get(material_key)
            .is_ok_and(material_wants_tangents)
            .then(|| data.compute_tangents())
            .flatten();

        // ── Transparency geometry ONLY (40 B / vertex, per-original-vertex,
        // *not* exploded), packed by the shared `mesh_pack` packer.
        //
        // A transparency-pass material (we returned early above for opaque)
        // builds NO visibility geometry: emitting it would also rasterize this
        // mesh into the opaque/visibility buffer, rendering it as a solid
        // occluder *in front of* its own transmission/blend — a glass surface
        // would read opaque-white. The gltf path is identical: a
        // transmission/blend/mask primitive maps to `GeometryKind::Transparency`
        // with the visibility offset `None` (see `mesh_buffer_geometry_kind`).
        let transparency_bytes = crate::mesh_pack::pack_transparency_bytes(
            &data.positions,
            normals,
            tangents.as_deref(),
            vertex_count,
        );

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
        let has_uvs1 = has_uvs && data.uvs1.is_some();
        if has_uvs {
            custom_attributes.push(MeshBufferVertexAttributeInfo::Custom(
                MeshBufferCustomVertexAttributeInfo::TexCoords {
                    index: 0,
                    data_size: 4,
                    component_len: 2,
                },
            ));
        }
        if has_uvs1 {
            custom_attributes.push(MeshBufferVertexAttributeInfo::Custom(
                MeshBufferCustomVertexAttributeInfo::TexCoords {
                    index: 1,
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
            if has_uvs1 {
                let uv1 = data.uvs1.as_ref().unwrap()[v];
                custom_attribute_bytes.extend_from_slice(&uv1[0].to_le_bytes());
                custom_attribute_bytes.extend_from_slice(&uv1[1].to_le_bytes());
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
            // Transparency-pass mesh: no visibility geometry (see the comment
            // on the transparency-bytes build above — it would double-render
            // the mesh as an opaque occluder).
            visibility_geometry_vertex: None,
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
        let mut mesh = Mesh::new(
            transform_key,
            material_key,
            double_sided,
            false,
            false,
            false,
        );
        // Transparency-pass mesh: transparency geometry only, NO visibility
        // geometry (see the buffer_info build above). This is the immutable
        // ground truth `collect_renderables` routes on.
        mesh.has_visibility_geometry = false;
        mesh.has_transparency_geometry = true;
        let mesh_key = self.meshes.insert_public(
            mesh,
            &self.materials,
            &self.transforms,
            buffer_info_key,
            None,
            Some(&transparency_bytes),
            &custom_attribute_bytes,
            &attribute_index_bytes,
            aabb,
            None,
            None,
            None,
        )?;

        self.sync_spatial_for_mesh(mesh_key);

        // A transparent mesh carries NO visibility geometry (see above), so it
        // can't participate in the shadow-generation pass — which rasterizes
        // visibility geometry. Default it to no cast / no receive so the shadow
        // pass never looks up a visibility buffer that doesn't exist (the
        // `cast: true` mesh default would otherwise make a shadow-casting light
        // try, and fail, to draw it). Mirrors the scene loader's transparent
        // shadow default; a caller may still override via `set_mesh_shadow_flags`.
        let _ = self.set_mesh_shadow_flags(
            mesh_key,
            crate::shadows::MeshShadowFlags::TRANSPARENT_DEFAULT,
        );

        // Register the per-mesh transparent pipeline key so the transparent
        // pass has a draw pipeline for this geometry. Mirrors what
        // `enable_mesh_instancing` does for instanced transparent meshes.
        let mesh_ref = self.meshes.get(mesh_key)?;
        // Only meshes that route to the transparent pass get a transparent
        // pipeline. An opaque material — including an opaque dynamic material
        // whose author WGSL targets the opaque contract (`input.coords`, …) —
        // must not be compiled against the transparent fragment.
        if !self.materials.is_transparency_pass(mesh_ref.material_key) {
            return Ok(mesh_key);
        }
        let writes_depth = self
            .materials
            .transparent_writes_depth(mesh_ref.material_key);
        let (mat_base, mat_pbr_features) =
            self.materials.transparent_variant(mesh_ref.material_key);
        let dynamic_shader_id = matches!(mat_base, crate::dynamic_materials::ShadingBase::Custom)
            .then(|| self.materials.shader_id(mesh_ref.material_key));
        let dynamic_shader =
            dynamic_shader_id.and_then(|id| self.dynamic_materials.shader_info_for(id));
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
                writes_depth,
                mat_base,
                mat_pbr_features,
                dynamic_shader_id,
                dynamic_shader,
            )
            .await?;

        Ok(mesh_key)
    }
}
