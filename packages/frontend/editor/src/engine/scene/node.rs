//! In-memory `Node`: a tree entry with reactive fields.
//!
//! Each field that drives the UI is wrapped in `Mutable<T>` / `MutableVec<T>`.
//! Nodes are always held behind `Arc`, so the same node handle can be referenced
//! by the tree, by the selection, and by any observing UI. Identity is a `NodeId`
//! (UUID) that's stable across edits, reorders, and Save/Load roundtrips.

use crate::engine::scene::types::{AssetStatus, LightConfig, LightKind, NodeKind, Trs};
use crate::prelude::*;

pub use awsm_renderer_editor_protocol::NodeId;

pub struct Node {
    pub id: NodeId,
    pub name: Mutable<String>,
    pub transform: Mutable<Trs>,
    pub kind: Mutable<NodeKind>,
    pub children: MutableVec<Arc<Node>>,
    /// UI-only: whether this subtree is expanded in the tree view.
    /// In-memory only; never serialized.
    pub expanded: Mutable<bool>,
    /// UI-only: renderer-side load state for `Model` nodes. Updated by
    /// `renderer_bridge`. `Idle` for non-Model nodes.
    pub asset_status: Mutable<AssetStatus>,
    /// When true, the row refuses pointer selection + drag start. Still
    /// serves as a valid drop target and still shows the context menu so
    /// the user can unlock it. Persists across save/load.
    pub locked: Mutable<bool>,
    /// Per-node visibility from the eye toggle. The renderer-bridge
    /// observes this and translates it into kind-specific renderer
    /// effects (mesh-hide for Model, intensity-zero for Light, skip for
    /// Collision wireframe). Group toggles propagate to descendants.
    /// Persists across save/load.
    pub visible: Mutable<bool>,
    /// When true, this node is the root of a prefab subtree. The editor
    /// renders it like any other node; the marker is a runtime concern
    /// for the player (which skips prefab subtrees during scene-build
    /// and exposes them for on-demand instantiation by per-game code).
    /// The flag is **root-only** — descendants don't inherit it, and
    /// any descendant may itself be marked to create a nested prefab.
    /// Persists across save/load.
    pub prefab: Mutable<bool>,
}

impl Node {
    fn new_inner(name: impl Into<String>, kind: NodeKind) -> Arc<Self> {
        Arc::new(Self {
            id: NodeId::new(),
            name: Mutable::new(name.into()),
            transform: Mutable::new(Trs::IDENTITY),
            kind: Mutable::new(kind),
            children: MutableVec::new(),
            expanded: Mutable::new(true),
            asset_status: Mutable::new(AssetStatus::Idle),
            locked: Mutable::new(false),
            visible: Mutable::new(true),
            prefab: Mutable::new(false),
        })
    }

    pub fn new_group(name: impl Into<String>) -> Arc<Self> {
        Self::new_inner(name, NodeKind::Group)
    }

    pub fn new_light(kind: LightKind) -> Arc<Self> {
        let label = match kind {
            LightKind::Directional => "Directional Light",
            LightKind::Point => "Point Light",
            LightKind::Spot => "Spot Light",
        };
        let node = Self::new_inner(label, NodeKind::Light(LightConfig::default_for(kind)));
        // Identity rotation aims a directional/spot beam straight along -Z
        // (horizontal), which grazes a ground plane and casts no visible
        // shadow. Tilt new directional/spot lights down ~50° so they light and
        // shadow the scene out of the box — the convention for a key light from
        // above. Point lights are omnidirectional, so they're left untouched.
        if matches!(kind, LightKind::Directional | LightKind::Spot) {
            let mut trs = node.transform.get();
            trs.rotation = glam::Quat::from_rotation_x(-50f32.to_radians()).to_array();
            node.transform.set(trs);
        }
        node
    }

    pub fn new_collision_box(name: impl Into<String>) -> Arc<Self> {
        Self::new_inner(
            name,
            NodeKind::Collider(crate::engine::scene::types::ColliderShape::default_box()),
        )
    }

    pub fn new_collision_sphere(name: impl Into<String>) -> Arc<Self> {
        Self::new_inner(
            name,
            NodeKind::Collider(crate::engine::scene::types::ColliderShape::default_sphere()),
        )
    }

    pub fn new_collision_capsule(name: impl Into<String>) -> Arc<Self> {
        Self::new_inner(
            name,
            NodeKind::Collider(crate::engine::scene::types::ColliderShape::default_capsule()),
        )
    }

    pub fn new_collision_cylinder(name: impl Into<String>) -> Arc<Self> {
        Self::new_inner(
            name,
            NodeKind::Collider(crate::engine::scene::types::ColliderShape::default_cylinder()),
        )
    }

    pub fn new_collision_cone(name: impl Into<String>) -> Arc<Self> {
        Self::new_inner(
            name,
            NodeKind::Collider(crate::engine::scene::types::ColliderShape::default_cone()),
        )
    }

    pub fn new_collision_ellipsoid(name: impl Into<String>) -> Arc<Self> {
        Self::new_inner(
            name,
            NodeKind::Collider(crate::engine::scene::types::ColliderShape::default_ellipsoid()),
        )
    }

    pub fn new_camera(name: impl Into<String>) -> Arc<Self> {
        Self::new_inner(
            name,
            NodeKind::Camera(crate::engine::scene::types::CameraConfig::default_perspective()),
        )
    }

