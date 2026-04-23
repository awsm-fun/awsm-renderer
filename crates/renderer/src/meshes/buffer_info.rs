//! Mesh buffer metadata and attribute layouts.

use super::error::{AwsmMeshError, Result};
use awsm_renderer_core::pipeline::vertex::VertexFormat;
use slotmap::new_key_type;

/// Storage for mesh buffer info records.
pub struct MeshBufferInfos {
    infos: slotmap::SlotMap<MeshBufferInfoKey, MeshBufferInfo>,
}

impl Default for MeshBufferInfos {
    fn default() -> Self {
        Self::new()
    }
}

impl MeshBufferInfos {
    /// Creates an empty buffer info store.
    pub fn new() -> Self {
        Self {
            infos: slotmap::SlotMap::with_key(),
        }
    }

    /// Inserts buffer info and returns its key.
    pub fn insert(&mut self, info: MeshBufferInfo) -> MeshBufferInfoKey {
        self.infos.insert(info)
    }

    /// Returns buffer info by key.
    pub fn get(&self, key: MeshBufferInfoKey) -> Result<&MeshBufferInfo> {
        self.infos
            .get(key)
            .ok_or(AwsmMeshError::BufferInfoNotFound(key))
    }

    /// Removes buffer info by key.
    pub fn remove(&mut self, key: MeshBufferInfoKey) -> Option<MeshBufferInfo> {
        self.infos.remove(key)
    }
}

/// Aggregate buffer info for a mesh.
#[derive(Debug, Clone)]
pub struct MeshBufferInfo {
    pub visibility_geometry_vertex: Option<MeshBufferVertexInfo>,
    pub transparency_geometry_vertex: Option<MeshBufferVertexInfo>,
    pub triangles: MeshBufferTriangleInfo,
    pub geometry_morph: Option<MeshBufferGeometryMorphInfo>,
    pub material_morph: Option<MeshBufferMaterialMorphInfo>,
    pub skin: Option<MeshBufferSkinInfo>,
}

/// Vertex buffer info for a mesh.
#[derive(Debug, Clone)]
pub struct MeshBufferVertexInfo {
    // Number of vertices (triangle_count * 3)
    pub count: usize,
}

impl MeshBufferVertexInfo {
    // Visibility buffer layout (exploded per-triangle-vertex):
    // - positions (vec3<f32>), 12 bytes per vertex
    // - triangle_index (u32), 4 bytes per vertex
    // - barycentric coordinates (vec2<f32>), 8 bytes per vertex
    // - normals (vec3<f32>), 12 bytes per vertex
    // - tangents (vec4<f32>), 16 bytes per vertex (w = handedness)
    // - original_vertex_index (u32), 4 bytes per vertex (for indexed skin/morph access)
    // Total size per vertex = 12 + 4 + 8 + 12 + 16 + 4 = 56 bytes
    /// Byte size for visibility geometry vertices.
    pub const VISIBILITY_GEOMETRY_BYTE_SIZE: usize = 56;

    // positions (vec3<f32>), 12 bytes per vertex
    // normals (vec3<f32>), 12 bytes per vertex
    // tangents (vec4<f32>), 16 bytes per vertex (w = handedness)
    // Total size per vertex = 12 + 12 + 16 = 40 bytes
    /// Byte size for transparency geometry vertices.
    pub const TRANSPARENCY_GEOMETRY_BYTE_SIZE: usize = 40;
    // 16 * 4floats for transform
    /// Byte size for instance transform data.
    pub const INSTANCING_BYTE_SIZE: usize = 64;

    /// Returns the visibility geometry buffer size in bytes, or `None` on overflow.
    pub fn checked_visibility_geometry_size(&self) -> Option<usize> {
        self.count.checked_mul(Self::VISIBILITY_GEOMETRY_BYTE_SIZE)
    }

    /// Returns the visibility geometry buffer size in bytes.
    ///
    /// # Panics
    /// Panics if the result would overflow `usize`.
    pub fn visibility_geometry_size(&self) -> usize {
        self.checked_visibility_geometry_size()
            .expect("visibility geometry size overflow")
    }

    /// Returns the transparency geometry buffer size in bytes, or `None` on overflow.
    pub fn checked_transparency_geometry_size(&self) -> Option<usize> {
        self.count
            .checked_mul(Self::TRANSPARENCY_GEOMETRY_BYTE_SIZE)
    }

    /// Returns the transparency geometry buffer size in bytes.
    ///
    /// # Panics
    /// Panics if the result would overflow `usize`.
    pub fn transparency_geometry_size(&self) -> usize {
        self.checked_transparency_geometry_size()
            .expect("transparency geometry size overflow")
    }
}

