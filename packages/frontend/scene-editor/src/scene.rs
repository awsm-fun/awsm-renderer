//! Editor scene model. Decoupled from any runtime game format — "Build"
//! packages a target-game artifact from a scene; everything else speaks
//! only this schema.
//!
//! Coordinate convention: right-handed, Y-up, meters.

pub mod assets;
pub mod mutate;
pub mod node;
pub mod snapshot;
pub mod types;

pub use assets::{AssetId, AssetSource, AssetTable};
pub use awsm_scene_schema::ShadowsConfig;
pub use node::{Node, NodeId};
pub use snapshot::SceneSnapshot;
pub use types::{
    AssetStatus, CameraConfig, CameraProjection, ColliderShape, EnvironmentConfig, IblConfig,
    LightConfig, LightKind, ModelRef, NodeKind, SkyboxConfig, Trs,
};

use crate::prelude::*;

/// Live, reactive scene. Held inside `AppState` as an `Arc<Scene>`.
pub struct Scene {
    pub environment: Mutable<EnvironmentConfig>,
    /// Renderer-wide shadow config. Editor mirrors the on-disk
    /// `EditorProject::shadows` block here; every change is pushed
    /// through `set_shadows_config` and takes effect on the next
    /// frame — resource-shape changes recreate the underlying
    /// textures + bind groups.
    pub shadows: Mutable<ShadowsConfig>,
    pub nodes: MutableVec<Arc<Node>>,
    /// Per-project asset table. Every `Model` node and every `Ktx`
    /// environment entry refers into this map by `AssetId`. Mutations
    /// bump `revision` so derived UI (unused-asset count, etc.) reacts.
    pub assets: Mutex<AssetTable>,
    /// Bumps on every mutation. Derived UI (e.g. stats) observes this to
    /// recompute in one place without needing per-field signals for every
    /// nested node.
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

    /// Call this after any mutation so that revision-derived signals tick.
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

impl SceneStats {
    pub fn format(&self) -> String {
        format!(
            "{} node{} · {} model · {} light · {} collision · {} camera · {} primitive · {} curve · {} sweep · {} instances · {} line · {} sprite · {} particle",
            self.nodes,
            if self.nodes == 1 { "" } else { "s" },
            self.models,
            self.lights,
            self.colliders,
            self.cameras,
            self.primitives,
            self.curves,
            self.sweeps,
            self.instances,
            self.lines,
            self.sprites,
            self.particles,
        )
    }
}
