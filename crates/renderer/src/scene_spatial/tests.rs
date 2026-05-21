//! Unit tests for the spatial index wrapper. Tests run on the host
//! target — no `wasm-bindgen-test` needed because `SceneSpatial` is
//! pure CPU.

use glam::{Mat4, Vec3};
use slotmap::{DenseSlotMap, KeyData};

use crate::{
    bounds::Aabb,
    frustum::Frustum,
    meshes::MeshKey,
    scene_spatial::{
        node::{SceneNode, SceneNodeFlags},
        query::NodeFilter,
        SceneSpatial,
    },
};

fn fake_mesh_key(slotmap: &mut DenseSlotMap<MeshKey, ()>) -> MeshKey {
    slotmap.insert(())
}

fn node(mesh_key: MeshKey, min: Vec3, max: Vec3) -> SceneNode {
    SceneNode {
        aabb: Aabb { min, max },
        mesh_key,
        flags: SceneNodeFlags {
            cast_shadows: true,
            receive_shadows: true,
            hidden: false,
            hud: false,
            dynamic: false,
        },
    }
}

#[test]
fn insert_and_envelope_query_finds_node() {
    let mut keys: DenseSlotMap<MeshKey, ()> = DenseSlotMap::with_key();
    let mut spatial = SceneSpatial::default();

    let key = fake_mesh_key(&mut keys);
    spatial.insert(node(
        key,
        Vec3::new(-1.0, -1.0, -1.0),
        Vec3::new(1.0, 1.0, 1.0),
    ));

    let hits: Vec<_> = spatial
        .query_envelope(&Aabb {
            min: Vec3::splat(-2.0),
            max: Vec3::splat(2.0),
        })
        .map(|n| n.mesh_key)
        .collect();
    assert_eq!(hits, vec![key]);
}

#[test]
fn update_replaces_old_envelope() {
    let mut keys: DenseSlotMap<MeshKey, ()> = DenseSlotMap::with_key();
    let mut spatial = SceneSpatial::default();

    let key = fake_mesh_key(&mut keys);
    spatial.insert(node(
        key,
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(1.0, 1.0, 1.0),
    ));

    // Move the node out to 100 units. The old envelope at the origin
    // must NOT match anymore.
    spatial.update(
        key,
        Aabb {
            min: Vec3::new(100.0, 100.0, 100.0),
            max: Vec3::new(101.0, 101.0, 101.0),
        },
    );

    let origin_hits: Vec<_> = spatial
        .query_envelope(&Aabb {
            min: Vec3::splat(-2.0),
            max: Vec3::splat(2.0),
        })
        .collect();
    assert!(
        origin_hits.is_empty(),
        "old envelope still resident after update"
    );

    let far_hits: Vec<_> = spatial
        .query_envelope(&Aabb {
            min: Vec3::splat(99.0),
            max: Vec3::splat(102.0),
        })
        .map(|n| n.mesh_key)
        .collect();
    assert_eq!(far_hits, vec![key]);
}

#[test]
fn remove_evicts_the_leaf() {
    let mut keys: DenseSlotMap<MeshKey, ()> = DenseSlotMap::with_key();
    let mut spatial = SceneSpatial::default();
    let key = fake_mesh_key(&mut keys);
    spatial.insert(node(key, Vec3::splat(-1.0), Vec3::splat(1.0)));
    spatial.remove(key);
    assert_eq!(spatial.len(), 0);
    let hits: Vec<_> = spatial
        .query_envelope(&Aabb {
            min: Vec3::splat(-2.0),
            max: Vec3::splat(2.0),
        })
        .collect();
    assert!(hits.is_empty());
}

#[test]
fn set_dynamic_moves_node_between_tree_and_sidecar() {
    let mut keys: DenseSlotMap<MeshKey, ()> = DenseSlotMap::with_key();
    let mut spatial = SceneSpatial::default();
    let key = fake_mesh_key(&mut keys);
    spatial.insert(node(key, Vec3::splat(-1.0), Vec3::splat(1.0)));
    assert!(!spatial.is_dynamic(key));

    spatial.set_dynamic(key, true);
    assert!(spatial.is_dynamic(key));
    // Query still surfaces the node from the sidecar.
    let hits: Vec<_> = spatial
        .query_envelope(&Aabb {
            min: Vec3::splat(-2.0),
            max: Vec3::splat(2.0),
        })
        .map(|n| n.mesh_key)
        .collect();
    assert_eq!(hits, vec![key]);

    spatial.set_dynamic(key, false);
    assert!(!spatial.is_dynamic(key));
}

#[test]
fn frustum_query_parity_with_linear_scan() {
    // 100 random AABBs vs. a known view-projection. The R*-tree-
    // pruned set must equal the linear-scan set exactly.
    let mut keys: DenseSlotMap<MeshKey, ()> = DenseSlotMap::with_key();
    let mut spatial = SceneSpatial::default();

    let view = Mat4::look_at_rh(Vec3::new(0.0, 0.0, 30.0), Vec3::ZERO, Vec3::Y);
    let proj = Mat4::perspective_rh(45.0_f32.to_radians(), 16.0 / 9.0, 0.1, 100.0);
    let frustum = Frustum::from_view_projection(proj * view);

    let mut linear_hits = Vec::new();
    for i in 0..100 {
        // Deterministic pseudo-random positions across the test volume.
        let t = i as f32;
        let x = (t * 1.3).sin() * 25.0;
        let y = (t * 0.7).cos() * 25.0;
        let z = ((t * 0.4).sin() * 25.0) + 5.0;
        let half = 0.5 + (t * 0.11).fract();
        let min = Vec3::new(x - half, y - half, z - half);
        let max = Vec3::new(x + half, y + half, z + half);
        let key = fake_mesh_key(&mut keys);
        let aabb = Aabb { min, max };
        if frustum.intersects_aabb(&aabb) {
            linear_hits.push(key);
        }
        spatial.insert(node(key, min, max));
    }

    let mut tree_hits: Vec<_> = spatial
        .query_frustum(&frustum, NodeFilter::default())
        .map(|n| n.mesh_key)
        .collect();
    tree_hits.sort_by_key(slotmap::Key::data);
    let mut linear_sorted = linear_hits.clone();
    linear_sorted.sort_by_key(slotmap::Key::data);

    assert_eq!(
        tree_hits, linear_sorted,
        "BVH frustum query disagrees with linear scan"
    );
}

