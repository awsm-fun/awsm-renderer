//! glTF buffer extraction and layout helpers.

// pub mod vertex;
//pub mod morph;
pub mod accessor;
pub mod attributes;
pub mod index;
pub mod mesh;
pub mod morph;
pub mod normals;
pub mod skin;
pub mod triangle;

use awsm_renderer_core::pipeline::primitive::FrontFace;
use glam::{Mat4, Quat, Vec3};
use std::collections::HashMap;

use crate::{
    buffers::{
        index::{generate_fresh_indices_from_primitive, GltfMeshBufferIndexInfo},
        mesh::convert_to_mesh_buffer,
    },
    data::GltfDataHints,
};
use awsm_renderer::meshes::buffer_info::{
    MeshBufferAttributeIndexInfo, MeshBufferGeometryMorphInfo, MeshBufferInfo,
    MeshBufferMaterialMorphAttributes, MeshBufferMaterialMorphInfo, MeshBufferSkinInfo,
    MeshBufferTriangleDataInfo, MeshBufferTriangleInfo, MeshBufferVertexAttributeInfo,
};

use crate::error::Result;

macro_rules! maybe_enter_span {
    ($enabled:expr, $($span_args:tt)*) => {{
        if $enabled {
            Some(tracing::span!($($span_args)*).entered())
        } else {
            None
        }
    }};
}

/// Packed buffers extracted from a glTF asset.
#[derive(Debug)]
pub struct GltfBuffers {
    pub raw: Vec<Vec<u8>>,
    // this isn't passed to the shader at all
    // just used in the pipeline for drawing
    pub index_bytes: Vec<u8>,

    // (The per-pass visibility/transparency vertex byte streams are no longer
    // produced by the decode — the renderer packs them at commit from the retained
    // typed source on each `MeshBufferInfoWithOffset`.)

    // Vertex attribute storage buffer (normals, UVs, colors, etc. per triangle)
    // these always follow the same interleaving pattern
    // although, not all primitives have all the same attributes
    // it's just that when they do, they follow the same order
    pub custom_attribute_vertex_bytes: Vec<u8>,

    // Triangle data buffer (vertex indices + material info per triangle)
    pub triangle_data_bytes: Vec<u8>,

    // just positions
    pub geometry_morph_bytes: Vec<u8>,
    // normal, tangent (TODO: TEXCOORD_n, COLOR_n)
    pub material_morph_bytes: Vec<u8>,

    // skins
    pub skin_joint_index_weight_bytes: Vec<u8>,

    // first level is mesh, second level is primitive
    pub meshes: Vec<Vec<MeshBufferInfoWithOffset>>,
}

impl GltfBuffers {
    /// Clones all buffer data for independent use.
    pub fn heavy_clone(&self) -> Self {
        Self {
            raw: self.raw.clone(),
            index_bytes: self.index_bytes.clone(),
            custom_attribute_vertex_bytes: self.custom_attribute_vertex_bytes.clone(),
            triangle_data_bytes: self.triangle_data_bytes.clone(),
            geometry_morph_bytes: self.geometry_morph_bytes.clone(),
            material_morph_bytes: self.material_morph_bytes.clone(),
            skin_joint_index_weight_bytes: self.skin_joint_index_weight_bytes.clone(),
            meshes: self.meshes.clone(),
        }
    }
}

fn compute_world_matrices(doc: &gltf::Document) -> HashMap<usize, Mat4> {
    let mut world = HashMap::new();

    for scene in doc.scenes() {
        for node in scene.nodes() {
            accumulate_world_matrix(&mut world, &node, Mat4::IDENTITY);
        }
    }

    world
}

fn accumulate_world_matrix(
    world: &mut HashMap<usize, Mat4>,
    node: &gltf::Node<'_>,
    parent_world: Mat4,
) {
    let local = node_local_matrix(node);
    let world_matrix = parent_world * local;
    world.insert(node.index(), world_matrix);

    for child in node.children() {
        accumulate_world_matrix(world, &child, world_matrix);
    }
}

