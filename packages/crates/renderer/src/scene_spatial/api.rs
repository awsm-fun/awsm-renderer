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
    /// External mesh-ingest crates (e.g. `awsm-renderer-gltf`) must call
    /// this after `Meshes::insert` / `Meshes::insert_public` so the new
    /// mesh participates in frustum culling on the next frame.
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
        // Skinned + instanced meshes are the canonical per-frame movers:
        // every animation tick remove+inserts them in the tree which
        // erodes R*-tree query quality fast. Route them to the linear-
        // scan sidecar by default. Callers that know better can override
        // via `set_mesh_dynamic`.
        let should_be_dynamic = mesh.instanced
            || self
                .meshes
                .mesh_skin_key(mesh_key)
                .map(|opt| opt.is_some())
                .unwrap_or(false);

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

        if self.scene_spatial.is_dynamic(mesh_key) != should_be_dynamic {
            self.scene_spatial.set_dynamic(mesh_key, should_be_dynamic);
        }
    }

    /// Routes a mesh through the linear-scan dynamic sidecar (when `true`)
    /// or back into the R*-tree (when `false`). Use this for meshes whose
    /// AABBs change every frame for reasons the auto-flagger can't see
    /// — e.g. CPU-side procedural movers.
    pub fn set_mesh_dynamic(&mut self, mesh_key: MeshKey, dynamic: bool) {
        self.scene_spatial.set_dynamic(mesh_key, dynamic);
    }

    /// Removes the spatial entry for a mesh. Used by the various mesh
    /// removal paths on `AwsmRenderer`.
    pub fn drop_spatial_for_mesh(&mut self, mesh_key: MeshKey) {
        self.scene_spatial.remove(mesh_key);
    }
}