#[test]
fn flag_filter_excludes_hidden_and_hud() {
    let mut keys: DenseSlotMap<MeshKey, ()> = DenseSlotMap::with_key();
    let mut spatial = SceneSpatial::default();
    let key_a = fake_mesh_key(&mut keys);
    let key_b = fake_mesh_key(&mut keys);
    let key_c = fake_mesh_key(&mut keys);

    let node_a = node(key_a, Vec3::splat(-1.0), Vec3::splat(1.0));
    let mut node_b = node(key_b, Vec3::splat(-1.0), Vec3::splat(1.0));
    let mut node_c = node(key_c, Vec3::splat(-1.0), Vec3::splat(1.0));
    node_b.flags.hidden = true;
    node_c.flags.hud = true;

    spatial.insert(node_a);
    spatial.insert(node_b);
    spatial.insert(node_c);

    let view = Mat4::look_at_rh(Vec3::new(0.0, 0.0, 10.0), Vec3::ZERO, Vec3::Y);
    let proj = Mat4::perspective_rh(60.0_f32.to_radians(), 1.0, 0.1, 100.0);
    let frustum = Frustum::from_view_projection(proj * view);

    let camera_hits: Vec<_> = spatial
        .query_frustum(&frustum, NodeFilter::camera_default())
        .map(|n| n.mesh_key)
        .collect();
    assert!(camera_hits.contains(&key_a));
    assert!(!camera_hits.contains(&key_b), "hidden node leaked through");
    assert!(camera_hits.contains(&key_c), "hud was excluded for camera");

    let shadow_hits: Vec<_> = spatial
        .query_frustum(&frustum, NodeFilter::shadow_caster())
        .map(|n| n.mesh_key)
        .collect();
    assert!(shadow_hits.contains(&key_a));
    assert!(!shadow_hits.contains(&key_b));
    assert!(!shadow_hits.contains(&key_c));
}

#[test]
fn cube_face_frustum_prunes_other_face_geometry() {
    // Step 1.8 verification: a 90° per-face cube frustum naturally
    // excludes geometry that sits along the OTHER face axes. Place
    // six clearly-separated AABBs at ±X, ±Y, ±Z; the +X face frustum
    // should return the +X box only.
    let mut keys: DenseSlotMap<MeshKey, ()> = DenseSlotMap::with_key();
    let mut spatial = SceneSpatial::default();

    let mut placed = Vec::new();
    for (axis, label) in [
        (Vec3::new(10.0, 0.0, 0.0), "+x"),
        (Vec3::new(-10.0, 0.0, 0.0), "-x"),
        (Vec3::new(0.0, 10.0, 0.0), "+y"),
        (Vec3::new(0.0, -10.0, 0.0), "-y"),
        (Vec3::new(0.0, 0.0, 10.0), "+z"),
        (Vec3::new(0.0, 0.0, -10.0), "-z"),
    ] {
        let key = fake_mesh_key(&mut keys);
        spatial.insert(node(key, axis - Vec3::splat(0.5), axis + Vec3::splat(0.5)));
        placed.push((key, label));
    }

    // Camera at origin looking at +X with 90° aspect-1 perspective.
    let view = Mat4::look_at_rh(Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0), Vec3::Y);
    let proj = Mat4::perspective_rh(90.0_f32.to_radians(), 1.0, 0.1, 100.0);
    let frustum = Frustum::from_view_projection(proj * view);

    let hits: Vec<_> = spatial
        .query_frustum(&frustum, NodeFilter::default())
        .map(|n| n.mesh_key)
        .collect();

    let plus_x_key = placed[0].0;
    assert!(hits.contains(&plus_x_key), "+x box should be in +x frustum");
    for (key, label) in &placed[1..] {
        assert!(
            !hits.contains(key),
            "{label} box leaked through +x frustum query"
        );
    }
}

#[test]
fn rebuild_if_needed_preserves_query_results() {
    let mut keys: DenseSlotMap<MeshKey, ()> = DenseSlotMap::with_key();
    let mut spatial = SceneSpatial::default();
    let mut inserted_keys = Vec::new();
    for i in 0..10 {
        let key = fake_mesh_key(&mut keys);
        let p = i as f32 * 2.0;
        spatial.insert(node(
            key,
            Vec3::new(p, p, p),
            Vec3::new(p + 1.0, p + 1.0, p + 1.0),
        ));
        inserted_keys.push(key);
    }
    spatial.mark_rebuild_needed();
    spatial.rebuild_if_needed();
    let hits: Vec<_> = spatial.iter_all().map(|n| n.mesh_key).collect();
    assert_eq!(hits.len(), inserted_keys.len());
}

// `KeyData` is referenced only to satisfy `MeshKey: Copy + Eq + Hash`
// expectation in the rstar-stored leaf data. Compile-checked here.
const _: fn() = || {
    fn assert_copy<T: Copy>() {}
    fn assert_eq_hash<T: Eq + std::hash::Hash>() {}
    assert_copy::<MeshKey>();
    assert_eq_hash::<MeshKey>();
    let _ = KeyData::default();
};