fn node_local_matrix(node: &gltf::Node<'_>) -> Mat4 {
    match node.transform() {
        gltf::scene::Transform::Matrix { matrix } => Mat4::from_cols_array_2d(&matrix),
        gltf::scene::Transform::Decomposed {
            translation,
            rotation,
            scale,
        } => {
            Mat4::from_translation(Vec3::from_array(translation))
                * Mat4::from_quat(Quat::from_array(rotation))
                * Mat4::from_scale(Vec3::from_array(scale))
        }
    }
}

fn determine_front_face(
    mesh_index: usize,
    doc: &gltf::Document,
    world_matrices: &HashMap<usize, Mat4>,
) -> FrontFace {
    let mut front_face: Option<FrontFace> = None;

    for node in doc.nodes() {
        if let Some(mesh) = node.mesh() {
            if mesh.index() == mesh_index {
                let det = world_matrices
                    .get(&node.index())
                    .map(Mat4::determinant)
                    .unwrap_or_else(|| node_local_matrix(&node).determinant());

                if det < 0.0 {
                    return FrontFace::Cw;
                }

                if front_face.is_none() {
                    front_face = Some(FrontFace::Ccw);
                }
            }
        }
    }

    front_face.unwrap_or(FrontFace::Ccw)
}

/// Mesh buffer info paired with byte offsets into the packed buffers.
#[derive(Clone, Debug)]
pub struct MeshBufferInfoWithOffset {
    pub triangles: MeshBufferTriangleInfoWithOffset,
    pub geometry_morph: Option<MeshBufferGeometryMorphInfoWithOffset>,
    pub material_morph: Option<MeshBufferMaterialMorphInfoWithOffset>,
    pub skin: Option<MeshBufferSkinInfoWithOffset>,
    // Retained TYPED source geometry (load-transaction model):
    // enough to derive EITHER pass representation at commit via the renderer's
    // GeometrySource. `populate_gltf_primitive` reads these to build the source;
    // the visibility/transparency offset fields above are legacy (removed in 5b-iv).
    pub source_positions: Vec<[f32; 3]>,
    pub source_normals: Vec<[f32; 3]>,
    pub source_uvs0: Option<Vec<[f32; 2]>>,
    /// Authored tangents (glTF `TANGENT`), if the asset shipped them. `None` ⇒ the
    /// renderer generates at commit iff a bound material samples a normal map.
    pub source_tangents: Option<Vec<[f32; 4]>>,
    pub source_indices: Vec<u32>,
    /// Winding for the packed visibility stream — carried so `populate_gltf_primitive`
    /// can build the renderer's `GeometrySource` (which packs at commit).
    pub source_front_face: FrontFace,
}

impl From<MeshBufferInfoWithOffset> for MeshBufferInfo {
    fn from(info: MeshBufferInfoWithOffset) -> Self {
        MeshBufferInfo {
            // The decode no longer knows the per-pass reps — the renderer rebuilds a
            // real `MeshBufferInfo` (with the vis/transp counts it actually packed) at
            // commit in `resolve_one`. This native view is consumed by `populate` only
            // for the pass-independent triangle/morph/skin layout, so leave both None.
            visibility_geometry_vertex: None,
            transparency_geometry_vertex: None,
            triangles: info.triangles.into(),
            geometry_morph: info.geometry_morph.map(|m| m.into()),
            material_morph: info.material_morph.map(|m| m.into()),
            skin: info.skin.map(|m| m.into()),
        }
    }
}

/// Triangle buffer info paired with byte offsets into the packed buffers.
#[derive(Clone, Debug)]
pub struct MeshBufferTriangleInfoWithOffset {
    pub count: usize,
    // custom attributes
    pub vertex_attribute_indices: MeshBufferAttributeIndexInfoWithOffset,
    pub vertex_attributes_offset: usize,
    pub vertex_attributes: Vec<MeshBufferVertexAttributeInfo>,
    pub vertex_attributes_size: usize,
    pub triangle_data: MeshBufferTriangleDataInfoWithOffset,
}

