//! MuJoCo's primitive geom shapes as meshes.
//!
//! Ported from the Phase-0 `physics-mujoco` template, which worked these out
//! against the live wasm build. Everything here is stated in MuJoCo's own terms:
//! **Z-up, metres, and cylinders/capsules along local Z** (our `meshgen`
//! primitives run along Y, so those get rotated). The geom node then sits under
//! the instance root's single Z-up→Y-up rotation like every other geom.
//!
//! All of these become *captured* meshes rather than `PrimitiveShape` recipes.
//! Two of MuJoCo's shapes — capsule and ellipsoid — have no `PrimitiveShape`
//! equivalent at all, and sim-bound geometry is stream-owned and not meant to be
//! edited, so one uniform path beats a recipe/capture split that would only make
//! some geoms editable.

use awsm_renderer_editor_protocol::mujoco::GeomKind;
use awsm_renderer_meshgen::MeshData;
use glam::Vec3;

/// What a geom's node needs beyond its pose: the geometry, and a scale for the
/// one shape that is expressed as a scaled unit primitive.
pub struct Primitive {
    pub mesh: MeshData,
    /// Node scale. `ONE` for everything except an ellipsoid, which is a unit
    /// sphere stretched per-axis. Safe on a sim-bound node: a pose frame writes
    /// translation and rotation only, so the scale survives every update.
    pub scale: [f32; 3],
    /// A stable label, also used to dedupe identical shapes into one asset.
    pub label: String,
}

/// Build the mesh for a primitive geom, or `None` for a kind that is not a
/// primitive (`Mesh`, `Hfield`, `Sdf` — those have their own paths).
///
/// `size` is MuJoCo's raw three-slot `geom_size`; what each slot means depends
/// on the kind, which is the one thing about MuJoCo primitives that cannot be
/// guessed. See [`GeomKind`] for the per-variant meaning.
pub fn build(kind: GeomKind, size: [f64; 3]) -> Option<Primitive> {
    let s = [size[0] as f32, size[1] as f32, size[2] as f32];
    let (mesh, scale, label) = match kind {
        // size = [x half-extent, y half-extent, grid spacing]. A 0 half-extent
        // means INFINITE in that axis, which we cannot draw — fall back to a
        // large finite quad, the same thing MuJoCo's own viewer does. Built in
        // XZ then rotated, because a MuJoCo plane's normal is +Z.
        GeomKind::Plane => {
            let half_x = if s[0] > 0.0 { s[0] } else { DEFAULT_PLANE_HALF };
            let half_y = if s[1] > 0.0 { s[1] } else { DEFAULT_PLANE_HALF };
            (
                rotate_y_to_z(awsm_renderer_meshgen::plane_mesh(
                    half_x * 2.0,
                    half_y * 2.0,
                    1,
                    1,
                )),
                Vec3::ONE,
                format!("plane {half_x}x{half_y}"),
            )
        }
        // size = [radius, _, _].
        GeomKind::Sphere => (
            awsm_renderer_meshgen::sphere_mesh(s[0], 32, 16),
            Vec3::ONE,
            format!("sphere r{}", s[0]),
        ),
        // size = [radius, half-length]; hemispherical caps, along local Z.
        GeomKind::Capsule => (
            capsule_mesh_z(s[0], s[1]),
            Vec3::ONE,
            format!("capsule r{} h{}", s[0], s[1]),
        ),
        // size = the three semi-axes: a unit sphere scaled per-axis.
        GeomKind::Ellipsoid => (
            awsm_renderer_meshgen::sphere_mesh(1.0, 32, 16),
            Vec3::new(s[0], s[1], s[2]),
            "ellipsoid".to_string(),
        ),
        // size = [radius, half-length], along local Z (meshgen's runs along Y).
        GeomKind::Cylinder => (
            rotate_y_to_z(awsm_renderer_meshgen::cylinder_mesh(s[0], s[1] * 2.0, 32)),
            Vec3::ONE,
            format!("cylinder r{} h{}", s[0], s[1]),
        ),
        // size = the three HALF-extents, so the box's full dimensions are 2x.
        GeomKind::Box => (
            awsm_renderer_meshgen::box_mesh(Vec3::new(s[0] * 2.0, s[1] * 2.0, s[2] * 2.0)),
            Vec3::ONE,
            format!("box {}x{}x{}", s[0], s[1], s[2]),
        ),
        GeomKind::Mesh | GeomKind::Hfield | GeomKind::Sdf => return None,
    };
    Some(Primitive {
        mesh,
        scale: scale.to_array(),
        label,
    })
}

