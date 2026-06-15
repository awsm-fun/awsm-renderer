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
    let frustum = Frustum::from_view_projection(projection * view);

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
    let frustum = Frustum::from_view_projection(projection * view);

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
    let frustum = Frustum::from_view_projection(projection * view);

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
    let frustum = Frustum::from_view_projection(projection);

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
    let frustum = Frustum::from_view_projection(projection);

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
    let frustum = Frustum::from_view_projection(projection);

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
    let frustum = Frustum::from_view_projection(projection * view);

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
