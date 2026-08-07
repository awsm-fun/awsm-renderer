//! Record a real simulation and bake it, end to end.
//!
//! The unit tests in `awsm-renderer-scene` cover the bake's rules against
//! synthetic captures; this one checks the two halves actually fit, on data a
//! real integrator produced.

use std::collections::HashMap;

use awsm_renderer_scene::mujoco::bake::{bake, Reduction};
use awsm_renderer_scene::{NodeId, TrackTarget, TrackValue, TransformProp};

#[path = "record_helper.rs"]
mod record_helper;
use record_helper::record;

/// The torso's baked translation track must still describe the fall the
/// simulation produced — reduction may drop keys, never the motion.
#[test]
fn the_baked_clip_still_falls() {
    let Some(c) = record("humanoid/humanoid.xml", 3.0, 60.0) else {
        return;
    };
    c.validate().unwrap();

    let torso = NodeId::new();
    let clip = bake(
        &c,
        &HashMap::from([(1u32, torso)]),
        "fall",
        Reduction::default(),
    )
    .unwrap();

    let track = clip
        .tracks
        .iter()
        .find(|t| {
            matches!(
                t.target,
                TrackTarget::Transform {
                    prop: TransformProp::Translation,
                    ..
                }
            )
        })
        .expect("the torso moved, so it must have a translation track");

    let z = |i: usize| match track.keys[i].value {
        TrackValue::Vec3(v) => v[2],
        _ => panic!("expected a vec3"),
    };
    assert!(z(0) > 1.0, "starts standing, got {}", z(0));
    assert!(
        z(track.keys.len() - 1) < 0.5,
        "ends on the floor, got {}",
        z(track.keys.len() - 1)
    );
    assert!((clip.duration - 3.0).abs() < 0.05, "{}", clip.duration);
    assert_eq!(track.times.len(), track.keys.len());
}

/// Reduction has to actually pay for itself on real data — physics is smooth,
/// so most sampled frames sit on the line between their neighbours.
#[test]
fn reduction_removes_most_keys_of_a_real_run() {
    let Some(c) = record("humanoid/humanoid.xml", 3.0, 60.0) else {
        return;
    };
    let binding: HashMap<u32, NodeId> = (0..c.geom_count).map(|g| (g, NodeId::new())).collect();

    let full = bake(&c, &binding, "full", Reduction::NONE).unwrap();
    let reduced = bake(&c, &binding, "reduced", Reduction::default()).unwrap();

    let keys = |clip: &awsm_renderer_scene::StoredAnimation| -> usize {
        clip.tracks.iter().map(|t| t.keys.len()).sum()
    };
    let (before, after) = (keys(&full), keys(&reduced));
    assert!(before > 0);
    eprintln!(
        "reduction: {after} of {before} keys kept ({:.0}%)",
        100.0 * after as f32 / before as f32
    );
    assert!(
        after < before / 2,
        "reduction kept {after} of {before} keys — not paying for itself"
    );

    // The humanoid's floor plane never moves, so it must contribute no track at
    // all: 20 geoms, fewer than 40 tracks.
    assert!(
        reduced.tracks.len() < 40,
        "{} tracks for 20 geoms — static geoms are being keyed",
        reduced.tracks.len()
    );
}

/// A clip baked from a capture is an ordinary clip: it round-trips through the
/// same TOML the project and the bundle are written in.
#[test]
fn the_baked_clip_round_trips_as_ordinary_animation_data() {
    let Some(c) = record("humanoid/humanoid.xml", 0.5, 30.0) else {
        return;
    };
    let binding: HashMap<u32, NodeId> = (0..c.geom_count).map(|g| (g, NodeId::new())).collect();
    let clip = bake(&c, &binding, "fall", Reduction::default()).unwrap();

    let toml_s = toml::to_string(&clip).unwrap();
    let back: awsm_renderer_scene::StoredAnimation = toml::from_str(&toml_s).unwrap();
    assert_eq!(back, clip);
}
