use awsm_renderer_core::pipeline::primitive::FrontFace;

use crate::buffers::MeshBufferTriangleDataInfoWithOffset;
use crate::error::Result;

// Pack triangle data (vertex indices)
pub(super) fn pack_triangle_data(
    triangle_indices: &[[usize; 3]],
    triangle_count: usize,
    offset: usize,
    triangle_data_bytes: &mut Vec<u8>,
    front_face: FrontFace,
    double_sided: bool,
) -> Result<MeshBufferTriangleDataInfoWithOffset> {
    debug_assert_eq!(triangle_indices.len(), triangle_count);
    for triangle in triangle_indices.iter().copied() {
        // Normalize winding order here
        let normalized_triangle = if double_sided {
            triangle // Keep original winding for double-sided materials
        } else {
            normalize_triangle_winding(triangle, front_face)
        };
        // Pack triangle vertex indices (3 * u32 = 12 bytes)
        triangle_data_bytes.extend_from_slice(&(normalized_triangle[0] as u32).to_le_bytes());
        triangle_data_bytes.extend_from_slice(&(normalized_triangle[1] as u32).to_le_bytes());
        triangle_data_bytes.extend_from_slice(&(normalized_triangle[2] as u32).to_le_bytes());
    }

    let size_per_triangle = 12; // 3 u32 indices
    let total_size = triangle_count * size_per_triangle;

    Ok(MeshBufferTriangleDataInfoWithOffset {
        size_per_triangle,
        offset,
        total_size,
    })
}

fn normalize_triangle_winding(triangle: [usize; 3], front_face: FrontFace) -> [usize; 3] {
    match front_face {
        FrontFace::Ccw => triangle,                               // Keep as-is
        FrontFace::Cw => [triangle[0], triangle[2], triangle[1]], // Flip winding
        _ => {
            // unreachable, but handle just in case
            tracing::warn!(
                "Unexpected winding order, returning original triangle: {:?}",
                triangle
            );
            triangle
        }
    }
}
