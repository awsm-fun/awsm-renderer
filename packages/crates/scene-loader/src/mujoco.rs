//! The **pose sink**: the renderer's entire contract with an external simulator.
//!
//! This is the whole of the MuJoCo runtime surface in this repo. There is no
//! networking here, no transport, no timing source, and no MuJoCo code — just a
//! binding resolved at load and a function that writes a frame of world poses
//! onto node transforms. Anything that can call it drives the renderer: the
//! reference template's wasm worker, a native harness over a websocket, a test
//! feeding a recorded capture. Running the simulation and getting frames here is
//! the player's job (see `docs/mujoco.md`).
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
use awsm_renderer_scene::mujoco::{segment_transform, MujocoComponent, Source};
use awsm_renderer_scene::tree::{EditorNode, NodeId};
use glam::{Quat, Vec3};

pub use awsm_renderer_scene::mujoco::FLOATS_PER_GEOM;

/// Floats per tendon waypoint: `[x, y, z]`. A waypoint is a point on a cable,
/// not a frame, so it carries no rotation.
pub const FLOATS_PER_WAYPOINT: usize = 3;

/// One loaded sim instance, ready to be driven.
///
/// Resolved at load by walking the instance's subtree — never read from a stored
/// map, which would be a second copy free to drift from the tree it describes.
#[derive(Debug, Clone)]
pub struct MujocoInstance {
    /// The instance root's node id. Its transform is the model's authored
    /// placement (plus the convention rotation) and is **not** stream-driven.
    pub root: NodeId,
    /// The instance root's transform key.
    ///
    /// Exposed so a consumer can parent its OWN nodes into the sim's frame —
    /// a debug overlay drawing contact points, say. Anything under here can use
    /// raw MuJoCo world coordinates verbatim, exactly as the geom nodes do,
    /// instead of duplicating the convention rotation and drifting from it.
    pub root_transform: Option<awsm_renderer::transforms::TransformKey>,
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
    /// `site_id → ` that site's transform. A SEPARATE array from `geoms`
    /// because MuJoCo indexes sites separately; sharing one would put a site's
    /// pose in a geom's slot.
    pub sites: Vec<Option<awsm_renderer::transforms::TransformKey>>,
    /// `tendon_id → ` that tendon's segment pool. A third id space again.
    pub tendons: Vec<TendonSlots>,
    /// `body_id → ` that body's transform. Sparse by nature: most bodies have
    /// no node, and the ones that do are a flex's skin joints.
    pub bodies: Vec<Option<awsm_renderer::transforms::TransformKey>>,
}

/// One tendon's preallocated chain of segment nodes.
///
/// The pool exists because a tendon's waypoint count changes as it wraps around
/// geometry, and this sink can only write to nodes that already exist. Segments
/// beyond the current waypoint count are hidden rather than destroyed.
#[derive(Debug, Clone, Default)]
pub struct TendonSlots {
    /// The most waypoints this tendon can ever have (the importer's pool size).
    /// Zero for a FIXED tendon, which has no path through space to draw.
    pub capacity: u32,
    /// `segment[i]` spans waypoint `i` → `i + 1`, so there is one fewer of these
    /// than the capacity.
    segments: Vec<Option<Segment>>,
    /// How many segments are currently un-hidden. Kept so visibility is
    /// **edge-triggered**: `set_mesh_hidden` bumps the TLAS revision and
    /// re-syncs the spatial index, so calling it every frame for every segment
    /// would churn the BVH for nothing.
    shown: u32,
}

impl TendonSlots {
    /// Floats one frame must carry for this tendon: a count, then a slot for
    /// every waypoint the pool can hold.
    pub fn frame_len(&self) -> usize {
        1 + self.capacity as usize * FLOATS_PER_WAYPOINT
    }
}

#[derive(Debug, Clone)]
struct Segment {
    transform: awsm_renderer::transforms::TransformKey,
    meshes: Vec<awsm_renderer::meshes::MeshKey>,
}

impl MujocoInstance {
    /// How many floats one GEOM frame must carry for this instance.
    pub fn frame_len(&self) -> usize {
        self.geoms.len() * FLOATS_PER_GEOM
    }

    /// How many floats one SITE frame must carry. Zero for a model with no
    /// sites, which is most robots.
    pub fn site_frame_len(&self) -> usize {
        self.sites.len() * FLOATS_PER_GEOM
    }

    /// How many floats one BODY frame must carry. Bodies drive a deformable's
    /// skin joints — a flex is skinned to the bodies that move it — so this is
    /// how a soft body deforms without a single vertex crossing the wire.
    pub fn body_frame_len(&self) -> usize {
        self.bodies.len() * FLOATS_PER_GEOM
    }

