use awsm_renderer_editor_protocol::BundleOptions;

use crate::prelude::*;

use super::{AssetTable, EnvironmentConfig, Node, NodeKind, PostProcessConfig, ShadowsConfig};

/// Live, reactive scene. Held inside the `EditorController` as an `Arc<Scene>`.
pub struct Scene {
    pub environment: Mutable<EnvironmentConfig>,
    pub shadows: Mutable<ShadowsConfig>,
    pub post_process: Mutable<PostProcessConfig>,
    /// Player-bundle export options (project-persisted; pre-export modal +
    /// MCP edit them).
    pub bundle_options: Mutable<BundleOptions>,
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
            post_process: Mutable::new(PostProcessConfig::default()),
            bundle_options: Mutable::new(BundleOptions::default()),
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
                    NodeKind::Light(_) => stats.lights += 1,
                    NodeKind::Collider(_) => stats.colliders += 1,
                    NodeKind::Camera(_) => stats.cameras += 1,
                    NodeKind::Mesh { .. } => stats.primitives += 1,
                    NodeKind::SkinnedMesh { .. } => stats.primitives += 1,
                    NodeKind::ClusterMesh { .. } => stats.primitives += 1,
                    NodeKind::Curve(_) => stats.curves += 1,
                    NodeKind::InstancesAlongCurve(_) => stats.instances += 1,
                    NodeKind::Instancer(_) => stats.instances += 1,
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
