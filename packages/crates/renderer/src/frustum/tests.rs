use glam::{Mat4, Vec3};

use crate::bounds::Aabb;
use crate::frustum::Frustum;
use crate::transforms::Transform;

fn instance_union_aabb(base: &Aabb, base_world: Mat4, instances: &[Transform]) -> Aabb {
    let first = base_world * instances[0].to_matrix();
    let mut combined = base.transformed(&first);
    for transform in &instances[1..] {
        let world = base_world * transform.to_matrix();
        let transformed = base.transformed(&world);
        combined.extend(&transformed);
    }
    combined
}

#[test]
fn perspective_frustum_culls_non_instanced() {
    let projection = Mat4::perspective_rh(90.0_f32.to_radians(), 1.0, 1.0, 10.0);
    let view = Mat4::IDENTITY;
    let frustum = Frustum::from_view_projection(projection * view, false);

    let base = Aabb::new_cube(1.0, 1.0);
    let inside = base.transformed(&Mat4::from_translation(Vec3::new(0.0, 0.0, -5.0)));
    let outside = base.transformed(&Mat4::from_translation(Vec3::new(0.0, 0.0, 5.0)));

    assert!(frustum.intersects_aabb(&inside));
    assert!(!frustum.intersects_aabb(&outside));
}

#[test]
fn perspective_frustum_culls_instanced_union() {
    let projection = Mat4::perspective_rh(60.0_f32.to_radians(), 1.0, 1.0, 20.0);
    let view = Mat4::IDENTITY;
    let frustum = Frustum::from_view_projection(projection * view, false);

    let base = Aabb::new_cube(1.0, 1.0);
    let base_world = Mat4::from_translation(Vec3::new(0.0, 0.0, -5.0));
    let instances = vec![
        Transform::IDENTITY,
        Transform::IDENTITY.with_translation(Vec3::new(100.0, 0.0, 0.0)),
    ];
    let union_inside = instance_union_aabb(&base, base_world, &instances);
    assert!(frustum.intersects_aabb(&union_inside));

    let base_world_far = Mat4::from_translation(Vec3::new(0.0, 0.0, 5.0));
    let union_outside = instance_union_aabb(&base, base_world_far, &instances);
    assert!(!frustum.intersects_aabb(&union_outside));
}

#[test]
fn orthographic_frustum_culls_non_instanced() {
    let projection = Mat4::orthographic_rh(-2.0, 2.0, -2.0, 2.0, 1.0, 10.0);
    let view = Mat4::IDENTITY;
    let frustum = Frustum::from_view_projection(projection * view, false);

    let base = Aabb::new_cube(1.0, 1.0);
    let inside = base.transformed(&Mat4::from_translation(Vec3::new(0.0, 0.0, -5.0)));
    let outside = base.transformed(&Mat4::from_translation(Vec3::new(3.0, 0.0, -5.0)));

    assert!(frustum.intersects_aabb(&inside));
    assert!(!frustum.intersects_aabb(&outside));
}

// An AABB that STRADDLES a frustum plane (part inside, part outside) must be
// KEPT — conservative culling. This is the classic p-vertex-test edge case; a
// wrong sign here would wrongly cull objects clipping the screen edge.
#[test]
fn straddling_aabb_is_kept() {
    let projection = Mat4::perspective_rh(90.0_f32.to_radians(), 1.0, 1.0, 10.0);
    let frustum = Frustum::from_view_projection(projection, false);

    // Centered ~5 units deep (well inside near/far). At z=-5 the right plane is
    // ~x=5 (90° fov); this box spans x∈[3,8], so it crosses the right plane.
    let straddling = Aabb::new(Vec3::new(3.0, -1.0, -6.0), Vec3::new(8.0, 1.0, -4.0));
    assert!(
        frustum.intersects_aabb(&straddling),
        "an AABB crossing a frustum plane must be kept"
    );
}

// A huge AABB that ENCLOSES the whole frustum (camera inside the object — e.g. a
// skybox shell, or standing inside a large imported model) must be KEPT.
#[test]
fn enclosing_aabb_is_kept() {
    let projection = Mat4::perspective_rh(90.0_f32.to_radians(), 1.0, 1.0, 10.0);
    let frustum = Frustum::from_view_projection(projection, false);

    let enclosing = Aabb::new(
        Vec3::new(-1000.0, -1000.0, -1000.0),
        Vec3::new(1000.0, 1000.0, 1000.0),
    );
    assert!(
        frustum.intersects_aabb(&enclosing),
        "an AABB enclosing the frustum (camera inside) must be kept"
    );
}

