//! Forward kinematics over a stage's UsdPhysics joints.
//!
//! The renderer never needs this — a running sim reports body poses and the
//! pose sink takes them as they are. It exists so a robot can be shown MOVING
//! with no simulator at all: the recorder (`awsm-renderer-isaac-record`) sweeps
//! the joints through a scripted motion and writes the resulting poses as a
//! capture, the same shape a live Isaac Lab stream carries.
//!
//! ## The joint model (UsdPhysics)
//!
//! A joint names two bodies and a frame in each (`localPos0/localRot0` in
//! `body0`'s space, `localPos1/localRot1` in `body1`'s). The joint holds the two
//! frames together up to its free motion `M(q)` — a rotation about the joint
//! axis for a revolute joint (degrees in USD), a translation along it for a
//! prismatic one — so
//!
//! ```text
//! child world = parent world · J_parent · M(q) · J_child⁻¹
//! ```
//!
//! A joint whose `body1` is empty ties `body0` to the world; the roles swap and
//! `q` is negated.
//!
//! ## The rest pose
//!
//! Assets do not reliably store joint positions (ANYmal authors none), so each
//! joint's REST position is solved from the authored body transforms: the
//! relative motion between the two joint frames, projected on the axis.
//!
//! What is left over — motion OFF the joint's freedom — is the asset's joint
//! frames disagreeing with its bodies (ANYmal's fixed foot joints sit 3 cm from
//! the foot bodies they hold). A simulator would snap the body onto the joint;
//! this module keeps the AUTHORED pose instead, folding the residual into the
//! child's joint frame, so FK at rest reproduces the export exactly and a
//! scripted capture starts where the import is. The residual is kept on the
//! joint and reported when it exceeds [`RESIDUAL_NOTE`].

use std::collections::{HashMap, VecDeque};

use glam::{DMat4, DQuat, DVec3};
use openusd::sdf;
use openusd::usd::{Prim, Stage};

use crate::stage::{value, value_f64, value_token};

/// Residual (metres, or radians) above which a joint's disagreement with its
/// bodies is reported.
pub const RESIDUAL_NOTE: f64 = 1e-3;

/// A joint's free motion.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum JointKind {
    Fixed,
    /// Rotation about `axis` (unit, in the joint frame); `q` in radians.
    Revolute {
        axis: DVec3,
    },
    /// Translation along `axis`; `q` in metres.
    Prismatic {
        axis: DVec3,
    },
}

/// One FK edge: a joint that drives `child` from `parent`.
#[derive(Debug, Clone)]
pub struct Joint {
    /// The joint prim's name (`panda_joint4`, `LF_KFE`).
    pub name: String,
    pub kind: JointKind,
    /// Sidecar body index; `None` = the world.
    pub parent: Option<usize>,
    /// Sidecar body index.
    pub child: usize,
    /// Joint frame in the parent's space.
    pub parent_frame: DMat4,
    /// Joint frame in the child's space.
    pub child_frame: DMat4,
    /// `+1`, or `-1` when USD names the child as `body0` (a joint to the
    /// world with an empty `body1`).
    pub sign: f64,
    /// `(lower, upper)` in radians / metres, when authored.
    pub limits: Option<(f64, f64)>,
    /// Position that reproduces the authored pose.
    pub rest: f64,
    /// How far the authored pose is from anything this joint can reach:
    /// `(metres, radians)` off-axis. Near zero for a consistent asset.
    pub residual: (f64, f64),
}

impl Joint {
    pub fn is_movable(&self) -> bool {
        !matches!(self.kind, JointKind::Fixed)
    }

    fn motion(&self, q: f64) -> DMat4 {
        let q = q * self.sign;
        match self.kind {
            JointKind::Fixed => DMat4::IDENTITY,
            JointKind::Revolute { axis } => DMat4::from_quat(DQuat::from_axis_angle(axis, q)),
            JointKind::Prismatic { axis } => DMat4::from_translation(axis * q),
        }
    }
}

