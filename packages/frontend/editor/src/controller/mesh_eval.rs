//! Editor-side modifier-stack evaluation: resolve a [`ModifierStack`] to baked
//! triangles, including the bases that need scene state.
//!
//! `awsm_meshgen::evaluate` handles the self-contained bases
//! (`Primitive`/`Lathe`/`Superquadric`); `Sweep` references a scene curve node
//! and `Captured` references the mesh store, so those are resolved here and fed
//! to `apply_modifiers`.

use awsm_editor_protocol::{MeshBase, ModifierStack};
use awsm_editor_protocol::{MeshDef, VertexOverrides};
use awsm_meshgen::MeshData;

use crate::engine::bridge::mesh_cache;
use crate::engine::scene::Scene;

/// Evaluate a full [`MeshDef`] to its baked geometry: run the modifier `stack`,
/// then layer the sparse per-vertex `overrides` on top. The `.mesh.bin` cache is
/// the bake of this — base + modifiers + authoring overrides. Per-vertex
/// authoring is terminal (the first authoring op freezes `stack` to a `Captured`
/// base), so applying overrides after the stack is order-stable.
pub(crate) fn evaluate_def(scene: &Scene, def: &MeshDef) -> MeshData {
    let mut md = evaluate_stack(scene, &def.stack);
    apply_overrides(&mut md, &def.overrides);
    // Sculpting (position overrides) shifts the surface, so recompute smooth
    // normals to match — UNLESS the author also set explicit normal overrides
    // (those always win, and `apply_overrides` already wrote them).
    if !def.overrides.positions.is_empty() && def.overrides.normals.is_empty() {
        md.compute_vertex_normals();
    }
    md
}

/// Layer sparse, index-keyed [`VertexOverrides`] onto an evaluated `MeshData`
/// in place. For each present index: positions replace; colors/normals/uvs
/// replace, creating the channel (filled with sensible defaults for untouched
/// verts) if no override-bearing channel exists yet. Out-of-range indices are
/// ignored. Normals are NOT recomputed when only positions are overridden — the
/// position-authoring command path recomputes them so an unauthored-normal mesh
/// keeps smooth shading; an explicit `normals` override always wins.
pub(crate) fn apply_overrides(md: &mut MeshData, ov: &VertexOverrides) {
    if ov.is_empty() {
        return;
    }
    let vcount = md.positions.len();
    for (&i, p) in &ov.positions {
        if let Some(slot) = md.positions.get_mut(i as usize) {
            *slot = *p;
        }
    }
    if !ov.colors.is_empty() {
        let colors = md
            .colors
            .get_or_insert_with(|| vec![[1.0, 1.0, 1.0, 1.0]; vcount]);
        if colors.len() < vcount {
            colors.resize(vcount, [1.0, 1.0, 1.0, 1.0]);
        }
        for (&i, c) in &ov.colors {
            if let Some(slot) = colors.get_mut(i as usize) {
                *slot = *c;
            }
        }
    }
    if !ov.normals.is_empty() {
        let normals = md
            .normals
            .get_or_insert_with(|| vec![[0.0, 0.0, 1.0]; vcount]);
        if normals.len() < vcount {
            normals.resize(vcount, [0.0, 0.0, 1.0]);
        }
        for (&i, n) in &ov.normals {
            if let Some(slot) = normals.get_mut(i as usize) {
                *slot = *n;
            }
        }
    }
    if !ov.uvs.is_empty() {
        if md.uvs.is_empty() {
            md.uvs.push(vec![[0.0, 0.0]; vcount]);
        }
        let uvs = &mut md.uvs[0];
        if uvs.len() < vcount {
            uvs.resize(vcount, [0.0, 0.0]);
        }
        for (&i, uv) in &ov.uvs {
            if let Some(slot) = uvs.get_mut(i as usize) {
                *slot = *uv;
            }
        }
    }
}

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
                    uvs: r.uv_sets,
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
