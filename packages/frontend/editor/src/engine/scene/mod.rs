//! Editor scene model — a live, reactive tree (`Mutable`/`MutableVec`) held by
//! the `EditorController`. UI-agnostic; adapted from the archived editor. The
//! old snapshot-history module is dropped — undo/redo is command-sourcing in the
//! controller now. Coordinate convention: right-handed, Y-up, meters.

// The scene model is the foundation consumed incrementally by the panels +
// renderer bridge + persistence across M4–M11; allow not-yet-wired surface.
#![allow(dead_code)]

pub mod assets;
pub mod mutate;
pub mod node;
pub mod types;

// The full scene-leaf surface is re-exported for the panels + persistence that
// land in M4–M11; allow the not-yet-consumed names now.
#[allow(unused_imports)]
pub use assets::{AssetId, AssetSource, AssetTable};
pub use awsm_scene_schema::ShadowsConfig;
pub use node::{Node, NodeId};
#[allow(unused_imports)]
pub use types::{
    AssetStatus, CameraConfig, CameraProjection, ColliderShape, EnvironmentConfig, IblConfig,
    LightConfig, LightKind, ModelRef, NodeKind, SkyboxConfig, Trs,
};

use crate::prelude::*;

/// Live, reactive scene. Held inside the `EditorController` as an `Arc<Scene>`.
pub struct Scene {
    pub environment: Mutable<EnvironmentConfig>,
    pub shadows: Mutable<ShadowsConfig>,
    pub nodes: MutableVec<Arc<Node>>,
    /// Per-project asset table. Every `Model` node + env entry refers into this
    /// by `AssetId`. Mutations bump `revision` so derived UI reacts.
    pub assets: Mutex<AssetTable>,
    /// Bumps on every mutation so revision-derived UI (stats etc.) recomputes.
    pub revision: Mutable<u64>,
}

impl Scene {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            environment: Mutable::new(EnvironmentConfig::default()),
            shadows: Mutable::new(ShadowsConfig::default()),
            nodes: MutableVec::new(),
            assets: Mutex::new(AssetTable::new()),
            revision: Mutable::new(0),
        })
    }

    /// Call after any mutation so revision-derived signals tick.
    pub fn bump_revision(&self) {
        self.revision.set(self.revision.get().wrapping_add(1));
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.lock_ref().is_empty()
    }

    pub fn stats(&self) -> SceneStats {
        fn walk(nodes: &[Arc<Node>], stats: &mut SceneStats) {
            for node in nodes {
                stats.nodes += 1;
                match &*node.kind.lock_ref() {
                    NodeKind::Group => stats.groups += 1,
                    NodeKind::Model(_) => stats.models += 1,
                    NodeKind::Light(_) => stats.lights += 1,
                    NodeKind::Collider(_) => stats.colliders += 1,
                    NodeKind::Camera(_) => stats.cameras += 1,
                    NodeKind::Primitive { .. } | NodeKind::Mesh { .. } => stats.primitives += 1,
                    NodeKind::Curve(_) => stats.curves += 1,
                    NodeKind::SweepAlongCurve { .. } => stats.sweeps += 1,
                    NodeKind::InstancesAlongCurve(_) => stats.instances += 1,
                    NodeKind::Line(_) => stats.lines += 1,
                    NodeKind::Sprite(_) => stats.sprites += 1,
                    NodeKind::ParticleEmitter(_) => stats.particles += 1,
                    NodeKind::Decal(_) => stats.decals += 1,
                }
                let children = node.children.lock_ref();
                walk(children.as_slice(), stats);
            }
        }
        let mut stats = SceneStats::default();
        let nodes = self.nodes.lock_ref();
        walk(nodes.as_slice(), &mut stats);
        stats
    }
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SceneStats {
    pub nodes: usize,
    pub groups: usize,
    pub models: usize,
    pub lights: usize,
    pub colliders: usize,
    pub cameras: usize,
    pub primitives: usize,
    pub curves: usize,
    pub sweeps: usize,
    pub instances: usize,
    pub lines: usize,
    pub sprites: usize,
    pub particles: usize,
    pub decals: usize,
}
