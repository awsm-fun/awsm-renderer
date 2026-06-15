//! The **bake**: lower the authoring [`EditorProject`] to a runtime
//! [`awsm_scene::Scene`]. This is the structural half — pure data, native-tested.
//!
//! The editor pairs it with the byte-producing half (build a **geometry-only** glb
//! per [`RuntimeMesh::Glb`] mesh via `awsm-glb-export`, gather textures + custom-
//! material folders) and writes the `scene.toml` + `assets/` directory. (Skinned/
//! morph meshes' glb re-export from source — `awsm_glb_export::reexport_clean`,
//! which preserves the rig — is the remaining follow-on; static geometry for now.)
//!
//! What's dropped vs the authoring project: the modifier-stack recipes + per-vertex
//! overrides collapse to a baked mesh (`RuntimeMesh`), and the editor-only library
//! snapshots (`editor_materials`, `custom_animations` refs) don't travel — only
//! what the player needs.

use awsm_scene::{
    AssetEntry as RtEntry, AssetSource as RtSource, AssetTable as RtTable, RuntimeMesh, Scene,
};

use crate::{AssetSource as AuthSource, EditorProject, MeshBase, MeshDef};

/// Lower an [`EditorProject`] to the runtime [`Scene`]. Mesh assets become
/// [`RuntimeMesh`] (cheap primitives stay procedural; everything else is marked
/// [`RuntimeMesh::Glb`] — the editor bakes the actual `assets/<id>.glb`). The
/// node hierarchy, materials, lights, cameras, clips (+ NLA mixer) and environment
/// carry over verbatim (shared CORE types); editor-only library snapshots drop.
pub fn project_to_scene(project: &EditorProject) -> Scene {
    let mut assets = RtTable::new();
    for (id, entry) in &project.assets.entries {
        assets.entries.insert(
            *id,
            RtEntry {
                source: lower_source(&entry.source),
                gltf_material_asset_ids: entry.gltf_material_asset_ids.clone(),
                gltf_image_asset_ids: entry.gltf_image_asset_ids.clone(),
                content_hash: entry.content_hash.clone(),
            },
        );
    }
    Scene {
        name: project.name.clone(),
        environment: project.environment.clone(),
        shadows: project.shadows.clone(),
        assets,
        custom_materials: project.custom_materials.clone(),
        animations: project.editor_animations.clone(),
        mixer: project.anim_mixer.clone(),
        nodes: project.nodes.clone(),
    }
}

/// Decide a mesh's runtime form. A bare primitive (primitive base, no modifiers,
/// no per-vertex overrides) stays procedural — the player regenerates it from
/// params, no side file. Everything else (modified / sweep / SDF / edited /
/// imported, skinned, morphed) bakes to a glb (the editor emits the bytes).
pub fn lower_mesh(def: &MeshDef) -> RuntimeMesh {
    if def.stack.modifiers.is_empty() && def.overrides.is_empty() {
        if let MeshBase::Primitive(shape) = &def.stack.base {
            return RuntimeMesh::Primitive(shape.clone());
        }
    }
    RuntimeMesh::Glb
}

fn lower_source(src: &AuthSource) -> RtSource {
    match src {
        AuthSource::Filename(n) => RtSource::Filename(n.clone()),
        AuthSource::Url(u) => RtSource::Url(u.clone()),
        AuthSource::Material(m) => RtSource::Material(m.clone()),
        AuthSource::Texture(t) => RtSource::Texture(t.clone()),
        AuthSource::Mesh(def) => RtSource::Mesh(lower_mesh(def)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AssetEntry, AssetSource, Axis, MeshDef, Modifier, ModifierStack, VertexOverrides};
    use awsm_scene::{
        scene_from_toml, scene_to_toml, AssetId, EditorNode, MeshRef, MeshShadowConfig, NodeId,
        NodeKind, PrimitiveShape,
    };

    fn primitive_meshdef(shape: PrimitiveShape) -> MeshDef {
        MeshDef {
            label: "m".into(),
            source: None,
            editable: false,
            stack: ModifierStack {
                base: MeshBase::Primitive(shape),
                modifiers: vec![],
            },
            overrides: VertexOverrides::default(),
        }
    }

    #[test]
    fn bare_primitive_stays_procedural_modified_bakes_glb() {
        let shape = PrimitiveShape::Box {
            dims: [1.0, 1.0, 1.0],
        };
        // Bare primitive → Primitive.
        assert_eq!(
            lower_mesh(&primitive_meshdef(shape.clone())),
            RuntimeMesh::Primitive(shape.clone())
        );
        // + a modifier → Glb.
        let mut modded = primitive_meshdef(shape.clone());
        modded.stack.modifiers.push(Modifier::Twist {
            axis: Axis::Y,
            turns: 1.0,
        });
        assert_eq!(lower_mesh(&modded), RuntimeMesh::Glb);
        // + a vertex override → Glb.
        let mut painted = primitive_meshdef(shape);
        painted.overrides.colors.insert(0, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(lower_mesh(&painted), RuntimeMesh::Glb);
    }

    #[test]
    fn project_bakes_to_a_round_tripping_scene() {
        let mesh_id = AssetId::new();
        let mut project = EditorProject {
            name: "demo".into(),
            ..Default::default()
        };
        project.assets.entries.insert(
            mesh_id,
            AssetEntry::new(AssetSource::Mesh(primitive_meshdef(
                PrimitiveShape::Sphere {
                    radius: 0.5,
                    segments_long: 16,
                    segments_lat: 12,
                },
            ))),
        );
        project.nodes.push(EditorNode {
            id: NodeId::new(),
            name: "Ball".into(),
            transform: Default::default(),
            kind: NodeKind::Mesh {
                mesh: MeshRef(mesh_id),
                material: None,
                shadow: MeshShadowConfig::default(),
            },
            locked: false,
            visible: true,
            prefab: false,
            children: vec![],
        });

        let scene = project_to_scene(&project);
        assert_eq!(scene.name, "demo");
        assert_eq!(scene.nodes.len(), 1);
        // The bare-primitive mesh lowered to a procedural primitive (no glb needed).
        match &scene.assets.entries[&mesh_id].source {
            awsm_scene::AssetSource::Mesh(RuntimeMesh::Primitive(_)) => {}
            other => panic!("expected procedural primitive, got {other:?}"),
        }
        // The runtime Scene serializes to scene.toml + round-trips.
        let toml = scene_to_toml(&scene).expect("scene.toml");
        assert_eq!(scene_from_toml(&toml).unwrap(), scene);
    }
}