    // ─────────────────────────────────────────────────────────────────────
    // Procedural-node constructors.
    //
    // Each variant uses the schema's `Default::default()` for both the
    // inline material and the kind-specific def, so the inserted node
    // renders out of the box. The user can then tweak knobs via the
    // inspector. `material: None` on the variants that carry it keeps
    // them on the inline-material path until the asset-table editor is
    // used to create a shared `AssetSource::Material(MaterialDef)`.
    // ─────────────────────────────────────────────────────────────────────

    pub fn new_curve(name: impl Into<String>) -> Arc<Self> {
        Self::new_inner(
            name,
            NodeKind::Curve(awsm_renderer_editor_protocol::CurveDef::default()),
        )
    }

    pub fn new_line(name: impl Into<String>) -> Arc<Self> {
        Self::new_inner(
            name,
            NodeKind::Line(awsm_renderer_editor_protocol::LineDef::default()),
        )
    }

    pub fn new_sprite(name: impl Into<String>) -> Arc<Self> {
        Self::new_inner(
            name,
            NodeKind::Sprite(awsm_renderer_editor_protocol::SpriteDef::default()),
        )
    }

    pub fn new_particle(name: impl Into<String>) -> Arc<Self> {
        Self::new_inner(
            name,
            NodeKind::ParticleEmitter(awsm_renderer_editor_protocol::ParticleEmitterDef::default()),
        )
    }

    pub fn new_decal(name: impl Into<String>) -> Arc<Self> {
        Self::new_inner(
            name,
            NodeKind::Decal(awsm_renderer_editor_protocol::DecalConfig::default()),
        )
    }

    /// The user picks the curve node + the source mesh node via the inspector
    /// after insert.
    pub fn new_instances(name: impl Into<String>) -> Arc<Self> {
        Self::new_inner(
            name,
            NodeKind::InstancesAlongCurve(
                awsm_renderer_editor_protocol::InstancesAlongCurveDef::default(),
            ),
        )
    }

    /// Explicit instancer: the user picks the mesh source via the inspector
    /// (or MCP `patch_kind`) after insert; the transform list is authored via
    /// `SetInstancerTransforms`.
    pub fn new_instancer(name: impl Into<String>) -> Arc<Self> {
        Self::new_inner(
            name,
            NodeKind::Instancer(awsm_renderer_editor_protocol::InstancerDef::default()),
        )
    }

    /// `Mesh` references an `AssetSource::Mesh(MeshDef)` by `MeshRef`.
    /// Until the asset-table editor lands, the user picks the ref via
    /// the inspector.
    pub fn new_mesh(name: impl Into<String>) -> Arc<Self> {
        Self::new_inner(
            name,
            NodeKind::Mesh {
                mesh: awsm_renderer_editor_protocol::MeshRef(
                    awsm_renderer_editor_protocol::AssetId::new(),
                ),
                material_variants: Vec::new(),
                selected_variant: None,
                shadow: Default::default(),
                lod: Default::default(),
            },
        )
    }

    /// Build a node with a custom transform and kind in one shot. Used by
    /// `Insert Model` to mirror a gltf node's local transform on the
    /// matching editor `Node`.
    pub fn new_with_transform_and_kind(
        name: impl Into<String>,
        transform: Trs,
        kind: NodeKind,
    ) -> Arc<Self> {
        Arc::new(Self {
            id: NodeId::new(),
            name: Mutable::new(name.into()),
            transform: Mutable::new(transform),
            kind: Mutable::new(kind),
            children: MutableVec::new(),
            expanded: Mutable::new(true),
            asset_status: Mutable::new(AssetStatus::Idle),
            locked: Mutable::new(false),
            visible: Mutable::new(true),
            prefab: Mutable::new(false),
        })
    }

    /// Deep-copy this node with fresh UUIDs at every level. Use this for
    /// `Duplicate`: the new subtree is fully independent of the original.
    pub fn deep_clone_with_new_ids(&self) -> Arc<Self> {
        self.deep_clone_inner(None)
    }

    /// Like [`deep_clone_with_new_ids`], but the cloned **root** takes `root_id`
    /// (descendants still get fresh ids). Lets a caller mint the root id up front
    /// so `duplicate_node` can echo it back to the agent (§6).
    pub fn deep_clone_with_root_id(&self, root_id: NodeId) -> Arc<Self> {
        self.deep_clone_inner(Some(root_id))
    }

    fn deep_clone_inner(&self, root_id: Option<NodeId>) -> Arc<Self> {
        Arc::new(Self {
            // `NodeId::new()` mints a fresh UUID — not a `Default`, so spell out
            // the fallback rather than `unwrap_or_default`.
            id: match root_id {
                Some(id) => id,
                None => NodeId::new(),
            },
            name: Mutable::new(self.name.get_cloned()),
            transform: Mutable::new(self.transform.get()),
            kind: Mutable::new(self.kind.get_cloned()),
            children: MutableVec::new_with_values(
                self.children
                    .lock_ref()
                    .iter()
                    .map(|child| child.deep_clone_with_new_ids())
                    .collect(),
            ),
            expanded: Mutable::new(self.expanded.get()),
            asset_status: Mutable::new(AssetStatus::Idle),
            locked: Mutable::new(self.locked.get()),
            visible: Mutable::new(self.visible.get()),
            prefab: Mutable::new(self.prefab.get()),
        })
    }
}
