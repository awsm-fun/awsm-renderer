//! Unit tests for the spatial index wrapper. Tests run on the host
//! target — no `wasm-bindgen-test` needed because `SceneSpatial` is
//! pure CPU.
//!
//! The load-bearing test is the randomized oracle at the bottom: every
//! query kind, after every mutation kind, must match a dumb linear scan
//! exactly. Spatial-structure bugs don't crash — they make one object
//! pop at one camera angle — so parity-with-linear is the referee.

use glam::{Mat4, Vec3};
use slotmap::DenseSlotMap;

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

    // Move the node out to 100 units — far past the fattening margin.
    // The old envelope at the origin must NOT match anymore.
    spatial.update(
        key,
        Aabb {
            min: Vec3::new(100.0, 100.0, 100.0),
            max: Vec3::new(101.0, 101.0, 101.0),
        },
    );
    spatial.maintain();

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
    spatial.maintain();
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
fn leaf_slot_reuse_after_remove_stays_consistent() {
    // Remove + insert cycles reuse BVH leaf slots; a stale slot→key
    // mapping would surface the WRONG mesh from a query.
    let mut keys: DenseSlotMap<MeshKey, ()> = DenseSlotMap::with_key();
    let mut spatial = SceneSpatial::default();

    let key_a = fake_mesh_key(&mut keys);
    spatial.insert(node(key_a, Vec3::splat(-1.0), Vec3::splat(1.0)));
    spatial.remove(key_a);

    let key_b = fake_mesh_key(&mut keys);
    spatial.insert(node(key_b, Vec3::splat(10.0), Vec3::splat(11.0)));
    spatial.maintain();

    let hits: Vec<_> = spatial
        .query_envelope(&Aabb {
            min: Vec3::splat(9.0),
            max: Vec3::splat(12.0),
        })
        .map(|n| n.mesh_key)
        .collect();
    assert_eq!(hits, vec![key_b]);
    assert!(spatial.get(key_a).is_none());
}

#[test]
fn frustum_query_parity_with_linear_scan() {
    // 100 deterministic AABBs vs. a known view-projection. The BVH-
    // pruned set must equal the linear-scan set exactly.
    let mut keys: DenseSlotMap<MeshKey, ()> = DenseSlotMap::with_key();
    let mut spatial = SceneSpatial::default();

    let view = Mat4::look_at_rh(Vec3::new(0.0, 0.0, 30.0), Vec3::ZERO, Vec3::Y);
    let proj = Mat4::perspective_rh(45.0_f32.to_radians(), 16.0 / 9.0, 0.1, 100.0);
    let frustum = Frustum::from_view_projection(proj * view);

    let mut linear_hits = Vec::new();
    for i in 0..100 {
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
    // A 90° per-face cube frustum naturally excludes geometry that sits
    // along the OTHER face axes. Place six clearly-separated AABBs at
    // ±X, ±Y, ±Z; the +X face frustum should return the +X box only.
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
fn mark_rebuild_preserves_query_results() {
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
    spatial.maintain();
    assert_eq!(spatial.iter_all().count(), inserted_keys.len());
    // Every inserted box is still findable through the rebuilt tree.
    for (i, key) in inserted_keys.iter().enumerate() {
        let p = i as f32 * 2.0;
        let hits: Vec<_> = spatial
            .query_envelope(&Aabb {
                min: Vec3::splat(p - 0.1),
                max: Vec3::splat(p + 1.1),
            })
            .map(|n| n.mesh_key)
            .collect();
        assert!(hits.contains(key), "box {i} lost across full rebuild");
    }
}

/// Deterministic xorshift (no `rand` dep; reproducible failures).
struct Rng(u64);
impl Rng {
    fn next_f32(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1 << 24) as f32
    }
    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + self.next_f32() * (hi - lo)
    }
    fn chance(&mut self, p: f32) -> bool {
        self.next_f32() < p
    }
}