    /// How many floats one TENDON frame must carry: per tendon, a live waypoint
    /// count followed by that tendon's full waypoint capacity. Fixed size even
    /// though the live count varies, so a producer can publish it into a
    /// preallocated shared buffer.
    pub fn tendon_frame_len(&self) -> usize {
        self.tendons.iter().map(TendonSlots::frame_len).sum()
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
    WrongLength {
        got: usize,
        expected: usize,
    },
    /// A tendon frame claimed more live waypoints than the pool can hold. Not
    /// clamped silently: it means the producer and the imported model disagree
    /// about the model, and the rest of the frame is therefore untrustworthy.
    TooManyWaypoints {
        tendon: usize,
        got: usize,
        capacity: u32,
    },
}

impl std::fmt::Display for PoseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PoseError::WrongLength { got, expected } => write!(
                f,
                "frame has {got} floats, this channel needs {expected} for this instance"
            ),
            PoseError::TooManyWaypoints {
                tendon,
                got,
                capacity,
            } => write!(
                f,
                "tendon {tendon} reported {got} waypoints but its pool holds {capacity}"
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
    write_poses(renderer, &instance.geoms, poses)
}

fn write_poses(
    renderer: &mut AwsmRenderer,
    slots: &[Option<awsm_renderer::transforms::TransformKey>],
    poses: &[f32],
) -> Result<(), PoseError> {
    let expected = slots.len() * FLOATS_PER_GEOM;
    if poses.len() != expected {
        return Err(PoseError::WrongLength {
            got: poses.len(),
            expected,
        });
    }
    for (geom_id, key) in slots.iter().enumerate() {
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

/// Apply one frame of site world poses — the optional site channel.
///
/// Same layout and same rules as [`apply_geom_poses`], indexed by **site** id.
/// A producer that has no sites simply never calls this.
pub fn apply_site_poses(
    renderer: &mut AwsmRenderer,
    instance: &MujocoInstance,
    poses: &[f32],
) -> Result<(), PoseError> {
    write_poses(renderer, &instance.sites, poses)
}

/// Apply one frame of BODY world poses — the channel that deforms flexes.
///
/// Same layout and rules as [`apply_geom_poses`], indexed by **body** id. A
/// model with no body-bound nodes needs it never.
pub fn apply_body_poses(
    renderer: &mut AwsmRenderer,
    instance: &MujocoInstance,
    poses: &[f32],
) -> Result<(), PoseError> {
    write_poses(renderer, &instance.bodies, poses)
}

/// Apply one frame of tendon waypoints — the optional tendon channel.
///
/// The frame is, per tendon in id order: **one live waypoint count**, then that
/// tendon's full waypoint capacity as `[x, y, z]` triples (see
/// [`MujocoInstance::tendon_frame_len`]). Slots past the live count are ignored,
/// so a producer can leave stale values there. A producer reads this straight
/// out of `mjData`'s `ten_wrapadr`/`ten_wrapnum`/`wrap_xpos`.
///
/// Takes `&mut` on the instance because segment visibility is edge-triggered:
/// hiding a mesh re-syncs the spatial index, so the sink remembers how many
/// segments it last showed rather than re-asserting the state every frame.
///
/// Each live segment is placed spanning consecutive waypoints; its authored
/// RADIUS (the node's x/y scale) is preserved, since a frame carries no width.
pub fn apply_tendon_waypoints(
    renderer: &mut AwsmRenderer,
    instance: &mut MujocoInstance,
    frame: &[f32],
) -> Result<(), PoseError> {
    let expected = instance.tendon_frame_len();
    if frame.len() != expected {
        return Err(PoseError::WrongLength {
            got: frame.len(),
            expected,
        });
    }
    let mut base = 0usize;
    for (tendon_id, slots) in instance.tendons.iter_mut().enumerate() {
        let stride = slots.frame_len();
        let block = &frame[base..base + stride];
        base += stride;
        if slots.capacity == 0 {
            continue;
        }
        let live = block[0] as usize;
        if live > slots.capacity as usize {
            return Err(PoseError::TooManyWaypoints {
                tendon: tendon_id,
                got: live,
                capacity: slots.capacity,
            });
        }
        let waypoint = |i: usize| -> [f32; 3] {
            let o = 1 + i * FLOATS_PER_WAYPOINT;
            [block[o], block[o + 1], block[o + 2]]
        };
        // A path of N waypoints draws N-1 segments; 0 or 1 waypoints draws none.
        let active = live.saturating_sub(1) as u32;
        for (i, segment) in slots.segments.iter().enumerate() {
            let Some(segment) = segment else { continue };
            if (i as u32) < active {
                let (translation, rotation, span) =
                    segment_transform(waypoint(i), waypoint(i + 1), 1.0);
                let radius = renderer
                    .transforms
                    .get_local(segment.transform)
                    .map(|t| t.scale)
                    .unwrap_or(Vec3::ONE);
                let _ = renderer.transforms.set_local(
                    segment.transform,
                    Transform {
                        translation: Vec3::from(translation),
                        rotation: Quat::from_array(rotation),
                        scale: Vec3::new(radius.x, radius.y, span[2]),
                    },
                );
            }
            // Only the segments crossing the show/hide boundary this frame get
            // touched — see `TendonSlots::shown`.
            let was = (i as u32) < slots.shown;
            let now = (i as u32) < active;
            if was != now {
                for mesh in &segment.meshes {
                    let _ = renderer.set_mesh_hidden(*mesh, !now);
                }
            }
        }
        slots.shown = active;
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
        let mut sites = vec![None; inst.site_count as usize];
        let mut bodies = vec![None; inst.body_count as usize];
        // Pool sizes come from the instance, not from counting nodes: a hidden
        // group can leave a pool with no nodes at all, and the frame layout
        // still has to reserve its slots.
        let mut tendons: Vec<TendonSlots> = inst
            .tendon_capacity
            .iter()
            .map(|capacity| TendonSlots {
                capacity: *capacity,
                segments: vec![None; capacity.saturating_sub(1) as usize],
                shown: 0,
            })
            .collect();
        collect_geoms(
            node,
            handles,
            &mut geoms,
            &mut sites,
            &mut tendons,
            &mut bodies,
        );
        out.push(MujocoInstance {
            root: node.id,
            root_transform: handles.get(&node.id).map(|h| h.transform),
            source: inst.source.clone(),
            model_name: inst.model_name.clone(),
            geoms,
            sites,
            tendons,
            bodies,
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
    sites: &mut [Option<awsm_renderer::transforms::TransformKey>],
    tendons: &mut [TendonSlots],
    bodies: &mut [Option<awsm_renderer::transforms::TransformKey>],
) {
    for child in &node.children {
        match &child.mujoco {
            Some(MujocoComponent::Geom(g)) => {
                if let Some(slot) = geoms.get_mut(g.geom_id as usize) {
                    *slot = handles.get(&child.id).map(|h| h.transform);
                }
            }
            Some(MujocoComponent::Site(s)) => {
                if let Some(slot) = sites.get_mut(s.site_id as usize) {
                    *slot = handles.get(&child.id).map(|h| h.transform);
                }
            }
            Some(MujocoComponent::TendonSegment(t)) => {
                if let Some(slots) = tendons.get_mut(t.tendon_id as usize) {
                    if let Some(slot) = slots.segments.get_mut(t.segment as usize) {
                        *slot = handles.get(&child.id).map(|h| Segment {
                            transform: h.transform,
                            meshes: h.meshes.clone(),
                        });
                        // The importer parks spare segments hidden. Seed `shown`
                        // from what was authored so the first frame's
                        // edge-trigger compares against the truth.
                        if child.visible {
                            slots.shown = slots.shown.max(t.segment + 1);
                        }
                    }
                }
            }
            Some(MujocoComponent::Body(b)) => {
                if let Some(slot) = bodies.get_mut(b.body_id as usize) {
                    *slot = handles.get(&child.id).map(|h| h.transform);
                }
            }
            _ => {}
        }
        collect_geoms(child, handles, geoms, sites, tendons, bodies);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slots(capacity: u32) -> TendonSlots {
        TendonSlots {
            capacity,
            segments: vec![None; capacity.saturating_sub(1) as usize],
            shown: 0,
        }
    }

    #[test]
    fn a_tendon_frame_reserves_a_count_plus_the_whole_pool() {
        // The layout a producer has to agree with, spelled out: the frame is
        // fixed-size even though the live waypoint count varies, so it can be
        // published into a preallocated shared buffer.
        assert_eq!(slots(6).frame_len(), 1 + 6 * 3);
        // A fixed tendon still occupies its slot — the index IS the tendon id.
        assert_eq!(slots(0).frame_len(), 1);
    }

    #[test]
    fn an_instances_tendon_frame_is_the_sum_of_its_tendons() {
        let inst = MujocoInstance {
            root: awsm_renderer_scene::tree::NodeId::new(),
            root_transform: None,
            source: Source {
                filename: "a.xml".into(),
                sha256: "x".into(),
                mujoco_version: "3.11.0".into(),
            },
            model_name: None,
            geoms: Vec::new(),
            sites: Vec::new(),
            bodies: Vec::new(),
            // arm26's shape, plus a fixed tendon to prove zero-capacity slots
            // are still counted.
            tendons: vec![slots(6), slots(6), slots(10), slots(0)],
        };
        assert_eq!(inst.tendon_frame_len(), 19 + 19 + 31 + 1);
    }

    #[test]
    fn a_pool_holds_one_fewer_segment_than_waypoint() {
        // Off-by-one here would silently drop a tendon's last span.
        assert_eq!(slots(6).segments.len(), 5);
        assert_eq!(slots(1).segments.len(), 0);
        assert_eq!(slots(0).segments.len(), 0);
    }
}
