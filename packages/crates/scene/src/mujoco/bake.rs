//! Baking a recorded capture into an ordinary animation clip.
//!
//! After this runs there is no MuJoCo left in the result: a [`StoredAnimation`]
//! with translation and rotation tracks on scene nodes, indistinguishable from a
//! hand-authored or glTF-imported clip. It scrubs, mixes, saves and plays back in
//! a bundle like any other. That is the whole design — "trajectory" is a bake
//! step, never a concept the rest of the system has to know about (see
//! `docs/mujoco.md`).
//!
//! Pure data in, pure data out: a capture plus a geom_id→node map. No editor, no
//! renderer, no filesystem — so the bake is natively unit-testable and the editor
//! command on top of it is a thin wrapper.
//!
//! ## Coordinates
//!
//! None are converted. A capture holds raw MuJoCo world poses, and a geom node
//! sits directly under the instance root that carries the single Z-up→Y-up
//! rotation — so the capture's pose IS the node's local transform, exactly as at
//! import. Converting here would apply the convention twice.

use std::collections::HashMap;

use awsm_renderer_mujoco_format::capture::Capture;

use crate::animation::{
    ClipDirection, ClipLoop, Interp, Keyframe, SamplerKind, StoredAnimation, StoredTrack,
    TrackTarget, TrackValue, TransformProp,
};
use crate::assets::AssetId;
use crate::tree::NodeId;

/// How aggressively to drop keyframes that add nothing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reduction {
    /// Metres. A translation key is dropped when it sits within this distance of
    /// the straight line between its neighbours. 1 mm is well under what is
    /// visible on a robot at any sane camera distance.
    pub position: f32,
    /// Radians. A rotation key is dropped when it is within this angle of the
    /// interpolation between its neighbours.
    pub rotation: f32,
}

impl Default for Reduction {
    fn default() -> Self {
        Self {
            position: 0.001,
            rotation: 0.002, // ~0.1°
        }
    }
}

impl Reduction {
    /// Keep every recorded frame. Useful for a test that wants to compare a bake
    /// against the capture sample-for-sample.
    pub const NONE: Self = Self {
        position: 0.0,
        rotation: 0.0,
    };
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The capture is of a different model than the instance it was aimed at.
    /// Loud on purpose: baking anyway would drive the wrong robot with poses
    /// that are individually plausible, which is close to undiagnosable.
    ModelMismatch {
        capture: String,
        instance: String,
    },
    Empty,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::ModelMismatch { capture, instance } => write!(
                f,
                "this capture is of a different model: it fingerprints {capture}, \
                 the instance was imported from {instance}"
            ),
            Error::Empty => write!(f, "the capture has no frames"),
        }
    }
}

impl std::error::Error for Error {}

/// Bake `capture` into a clip driving `binding`'s nodes.
///
/// `binding` maps MuJoCo geom id → the scene node standing for it, which the
/// caller resolves by walking an instance's subtree — geoms the scene chose not
/// to render simply have no entry and produce no tracks.
///
/// Geoms that never move produce **no tracks at all** rather than a flat pair.
/// A robot's world is mostly static (floors, fixtures, the scenery half of a
/// menagerie scene), and a clip that pins those nodes every frame would both
/// bloat the bundle and quietly fight anything else trying to animate them.
pub fn bake(
    capture: &Capture,
    binding: &HashMap<u32, NodeId>,
    body_binding: &HashMap<u32, NodeId>,
    name: impl Into<String>,
    reduction: Reduction,
) -> Result<StoredAnimation, Error> {
    if capture.frames.is_empty() {
        return Err(Error::Empty);
    }
    let t0 = capture.frames[0].time;
    let times: Vec<f64> = capture.frames.iter().map(|f| f.time - t0).collect();

    let mut tracks = Vec::new();
    append_tracks(
        &mut tracks,
        capture,
        binding,
        &times,
        reduction,
        |frame, id| frame.geom(id),
    );
    // The BODY channel, when the capture has one — the same mechanism against a
    // different id space. A flex is skinned to the bodies its cage rides, so
    // tracks on those joint nodes ARE the deformation: after this bake there is
    // no deformable left in the result either, just a clip moving joints.
    if capture.body_count > 0 {
        append_tracks(
            &mut tracks,
            capture,
            body_binding,
            &times,
            reduction,
            |frame, id| frame.body(id),
        );
    }
    // Deterministic output: a HashMap iterates in an arbitrary order, and a clip
    // whose track order changed between runs would defeat golden comparison.
    tracks.sort_by_key(|t| match &t.target {
        TrackTarget::Transform { node, prop } => {
            (node.0, matches!(prop, TransformProp::Rotation) as u8)
        }
        _ => (uuid::Uuid::nil(), 0),
    });

    Ok(StoredAnimation {
        id: AssetId::new(),
        name: name.into(),
        duration: times.last().copied().unwrap_or(0.0),
        loop_style: ClipLoop::Once,
        speed: 1.0,
        direction: ClipDirection::Forward,
        color: String::new(),
        tracks,
    })
}

