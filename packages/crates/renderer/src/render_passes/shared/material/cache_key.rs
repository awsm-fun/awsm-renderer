//! Shared material shader cache key helpers.

use crate::meshes::buffer_info::{
    MeshBufferCustomVertexAttributeInfo, MeshBufferInfo, MeshBufferVertexAttributeInfo,
    MeshBufferVisibilityVertexAttributeInfo,
};

/// Vertex attribute flags used for material shader specialization.
#[derive(Hash, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ShaderMaterialVertexAttributes {
    pub normals: bool,
    pub tangents: bool,
    pub color_sets: Option<u32>,
    /// Number of distinct `TEXCOORD_n` sets present on the mesh.
    ///
    /// Stored as (highest index + 1) so `Some(1)` means only `TEXCOORD_0` exists.
    /// The shader uses this to decide whether `pbr_should_run` can safely sample every texture.
    pub uv_sets: Option<u32>,
}

impl From<&MeshBufferInfo> for ShaderMaterialVertexAttributes {
    fn from(mesh_buffer_info: &MeshBufferInfo) -> Self {
        let mut _self = Self::default();

        // NOTE: We iterate over ALL attributes (including visibility attributes like positions/normals/tangents)
        // to detect their *presence* on the mesh. However, visibility attributes are NOT stored in the
        // attribute_data buffer - they go in visibility_data and geometry textures instead.
        //
        // The shader template uses these flags differently:
        // - normals/tangents presence is used for validation (pbr_should_run checks)
        // - BUT they are NOT included in uv_sets_index calculation since they're not in attribute_data
        //
        // Only custom attributes (colors, UVs, joints, weights) affect the attribute_data layout.

        for attr in &mesh_buffer_info.triangles.vertex_attributes {
            match attr {
                MeshBufferVertexAttributeInfo::Visibility(vis) => match vis {
                    MeshBufferVisibilityVertexAttributeInfo::Positions { .. } => {
                        // Visibility attribute - goes in visibility_data buffer, not attribute_data
                    }
                    MeshBufferVisibilityVertexAttributeInfo::Normals { .. } => {
                        // Visibility attribute - goes in visibility_data buffer, not attribute_data
                        // We still track its presence for shader validation
                        _self.normals = true;
                    }
                    MeshBufferVisibilityVertexAttributeInfo::Tangents { .. } => {
                        // Visibility attribute - goes in visibility_data buffer, not attribute_data
                        // We still track its presence for shader validation
                        _self.tangents = true;
                    }
                },
                MeshBufferVertexAttributeInfo::Custom(custom) => match custom {
                    MeshBufferCustomVertexAttributeInfo::Colors { index, .. } => {
                        // Custom attribute - goes in attribute_data buffer
                        _self.color_sets = match _self.color_sets {
                            Some(existing) => Some(existing.max(*index + 1)),
                            None => Some(*index + 1),
                        }
                    }
                    MeshBufferCustomVertexAttributeInfo::TexCoords { index, .. } => {
                        // Custom attribute - goes in attribute_data buffer
                        _self.uv_sets = match _self.uv_sets {
                            Some(existing) => Some(existing.max(*index + 1)),
                            None => Some(*index + 1),
                        }
                    }
                },
            }
        }

        _self
    }
}
