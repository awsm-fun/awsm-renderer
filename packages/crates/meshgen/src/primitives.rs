//! Primitive shape generators: plane, box, sphere, cylinder, cone, torus, sprite quad.

use std::f32::consts::TAU;

use awsm_scene::PrimitiveShape;
use glam::Vec3;

use crate::mesh_data::MeshData;

/// Map a scene [`PrimitiveShape`] to its generated [`MeshData`]. Always available
/// (no `authoring` feature needed) — the player regenerates primitive meshes from
/// params this way, and the editor's modifier-stack eval calls it for the base.
pub fn primitive_mesh(shape: &PrimitiveShape) -> MeshData {
    match shape {
        PrimitiveShape::Plane {
            width,
            depth,
            segments_x,
            segments_z,
        } => plane_mesh(*width, *depth, *segments_x, *segments_z),
        PrimitiveShape::Box { dims } => box_mesh(Vec3::from_array(*dims)),
        PrimitiveShape::Sphere {
            radius,
            segments_long,
            segments_lat,
        } => sphere_mesh(*radius, *segments_long, *segments_lat),
        PrimitiveShape::Cylinder {
            radius,
            height,
            radial_segments,
        } => cylinder_mesh(*radius, *height, *radial_segments),
        PrimitiveShape::Cone {
            radius,
            height,
            radial_segments,
        } => cone_mesh(*radius, *height, *radial_segments),
        PrimitiveShape::Torus {
            radius,
            thickness,
            segments_major,
            segments_minor,
        } => torus_mesh(*radius, *thickness, *segments_major, *segments_minor),
    }
}

/// XZ-plane facing +Y. `width` along X, `depth` along Z, centred at origin.
///
/// Index winding matches the +Y vertex normal so the camera looking down
/// from above sees the front face. Picking the indices CCW from below
/// (the more "natural" reading order across grid rows) would put the
/// front on -Y, which contradicts the normal and makes the plane render
/// invisible from a top-down view under WebGPU's default CCW-front +
/// backface cull. The triangulation here matches the right-hand rule
/// for a +Y face: `a → d → c, a → c → b`.
pub fn plane_mesh(width: f32, depth: f32, segments_x: u32, segments_z: u32) -> MeshData {
    let sx = segments_x.max(1) as usize;
    let sz = segments_z.max(1) as usize;
    let mut positions = Vec::with_capacity((sx + 1) * (sz + 1));
    let mut uvs = Vec::with_capacity((sx + 1) * (sz + 1));
    let mut normals = Vec::with_capacity((sx + 1) * (sz + 1));
    let mut indices = Vec::with_capacity(sx * sz * 6);
    let half_w = width * 0.5;
    let half_d = depth * 0.5;
    for z in 0..=sz {
        let v = z as f32 / sz as f32;
        for x in 0..=sx {
            let u = x as f32 / sx as f32;
            positions.push([-half_w + u * width, 0.0, -half_d + v * depth]);
            uvs.push([u, v]);
            normals.push([0.0, 1.0, 0.0]);
        }
    }
    let stride = sx + 1;
    for z in 0..sz {
        for x in 0..sx {
            let a = (z * stride + x) as u32;
            let b = (z * stride + x + 1) as u32;
            let c = ((z + 1) * stride + x + 1) as u32;
            let d = ((z + 1) * stride + x) as u32;
            indices.extend_from_slice(&[a, d, c, a, c, b]);
        }
    }
    MeshData {
        positions,
        normals: Some(normals),
        uvs: vec![uvs],
        colors: None,
        indices,
    }
}

