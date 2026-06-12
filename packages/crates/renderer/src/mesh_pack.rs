//! The single GPU-buffer packer for the visibility + transparency geometry
//! streams (see `docs/buffers.md`).
//!
//! Both upload front-ends pack through here so their bytes can't drift: the
//! raw-mesh path (`add_raw_mesh` / `add_raw_mesh_transparent`, from `MeshData`)
//! today, and — as the convergence point — the gltf populate path's vertex
//! builders (a follow-on; see `docs/plans/mesh-pipeline-overhaul.md`). The byte
//! layouts here are the canonical definition.

use awsm_renderer_core::pipeline::primitive::FrontFace;

/// Barycentric coords per triangle corner (matches the gltf path's
/// `buffers/mesh/visibility.rs`).
const BARYCENTRICS: [[f32; 2]; 3] = [[1.0, 0.0], [0.0, 1.0], [0.0, 0.0]];

/// The default-tangent fallback when no per-vertex tangent is supplied (a surface
/// with no normal map never reads it). Matches the gltf populate path.
const SYNTHETIC_TANGENT: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

/// Visibility geometry — 56 bytes per **exploded** vertex (one record per
/// triangle corner):
/// `position(12) | triangle_index(4) | barycentric(8) | normal(12) | tangent(16)
///   | original_vertex_index(4)`.
///
/// `tangents`, when `Some`, is indexed per original vertex; `None` packs the
/// synthetic fallback.
///
/// `front_face`: [`FrontFace::Cw`] emits each triangle's corners in `[0, 2, 1]`
/// order (with matching barycentrics) so clockwise-authored sources rasterize
/// with the same facing as the default counter-clockwise convention — the same
/// swizzle the gltf populate path applies. [`FrontFace::Ccw`] is the identity
/// order.
pub fn pack_visibility_bytes(
    positions: &[[f32; 3]],
    normals: &[[f32; 3]],
    tangents: Option<&[[f32; 4]]>,
    indices: &[u32],
    front_face: FrontFace,
) -> Vec<u8> {
    // Corner emission order per triangle (and the barycentric that rides with
    // each corner's slot).
    let corner_order: [usize; 3] = match front_face {
        FrontFace::Cw => [0, 2, 1],
        _ => [0, 1, 2],
    };
    let triangle_count = indices.len() / 3;
    let mut out = Vec::with_capacity(triangle_count * 3 * 56);
    for (triangle_index, tri) in indices.chunks_exact(3).enumerate() {
        for (slot, &corner) in corner_order.iter().enumerate() {
            let vertex_index = tri[corner];
            let v = vertex_index as usize;
            let pos = positions[v];
            let normal = normals[v];
            let bary = BARYCENTRICS[corner_order[slot]];
            let tan = tangents.map(|t| t[v]).unwrap_or(SYNTHETIC_TANGENT);
            // position (12)
            out.extend_from_slice(&pos[0].to_le_bytes());
            out.extend_from_slice(&pos[1].to_le_bytes());
            out.extend_from_slice(&pos[2].to_le_bytes());
            // triangle_index (4)
            out.extend_from_slice(&(triangle_index as u32).to_le_bytes());
            // barycentric (8)
            out.extend_from_slice(&bary[0].to_le_bytes());
            out.extend_from_slice(&bary[1].to_le_bytes());
            // normal (12)
            out.extend_from_slice(&normal[0].to_le_bytes());
            out.extend_from_slice(&normal[1].to_le_bytes());
            out.extend_from_slice(&normal[2].to_le_bytes());
            // tangent (16)
            out.extend_from_slice(&tan[0].to_le_bytes());
            out.extend_from_slice(&tan[1].to_le_bytes());
            out.extend_from_slice(&tan[2].to_le_bytes());
            out.extend_from_slice(&tan[3].to_le_bytes());
            // original_vertex_index (4)
            out.extend_from_slice(&vertex_index.to_le_bytes());
        }
    }
    out
}