/// A stage's kinematic tree, over the sidecar's bodies.
#[derive(Debug, Clone, Default)]
pub struct Articulation {
    /// FK edges, parents before children.
    pub joints: Vec<Joint>,
    /// Every sidecar body's authored (rest) world frame, rigid.
    pub rest: Vec<DMat4>,
    /// Anything the FK could not model (loops, unsupported joint types).
    pub notes: Vec<String>,
}

impl Articulation {
    /// Read every enabled joint on the stage. `bodies` maps a rigid-body prim
    /// path to its sidecar index; `rest` is each sidecar body's rigid world
    /// frame (index 0, the world, is identity) and `scales` its world scale,
    /// which the joint frames' `localPos` is authored in.
    pub fn read(
        stage: &Stage,
        paths: &[sdf::Path],
        bodies: &HashMap<sdf::Path, usize>,
        rest: &[DMat4],
        scales: &[DVec3],
    ) -> Self {
        let mut notes = Vec::new();
        let mut edges = Vec::new();
        for path in paths {
            let Ok(prim) = stage.prim(path.clone()) else {
                continue;
            };
            let Some(ty) = prim.type_name().ok().flatten() else {
                continue;
            };
            let ty = ty.as_str().to_string();
            if !ty.starts_with("Physics") || !ty.ends_with("Joint") {
                continue;
            }
            if matches!(
                value(&prim.attribute("physics:jointEnabled")),
                Some(sdf::Value::Bool(false))
            ) {
                continue;
            }
            match read_joint(&prim, &ty, bodies, rest, scales) {
                Ok(Some(j)) => {
                    if j.residual.0 > RESIDUAL_NOTE || j.residual.1 > RESIDUAL_NOTE {
                        notes.push(format!(
                            "joint {}: authored bodies are {:.4} m / {:.4} rad off its frames; the authored pose is kept",
                            j.name, j.residual.0, j.residual.1
                        ));
                    }
                    edges.push(j)
                }
                Ok(None) => {}
                Err(e) => notes.push(format!("{path}: {e}")),
            }
        }

        // Tree order: breadth-first from the bodies nothing drives. A body
        // driven twice (a kinematic loop, a mimic pair modelled as two joints
        // to one body) keeps its first joint; the rest are reported.
        let mut driven: HashMap<usize, usize> = HashMap::new();
        let mut kept = Vec::new();
        for j in edges {
            if let Some(&first) = driven.get(&j.child) {
                let other: &Joint = &kept[first];
                notes.push(format!(
                    "joint {} drives the same body as {}; ignored (a loop)",
                    j.name, other.name
                ));
                continue;
            }
            driven.insert(j.child, kept.len());
            kept.push(j);
        }
        let mut children: HashMap<Option<usize>, Vec<usize>> = HashMap::new();
        for (i, j) in kept.iter().enumerate() {
            children.entry(j.parent).or_default().push(i);
        }
        let mut order = Vec::with_capacity(kept.len());
        let mut queue: VecDeque<Option<usize>> = VecDeque::new();
        queue.push_back(None);
        for body in 0..rest.len() {
            if !driven.contains_key(&body) {
                queue.push_back(Some(body));
            }
        }
        let mut seen = vec![false; kept.len()];
        while let Some(parent) = queue.pop_front() {
            for &i in children.get(&parent).map(Vec::as_slice).unwrap_or(&[]) {
                if !seen[i] {
                    seen[i] = true;
                    order.push(i);
                    queue.push_back(Some(kept[i].child));
                }
            }
        }
        for (i, j) in kept.iter().enumerate() {
            if !seen[i] {
                notes.push(format!("joint {} is part of a cycle; ignored", j.name));
            }
        }
        let joints = order.into_iter().map(|i| kept[i].clone()).collect();
        Self {
            joints,
            rest: rest.to_vec(),
            notes,
        }
    }

