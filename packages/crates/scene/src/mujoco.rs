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

/// Re-exported so consumers of the component don't need a direct dependency on
/// the interchange crate just to name the types the component is made of.
pub use awsm_renderer_mujoco_format::sidecar::{GeomKind, Source};

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::{EditorNode, NodeId, NodeKind};

    fn source() -> Source {
        Source {
            filename: "go2.xml".into(),
            sha256: "a".repeat(64),
            mujoco_version: "3.11.0".into(),
        }
    }

    fn node(mujoco: Option<MujocoComponent>) -> EditorNode {
        EditorNode {
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