impl From<MeshBufferTriangleInfoWithOffset> for MeshBufferTriangleInfo {
    fn from(info: MeshBufferTriangleInfoWithOffset) -> Self {
        MeshBufferTriangleInfo {
            count: info.count,
            vertex_attribute_indices: info.vertex_attribute_indices.into(),
            vertex_attributes: info.vertex_attributes.into_iter().collect(),
            vertex_attributes_size: info.vertex_attributes_size,
            triangle_data: info.triangle_data.into(),
        }
    }
}

/// Index buffer info paired with byte offsets into the packed buffers.
#[derive(Debug, Clone)]
pub struct MeshBufferAttributeIndexInfoWithOffset {
    pub offset: usize,
    pub count: usize,
}

impl MeshBufferAttributeIndexInfoWithOffset {
    /// Returns total byte size if it fits in `usize`.
    pub fn checked_total_size(&self) -> Option<usize> {
        self.count.checked_mul(4) // guaranteed u32
    }

    /// Returns the total size in bytes for the index data.
    pub fn total_size(&self) -> usize {
        self.checked_total_size()
            .expect("index byte size overflowed usize")
    }
}

impl From<MeshBufferAttributeIndexInfoWithOffset> for MeshBufferAttributeIndexInfo {
    fn from(info: MeshBufferAttributeIndexInfoWithOffset) -> Self {
        MeshBufferAttributeIndexInfo { count: info.count }
    }
}

/// Geometry morph info paired with byte offsets into the packed buffers.
#[derive(Debug, Clone)]
pub struct MeshBufferGeometryMorphInfoWithOffset {
    pub targets_len: usize,
    pub vertex_stride_size: usize, // Size per vertex across all targets (position + normal + tangent)
    pub values_size: usize,
    pub values_offset: usize,
}

impl From<MeshBufferGeometryMorphInfoWithOffset> for MeshBufferGeometryMorphInfo {
    fn from(info: MeshBufferGeometryMorphInfoWithOffset) -> Self {
        MeshBufferGeometryMorphInfo {
            targets_len: info.targets_len,
            vertex_stride_size: info.vertex_stride_size,
            values_size: info.values_size,
        }
    }
}

/// Material morph info paired with byte offsets into the packed buffers.
#[derive(Debug, Clone)]
pub struct MeshBufferMaterialMorphInfoWithOffset {
    pub attributes: MeshBufferMaterialMorphAttributes, // Which attributes are present
    pub targets_len: usize,
    pub vertex_stride_size: usize, // Size per original vertex across all targets
    pub values_size: usize,
    pub values_offset: usize,
}

impl From<MeshBufferMaterialMorphInfoWithOffset> for MeshBufferMaterialMorphInfo {
    fn from(info: MeshBufferMaterialMorphInfoWithOffset) -> Self {
        MeshBufferMaterialMorphInfo {
            attributes: info.attributes,
            targets_len: info.targets_len,
            vertex_stride_size: info.vertex_stride_size,
            values_size: info.values_size,
        }
    }
}

/// Skin buffer info paired with byte offsets into the packed buffers.
#[derive(Debug, Clone)]
pub struct MeshBufferSkinInfoWithOffset {
    pub set_count: usize,
    pub index_weights_size: usize,
    // Offsets for GLTF population (not needed after we feed the dynamic storage buffer)
    pub index_weights_offset: usize,
}

impl From<MeshBufferSkinInfoWithOffset> for MeshBufferSkinInfo {
    fn from(info: MeshBufferSkinInfoWithOffset) -> Self {
        MeshBufferSkinInfo {
            set_count: info.set_count,
            index_weights_size: info.index_weights_size,
        }
    }
}

/// Triangle data info paired with byte offsets into the packed buffers.
#[derive(Debug, Clone)]
pub struct MeshBufferTriangleDataInfoWithOffset {
    pub size_per_triangle: usize,
    pub total_size: usize,
    pub offset: usize,
}

impl From<MeshBufferTriangleDataInfoWithOffset> for MeshBufferTriangleDataInfo {
    fn from(info: MeshBufferTriangleDataInfoWithOffset) -> Self {
        MeshBufferTriangleDataInfo {
            size_per_triangle: info.size_per_triangle,
            total_size: info.total_size,
        }
    }
}

