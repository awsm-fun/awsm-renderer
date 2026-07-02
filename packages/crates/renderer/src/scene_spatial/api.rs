//! `AwsmRenderer`-level hooks that keep the spatial index in sync with
//! `Meshes`. Each mutation that adds, removes, moves, or flips a flag on
//! a mesh calls back here exactly once so the index never diverges from
//! `Mesh::world_aabb`.

use crate::{meshes::MeshKey, AwsmRenderer};

use super::node::{SceneNode, SceneNodeFlags};

impl AwsmRenderer {
    /// Reflects the mesh's current `world_aabb` + flags into the spatial
    /// index. Idempotent — safe to call after insert OR after a transform
    /// update. If the mesh has no world AABB yet (procedural / mid-load),
    /// the spatial entry is removed so the index never carries a stale box.
    ///
    /// Called for each mesh wired by `resolve_geometry` (commit) / `resolve_one`
    /// (eager `add_raw_mesh`) so the new mesh participates in frustum culling on
    /// the next frame. (`AwsmRenderer::resolve_geometry` calls this per wired key.)
    ///
    /// Per-frame movers (skinned, instanced, physics-driven) need no special
    /// routing: the BVH absorbs incremental leaf updates directly (see
    /// [`super::SceneSpatial::maintain`]).
    pub fn sync_spatial_for_mesh(&mut self, mesh_key: MeshKey) {
        let Ok(mesh) = self.meshes.get(mesh_key) else {
            self.scene_spatial.remove(mesh_key);
            return;
        };
        let Some(world_aabb) = mesh.world_aabb.clone() else {
            self.scene_spatial.remove(mesh_key);
            return;
        };
        let flags = SceneNodeFlags::from_mesh(mesh);

        // If the node already exists, do a lightweight envelope update +
        // flag refresh. Otherwise, insert from scratch.
        if self.scene_spatial.get(mesh_key).is_some() {
            self.scene_spatial.update(mesh_key, world_aabb);
            self.scene_spatial.set_flags(mesh_key, flags);
        } else {
            self.scene_spatial.insert(SceneNode {
                aabb: world_aabb,
                mesh_key,
                flags,
            });
        }
    }

    /// Removes the spatial entry for a mesh. Used by the various mesh
    /// removal paths on `AwsmRenderer`.
    pub fn drop_spatial_for_mesh(&mut self, mesh_key: MeshKey) {
        self.scene_spatial.remove(mesh_key);
    }
}
