//! Editor-side modifier-stack evaluation: resolve a [`ModifierStack`] to baked
//! triangles, including the bases that need scene state.
//!
//! `awsm_meshgen::evaluate` handles the self-contained bases
//! (`Primitive`/`Lathe`/`Superquadric`); `Sweep` references a scene curve node
//! and `Captured` references the mesh store, so those are resolved here and fed
//! to `apply_modifiers`.

use awsm_meshgen::MeshData;
use awsm_scene_schema::modifier::{MeshBase, ModifierStack};

use crate::engine::bridge::mesh_cache;
use crate::engine::scene::Scene;

/// Evaluate `stack` to a baked mesh, resolving scene-dependent bases.
pub(crate) fn evaluate_stack(scene: &Scene, stack: &ModifierStack) -> MeshData {
    match &stack.base {
        MeshBase::Sweep(def) => {
            let base = super::export::sweep_mesh(scene, def).unwrap_or_default();
            awsm_meshgen::apply_modifiers(base, &stack.modifiers)
        }
        MeshBase::Captured(mesh_ref) => {
            let base = mesh_cache::get_raw(mesh_ref.0)
                .map(|r| MeshData {
                    positions: r.positions,
                    normals: r.normals,
                    uvs: r.uvs,
                    colors: r.colors,
                    indices: r.indices,
                })
                .unwrap_or_default();
            awsm_meshgen::apply_modifiers(base, &stack.modifiers)
        }
        // Pure bases evaluate entirely in meshgen.
        _ => awsm_meshgen::evaluate(stack),
    }
}
