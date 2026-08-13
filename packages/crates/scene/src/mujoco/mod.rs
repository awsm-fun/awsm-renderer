//! What a node remembers about the MuJoCo model it came from.
//!
//! This is the seam between a scene and an external simulator. The sim process
//! owns its MJCF/URDF outright — we never archive or transport it — so all a scene
//! keeps is enough to say *which* model an instance is, and which geom each node
//! stands for. A harness matches its loaded models to instances by fingerprint and
//! fails loudly on a mismatch, rather than silently driving the wrong robot.
//!
//! Lives on [`EditorNode`](crate::tree::EditorNode), which both the authoring
//! project and the baked runtime scene are built from — so the component reaches
//! the bundle by construction rather than by a second implementation.

pub mod bake;

/// Re-exported so consumers of the component don't need a direct dependency on
/// the interchange crate just to name the types the component is made of.
pub use awsm_renderer_mujoco_format::capture::{Capture, FLOATS_PER_GEOM};
pub use awsm_renderer_mujoco_format::sidecar::{
    GeomKind, Material as MujocoMaterial, Sidecar, Source,
};

/// A node's MuJoCo role. A node is an instance root or a geom, never both, so
/// this is one field rather than two optional ones.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum MujocoComponent {
    /// The root of one imported model. **User-placeable**: its transform is the
    /// model's initial placement in the composed world, and the Z-up→Y-up
    /// convention rotation rides on it too — so the pose stream can apply raw
    /// MuJoCo world poses to the geoms beneath with no per-frame conversion.
    Instance(MujocoInstance),
    /// One geom of the enclosing instance. Its transform is **stream-owned**:
    /// the editor locks the gizmo and the sim writes it every frame.
    Geom(MujocoGeom),
    /// One **site** of the enclosing instance — a massless marker frame.
    /// Stream-owned like a geom, but addressed by SITE id, which is a separate
    /// space; a site's pose in a geom's slot would drive the wrong node.
    Site(MujocoSite),
    /// One preallocated **segment** of a spatial tendon. A tendon's waypoint
    /// count changes as it wraps and unwraps, so segments are created once and
    /// the unused tail is hidden — never created per frame.
    TendonSegment(MujocoTendonSegment),
    /// A deformable's visible surface, imported at its initial-pose shape.
    Flex(MujocoFlex),
    /// A node bound to a MuJoCo BODY rather than a geom or site.
    ///
    /// Today these are the skin joints of a flex — a deformable is skinned to
    /// the bodies that move it, so those bodies need nodes to be joints of. The
    /// channel is deliberately general (any body, by id) rather than
    /// flex-specific: a body is a body, and mocap markers or attachment points
    /// would bind the same way.
    Body(MujocoBody),
}

/// The root of an imported MuJoCo model.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct MujocoInstance {
    /// Which model this is: source filename + content hash + the MuJoCo that
    /// compiled it. The whole basis of binding a running sim to this subtree.
    pub source: Source,
    /// The compiled model's own name, for display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    /// How many geoms the source model has — i.e. the id space the stream
    /// addresses, INCLUDING geoms this scene chose not to render.
    ///
    /// Stored because it cannot be recovered from the subtree: an import that
    /// skips hidden groups leaves gaps, and a stream frame sized to the visible
    /// nodes instead of the real geom count would be silently misaligned from
    /// the first skipped geom onward.
    pub geom_count: u32,
    /// How many sites the source model has — the id space the site channel
    /// addresses. Same reasoning as `geom_count`.
    #[serde(default)]
    pub site_count: u32,
    /// Per-tendon waypoint capacity, indexed by tendon id — the pool sizes the
    /// importer allocated. A stream frame's tendon channel is laid out against
    /// these, so a consumer never has to re-derive them from the tree.
    #[serde(default)]
    pub tendon_capacity: Vec<u32>,
    /// Per-flex surface vertex counts, indexed by flex id — the same role
    /// `tendon_capacity` plays, sizing a stream frame without walking the tree.
    #[serde(default)]
    pub flex_vertex_counts: Vec<u32>,
    /// How many bodies the source model has, so a body frame can be sized
    /// without walking the tree. Most bodies have no node; the id space is
    /// still the model's.
    #[serde(default)]
    pub body_count: u32,
    /// Which MuJoCo visibility groups this instance renders. Defaults to
    /// MuJoCo's own convention (0–2 visible), which is what keeps a menagerie
    /// model showing its visual meshes rather than its collision capsules.
    #[serde(default = "default_visible_groups")]
    pub visible_groups: Vec<i32>,
}

fn default_visible_groups() -> Vec<i32> {
    vec![0, 1, 2]
}

/// MuJoCo's own default: groups 0, 1 and 2 are visible.
pub const DEFAULT_VISIBLE_GROUPS: [i32; 3] = [0, 1, 2];

impl MujocoInstance {
    pub fn new(source: Source, geom_count: u32) -> Self {
        Self {
            source,
            model_name: None,
            geom_count,
            site_count: 0,
            tendon_capacity: Vec::new(),
            flex_vertex_counts: Vec::new(),
            body_count: 0,
            visible_groups: default_visible_groups(),
        }
    }
}