/// Build translation/rotation tracks for one id space and append them.
///
/// Shared by the geom and body channels: they differ only in which id space they
/// index and which accessor reads a frame. Everything that matters — the
/// quaternion order, the hemisphere fix, the static-node skip, the reduction —
/// is therefore identical for both by construction, rather than by two copies
/// agreeing.
fn append_tracks(
    tracks: &mut Vec<StoredTrack>,
    capture: &Capture,
    binding: &HashMap<u32, NodeId>,
    times: &[f64],
    reduction: Reduction,
    pose_of: impl Fn(
        &awsm_renderer_mujoco_format::capture::Frame,
        usize,
    ) -> Option<([f32; 3], [f32; 4])>,
) {
    for (id, node) in binding {
        let mut positions: Vec<[f32; 3]> = Vec::with_capacity(capture.frames.len());
        let mut rotations: Vec<[f32; 4]> = Vec::with_capacity(capture.frames.len());
        let mut complete = true;
        for frame in &capture.frames {
            let Some((pos, quat)) = pose_of(frame, *id as usize) else {
                complete = false;
                break;
            };
            positions.push(pos);
            // MuJoCo quaternions are [w, x, y, z]; ours are [x, y, z, w].
            rotations.push([quat[1], quat[2], quat[3], quat[0]]);
        }
        if !complete {
            // A frame too short to cover this id. `Capture::validate` already
            // rejects that, so reaching here means an unvalidated capture —
            // skip it rather than bake half a track.
            continue;
        }
        make_continuous(&mut rotations);

        if moves(&positions, reduction.position.max(1e-6))
            || rotates(&rotations, reduction.rotation.max(1e-6))
        {
            let keep = keep_mask(times, &positions, &rotations, reduction);
            tracks.push(track(
                *node,
                TransformProp::Translation,
                times,
                &keep,
                positions.iter().map(|p| TrackValue::Vec3(*p)),
            ));
            tracks.push(track(
                *node,
                TransformProp::Rotation,
                times,
                &keep,
                rotations.iter().map(|q| TrackValue::Quat(*q)),
            ));
        }
    }
}

/// Resolve the geom_id→node binding by walking an instance's subtree.
///
/// The binding is derived, never stored: every geom node already carries its own
/// id, so a stored map would be a second copy free to drift from the tree it
/// describes (see [`super::MujocoGeom`]).
pub fn binding_of(instance: &crate::tree::EditorNode) -> HashMap<u32, NodeId> {
    fn walk(node: &crate::tree::EditorNode, out: &mut HashMap<u32, NodeId>) {
        if let Some(super::MujocoComponent::Geom(g)) = &node.mujoco {
            out.insert(g.geom_id, node.id);
        }
        for c in &node.children {
            walk(c, out);
        }
    }
    let mut out = HashMap::new();
    for c in &instance.children {
        walk(c, &mut out);
    }
    out
}

/// Resolve the body_id→node binding by walking an instance's subtree.
///
/// The body id space is SEPARATE from the geom one, and both are dense from
/// zero — so a body pose applied through a geom binding would drive a real node
/// with a real pose belonging to something else, which looks like a physics bug
/// rather than a mix-up. Today these nodes are a flex's skin joints.
pub fn body_binding_of(instance: &crate::tree::EditorNode) -> HashMap<u32, NodeId> {
    fn walk(node: &crate::tree::EditorNode, out: &mut HashMap<u32, NodeId>) {
        if let Some(super::MujocoComponent::Body(b)) = &node.mujoco {
            out.insert(b.body_id, node.id);
        }
        for c in &node.children {
            walk(c, out);
        }
    }
    let mut out = HashMap::new();
    for c in &instance.children {
        walk(c, &mut out);
    }
    out
}