    /// Body world frames (by sidecar index) with joint `i` at `q[i]`. Bodies
    /// no joint drives stay at rest.
    pub fn forward(&self, q: &[f64]) -> Vec<DMat4> {
        let mut world = self.rest.clone();
        for (j, &qj) in self.joints.iter().zip(q) {
            let parent = j.parent.map_or(DMat4::IDENTITY, |p| world[p]);
            world[j.child] = parent * j.parent_frame * j.motion(qj) * j.child_frame.inverse();
        }
        world
    }

    /// Every joint at its rest position.
    pub fn rest_positions(&self) -> Vec<f64> {
        self.joints.iter().map(|j| j.rest).collect()
    }
}

fn read_joint(
    prim: &Prim,
    ty: &str,
    bodies: &HashMap<sdf::Path, usize>,
    rest: &[DMat4],
    scales: &[DVec3],
) -> Result<Option<Joint>, String> {
    let body = |rel: &str| -> Option<usize> {
        let target = prim.relationship(rel).targets().ok()?.into_iter().next()?;
        nearest_body(&target, bodies)
    };
    let (b0, b1) = (body("physics:body0"), body("physics:body1"));
    let frame = |pos: &str, rot: &str, body: Option<usize>| -> DMat4 {
        let p = value(&prim.attribute(pos))
            .and_then(|v| crate::stage::value_vec3(&v))
            .unwrap_or([0.0; 3]);
        let q = value(&prim.attribute(rot))
            .and_then(|v| value_quat(&v))
            .unwrap_or(DQuat::IDENTITY);
        // localPos is in the body's LOCAL space, which carries the body's
        // scale; the rigid rest frame does not, so apply it here. (Isaac
        // bodies are unscaled; this keeps a scaled one honest.)
        let scale = body.map_or(DVec3::ONE, |b| scales[b]);
        DMat4::from_rotation_translation(q.normalize(), DVec3::from_array(p) * scale)
    };
    let (parent, child, parent_frame, child_frame, sign) = match (b0, b1) {
        (p, Some(c)) => (
            p,
            c,
            frame("physics:localPos0", "physics:localRot0", p),
            frame("physics:localPos1", "physics:localRot1", Some(c)),
            1.0,
        ),
        (Some(c), None) => (
            None,
            c,
            frame("physics:localPos1", "physics:localRot1", None),
            frame("physics:localPos0", "physics:localRot0", Some(c)),
            -1.0,
        ),
        (None, None) => return Ok(None),
    };
    if Some(child) == parent {
        return Ok(None);
    }
    let axis = || {
        let token = value(&prim.attribute("physics:axis"))
            .and_then(|v| value_token(&v))
            .unwrap_or_else(|| "X".into());
        match token.as_str() {
            "Y" => DVec3::Y,
            "Z" => DVec3::Z,
            _ => DVec3::X,
        }
    };
    let limit = |name: &str| value(&prim.attribute(name)).and_then(|v| value_f64(&v));
    let (kind, unit) = match ty {
        "PhysicsRevoluteJoint" => (JointKind::Revolute { axis: axis() }, 1f64.to_radians()),
        "PhysicsPrismaticJoint" => (JointKind::Prismatic { axis: axis() }, 1.0),
        "PhysicsFixedJoint" | "PhysicsJoint" => (JointKind::Fixed, 1.0),
        other => {
            return Err(format!(
                "{other} is not modelled (held at its authored pose)"
            ))
        }
    };
    let limits = match (limit("physics:lowerLimit"), limit("physics:upperLimit")) {
        // USD writes ±inf (or huge) for "no limit".
        (Some(lo), Some(hi)) if lo.is_finite() && hi.is_finite() && lo <= hi => {
            Some((lo * unit, hi * unit))
        }
        _ => None,
    };

    // Rest position: the relative motion between the two joint frames at the
    // authored pose, projected on the joint's freedom.
    let parent_world = parent.map_or(DMat4::IDENTITY, |p| rest[p]);
    let rel = (parent_world * parent_frame).inverse() * (rest[child] * child_frame);
    let (_, rot, pos) = rel.to_scale_rotation_translation();
    let (rest_q, residual) = match kind {
        JointKind::Fixed => (0.0, (pos.length(), rot.angle_between(DQuat::IDENTITY))),
        JointKind::Revolute { axis } => {
            let rot = if rot.w < 0.0 { -rot } else { rot };
            let q = 2.0 * DVec3::new(rot.x, rot.y, rot.z).dot(axis).atan2(rot.w);
            let off = rot.angle_between(DQuat::from_axis_angle(axis, q));
            (q * sign_of(sign), (pos.length(), off))
        }
        JointKind::Prismatic { axis } => {
            let q = pos.dot(axis);
            (
                q * sign_of(sign),
                (
                    (pos - axis * q).length(),
                    rot.angle_between(DQuat::IDENTITY),
                ),
            )
        }
    };
    // Keep the authored pose: fold the residual into the child's frame, so
    // `parent · J_parent · M(rest) · J_child⁻¹` IS the authored child.
    let draft = Joint {
        name: String::new(),
        kind,
        parent,
        child,
        parent_frame,
        child_frame,
        sign,
        limits,
        rest: rest_q,
        residual,
    };
    let child_frame = rest[child].inverse() * parent_world * parent_frame * draft.motion(rest_q);
    Ok(Some(Joint {
        name: prim.path().name().unwrap_or("joint").to_string(),
        kind,
        parent,
        child,
        parent_frame,
        child_frame,
        sign,
        limits,
        rest: rest_q,
        residual,
    }))
}