impl GltfBuffers {
    /// Builds packed buffers from a glTF document and raw buffer data.
    pub fn new(doc: &gltf::Document, buffers: Vec<Vec<u8>>, hints: GltfDataHints) -> Result<Self> {
        let _maybe_total_span_guard = maybe_enter_span!(
            hints.render_timings,
            tracing::Level::INFO,
            "GLTF buffers build",
            mesh_count = doc.meshes().len()
        );
        let world_matrices = compute_world_matrices(doc);

        // refactor original buffers into the format we want
        // namely, pack the data in a predictable order
        // arranged by primitive
        // with indices as a separate buffer

        let mut index_bytes: Vec<u8> = Vec::new();
        let mut custom_attribute_vertex_bytes: Vec<u8> = Vec::new();
        let mut triangle_data_bytes: Vec<u8> = Vec::new();
        let mut geometry_morph_bytes: Vec<u8> = Vec::new();
        let mut material_morph_bytes: Vec<u8> = Vec::new();
        let mut skin_joint_index_weight_bytes: Vec<u8> = Vec::new();
        let mut meshes: Vec<Vec<MeshBufferInfoWithOffset>> = Vec::new();

        for mesh in doc.meshes() {
            let _maybe_mesh_span_guard = maybe_enter_span!(
                hints.render_timings,
                tracing::Level::INFO,
                "GLTF mesh build",
                mesh_index = mesh.index(),
                primitive_count = mesh.primitives().len()
            );
            let mut primitive_buffer_infos = Vec::new();

            let front_face = determine_front_face(mesh.index(), doc, &world_matrices);
            for primitive in mesh.primitives() {
                let _maybe_primitive_span_guard = maybe_enter_span!(
                    hints.render_timings,
                    tracing::Level::INFO,
                    "GLTF primitive build",
                    mesh_index = mesh.index(),
                    primitive_index = primitive.index()
                );
                let maybe_index_info = {
                    let _maybe_index_stage_span_guard = maybe_enter_span!(
                        hints.render_timings,
                        tracing::Level::INFO,
                        "build_indices"
                    );
                    GltfMeshBufferIndexInfo::maybe_new(&primitive, &buffers, &mut index_bytes)
                }?;
                let index: MeshBufferAttributeIndexInfoWithOffset = match maybe_index_info {
                    Some(info) => info.into(),
                    None => generate_fresh_indices_from_primitive(&primitive, &mut index_bytes)?,
                };

                // Step 2: Convert to mesh buffer format. NOTE the geometry KIND is no
                // longer decided here — the renderer derives it at commit from the
                // bound materials; the decode retains the
                // pass-independent typed source on the returned info.
                let mesh_buffer_info = {
                    let _maybe_convert_stage_span_guard = maybe_enter_span!(
                        hints.render_timings,
                        tracing::Level::INFO,
                        "convert_to_mesh_buffer"
                    );
                    convert_to_mesh_buffer(
                        &primitive,
                        hints.render_timings,
                        front_face,
                        &buffers,
                        &index,
                        &index_bytes,
                        &mut custom_attribute_vertex_bytes,
                        &mut triangle_data_bytes,
                        &mut geometry_morph_bytes,
                        &mut material_morph_bytes,
                        &mut skin_joint_index_weight_bytes,
                    )?
                };

                primitive_buffer_infos.push(mesh_buffer_info);
            }

            meshes.push(primitive_buffer_infos);
        }

        if hints.render_timings {
            tracing::info!(
                mesh_count = meshes.len(),
                index_bytes_len = index_bytes.len(),
                custom_attribute_vertex_bytes_len = custom_attribute_vertex_bytes.len(),
                triangle_data_bytes_len = triangle_data_bytes.len(),
                geometry_morph_bytes_len = geometry_morph_bytes.len(),
                skin_joint_index_weight_bytes_len = skin_joint_index_weight_bytes.len(),
                "GLTF buffers build summary"
            );
        }

        Ok(Self {
            raw: buffers,
            index_bytes,
            custom_attribute_vertex_bytes,
            triangle_data_bytes,
            meshes,
            geometry_morph_bytes,
            material_morph_bytes,
            skin_joint_index_weight_bytes,
        })
    }
}