/// Flip quaternions into a consistent hemisphere.
///
/// `q` and `-q` are the same rotation, and a simulator is free to emit either
/// from frame to frame. Interpolating across a sign flip takes the long way
/// round — a limb visibly spinning 360° between two keys that are, in fact,
/// almost identical. This is the single most likely way baked physics looks
/// broken, and it costs one dot product per frame to prevent.
fn make_continuous(rotations: &mut [[f32; 4]]) {
    for i in 1..rotations.len() {
        let (prev, cur) = (rotations[i - 1], rotations[i]);
        if dot(prev, cur) < 0.0 {
            rotations[i] = [-cur[0], -cur[1], -cur[2], -cur[3]];
        }
    }
}

fn dot(a: [f32; 4], b: [f32; 4]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3]
}

fn moves(positions: &[[f32; 3]], eps: f32) -> bool {
    let first = positions[0];
    positions
        .iter()
        .any(|p| (0..3).any(|a| (p[a] - first[a]).abs() > eps))
}

fn rotates(rotations: &[[f32; 4]], eps: f32) -> bool {
    let first = rotations[0];
    // |dot| = cos(half-angle); 1 - |dot| grows ~ angle²/8 for small angles.
    let threshold = 1.0 - (eps / 2.0).cos();
    rotations
        .iter()
        .any(|q| 1.0 - dot(first, *q).abs() > threshold)
}

/// Which frames survive reduction.
///
/// A frame is dropped when BOTH its position and its rotation lie within
/// tolerance of the straight line between the last kept frame and the next
/// candidate — evaluated against the kept frame, not the original neighbours, so
/// error cannot accumulate across a long run of dropped keys. Position and
/// rotation share one mask because they share the clip's `times` array; keeping a
/// key for one and not the other would need two time bases per geom.
fn keep_mask(
    times: &[f64],
    positions: &[[f32; 3]],
    rotations: &[[f32; 4]],
    reduction: Reduction,
) -> Vec<bool> {
    let n = times.len();
    let mut keep = vec![false; n];
    if n == 0 {
        return keep;
    }
    keep[0] = true;
    keep[n - 1] = true;
    if n <= 2 || (reduction.position <= 0.0 && reduction.rotation <= 0.0) {
        return vec![true; n];
    }

    let mut anchor = 0usize;
    for i in 1..n - 1 {
        let next = i + 1;
        let span = times[next] - times[anchor];
        let t = if span.abs() < f64::EPSILON {
            0.0
        } else {
            ((times[i] - times[anchor]) / span) as f32
        };

        let mut ok = true;
        for ((from, to), here) in positions[anchor]
            .iter()
            .zip(positions[next].iter())
            .zip(positions[i].iter())
        {
            if (here - (from + (to - from) * t)).abs() > reduction.position {
                ok = false;
                break;
            }
        }
        if ok {
            // nlerp is enough to *measure* deviation: over a span small enough to
            // be a drop candidate it differs from slerp by far less than the
            // tolerance being tested.
            let mut lerped = [0.0f32; 4];
            for a in 0..4 {
                lerped[a] = rotations[anchor][a] + (rotations[next][a] - rotations[anchor][a]) * t;
            }
            let len = dot(lerped, lerped).sqrt();
            if len > 1e-6 {
                for v in &mut lerped {
                    *v /= len;
                }
                let angle = 2.0 * dot(lerped, rotations[i]).abs().clamp(0.0, 1.0).acos();
                if angle > reduction.rotation {
                    ok = false;
                }
            }
        }
        if !ok {
            keep[i] = true;
            anchor = i;
        }
    }
    keep
}

fn zeroed_like(v: &TrackValue) -> TrackValue {
    match v {
        TrackValue::Vec2(_) => TrackValue::Vec2([0.0; 2]),
        TrackValue::Vec3(_) => TrackValue::Vec3([0.0; 3]),
        TrackValue::Vec4(_) => TrackValue::Vec4([0.0; 4]),
        TrackValue::Quat(_) => TrackValue::Quat([0.0; 4]),
        TrackValue::Scalar(_) => TrackValue::Scalar(0.0),
    }
}

