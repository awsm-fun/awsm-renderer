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
        &HashMap::new(),
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

    let full = bake(&c, &binding, &HashMap::new(), "full", Reduction::NONE).unwrap();
    let reduced = bake(
        &c,
        &binding,
        &HashMap::new(),
        "reduced",
        Reduction::default(),
    )
    .unwrap();

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
    let clip = bake(&c, &binding, &HashMap::new(), "fall", Reduction::default()).unwrap();

    let toml_s = toml::to_string(&clip).unwrap();
    let back: awsm_renderer_scene::StoredAnimation = toml::from_str(&toml_s).unwrap();
    assert_eq!(back, clip);
}

/// A recorded FLEX bakes into a clip that deforms it.
///
/// The end-to-end version of the body channel: a real simulation of MuJoCo's
/// flag, recorded with its 173 body frames, baked against a body binding. What
/// comes out is an ordinary animation clip — no MuJoCo, no deformable, no
/// per-vertex anything — whose tracks drive the flex's skin joints.
#[test]
fn a_recorded_flex_bakes_into_a_deforming_clip() {
    let Some(c) = record("flex/flag.xml", 2.0, 30.0) else {
        return;
    };
    c.validate().unwrap();
    assert!(
        c.body_count > 0,
        "a flex model must record a body channel — without it nothing can \
         replay the deformation"
    );

    // One node per body, as the importer mints for a flex's skin joints.
    let nodes: Vec<NodeId> = (0..c.body_count).map(|_| NodeId::new()).collect();
    let body_binding: HashMap<u32, Vec<NodeId>> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (i as u32, vec![*n]))
        .collect();

    let clip = bake(
        &c,
        &HashMap::new(),
        &body_binding,
        "wave",
        Reduction::default(),
    )
    .unwrap();

    let keyed: std::collections::HashSet<NodeId> = clip
        .tracks
        .iter()
        .filter_map(|t| match t.target {
            TrackTarget::Transform { node, .. } => Some(node),
            _ => None,
        })
        .collect();

    // The flag is pinned at one edge and flaps: most of it moves, and the
    // world body (id 0) never does.
    assert!(
        !keyed.contains(&nodes[0]),
        "body 0 is the world body and never moves, so it must not be keyed"
    );
    assert!(
        keyed.len() > c.body_count as usize / 2,
        "most of a flapping flag's bodies should be keyed, got {} of {}",
        keyed.len(),
        c.body_count
    );

    // And the motion is real, not a flat pair of keys.
    let moving = clip
        .tracks
        .iter()
        .find(|t| {
            matches!(
                t.target,
                TrackTarget::Transform {
                    prop: TransformProp::Translation,
                    ..
                }
            ) && t.keys.len() > 2
        })
        .expect("some body must carry a multi-key translation track");
    let extent = |axis: usize| {
        let vs: Vec<f32> = moving
            .keys
            .iter()
            .filter_map(|k| match k.value {
                TrackValue::Vec3(v) => Some(v[axis]),
                _ => None,
            })
            .collect();
        vs.iter().cloned().fold(f32::MIN, f32::max) - vs.iter().cloned().fold(f32::MAX, f32::min)
    };
    let travelled = (0..3).map(extent).fold(0.0f32, f32::max);
    println!(
        "flag: {} of {} bodies keyed; largest single-axis travel {:.3} m",
        keyed.len(),
        c.body_count,
        travelled
    );
    assert!(
        travelled > 0.01,
        "the flag must visibly move, got {travelled} m"
    );
}
