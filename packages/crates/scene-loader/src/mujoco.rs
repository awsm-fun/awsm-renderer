//! The **pose sink**: the renderer's entire contract with an external simulator.
//!
//! This is the whole of the MuJoCo runtime surface in this repo. There is no
//! networking here, no transport, no timing source, and no MuJoCo code — just a
//! binding resolved at load and a function that writes a frame of world poses
//! onto node transforms. Anything that can call it drives the renderer: the
//! reference template's wasm worker, a native harness over a websocket, a test
//! feeding a recorded capture. Running the simulation and getting frames here is
//! the player's job (see `docs/plans/mujoco.md`).
//!
//! ## Coordinates
//!
//! Poses go in as **raw MuJoCo world poses** — Z-up, metres, `[w, x, y, z]` — and
//! are written as the geom node's *local* transform. That works because every
//! geom node sits directly under its instance root, which carries the single
//! Z-up→Y-up convention rotation. There is deliberately no per-frame conversion
//! math anywhere: moving the instance root moves the whole robot, and the stream
//! never has to know.
//!
//! ## Scale is preserved
//!
//! A frame carries translation and rotation only. Ellipsoid geoms are a unit
//! sphere scaled per-axis by the node's scale, so overwriting scale would flatten
//! them on the first frame.

use std::collections::HashMap;

use awsm_renderer::transforms::Transform;
use awsm_renderer::AwsmRenderer;
use awsm_renderer_scene::mujoco::{MujocoComponent, Source};
use awsm_renderer_scene::tree::{EditorNode, NodeId};
use glam::{Quat, Vec3};

pub use awsm_renderer_scene::mujoco::FLOATS_PER_GEOM;

/// One loaded sim instance, ready to be driven.
///
/// Resolved at load by walking the instance's subtree — never read from a stored
/// map, which would be a second copy free to drift from the tree it describes.
#[derive(Debug, Clone)]
pub struct MujocoInstance {
    /// The instance root's node id. Its transform is the model's authored
    /// placement (plus the convention rotation) and is **not** stream-driven.
    pub root: NodeId,
    /// The model this instance was imported from. A harness matches its own
    /// loaded models against this and fails loudly rather than driving the wrong
    /// robot; see [`Self::matches`].
    pub source: Source,
    /// The compiled model's own name, when it had one.
    pub model_name: Option<String>,
    /// `geom_id → ` that geom's transform, indexed directly by geom id and sized
    /// to the model's full geom count.
    ///
    /// A `Vec<Option<_>>` rather than a map because this is indexed once per geom
    /// per frame: an array lookup is the right shape for a hot path, and geoms
    /// the scene chose not to render (hidden groups, unsupported kinds) are
    /// simply `None` — the id space stays the model's, so a frame never has to be
    /// re-indexed.
    pub geoms: Vec<Option<awsm_renderer::transforms::TransformKey>>,
}

impl MujocoInstance {
    /// How many floats one frame must carry for this instance.
    pub fn frame_len(&self) -> usize {
        self.geoms.len() * FLOATS_PER_GEOM
    }

    /// Whether `source` is the same compiled model this instance was imported
    /// from. Compares the content hash, not the filename — a harness that
    /// renamed the file is still driving the right robot, and one that edited it
    /// is not.
    pub fn matches(&self, source: &Source) -> bool {
        self.source.sha256 == source.sha256
    }
}

/// Why a frame was rejected.
///
/// Applying a mis-sized frame would drive every geom past the first mismatch
/// from the wrong slot — motion that reads as a physics bug rather than a
/// protocol one — so this is an error, never a silent truncation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PoseError {
    WrongLength { got: usize, expected: usize },
}

impl std::fmt::Display for PoseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PoseError::WrongLength { got, expected } => write!(
                f,
                "pose frame has {got} floats, this instance needs {expected} \
                 ({FLOATS_PER_GEOM} per geom x geom count)"
            ),
        }
    }
}

impl std::error::Error for PoseError {}

/// Apply one frame of world poses.
///
/// `poses` is `7 * geom_count` floats — `[px, py, pz, qw, qx, qy, qz]` per geom,
/// indexed by MuJoCo geom id — exactly the layout a capture frame carries and
/// exactly what a `mjData` read produces, so a producer never has to reshape.
///
/// Geoms with no node (hidden groups, unsupported kinds) are skipped; their
/// slots must still be present in the frame, because the index IS the geom id.
///
/// Does not touch the transform hierarchy beyond the geoms themselves — the
/// caller flushes world transforms once per frame as usual.
pub fn apply_geom_poses(
    renderer: &mut AwsmRenderer,
    instance: &MujocoInstance,
    poses: &[f32],
) -> Result<(), PoseError> {
    let expected = instance.frame_len();
    if poses.len() != expected {
        return Err(PoseError::WrongLength {
            got: poses.len(),
            expected,
        });
    }
    for (geom_id, key) in instance.geoms.iter().enumerate() {
        let Some(key) = key else { continue };
        let p = &poses[geom_id * FLOATS_PER_GEOM..(geom_id + 1) * FLOATS_PER_GEOM];
        // Read-modify-write so the node's authored SCALE survives: an ellipsoid
        // geom is a unit sphere scaled per axis, and a frame carries no scale.
        let scale = renderer
            .transforms
            .get_local(*key)
            .map(|t| t.scale)
            .unwrap_or(Vec3::ONE);
        let _ = renderer.transforms.set_local(
            *key,
            Transform {
                translation: Vec3::new(p[0], p[1], p[2]),
                // MuJoCo quaternions are [w, x, y, z]; glam's are [x, y, z, w].
                rotation: Quat::from_xyzw(p[4], p[5], p[6], p[3]),
                scale,
            },
        );
    }
    Ok(())
}

/// Find every sim instance in a loaded scene and resolve its geom binding.
///
/// `handles` is the loader's `NodeId → NodeHandles` map; the transform key of
/// each geom node comes from there, so a geom that failed to materialize is
/// `None` rather than a dangling reference.
pub fn resolve_instances(
    nodes: &[EditorNode],
    handles: &HashMap<NodeId, crate::NodeHandles>,
) -> Vec<MujocoInstance> {
    let mut out = Vec::new();
    for node in nodes {
        walk(node, handles, &mut out);
    }
    out
}

fn walk(
    node: &EditorNode,
    handles: &HashMap<NodeId, crate::NodeHandles>,
    out: &mut Vec<MujocoInstance>,
) {
    if let Some(MujocoComponent::Instance(inst)) = &node.mujoco {
        let mut geoms = vec![None; inst.geom_count as usize];
        collect_geoms(node, handles, &mut geoms);
        out.push(MujocoInstance {
            root: node.id,
            source: inst.source.clone(),
            model_name: inst.model_name.clone(),
            geoms,
        });
        // An instance never nests inside another, so stop descending here.
        return;
    }
    for child in &node.children {
        walk(child, handles, out);
    }
}

fn collect_geoms(
    node: &EditorNode,
    handles: &HashMap<NodeId, crate::NodeHandles>,
    geoms: &mut [Option<awsm_renderer::transforms::TransformKey>],
) {
    for child in &node.children {
        if let Some(MujocoComponent::Geom(g)) = &child.mujoco {
            if let Some(slot) = geoms.get_mut(g.geom_id as usize) {
                *slot = handles.get(&child.id).map(|h| h.transform);
            }
        }
        collect_geoms(child, handles, geoms);
    }
}