fn sign_of(s: f64) -> f64 {
    if s < 0.0 {
        -1.0
    } else {
        1.0
    }
}

fn nearest_body(path: &sdf::Path, bodies: &HashMap<sdf::Path, usize>) -> Option<usize> {
    let mut p = Some(path.clone());
    while let Some(cur) = p {
        if let Some(b) = bodies.get(&cur) {
            return Some(*b);
        }
        if cur.is_abs_root() {
            break;
        }
        p = cur.parent();
    }
    None
}

fn value_quat(v: &sdf::Value) -> Option<DQuat> {
    match v {
        sdf::Value::Quatf(q) => Some(DQuat::from_xyzw(
            q.x as f64, q.y as f64, q.z as f64, q.w as f64,
        )),
        sdf::Value::Quatd(q) => Some(DQuat::from_xyzw(q.x, q.y, q.z, q.w)),
        sdf::Value::Quath(q) => Some(DQuat::from_xyzw(
            q.x.to_f64(),
            q.y.to_f64(),
            q.z.to_f64(),
            q.w.to_f64(),
        )),
        _ => None,
    }
}

/// Per joint: how far it may swing below and above its rest position.
pub struct JointSweep {
    pub rest: f64,
    /// Swing below / above rest (radians or metres).
    pub down: f64,
    pub up: f64,
    phase: f64,
    bounds: (f64, f64),
}

/// A scripted, looping sweep of every movable joint around its rest position.
pub struct Sweep {
    pub joints: Vec<JointSweep>,
}