/// Half-size for a MuJoCo plane authored as infinite. Large enough to read as
/// ground in an editor viewport without pushing the scene bounds (and the
/// auto-framing that follows them) somewhere useless.
const DEFAULT_PLANE_HALF: f32 = 10.0;

/// A capsule with its long axis along **Z** (MuJoCo's convention): two
/// hemisphere caps of `radius` around a cylindrical wall of half-length
/// `half_len`.
///
/// Built as a lat-long sphere split at the equator with the two halves pushed
/// apart — the equator pair forms the wall, and the sphere normals at the split
/// are exactly the wall's radial normals, so no seam shading. Same layout and
/// winding as `meshgen::sphere_mesh` (built along Y, then rotated Y→Z).
fn capsule_mesh_z(radius: f32, half_len: f32) -> MeshData {
    use std::f32::consts::{PI, TAU};
    let radial = 24usize;
    let half_rings = 8usize;
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut uvs = Vec::new();
    let mut indices = Vec::new();

    let mut ring = |theta: f32, offset: f32, v: f32| {
        let (sin_t, cos_t) = theta.sin_cos();
        for lon in 0..=radial {
            let u = lon as f32 / radial as f32;
            let phi = u * TAU;
            let (sin_p, cos_p) = phi.sin_cos();
            let n = [sin_t * cos_p, cos_t, sin_t * sin_p];
            positions.push([n[0] * radius, n[1] * radius + offset, n[2] * radius]);
            normals.push(n);
            uvs.push([u, v]);
        }
    };
    // Top hemisphere (offset +half_len), then bottom (−half_len). The two
    // consecutive equator rings — same theta, different offset — are the wall.
    let total_rings = half_rings * 2 + 1;
    let mut vi = 0.0;
    for i in 0..=half_rings {
        ring(
            PI * 0.5 * i as f32 / half_rings as f32,
            half_len,
            vi / total_rings as f32,
        );
        vi += 1.0;
    }
    for i in 0..=half_rings {
        ring(
            PI * 0.5 + PI * 0.5 * i as f32 / half_rings as f32,
            -half_len,
            vi / total_rings as f32,
        );
        vi += 1.0;
    }

    let stride = radial + 1;
    for lat in 0..total_rings {
        for lon in 0..radial {
            let a = (lat * stride + lon) as u32;
            let b = (lat * stride + lon + 1) as u32;
            let c = ((lat + 1) * stride + lon + 1) as u32;
            let d = ((lat + 1) * stride + lon) as u32;
            indices.extend_from_slice(&[a, b, c, a, c, d]);
        }
    }

    rotate_y_to_z(MeshData {
        positions,
        normals: Some(normals),
        uvs: vec![uvs],
        colors: None,
        indices,
    })
}

/// Rotate a mesh +90° about X so its +Y axis becomes +Z: `(x, y, z) → (x, −z, y)`.
///
/// A proper rotation, so winding and normals stay valid — which is why this is a
/// coordinate swap with a sign rather than a bare axis shuffle.
fn rotate_y_to_z(mut mesh: MeshData) -> MeshData {
    for p in &mut mesh.positions {
        *p = [p[0], -p[2], p[1]];
    }
    if let Some(normals) = &mut mesh.normals {
        for n in normals {
            *n = [n[0], -n[2], n[1]];
        }
    }
    mesh
}

/// A **unit** cylinder along local Z: radius 1, half-length 1.
///
/// The tendon-segment mesh. Segments change length every frame as a tendon
/// wraps, so the length lives in the node's Z scale rather than the geometry —
/// a cylinder is the one shape that scales along its axis without distorting,
/// which is why segments are cylinders and not the capsules MuJoCo draws. At
/// tendon widths (millimetres) the missing end-caps are not visible.
pub fn unit_cylinder_z() -> MeshData {
    rotate_y_to_z(awsm_renderer_meshgen::cylinder_mesh(1.0, 2.0, 16))
}