/// Triangle metadata for a mesh.
#[derive(Debug, Clone)]
pub struct MeshBufferTriangleInfo {
    // Number of triangles in this primitive
    pub count: usize,
    // Per-vertex indices (3 per triangle, indexing into vertex buffer)
    pub vertex_attribute_indices: MeshBufferAttributeIndexInfo,
    // Per-vertex attribute data (original vertex layout for indexed access)
    pub vertex_attributes: Vec<MeshBufferVertexAttributeInfo>,
    // Total size of all vertex attribute data
    pub vertex_attributes_size: usize,
    // Triangle data buffer (vertex indices + material info per triangle)
    pub triangle_data: MeshBufferTriangleDataInfo,
}

impl MeshBufferTriangleInfo {
    /// Returns the stride (in bytes) across all custom vertex attributes (UVs, colors, joints, weights).
    /// Note: This only includes attributes stored in the attribute_data buffer, NOT visibility attributes
    /// (positions, normals, tangents) which are stored in the visibility_data buffer.
    pub fn vertex_attribute_stride(&self) -> usize {
        self.vertex_attributes
            .iter()
            .map(|attr| attr.vertex_size())
            .sum()
    }

    /// Debug helper that returns attribute values as f32 arrays for each vertex.
    pub fn debug_get_attribute_vec_f32(
        &self,
        info: &MeshBufferVertexAttributeInfo,
        data: &[u8],
    ) -> Vec<Vec<f32>> {
        let mut out = Vec::new();
        let mut offset = 0;
        while offset < data.len() {
            for attr in &self.vertex_attributes {
                if std::mem::discriminant(attr) == std::mem::discriminant(info) {
                    let attr_data = &data[offset..offset + attr.vertex_size()];
                    let mut values = Vec::new();
                    for value in
                        attr_data
                            .chunks(attr.data_size())
                            .map(|chunk| match attr.data_size() {
                                1 => chunk[0] as f32,
                                2 => u16::from_le_bytes(chunk.try_into().unwrap()) as f32,
                                4 => f32::from_le_bytes(chunk.try_into().unwrap()),
                                _ => {
                                    panic!("Unsupported vertex attribute data size for debugging")
                                }
                            })
                    {
                        values.push(value);
                    }

                    out.push(values);
                }

                offset += attr.vertex_size();
            }
        }
        out
    }
}

/// Index buffer info for vertex attributes.
#[derive(Debug, Clone)]
pub struct MeshBufferAttributeIndexInfo {
    // Number of index elements for this primitive (triangle_count * 3)
    pub count: usize,
}

impl MeshBufferAttributeIndexInfo {
    /// Debug helper that expands index buffer bytes into per-triangle index lists.
    pub fn debug_to_vec(&self, data: &[u8]) -> Vec<Vec<usize>> {
        data.chunks(12)
            .map(|chunk| {
                chunk
                    .chunks(4)
                    .map(|c| u32::from_le_bytes(c.try_into().unwrap()) as usize)
                    .collect()
            })
            .collect()
    }
}

impl MeshBufferAttributeIndexInfo {
    // The size in bytes of the index buffer for this primitive
    /// Returns the total byte size for the index buffer.
    pub fn total_size(&self) -> usize {
        self.count * 4 // always u32
    }
}

/// Triangle data buffer info.
#[derive(Debug, Clone)]
pub struct MeshBufferTriangleDataInfo {
    // Size per triangle (vertex indices, typically 12 bytes (3 u32 indices))
    pub size_per_triangle: usize,
    // Total size of the triangle data for this mesh
    pub total_size: usize,
}

/// Information about geometry morphs (positions, normals, tangents).
#[derive(Debug, Clone)]
pub struct MeshBufferGeometryMorphInfo {
    pub targets_len: usize,
    pub vertex_stride_size: usize, // Size per vertex across all targets (position + normal + tangent)
    pub values_size: usize,
}

/// Information about material morphs (normals and tangents).
#[derive(Debug, Clone)]
pub struct MeshBufferMaterialMorphInfo {
    pub attributes: MeshBufferMaterialMorphAttributes, // Which attributes are present
    pub targets_len: usize,
    pub vertex_stride_size: usize, // Size per original vertex across all targets
    pub values_size: usize,
}

/// Attribute flags for material morphs.
#[derive(Hash, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MeshBufferMaterialMorphAttributes {
    pub normal: bool,
    pub tangent: bool,
}

/// Information about skin (indices and weights).
#[derive(Debug, Clone)]
pub struct MeshBufferSkinInfo {
    // 4 joint influences per set
    pub set_count: usize, // Number of skin sets (JOINTS_0/WEIGHTS_0, JOINTS_1/WEIGHTS_1, etc.)

    // Buffer size info
    pub index_weights_size: usize, // Total bytes: original_vertices * set_count * 16 (vec4<u32>) * 2 (index and weights)
}