/// One geom of an instance.
///
/// Deliberately does NOT store a geom_id→node_id map on the instance: every geom
/// node already knows its own id, so the binding is resolved by walking the
/// subtree at load. A stored map would be a second copy free to drift out of sync
/// with the tree it describes — and drift here means poses applied to the wrong
/// nodes, which looks like a physics bug rather than a data bug.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct MujocoGeom {
    /// Index into the source model's geom array — the address the pose stream
    /// uses. Stable for a given compiled model, which is what the instance's
    /// fingerprint guarantees.
    pub geom_id: u32,
    /// The geom's MuJoCo visibility group, kept verbatim (not resolved against
    /// the instance's `visible_groups`) so toggling a group later is a display
    /// decision and not a re-import.
    pub group: i32,
    /// Shape kind, for display and for the inspector's read-only readout.
    pub kind: GeomKind,
    /// Owning MuJoCo body index. Not used to place anything — geom world poses
    /// arrive directly — but it is how a skin's joints and a harness's body-level
    /// queries line up with our nodes.
    pub body: u32,
}

/// One site of an instance. Deliberately its own component rather than a flag
/// on [`MujocoGeom`]: MuJoCo indexes sites separately from geoms, and a shared
/// id space would silently cross the two channels.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct MujocoSite {
    /// Index into the source model's SITE array.
    pub site_id: u32,
    /// The site's own visibility group (MuJoCo's `sitegroup`).
    pub group: i32,
    /// Draw shape.
    pub kind: GeomKind,
    /// Owning MuJoCo body index.
    pub body: u32,
}

/// Place one tendon segment: the unit-Z cylinder spanning waypoints `a` to `b`.
///
/// Returns `(translation, rotation, scale)`. The mesh is a unit cylinder along
/// local Z, so the span's length lives entirely in the Z scale — this is the one
/// piece of tendon geometry both the editor (initial pose) and the runtime pose
/// sink have to agree on, so it lives here rather than in either of them.
///
/// A zero-length span (two coincident waypoints — MuJoCo emits these for pulley
/// wrap points) collapses to a zero Z scale rather than producing a NaN
/// direction.
pub fn segment_transform(a: [f32; 3], b: [f32; 3], width: f32) -> ([f32; 3], [f32; 4], [f32; 3]) {
    let (a, b) = (glam::Vec3::from(a), glam::Vec3::from(b));
    let span = b - a;
    let len = span.length();
    let rotation = if len > 1e-9 {
        glam::Quat::from_rotation_arc(glam::Vec3::Z, span / len)
    } else {
        glam::Quat::IDENTITY
    };
    (
        ((a + b) * 0.5).to_array(),
        rotation.to_array(),
        [width, width, len * 0.5],
    )
}

/// A flex — MuJoCo's deformable — as a single surface mesh node.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct MujocoFlex {
    /// Index into the source model's FLEX array — a fourth id space.
    pub flex_id: u32,
    /// The flex's visibility group (MuJoCo's `flexgroup`).
    pub group: i32,
    /// How many vertices the surface has, so a consumer can size a vertex
    /// frame without re-reading the mesh asset.
    pub vertex_count: u32,
    /// Whether every vertex is rigidly attached to a body. A flex that
    /// interpolates its vertices from a cage of nodes is not, and can only be
    /// deformed by streaming positions.
    pub body_attached: bool,
}

/// A node driven by a MuJoCo body's world pose.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct MujocoBody {
    /// Index into the source model's BODY array — a fifth id space.
    pub body_id: u32,
}