impl Sweep {
    /// `amplitude`: the fraction of each joint's room (≤ 45°, or its travel
    /// for a slider) it swings on each side of rest.
    pub fn new(arm: &Articulation, amplitude: f64) -> Self {
        let joints = arm
            .joints
            .iter()
            .enumerate()
            .map(|(i, j)| {
                let cap = match j.kind {
                    JointKind::Fixed => 0.0,
                    JointKind::Revolute { .. } => 45f64.to_radians(),
                    JointKind::Prismatic { .. } => {
                        j.limits.map_or(0.05, |(lo, hi)| (hi - lo) / 2.0)
                    }
                };
                // Room on each side of rest, within the limits. A rest pose
                // OUTSIDE its limits (the Franka authors joint 4 at 0°, past
                // its -4° stop) only swings back toward the legal range.
                let (room_down, room_up, bounds) = match j.limits {
                    Some((lo, hi)) => (
                        (j.rest - lo).clamp(0.0, cap),
                        (hi - j.rest).clamp(0.0, cap),
                        (lo.min(j.rest), hi.max(j.rest)),
                    ),
                    None => (cap, cap, (f64::NEG_INFINITY, f64::INFINITY)),
                };
                JointSweep {
                    rest: j.rest,
                    down: amplitude * room_down,
                    up: amplitude * room_up,
                    // Golden-angle phases: neighbouring joints never move in
                    // lockstep, and no two share a phase.
                    phase: i as f64 * 2.399_963_229_728_653,
                    bounds,
                }
            })
            .collect();
        Self { joints }
    }

    pub fn moving(&self) -> usize {
        self.joints
            .iter()
            .filter(|s| s.down > 0.0 || s.up > 0.0)
            .count()
    }

    /// Joint positions at phase angle `wt`. Starts at rest (`wt = 0`) and is
    /// periodic in `wt`, so a whole number of cycles loops without a seam.
    pub fn at(&self, wt: f64) -> Vec<f64> {
        self.joints
            .iter()
            .map(|s| {
                // d ∈ [-1 - sin φ, 1 - sin φ] ⊂ [-2, 2], zero at wt = 0.
                let d = (wt + s.phase).sin() - s.phase.sin();
                let reach = if d >= 0.0 { s.up } else { s.down };
                (s.rest + d / 2.0 * reach).clamp(s.bounds.0, s.bounds.1)
            })
            .collect()
    }
}

/// Timing and size of a recorded sweep.
#[derive(Debug, Clone, Copy)]
pub struct Motion {
    /// Length of one loop.
    pub seconds: f64,
    pub fps: f64,
    /// See [`Sweep::new`].
    pub amplitude: f64,
    /// Oscillations per loop; whole numbers loop seamlessly.
    pub cycles: u32,
}

/// Record `motion` as a capture of `doc`'s geoms: frame 0 is the rest pose
/// (the imported pose, so playback starts without a jump), and the last
/// frame leads back into the first.
pub fn record(
    doc: &awsm_renderer_mujoco_format::Sidecar,
    arm: &Articulation,
    motion: &Motion,
) -> awsm_renderer_mujoco_format::Capture {
    use awsm_renderer_mujoco_format::capture::{Capture, Frame};
    let frames = (motion.seconds * motion.fps).round().max(1.0) as usize;
    let omega = std::f64::consts::TAU * motion.cycles as f64 / motion.seconds;
    let sweep = Sweep::new(arm, motion.amplitude);
    let mut capture = Capture::new(doc.source.clone(), doc.geoms.len() as u32);
    for i in 0..frames {
        let t = i as f64 / motion.fps;
        capture.frames.push(Frame {
            time: t,
            geom_poses: geom_poses(doc, &arm.forward(&sweep.at(omega * t))),
            body_poses: Vec::new(),
        });
    }
    capture
}

/// Every geom's world pose (`[px,py,pz,qw,qx,qy,qz]`, f32) for body frames
/// `bodies` — the pose-sink frame layout.
pub fn geom_poses(doc: &awsm_renderer_mujoco_format::Sidecar, bodies: &[DMat4]) -> Vec<f32> {
    let mut out = Vec::with_capacity(doc.geoms.len() * 7);
    for g in &doc.geoms {
        let local = DMat4::from_rotation_translation(
            DQuat::from_xyzw(g.quat[1], g.quat[2], g.quat[3], g.quat[0]),
            DVec3::from_array(g.pos),
        );
        let (_, r, p) = (bodies[g.body] * local).to_scale_rotation_translation();
        let r = r.normalize();
        out.extend([p.x, p.y, p.z, r.w, r.x, r.y, r.z].map(|v| v as f32));
    }
    out
}