impl MeshBufferInfo {
    // Helper to get triangle count
    /// Returns the number of triangles in this mesh.
    pub fn triangle_count(&self) -> usize {
        self.triangles.count
    }

    // Helper to check if we have a specific vertex attribute
    /// Returns true if the mesh includes the given vertex attribute.
    pub fn has_vertex_attribute(&self, attr: &MeshBufferVertexAttributeInfo) -> bool {
        self.triangles
            .vertex_attributes
            .iter()
            .any(|a| a.variant_equals(attr))
    }
}

/// Visibility attributes: positions, normals, tangents.
/// These are stored in the visibility_data buffer and transformed in the geometry pass.
#[derive(Hash, Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeshBufferVisibilityVertexAttributeInfo {
    /// XYZ vertex positions.
    Positions {
        data_size: usize,
        component_len: usize,
    },

    /// XYZ vertex normals.
    Normals {
        data_size: usize,
        component_len: usize,
    },

    /// XYZW vertex tangents where the `w` component is a sign value indicating the
    /// handedness of the tangent basis.
    Tangents {
        data_size: usize,
        component_len: usize,
    },
}

impl MeshBufferVisibilityVertexAttributeInfo {
    /// Returns the vertex size in bytes for this visibility attribute.
    pub fn vertex_size(&self) -> usize {
        match self {
            MeshBufferVisibilityVertexAttributeInfo::Positions {
                component_len,
                data_size,
            } => *component_len * *data_size,
            MeshBufferVisibilityVertexAttributeInfo::Normals {
                component_len,
                data_size,
            } => *component_len * *data_size,
            MeshBufferVisibilityVertexAttributeInfo::Tangents {
                component_len,
                data_size,
            } => *component_len * *data_size,
        }
    }
}

/// Custom vertex attributes that can be attached to meshes.
/// These are general-purpose vertex data (colors, UVs, etc.)
///
/// NOTE: Joints and Weights are NOT custom attributes!
/// They are skinning data handled by the separate skin system.
#[derive(Hash, Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeshBufferCustomVertexAttributeInfo {
    /// RGB or RGBA vertex color.
    Colors {
        index: u32,
        data_size: usize,
        component_len: usize,
    },

    /// UV texture co-ordinates.
    TexCoords {
        index: u32,
        data_size: usize,
        component_len: usize,
    },
}

impl MeshBufferCustomVertexAttributeInfo {
    /// Returns the packed vertex format for this attribute.
    pub fn vertex_format(&self) -> VertexFormat {
        match self {
            MeshBufferCustomVertexAttributeInfo::Colors { component_len, .. } => {
                match component_len {
                    4 => VertexFormat::Float32x4,
                    3 => VertexFormat::Float32x3,
                    2 => VertexFormat::Float32x2,
                    1 => VertexFormat::Unorm8x4, // Packed RGBA8
                    _ => panic!("Unsupported color attribute component length"),
                }
            }
            MeshBufferCustomVertexAttributeInfo::TexCoords { component_len, .. } => {
                match component_len {
                    2 => VertexFormat::Float32x2,
                    3 => VertexFormat::Float32x3,
                    4 => VertexFormat::Float32x4,
                    _ => panic!("Unsupported texcoord attribute component length"),
                }
            }
        }
    }

    /// Returns the vertex size in bytes for this custom attribute.
    pub fn vertex_size(&self) -> usize {
        match self {
            MeshBufferCustomVertexAttributeInfo::Colors {
                component_len,
                data_size,
                index: _,
            } => *component_len * *data_size,
            MeshBufferCustomVertexAttributeInfo::TexCoords {
                component_len,
                data_size,
                index: _,
            } => *component_len * *data_size,
        }
    }
}

/// Combined enum for all vertex attribute types (used during GLTF loading before separation).
#[derive(Hash, Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeshBufferVertexAttributeInfo {
    Visibility(MeshBufferVisibilityVertexAttributeInfo),
    Custom(MeshBufferCustomVertexAttributeInfo),
}

impl MeshBufferVertexAttributeInfo {
    /// Returns true if the two attributes are the same kind, ignoring indices.
    pub fn variant_equals(&self, other: &Self) -> bool {
        match self {
            MeshBufferVertexAttributeInfo::Visibility(vis_self) => match other {
                MeshBufferVertexAttributeInfo::Visibility(vis_other) => {
                    std::mem::discriminant(vis_self) == std::mem::discriminant(vis_other)
                }
                _ => false,
            },
            MeshBufferVertexAttributeInfo::Custom(custom_self) => match other {
                MeshBufferVertexAttributeInfo::Custom(custom_other) => {
                    std::mem::discriminant(custom_self) == std::mem::discriminant(custom_other)
                }
                _ => false,
            },
        }
    }
}

impl MeshBufferVertexAttributeInfo {
    /// Returns true if this is a visibility attribute (positions, normals, tangents).
    pub fn is_visibility_attribute(&self) -> bool {
        matches!(self, MeshBufferVertexAttributeInfo::Visibility(_))
    }