/// Transparency geometry — 40 bytes per **original** (non-exploded) vertex,
/// drawn with the index buffer:
/// `position(12) | normal(12) | tangent(16)`.
pub fn pack_transparency_bytes(
    positions: &[[f32; 3]],
    normals: &[[f32; 3]],
    tangents: Option<&[[f32; 4]]>,
    vertex_count: usize,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(vertex_count * 40);
    for (v, normal) in normals.iter().enumerate().take(vertex_count) {
        let pos = positions[v];
        let tan = tangents.map(|t| t[v]).unwrap_or(SYNTHETIC_TANGENT);
        out.extend_from_slice(&pos[0].to_le_bytes());
        out.extend_from_slice(&pos[1].to_le_bytes());
        out.extend_from_slice(&pos[2].to_le_bytes());
        out.extend_from_slice(&normal[0].to_le_bytes());
        out.extend_from_slice(&normal[1].to_le_bytes());
        out.extend_from_slice(&normal[2].to_le_bytes());
        out.extend_from_slice(&tan[0].to_le_bytes());
        out.extend_from_slice(&tan[1].to_le_bytes());
        out.extend_from_slice(&tan[2].to_le_bytes());
        out.extend_from_slice(&tan[3].to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // A 1-triangle mesh; checks the byte layout is exactly as documented (the
    // regression guard locking the packed format).
    fn tri() -> (Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<[f32; 4]>, Vec<u32>) {
        (
            vec![[1.0, 2.0, 3.0], [4.0, 5.0, 6.0], [7.0, 8.0, 9.0]],
            vec![[0.0, 0.0, 1.0]; 3],
            vec![[1.0, 0.0, 0.0, 1.0]; 3],
            vec![0, 1, 2],
        )
    }

    #[test]
    fn visibility_layout_is_56_bytes_per_corner() {
        let (p, n, t, i) = tri();
        let bytes = pack_visibility_bytes(&p, &n, Some(&t), &i, FrontFace::Ccw);
        assert_eq!(bytes.len(), 56 * 3, "56 bytes per exploded corner");
        // corner 0: position == p[0]
        let read_f32 = |off: usize| f32::from_le_bytes(bytes[off..off + 4].try_into().unwrap());
        assert_eq!([read_f32(0), read_f32(4), read_f32(8)], [1.0, 2.0, 3.0]);
        // triangle_index (offset 12) == 0; original_vertex_index (offset 52) == 0
        assert_eq!(u32::from_le_bytes(bytes[12..16].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(bytes[52..56].try_into().unwrap()), 0);
        // corner 1 original_vertex_index (offset 56+52) == 1
        assert_eq!(u32::from_le_bytes(bytes[108..112].try_into().unwrap()), 1);
    }

    #[test]
    fn transparency_layout_is_40_bytes_per_vertex() {
        let (p, n, t, _) = tri();
        let bytes = pack_transparency_bytes(&p, &n, Some(&t), 3);
        assert_eq!(bytes.len(), 40 * 3, "40 bytes per vertex");
        let read_f32 = |off: usize| f32::from_le_bytes(bytes[off..off + 4].try_into().unwrap());
        // vertex 0: position then normal then tangent
        assert_eq!([read_f32(0), read_f32(4), read_f32(8)], [1.0, 2.0, 3.0]);
        assert_eq!([read_f32(12), read_f32(16), read_f32(20)], [0.0, 0.0, 1.0]);
        assert_eq!(read_f32(36), 1.0); // tangent.w
    }

    #[test]
    fn cw_front_face_swizzles_corners_and_barycentrics() {
        let (p, n, t, i) = tri();
        let ccw = pack_visibility_bytes(&p, &n, Some(&t), &i, FrontFace::Ccw);
        let cw = pack_visibility_bytes(&p, &n, Some(&t), &i, FrontFace::Cw);
        // Cw corner order is [0, 2, 1]: slot 1 carries vertex 2 (+ its
        // barycentric), slot 2 carries vertex 1 — i.e. records permuted.
        assert_eq!(&cw[0..56], &ccw[0..56], "slot 0 identical");
        assert_eq!(&cw[56..112], &ccw[112..168], "slot 1 = ccw corner 2");
        assert_eq!(&cw[112..168], &ccw[56..112], "slot 2 = ccw corner 1");
    }

    #[test]
    fn none_tangents_pack_synthetic() {
        let (p, n, _, i) = tri();
        let bytes = pack_visibility_bytes(&p, &n, None, &i, FrontFace::Ccw);
        // corner 0 tangent at offset 36..52 == [0,0,0,1]
        // (pos 12 + triangle_index 4 + barycentric 8 + normal 12 = 36).
        let read_f32 = |off: usize| f32::from_le_bytes(bytes[off..off + 4].try_into().unwrap());
        assert_eq!(
            [read_f32(36), read_f32(40), read_f32(44), read_f32(48)],
            [0.0, 0.0, 0.0, 1.0]
        );
    }
}