/// Axis-aligned box of given dimensions, centred at origin. 24 vertices (4 per face) so
/// each face has its own normal and UV.
pub fn box_mesh(dims: Vec3) -> MeshData {
    let h = dims * 0.5;
    let positions = vec![
        // +X face
        [h.x, -h.y, -h.z],
        [h.x, h.y, -h.z],
        [h.x, h.y, h.z],
        [h.x, -h.y, h.z],
        // -X face
        [-h.x, -h.y, h.z],
        [-h.x, h.y, h.z],
        [-h.x, h.y, -h.z],
        [-h.x, -h.y, -h.z],
        // +Y face
        [-h.x, h.y, -h.z],
        [-h.x, h.y, h.z],
        [h.x, h.y, h.z],
        [h.x, h.y, -h.z],
        // -Y face
        [-h.x, -h.y, h.z],
        [-h.x, -h.y, -h.z],
        [h.x, -h.y, -h.z],
        [h.x, -h.y, h.z],
        // +Z face
        [h.x, -h.y, h.z],
        [h.x, h.y, h.z],
        [-h.x, h.y, h.z],
        [-h.x, -h.y, h.z],
        // -Z face
        [-h.x, -h.y, -h.z],
        [-h.x, h.y, -h.z],
        [h.x, h.y, -h.z],
        [h.x, -h.y, -h.z],
    ];
    let normals = vec![
        [1.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [-1.0, 0.0, 0.0],
        [-1.0, 0.0, 0.0],
        [-1.0, 0.0, 0.0],
        [-1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, -1.0, 0.0],
        [0.0, -1.0, 0.0],
        [0.0, -1.0, 0.0],
        [0.0, -1.0, 0.0],
        [0.0, 0.0, 1.0],
        [0.0, 0.0, 1.0],
        [0.0, 0.0, 1.0],
        [0.0, 0.0, 1.0],
        [0.0, 0.0, -1.0],
        [0.0, 0.0, -1.0],
        [0.0, 0.0, -1.0],
        [0.0, 0.0, -1.0],
    ];
    let mut uvs = Vec::with_capacity(24);
    for _ in 0..6 {
        uvs.extend_from_slice(&[[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]]);
    }
    let mut indices = Vec::with_capacity(36);
    for face in 0..6u32 {
        let base = face * 4;
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    MeshData {
        positions,
        normals: Some(normals),
        uvs: vec![uvs],
        colors: None,
        indices,
    }
}

/// UV-sphere centred at origin.
pub fn sphere_mesh(radius: f32, segments_long: u32, segments_lat: u32) -> MeshData {
    let segments_long = segments_long.max(3) as usize;
    let segments_lat = segments_lat.max(2) as usize;
    let mut positions = Vec::with_capacity((segments_lat + 1) * (segments_long + 1));
    let mut normals = Vec::with_capacity((segments_lat + 1) * (segments_long + 1));
    let mut uvs = Vec::with_capacity((segments_lat + 1) * (segments_long + 1));
    let mut indices = Vec::new();
    for lat in 0..=segments_lat {
        let v = lat as f32 / segments_lat as f32;
        let theta = v * std::f32::consts::PI;
        let sin_t = theta.sin();
        let cos_t = theta.cos();
        for lon in 0..=segments_long {
            let u = lon as f32 / segments_long as f32;
            let phi = u * TAU;
            let sin_p = phi.sin();
            let cos_p = phi.cos();
            let nx = sin_t * cos_p;
            let ny = cos_t;
            let nz = sin_t * sin_p;
            positions.push([nx * radius, ny * radius, nz * radius]);
            normals.push([nx, ny, nz]);
            uvs.push([u, v]);
        }
    }
    let stride = segments_long + 1;
    for lat in 0..segments_lat {
        for lon in 0..segments_long {
            let a = (lat * stride + lon) as u32;
            let b = (lat * stride + lon + 1) as u32;
            let c = ((lat + 1) * stride + lon + 1) as u32;
            let d = ((lat + 1) * stride + lon) as u32;
            indices.extend_from_slice(&[a, b, c, a, c, d]);
        }
    }
    MeshData {
        positions,
        normals: Some(normals),
        uvs: vec![uvs],
        colors: None,
        indices,
    }
}

/// Cylinder along the Y axis, centred at origin, with end caps.
pub fn cylinder_mesh(radius: f32, height: f32, radial_segments: u32) -> MeshData {
    let segs = radial_segments.max(3) as usize;
    let half_h = height * 0.5;
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut uvs = Vec::new();
    let mut indices = Vec::new();

    // Side
    for i in 0..=segs {
        let u = i as f32 / segs as f32;
        let phi = u * TAU;
        let x = phi.cos();
        let z = phi.sin();
        positions.push([x * radius, -half_h, z * radius]);
        normals.push([x, 0.0, z]);
        uvs.push([u, 0.0]);
        positions.push([x * radius, half_h, z * radius]);
        normals.push([x, 0.0, z]);
        uvs.push([u, 1.0]);
    }
    for i in 0..segs {
        let a = (i * 2) as u32;
        let b = (i * 2 + 1) as u32;
        let c = (i * 2 + 3) as u32;
        let d = (i * 2 + 2) as u32;
        indices.extend_from_slice(&[a, b, c, a, c, d]);
    }
    // Top cap
    let top_center = positions.len() as u32;
    positions.push([0.0, half_h, 0.0]);
    normals.push([0.0, 1.0, 0.0]);
    uvs.push([0.5, 0.5]);
    let top_ring_start = positions.len() as u32;
    for i in 0..=segs {
        let u = i as f32 / segs as f32;
        let phi = u * TAU;
        positions.push([phi.cos() * radius, half_h, phi.sin() * radius]);
        normals.push([0.0, 1.0, 0.0]);
        uvs.push([phi.cos() * 0.5 + 0.5, phi.sin() * 0.5 + 0.5]);
    }
    for i in 0..segs as u32 {
        indices.extend_from_slice(&[top_center, top_ring_start + i, top_ring_start + i + 1]);
    }
    // Bottom cap
    let bot_center = positions.len() as u32;
    positions.push([0.0, -half_h, 0.0]);
    normals.push([0.0, -1.0, 0.0]);
    uvs.push([0.5, 0.5]);
    let bot_ring_start = positions.len() as u32;
    for i in 0..=segs {
        let u = i as f32 / segs as f32;
        let phi = u * TAU;
        positions.push([phi.cos() * radius, -half_h, phi.sin() * radius]);
        normals.push([0.0, -1.0, 0.0]);
        uvs.push([phi.cos() * 0.5 + 0.5, phi.sin() * 0.5 + 0.5]);
    }
    for i in 0..segs as u32 {
        indices.extend_from_slice(&[bot_center, bot_ring_start + i + 1, bot_ring_start + i]);
    }
    MeshData {
        positions,
        normals: Some(normals),
        uvs: vec![uvs],
        colors: None,
        indices,
    }
}

/// Cone along the Y axis, apex up, base at -y.
pub fn cone_mesh(radius: f32, height: f32, radial_segments: u32) -> MeshData {
    let segs = radial_segments.max(3) as usize;
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut uvs = Vec::new();
    let mut indices = Vec::new();

    let apex = [0.0, height * 0.5, 0.0];
    let base_y = -height * 0.5;

    for i in 0..segs {
        let phi = (i as f32 / segs as f32) * TAU;
        let next_phi = ((i + 1) as f32 / segs as f32) * TAU;
        let p0 = [phi.cos() * radius, base_y, phi.sin() * radius];
        let p1 = [next_phi.cos() * radius, base_y, next_phi.sin() * radius];
        let base_idx = positions.len() as u32;
        positions.push(apex);
        positions.push(p1);
        positions.push(p0);
        let edge_a = Vec3::from_array(p1) - Vec3::from_array(apex);
        let edge_b = Vec3::from_array(p0) - Vec3::from_array(apex);
        let n = edge_a.cross(edge_b).normalize_or_zero().to_array();
        normals.extend_from_slice(&[n, n, n]);
        uvs.extend_from_slice(&[
            [(i as f32 + 0.5) / segs as f32, 1.0],
            [(i + 1) as f32 / segs as f32, 0.0],
            [i as f32 / segs as f32, 0.0],
        ]);
        indices.extend_from_slice(&[base_idx, base_idx + 1, base_idx + 2]);
    }
    // Base cap
    let center_idx = positions.len() as u32;
    positions.push([0.0, base_y, 0.0]);
    normals.push([0.0, -1.0, 0.0]);
    uvs.push([0.5, 0.5]);
    let ring_start = positions.len() as u32;
    for i in 0..=segs {
        let phi = (i as f32 / segs as f32) * TAU;
        positions.push([phi.cos() * radius, base_y, phi.sin() * radius]);
        normals.push([0.0, -1.0, 0.0]);
        uvs.push([phi.cos() * 0.5 + 0.5, phi.sin() * 0.5 + 0.5]);
    }
    for i in 0..segs as u32 {
        indices.extend_from_slice(&[center_idx, ring_start + i + 1, ring_start + i]);
    }
    MeshData {
        positions,
        normals: Some(normals),
        uvs: vec![uvs],
        colors: None,
        indices,
    }
}

/// Torus around the Y axis with major `radius` and tube `thickness`.
pub fn torus_mesh(
    radius: f32,
    thickness: f32,
    segments_major: u32,
    segments_minor: u32,
) -> MeshData {
    let smaj = segments_major.max(3) as usize;
    let smin = segments_minor.max(3) as usize;
    let mut positions = Vec::with_capacity((smaj + 1) * (smin + 1));
    let mut normals = Vec::with_capacity((smaj + 1) * (smin + 1));
    let mut uvs = Vec::with_capacity((smaj + 1) * (smin + 1));
    let mut indices = Vec::new();
    for i in 0..=smaj {
        let u = i as f32 / smaj as f32;
        let phi = u * TAU;
        let cos_p = phi.cos();
        let sin_p = phi.sin();
        for j in 0..=smin {
            let v = j as f32 / smin as f32;
            let theta = v * TAU;
            let cos_t = theta.cos();
            let sin_t = theta.sin();
            let nx = cos_t * cos_p;
            let nz = cos_t * sin_p;
            let ny = sin_t;
            positions.push([
                (radius + thickness * cos_t) * cos_p,
                thickness * sin_t,
                (radius + thickness * cos_t) * sin_p,
            ]);
            normals.push([nx, ny, nz]);
            uvs.push([u, v]);
        }
    }
    let stride = smin + 1;
    for i in 0..smaj {
        for j in 0..smin {
            let a = (i * stride + j) as u32;
            let b = (i * stride + j + 1) as u32;
            let c = ((i + 1) * stride + j + 1) as u32;
            let d = ((i + 1) * stride + j) as u32;
            indices.extend_from_slice(&[a, b, c, a, c, d]);
        }
    }
    MeshData {
        positions,
        normals: Some(normals),
        uvs: vec![uvs],
        colors: None,
        indices,
    }
}

/// Unit-extents quad in the XY plane facing +Z. Used for sprites/billboards.
pub fn sprite_quad(size_x: f32, size_y: f32) -> MeshData {
    let hx = size_x * 0.5;
    let hy = size_y * 0.5;
    MeshData {
        positions: vec![
            [-hx, -hy, 0.0],
            [hx, -hy, 0.0],
            [hx, hy, 0.0],
            [-hx, hy, 0.0],
        ],
        normals: Some(vec![[0.0, 0.0, 1.0]; 4]),
        uvs: vec![vec![[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]]],
        colors: None,
        indices: vec![0, 1, 2, 0, 2, 3],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_has_36_indices() {
        let m = box_mesh(Vec3::ONE);
        assert_eq!(m.indices.len(), 36);
        assert_eq!(m.positions.len(), 24);
    }

    #[test]
    fn sphere_invariants() {
        let m = sphere_mesh(1.0, 16, 8);
        // every vertex on unit sphere
        for p in &m.positions {
            let len2 = p[0] * p[0] + p[1] * p[1] + p[2] * p[2];
            assert!((len2 - 1.0).abs() < 1.0e-4);
        }
    }
}