// An AABB entirely beyond the FAR plane must be culled (exercises the far plane
// specifically, distinct from the near/side-plane cases above).
#[test]
fn beyond_far_plane_is_culled() {
    let projection = Mat4::perspective_rh(90.0_f32.to_radians(), 1.0, 1.0, 10.0);
    let frustum = Frustum::from_view_projection(projection, false);

    // far = 10; this box sits ~50 units deep, well past it.
    let beyond_far = Aabb::new(Vec3::new(-1.0, -1.0, -51.0), Vec3::new(1.0, 1.0, -49.0));
    assert!(
        !frustum.intersects_aabb(&beyond_far),
        "an AABB beyond the far plane must be culled"
    );
}

#[test]
fn orthographic_frustum_culls_instanced_union() {
    let projection = Mat4::orthographic_rh(-2.0, 2.0, -2.0, 2.0, 1.0, 10.0);
    let view = Mat4::IDENTITY;
    let frustum = Frustum::from_view_projection(projection * view, false);

    let base = Aabb::new_cube(1.0, 1.0);
    let base_world = Mat4::from_translation(Vec3::new(0.0, 0.0, -5.0));
    let instances = vec![
        Transform::IDENTITY,
        Transform::IDENTITY.with_translation(Vec3::new(0.0, 5.0, 0.0)),
    ];
    let union_inside = instance_union_aabb(&base, base_world, &instances);
    assert!(frustum.intersects_aabb(&union_inside));

    let base_world_far = Mat4::from_translation(Vec3::new(0.0, 0.0, -5.0));
    let outside_instances = vec![
        Transform::IDENTITY.with_translation(Vec3::new(5.0, 0.0, 0.0)),
        Transform::IDENTITY.with_translation(Vec3::new(6.0, 0.0, 0.0)),
    ];
    let union_outside = instance_union_aabb(&base, base_world_far, &outside_instances);
    assert!(!frustum.intersects_aabb(&union_outside));
}

/// 003: forward-Z and reverse-Z projections of the SAME camera must extract
/// the SAME six world-space planes — the near/far halfspaces just live in
/// different matrix rows. A mismatch here means the convention-dependent row
/// swap in `from_view_projection` is wrong.
#[test]
fn reverse_z_extraction_matches_forward_world_planes() {
    use crate::depth_convention::DepthConvention;
    let f = DepthConvention { reverse_z: false };
    let r = DepthConvention { reverse_z: true };
    let view = Mat4::look_at_rh(
        glam::Vec3::new(3.0, 4.0, 5.0),
        glam::Vec3::ZERO,
        glam::Vec3::Y,
    );
    let (fov, aspect, near, far) = (60.0_f32.to_radians(), 16.0 / 9.0, 0.3, 250.0);
    let fwd = Frustum::from_view_projection(f.perspective(fov, aspect, near, far) * view, false);
    let rev = Frustum::from_view_projection(r.perspective(fov, aspect, near, far) * view, true);
    // The reverse projection is INFINITE-far: it has no far plane. Plane 5
    // must be the explicit always-pass sentinel; compare only planes 0-4.
    assert_eq!(rev.planes[5].normal, glam::Vec3::ZERO);
    assert!(rev.planes[5].d > 0.0, "infinite far plane must never cull");
    for (i, (pf, pr)) in fwd.planes.iter().zip(rev.planes.iter()).enumerate().take(5) {
        let n_dot = pf.normal.normalize().dot(pr.normal.normalize());
        assert!(
            n_dot > 0.9999,
            "plane {i} normal mismatch: {:?} vs {:?}",
            pf.normal,
            pr.normal
        );
        // Compare the plane's distance-from-origin along its (normalized) normal.
        let df = pf.d / pf.normal.length();
        let dr = pr.d / pr.normal.length();
        // Relative tolerance: the far plane's offset is O(far) and the two
        // conventions round differently through f32 matrix products (that
        // precision redistribution is reverse-Z's entire point).
        let tol = (df.abs() * 1e-4).max(1e-3);
        assert!(
            (df - dr).abs() < tol,
            "plane {i} offset mismatch: {df} vs {dr}"
        );
    }
}