/// One segment of a tendon's preallocated chain.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct MujocoTendonSegment {
    /// Index into the source model's TENDON array — a third id space alongside
    /// geoms and sites.
    pub tendon_id: u32,
    /// Which segment of that tendon's chain this is: the span from waypoint
    /// `segment` to `segment + 1`.
    pub segment: u32,
    /// The tendon's visibility group (MuJoCo's `tendongroup`).
    pub group: i32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::{EditorNode, NodeId, NodeKind};

    #[test]
    fn a_segment_spans_its_two_waypoints() {
        let a = [1.0, 2.0, 3.0];
        let b = [1.0, 2.0, 5.0];
        let (t, r, s) = segment_transform(a, b, 0.01);
        assert_eq!(t, [1.0, 2.0, 4.0], "sits at the midpoint");
        assert!((s[2] - 1.0).abs() < 1e-6, "half the 2m span: {s:?}");
        assert_eq!([s[0], s[1]], [0.01, 0.01], "width is radial");
        // Already along +Z, so the rotation is a no-op; check by transporting the
        // mesh's own tip and landing on the far waypoint.
        let tip = glam::Quat::from_array(r) * glam::Vec3::new(0.0, 0.0, s[2]) + glam::Vec3::from(t);
        assert!(tip.abs_diff_eq(glam::Vec3::from(b), 1e-5), "{tip:?}");
    }

    #[test]
    fn a_segment_rotates_onto_an_arbitrary_span() {
        let a = [0.0, 0.0, 0.0];
        let b = [0.3, -0.4, 0.0];
        let (t, r, s) = segment_transform(a, b, 0.02);
        let tip = glam::Quat::from_array(r) * glam::Vec3::new(0.0, 0.0, s[2]) + glam::Vec3::from(t);
        assert!(tip.abs_diff_eq(glam::Vec3::from(b), 1e-5), "{tip:?}");
        assert!((s[2] - 0.25).abs() < 1e-6, "half of the 0.5m span: {s:?}");
    }

    #[test]
    fn coincident_waypoints_collapse_instead_of_going_nan() {
        // MuJoCo emits these for pulley wrap points; a normalize() here would
        // poison the whole node transform.
        let (t, r, s) = segment_transform([0.5; 3], [0.5; 3], 0.01);
        assert!(t.iter().chain(&r).chain(&s).all(|v| v.is_finite()));
        assert_eq!(s[2], 0.0);
    }

    fn source() -> Source {
        Source {
            filename: "go2.xml".into(),
            sha256: "a".repeat(64),
            mujoco_version: "3.11.0".into(),
        }
    }

    fn node(mujoco: Option<MujocoComponent>) -> EditorNode {
        EditorNode {
            physics: None,
            id: NodeId::new(),
            name: "n".into(),
            transform: Default::default(),
            kind: NodeKind::Group,
            locked: false,
            visible: true,
            prefab: false,
            mujoco,
            children: vec![],
        }
    }

    /// The component has to survive BOTH formats the plan's parity checklist
    /// names: the authoring project (toml) and the baked runtime scene (toml,
    /// and json for the editor's own transport).
    #[test]
    fn round_trips_through_toml_and_json() {
        for c in [
            MujocoComponent::Instance(MujocoInstance::new(source(), 56)),
            MujocoComponent::Geom(MujocoGeom {
                geom_id: 12,
                group: 2,
                kind: GeomKind::Mesh,
                body: 3,
            }),
        ] {
            let n = node(Some(c));
            let json = serde_json::to_string(&n).unwrap();
            assert_eq!(serde_json::from_str::<EditorNode>(&json).unwrap(), n);
            let toml_s = toml::to_string(&n).unwrap();
            assert_eq!(toml::from_str::<EditorNode>(&toml_s).unwrap(), n);
        }
    }

    /// Every project and bundle written before this feature has no `mujoco` key.
    /// They must load, not error.
    #[test]
    fn absent_on_legacy_nodes() {
        let json = r#"{
            "id": "00000000-0000-0000-0000-000000000001",
            "name": "old",
            "kind": "group"
        }"#;
        let n: EditorNode = serde_json::from_str(json).unwrap();
        assert_eq!(n.mujoco, None);
        // ...and a node without the component must not write the key either, so
        // existing files round-trip byte-comparably rather than growing noise.
        assert!(!serde_json::to_string(&n).unwrap().contains("mujoco"));
    }

    /// Pins the on-disk shape. These key names are what a saved project, a baked
    /// bundle, and any third-party tool that writes one all agree on, so a serde
    /// rename here is a compatibility break and should have to be typed out.
    #[test]
    fn the_on_disk_shape_is_stable() {
        let mut n = node(Some(MujocoComponent::Instance(MujocoInstance {
            model_name: Some("go2".into()),
            ..MujocoInstance::new(source(), 56)
        })));
        n.id = NodeId(uuid::Uuid::nil());
        n.name = "go2".into();
        let toml_s = toml::to_string(&n).unwrap();
        for expected in [
            "[mujoco.instance]",
            "geom_count = 56",
            "model_name = \"go2\"",
            "[mujoco.instance.source]",
            "filename = \"go2.xml\"",
            "mujoco_version = \"3.11.0\"",
        ] {
            assert!(
                toml_s.contains(expected),
                "missing {expected} in:\n{toml_s}"
            );
        }

        let g = node(Some(MujocoComponent::Geom(MujocoGeom {
            geom_id: 12,
            group: 2,
            kind: GeomKind::Capsule,
            body: 3,
        })));
        let toml_g = toml::to_string(&g).unwrap();
        for expected in [
            "[mujoco.geom]",
            "geom_id = 12",
            "group = 2",
            "kind = \"capsule\"",
            "body = 3",
        ] {
            assert!(
                toml_g.contains(expected),
                "missing {expected} in:\n{toml_g}"
            );
        }
    }

    #[test]
    fn visible_groups_default_to_mujocos_convention() {
        // An instance written without the key (or by a third-party tool) must
        // land on 0-2 — NOT on "all groups", which would render menagerie
        // collision capsules over the robot.
        let json = format!(
            r#"{{"instance":{{"source":{},"geom_count":56}}}}"#,
            serde_json::to_string(&source()).unwrap()
        );
        let c: MujocoComponent = serde_json::from_str(&json).unwrap();
        let MujocoComponent::Instance(i) = c else {
            panic!("expected an instance")
        };
        assert_eq!(i.visible_groups, DEFAULT_VISIBLE_GROUPS);
    }
}