/// The oracle: hundreds of randomized mutation rounds (insert, small
/// drift, teleport, remove, flag flips, occasional full-rebuild), each
/// followed by `maintain()` and every query kind compared against a
/// dumb linear scan over the exact mirror. Any divergence — a stale
/// fattened bound, a bad slot reuse, a refit miss — fails here with the
/// deterministic seed to reproduce.
#[test]
fn randomized_mutations_match_linear_scan_oracle() {
    let mut keys: DenseSlotMap<MeshKey, ()> = DenseSlotMap::with_key();
    let mut spatial = SceneSpatial::default();
    let mut rng = Rng(0x5EED_5EED_5EED_5EED);
    let mut live: Vec<MeshKey> = Vec::new();

    let random_box = |rng: &mut Rng| {
        let c = Vec3::new(
            rng.range(-60.0, 60.0),
            rng.range(-10.0, 30.0),
            rng.range(-60.0, 60.0),
        );
        let h = Vec3::new(
            rng.range(0.2, 3.0),
            rng.range(0.2, 3.0),
            rng.range(0.2, 3.0),
        );
        Aabb {
            min: c - h,
            max: c + h,
        }
    };

    let view = Mat4::look_at_rh(Vec3::new(0.0, 15.0, 70.0), Vec3::ZERO, Vec3::Y);
    let proj = Mat4::perspective_rh(50.0_f32.to_radians(), 16.0 / 9.0, 0.1, 200.0);
    let frustum = Frustum::from_view_projection(proj * view);

    for round in 0..300 {
        // ── mutate ──
        for _ in 0..8 {
            let roll = rng.next_f32();
            if roll < 0.35 || live.is_empty() {
                // Insert.
                let key = fake_mesh_key(&mut keys);
                let aabb = random_box(&mut rng);
                let mut n = node(key, aabb.min, aabb.max);
                n.flags.hidden = rng.chance(0.1);
                n.flags.hud = rng.chance(0.05);
                n.flags.cast_shadows = !rng.chance(0.15);
                spatial.insert(n);
                live.push(key);
            } else if roll < 0.75 {
                // Move: small drift (inside the margin sometimes) or teleport.
                let key = live[(rng.next_f32() * live.len() as f32) as usize % live.len()];
                let aabb = if rng.chance(0.3) {
                    random_box(&mut rng) // teleport
                } else {
                    let cur = spatial.get(key).unwrap().aabb.clone();
                    let d = Vec3::new(
                        rng.range(-0.2, 0.2),
                        rng.range(-0.2, 0.2),
                        rng.range(-0.2, 0.2),
                    );
                    Aabb {
                        min: cur.min + d,
                        max: cur.max + d,
                    }
                };
                spatial.update(key, aabb);
            } else if roll < 0.9 {
                // Remove.
                let idx = (rng.next_f32() * live.len() as f32) as usize % live.len();
                let key = live.swap_remove(idx);
                spatial.remove(key);
            } else {
                // Flag flip.
                let key = live[(rng.next_f32() * live.len() as f32) as usize % live.len()];
                let mut flags = spatial.get(key).unwrap().flags;
                flags.hidden = rng.chance(0.5);
                spatial.set_flags(key, flags);
            }
        }
        if round % 37 == 0 {
            spatial.mark_rebuild_needed();
        }
        spatial.maintain();

        assert_eq!(spatial.len(), live.len(), "leaf count diverged");

        // ── verify: every query kind vs the linear oracle ──
        let envelope = random_box(&mut rng);
        let mut tree: Vec<_> = spatial
            .query_envelope(&envelope)
            .map(|n| n.mesh_key)
            .collect();
        let mut linear: Vec<_> = spatial
            .iter_all()
            .filter(|n| {
                envelope.min.x <= n.aabb.max.x
                    && envelope.max.x >= n.aabb.min.x
                    && envelope.min.y <= n.aabb.max.y
                    && envelope.max.y >= n.aabb.min.y
                    && envelope.min.z <= n.aabb.max.z
                    && envelope.max.z >= n.aabb.min.z
            })
            .map(|n| n.mesh_key)
            .collect();
        tree.sort_by_key(slotmap::Key::data);
        linear.sort_by_key(slotmap::Key::data);
        assert_eq!(tree, linear, "envelope query diverged at round {round}");

        for filter in [
            NodeFilter::default(),
            NodeFilter::camera_default(),
            NodeFilter::shadow_caster(),
        ] {
            let mut tree: Vec<_> = spatial
                .query_frustum(&frustum, filter)
                .map(|n| n.mesh_key)
                .collect();
            let mut linear: Vec<_> = spatial
                .iter_all()
                .filter(|n| frustum.intersects_aabb(&n.aabb) && filter.matches(n))
                .map(|n| n.mesh_key)
                .collect();
            tree.sort_by_key(slotmap::Key::data);
            linear.sort_by_key(slotmap::Key::data);
            assert_eq!(tree, linear, "frustum query diverged at round {round}");
        }
    }
}