fn track(
    node: NodeId,
    prop: TransformProp,
    times: &[f64],
    keep: &[bool],
    values: impl Iterator<Item = TrackValue>,
) -> StoredTrack {
    let mut kept_times = Vec::new();
    let mut keys = Vec::new();
    for (i, value) in values.enumerate() {
        if !keep[i] {
            continue;
        }
        kept_times.push(times[i]);
        // Zeroed tangents, matching what the editor's own `new_keyframe` writes
        // for a linear key. They are meaningless outside cubic interpolation, and
        // echoing `value` into both would triple the clip's on-disk size while
        // implying a cubic tangent that happens to equal the value.
        keys.push(Keyframe {
            value,
            interp: Interp::Linear,
            in_tangent: zeroed_like(&value),
            out_tangent: zeroed_like(&value),
        });
    }
    StoredTrack {
        target: TrackTarget::Transform { node, prop },
        sampler: SamplerKind::Linear,
        mute: false,
        solo: false,
        expanded: false,
        times: kept_times,
        keys,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use awsm_renderer_mujoco_format::capture::Frame;
    use awsm_renderer_mujoco_format::sidecar::Source;

    fn source(name: &str) -> Source {
        Source {
            filename: name.into(),
            sha256: "d".repeat(64),
            mujoco_version: "3.11.0".into(),
        }
    }

    /// A capture where geom 0 slides along x, geom 1 never moves.
    fn capture(frames: usize) -> Capture {
        let mut c = Capture::new(source("m.xml"), 2);
        for i in 0..frames {
            let mut f = Frame {
                time: i as f64 / 60.0,
                geom_poses: Vec::new(),
                body_poses: Vec::new(),
            };
            f.push_geom([i as f32 * 0.1, 0.0, 0.0], [1.0, 0.0, 0.0, 0.0]);
            f.push_geom([5.0, 0.0, 0.0], [1.0, 0.0, 0.0, 0.0]);
            c.frames.push(f);
        }
        c
    }

    fn binding() -> (HashMap<u32, NodeId>, NodeId, NodeId) {
        let a = NodeId::new();
        let b = NodeId::new();
        (HashMap::from([(0, a), (1, b)]), a, b)
    }

    /// A capture with a body channel bakes tracks for the flex's joints too.
    ///
    /// This is what makes a deformable replayable with no simulator: the flex is
    /// skinned to these bodies, so keying them IS keying the deformation.
    #[test]
    fn the_body_channel_bakes_tracks_for_its_own_id_space() {
        let mut c = Capture::new(source("flex.xml"), 1).with_bodies(2);
        for i in 0..10 {
            let mut f = Frame {
                time: i as f64 / 60.0,
                geom_poses: Vec::new(),
                body_poses: Vec::new(),
            };
            f.push_geom([0.0, 0.0, 0.0], [1.0, 0.0, 0.0, 0.0]); // a static floor
            f.push_body([0.0, 0.0, 0.0], [1.0, 0.0, 0.0, 0.0]); // body 0: world
            f.push_body([i as f32 * 0.1, 0.0, 0.0], [1.0, 0.0, 0.0, 0.0]); // moving
            c.frames.push(f);
        }
        c.validate().expect("a full body channel is valid");

        let geom_node = NodeId::new();
        let still_body = NodeId::new();
        let moving_body = NodeId::new();
        let clip = bake(
            &c,
            &HashMap::from([(0, geom_node)]),
            &HashMap::from([(0, still_body), (1, moving_body)]),
            "wave",
            Reduction::default(),
        )
        .unwrap();

        let nodes: Vec<NodeId> = clip
            .tracks
            .iter()
            .filter_map(|t| match t.target {
                TrackTarget::Transform { node, .. } => Some(node),
                _ => None,
            })
            .collect();
        assert!(
            nodes.contains(&moving_body),
            "the moving body must be keyed — without it the flex never deforms"
        );
        assert!(
            !nodes.contains(&still_body),
            "a body that never moves gets no tracks, same rule as a static geom"
        );
        assert!(
            !nodes.contains(&geom_node),
            "the static floor geom must still get no tracks"
        );
        assert_eq!(clip.tracks.len(), 2, "one T + one R for the moving body");
    }

    /// The two id spaces are separate, and both are dense from zero — so a body
    /// pose must never reach a geom's node. That mix-up would drive a real node
    /// with a real pose belonging to something else.
    #[test]
    fn the_geom_and_body_id_spaces_do_not_bleed_into_each_other() {
        let mut c = Capture::new(source("flex.xml"), 1).with_bodies(1);
        for i in 0..10 {
            let mut f = Frame {
                time: i as f64 / 60.0,
                geom_poses: Vec::new(),
                body_poses: Vec::new(),
            };
            // Geom 0 is still; body 0 — the SAME id — moves.
            f.push_geom([0.0, 0.0, 0.0], [1.0, 0.0, 0.0, 0.0]);
            f.push_body([i as f32 * 0.1, 0.0, 0.0], [1.0, 0.0, 0.0, 0.0]);
            c.frames.push(f);
        }
        let geom_node = NodeId::new();
        let body_node = NodeId::new();
        let clip = bake(
            &c,
            &HashMap::from([(0, geom_node)]),
            &HashMap::from([(0, body_node)]),
            "wave",
            Reduction::default(),
        )
        .unwrap();
        for t in &clip.tracks {
            if let TrackTarget::Transform { node, .. } = t.target {
                assert_eq!(
                    node, body_node,
                    "only the BODY node may be keyed — geom 0 never moved"
                );
            }
        }
        assert_eq!(clip.tracks.len(), 2);
    }

    /// A partial body channel is refused: it would deform part of a flex and
    /// leave the rest at its bind pose, which reads as a physics bug.
    #[test]
    fn a_partial_body_channel_is_invalid() {
        let mut c = Capture::new(source("flex.xml"), 1).with_bodies(2);
        let mut f = Frame {
            time: 0.0,
            geom_poses: Vec::new(),
            body_poses: Vec::new(),
        };
        f.push_geom([0.0, 0.0, 0.0], [1.0, 0.0, 0.0, 0.0]);
        f.push_body([0.0, 0.0, 0.0], [1.0, 0.0, 0.0, 0.0]); // only one of two
        c.frames.push(f);
        assert!(matches!(
            c.validate(),
            Err(awsm_renderer_mujoco_format::capture::Error::BodyFrameSize { .. })
        ));
    }

    #[test]
    fn static_geoms_get_no_tracks_at_all() {
        let (bind, moving, still) = binding();
        let clip = bake(
            &capture(10),
            &bind,
            &HashMap::new(),
            "run",
            Reduction::default(),
        )
        .unwrap();
        let nodes: Vec<NodeId> = clip
            .tracks
            .iter()
            .filter_map(|t| match t.target {
                TrackTarget::Transform { node, .. } => Some(node),
                _ => None,
            })
            .collect();
        assert!(nodes.contains(&moving));
        assert!(
            !nodes.contains(&still),
            "a geom that never moves must not be keyed — it would bloat the \
             bundle and fight anything else animating that node"
        );
        assert_eq!(clip.tracks.len(), 2, "one T + one R track for the mover");
    }

    #[test]
    fn a_straight_slide_reduces_to_its_endpoints() {
        let (bind, _, _) = binding();
        let clip = bake(
            &capture(60),
            &bind,
            &HashMap::new(),
            "run",
            Reduction::default(),
        )
        .unwrap();
        let t = &clip.tracks[0];
        assert_eq!(
            t.times.len(),
            2,
            "perfectly linear motion needs only its endpoints, got {} keys",
            t.times.len()
        );
        assert_eq!(
            t.keys.len(),
            t.times.len(),
            "keys and times must stay aligned"
        );
    }

    #[test]
    fn reduction_none_keeps_every_frame() {
        let (bind, _, _) = binding();
        let clip = bake(&capture(60), &bind, &HashMap::new(), "run", Reduction::NONE).unwrap();
        assert_eq!(clip.tracks[0].times.len(), 60);
    }

    /// The single most likely way baked physics looks broken: a simulator emits
    /// `q` on one frame and `-q` (the same rotation) on the next, and
    /// interpolation takes the long way — a limb spinning a full turn between two
    /// keys that are nearly identical.
    #[test]
    fn quaternion_sign_flips_are_made_continuous() {
        // 0°, 60°, 120° about Z — a real rotation — with the last frame written
        // negated, exactly as a simulator is free to do.
        let q = |deg: f32| {
            let h = deg.to_radians() / 2.0;
            [h.cos(), 0.0, 0.0, h.sin()] // MuJoCo order: [w, x, y, z]
        };
        let neg = |q: [f32; 4]| [-q[0], -q[1], -q[2], -q[3]];
        let mut c = Capture::new(source("m.xml"), 1);
        for (i, quat) in [q(0.0), q(60.0), neg(q(120.0))].iter().enumerate() {
            let mut f = Frame {
                time: i as f64,
                geom_poses: Vec::new(),
                body_poses: Vec::new(),
            };
            f.push_geom([0.0; 3], *quat);
            c.frames.push(f);
        }
        let node = NodeId::new();
        let clip = bake(
            &c,
            &HashMap::from([(0, node)]),
            &HashMap::new(),
            "spin",
            Reduction::NONE,
        )
        .unwrap();
        let rot = clip
            .tracks
            .iter()
            .find(|t| {
                matches!(
                    t.target,
                    TrackTarget::Transform {
                        prop: TransformProp::Rotation,
                        ..
                    }
                )
            })
            .expect("a rotation track");
        let quats: Vec<[f32; 4]> = rot
            .keys
            .iter()
            .map(|k| match k.value {
                TrackValue::Quat(q) => q,
                _ => panic!("expected a quaternion"),
            })
            .collect();
        assert_eq!(quats.len(), 3);
        for w in quats.windows(2) {
            assert!(
                dot(w[0], w[1]) > 0.0,
                "consecutive keys must stay in one hemisphere: {:?} then {:?}",
                w[0],
                w[1]
            );
        }
    }

    /// The other half of the same property: a frame that ONLY flips sign is not
    /// motion, so it must not resurrect a static geom into a keyed one.
    #[test]
    fn a_pure_sign_flip_is_not_mistaken_for_rotation() {
        let mut c = Capture::new(source("m.xml"), 1);
        for (i, quat) in [[1.0, 0.0, 0.0, 0.0], [-1.0, 0.0, 0.0, 0.0]]
            .iter()
            .enumerate()
        {
            let mut f = Frame {
                time: i as f64,
                geom_poses: Vec::new(),
                body_poses: Vec::new(),
            };
            f.push_geom([0.0; 3], *quat);
            c.frames.push(f);
        }
        let clip = bake(
            &c,
            &HashMap::from([(0, NodeId::new())]),
            &HashMap::new(),
            "still",
            Reduction::default(),
        )
        .unwrap();
        assert!(
            clip.tracks.is_empty(),
            "q and -q are the same rotation; nothing moved"
        );
    }

    #[test]
    fn track_order_is_deterministic() {
        // A HashMap iterates arbitrarily; a clip whose track order moved between
        // runs would defeat golden comparison in the browser suite.
        let (bind, _, _) = binding();
        let a = bake(
            &capture(10),
            &bind,
            &HashMap::new(),
            "run",
            Reduction::default(),
        )
        .unwrap();
        let b = bake(
            &capture(10),
            &bind,
            &HashMap::new(),
            "run",
            Reduction::default(),
        )
        .unwrap();
        assert_eq!(a.tracks, b.tracks);
    }

    #[test]
    fn an_empty_capture_is_an_error_not_an_empty_clip() {
        let (bind, _, _) = binding();
        assert_eq!(
            bake(
                &Capture::new(source("m.xml"), 2),
                &bind,
                &HashMap::new(),
                "run",
                Reduction::default()
            ),
            Err(Error::Empty)
        );
    }

    #[test]
    fn duration_and_times_are_relative_to_the_first_frame() {
        let mut c = capture(3);
        for (i, f) in c.frames.iter_mut().enumerate() {
            f.time = 100.0 + i as f64; // a capture that started late
        }
        let (bind, _, _) = binding();
        let clip = bake(&c, &bind, &HashMap::new(), "run", Reduction::NONE).unwrap();
        assert_eq!(clip.tracks[0].times.first(), Some(&0.0));
        assert_eq!(clip.duration, 2.0);
    }
}