    /// Returns true if this is a custom attribute (UVs, colors, joints, weights).
    pub fn is_custom_attribute(&self) -> bool {
        matches!(self, MeshBufferVertexAttributeInfo::Custom(_))
    }
    fn primary_val(&self) -> usize {
        match self {
            MeshBufferVertexAttributeInfo::Visibility(vis) => match vis {
                MeshBufferVisibilityVertexAttributeInfo::Positions { .. } => 0,
                MeshBufferVisibilityVertexAttributeInfo::Normals { .. } => 1,
                MeshBufferVisibilityVertexAttributeInfo::Tangents { .. } => 2,
            },
            MeshBufferVertexAttributeInfo::Custom(custom) => match custom {
                MeshBufferCustomVertexAttributeInfo::Colors { .. } => 3,
                MeshBufferCustomVertexAttributeInfo::TexCoords { .. } => 4,
            },
        }
    }

    fn secondary_val(&self) -> u32 {
        match self {
            MeshBufferVertexAttributeInfo::Visibility(_) => 0,
            MeshBufferVertexAttributeInfo::Custom(custom) => match custom {
                MeshBufferCustomVertexAttributeInfo::Colors { index, .. } => *index,
                MeshBufferCustomVertexAttributeInfo::TexCoords { index, .. } => *index,
            },
        }
    }

    /// Forces the stored data size for this attribute.
    pub fn force_data_size(&mut self, new_size: usize) {
        match self {
            MeshBufferVertexAttributeInfo::Visibility(vis) => match vis {
                MeshBufferVisibilityVertexAttributeInfo::Positions { data_size, .. } => {
                    *data_size = new_size
                }
                MeshBufferVisibilityVertexAttributeInfo::Normals { data_size, .. } => {
                    *data_size = new_size
                }
                MeshBufferVisibilityVertexAttributeInfo::Tangents { data_size, .. } => {
                    *data_size = new_size
                }
            },
            MeshBufferVertexAttributeInfo::Custom(custom) => match custom {
                MeshBufferCustomVertexAttributeInfo::Colors { data_size, .. } => {
                    *data_size = new_size
                }
                MeshBufferCustomVertexAttributeInfo::TexCoords { data_size, .. } => {
                    *data_size = new_size
                }
            },
        }
    }

    /// Returns the size in bytes per component for this attribute.
    pub fn data_size(&self) -> usize {
        match self {
            MeshBufferVertexAttributeInfo::Visibility(vis) => match vis {
                MeshBufferVisibilityVertexAttributeInfo::Positions { data_size, .. } => *data_size,
                MeshBufferVisibilityVertexAttributeInfo::Normals { data_size, .. } => *data_size,
                MeshBufferVisibilityVertexAttributeInfo::Tangents { data_size, .. } => *data_size,
            },
            MeshBufferVertexAttributeInfo::Custom(custom) => match custom {
                MeshBufferCustomVertexAttributeInfo::Colors { data_size, .. } => *data_size,
                MeshBufferCustomVertexAttributeInfo::TexCoords { data_size, .. } => *data_size,
            },
        }
    }

    /// Returns the number of components for this attribute.
    pub fn component_len(&self) -> usize {
        match self {
            MeshBufferVertexAttributeInfo::Visibility(vis) => match vis {
                MeshBufferVisibilityVertexAttributeInfo::Positions { component_len, .. } => {
                    *component_len
                }
                MeshBufferVisibilityVertexAttributeInfo::Normals { component_len, .. } => {
                    *component_len
                }
                MeshBufferVisibilityVertexAttributeInfo::Tangents { component_len, .. } => {
                    *component_len
                }
            },
            MeshBufferVertexAttributeInfo::Custom(custom) => match custom {
                MeshBufferCustomVertexAttributeInfo::Colors { component_len, .. } => *component_len,
                MeshBufferCustomVertexAttributeInfo::TexCoords { component_len, .. } => {
                    *component_len
                }
            },
        }
    }

    /// Returns the total vertex size in bytes for this attribute.
    pub fn vertex_size(&self) -> usize {
        match self {
            MeshBufferVertexAttributeInfo::Visibility(vis) => vis.vertex_size(),
            MeshBufferVertexAttributeInfo::Custom(custom) => custom.vertex_size(),
        }
    }
}

impl PartialOrd for MeshBufferVertexAttributeInfo {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for MeshBufferVertexAttributeInfo {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match self.primary_val().cmp(&other.primary_val()) {
            std::cmp::Ordering::Equal => self.secondary_val().cmp(&other.secondary_val()),
            ordering => ordering,
        }
    }
}

new_key_type! {
    /// SlotMap key for mesh buffer info records.
    pub struct MeshBufferInfoKey;
}
