//! The **bake**: lower the authoring [`EditorProject`] to a runtime
//! [`awsm_renderer_scene::Scene`]. This is the structural half — pure data, native-tested.
//!
//! The editor pairs it with the byte-producing half (build a **geometry-only** glb
//! per [`RuntimeMesh::Glb`] mesh via `awsm-renderer-glb-export`, gather textures + custom-
//! material folders) and writes the `scene.toml` + `assets/` directory. (Skinned/
//! morph meshes' glb re-export from source — `awsm_renderer_glb_export::reexport_clean`,
//! which preserves the rig — is the remaining follow-on; static geometry for now.)
//!
//! What's dropped vs the authoring project: the modifier-stack recipes + per-vertex
//! overrides collapse to a baked mesh (`RuntimeMesh`), and the editor-only library
//! snapshots (`editor_materials`, `custom_animations` refs) don't travel — only
//! what the player needs.

use awsm_renderer_scene::{
    AssetEntry as RtEntry, AssetLod, AssetSource as RtSource, AssetTable as RtTable, RuntimeMesh,
    Scene,
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
        // Buffer-data entries are editor-only (the player resolves a buffer
        // override by its asset-id filename, never via the runtime table), so
        // they don't lower — skip them.
        let Some(source) = lower_source(&entry.source) else {
            continue;
        };
        assets.entries.insert(
            *id,
            RtEntry {
                source,
                gltf_material_asset_ids: entry.gltf_material_asset_ids.clone(),
                gltf_image_asset_ids: entry.gltf_image_asset_ids.clone(),
                content_hash: entry.content_hash.clone(),
                // Set by the editor's bundle bake from each texture's source MIME
                // (see `controller::export`). `None` here ⇒ the loader defaults to
                // PNG, which is what this lowering path has always emitted.
                texture_encoding: None,
                // Likewise bake-set, on normal-use KTX2 artifacts only.
                texture_two_channel_normal: false,
                // Structural lowering only — the resolved LOD is filled in by the
                // editor's export bake (bundle) or the sidecar read (editor load).
                lod: AssetLod::None,
            },
        );
    }
    Scene {
        name: project.name.clone(),
        environment: project.environment.clone(),
        shadows: project.shadows.clone(),
        post_process: project.post_process.clone(),
        assets,
        custom_materials: project.custom_materials.clone(),
        animations: project.editor_animations.clone(),
        mixer: project.anim_mixer.clone(),
        nodes: project.nodes.clone(),
        // Carry the authored viewing camera through to the bundle, so a
        // framing composed in the editor is the framing the player renders
        // instead of being re-derived as constants in player code.
        active_camera: project.active_camera,
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

/// Lower one authoring asset source to its runtime form. Returns `None` for
/// editor-only sources that don't travel to the player (buffer data — resolved by
/// asset-id filename, not the runtime table).
fn lower_source(src: &AuthSource) -> Option<RtSource> {
    Some(match src {
        AuthSource::Filename(n) => RtSource::Filename(n.clone()),
        AuthSource::Url(u) => RtSource::Url(u.clone()),
        AuthSource::Material(m) => RtSource::Material(m.clone()),
        AuthSource::Texture(t) => RtSource::Texture(t.clone()),
        AuthSource::Mesh(def) => RtSource::Mesh(lower_mesh(def)),
        AuthSource::Buffer(_) => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AssetEntry, AssetSource, Axis, MeshDef, Modifier, ModifierStack, VertexOverrides};
    use awsm_renderer_scene::{
        scene_from_toml, scene_to_toml, AssetId, EditorNode, MeshLodConfig, MeshRef,
        MeshShadowConfig, NodeId, NodeKind, PrimitiveShape,
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
            mujoco: None,
            id: NodeId::new(),
            name: "Ball".into(),
            transform: Default::default(),
            kind: NodeKind::Mesh {
                mesh: MeshRef(mesh_id),
                material_variants: Vec::new(),
                selected_variant: None,
                shadow: MeshShadowConfig::default(),
                lod: MeshLodConfig::default(),
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
            awsm_renderer_scene::AssetSource::Mesh(RuntimeMesh::Primitive(_)) => {}
            other => panic!("expected procedural primitive, got {other:?}"),
        }
        // The runtime Scene serializes to scene.toml + round-trips.
        let toml = scene_to_toml(&scene).expect("scene.toml");
        assert_eq!(scene_from_toml(&toml).unwrap(), scene);
    }

    /// A MuJoCo instance must reach the bundle's `scene.toml` intact. This is the
    /// export half of the plan's parity checklist: a scene that bakes fine but
    /// loses the fingerprint renders correctly and can never bind a sim to it,
    /// which is a silent failure rather than a loud one.
    #[test]
    fn the_mujoco_component_survives_the_bundle_bake() {
        use awsm_renderer_scene::mujoco::{
            GeomKind, MujocoComponent, MujocoGeom, MujocoInstance, Source,
        };

        let source = Source {
            filename: "go2.xml".into(),
            sha256: "b".repeat(64),
            mujoco_version: "3.11.0".into(),
        };
        let geom = EditorNode {
            mujoco: Some(MujocoComponent::Geom(MujocoGeom {
                geom_id: 12,
                group: 2,
                kind: GeomKind::Mesh,
                body: 3,
            })),
            id: NodeId::new(),
            name: "geom_12".into(),
            transform: Default::default(),
            kind: NodeKind::Group,
            locked: false,
            visible: true,
            prefab: false,
            children: vec![],
        };
        let mut project = EditorProject {
            name: "sim".into(),
            ..Default::default()
        };
        project.nodes.push(EditorNode {
            mujoco: Some(MujocoComponent::Instance(MujocoInstance::new(
                source.clone(),
                56,
            ))),
            id: NodeId::new(),
            name: "go2".into(),
            transform: Default::default(),
            kind: NodeKind::Group,
            locked: false,
            visible: true,
            prefab: false,
            children: vec![geom],
        });

        let scene = project_to_scene(&project);
        // Through the real serializer the player reads, not just in memory.
        let baked = scene_from_toml(&scene_to_toml(&scene).expect("scene.toml")).unwrap();

        let Some(MujocoComponent::Instance(inst)) = &baked.nodes[0].mujoco else {
            panic!("instance component lost in the bake: {:?}", baked.nodes[0]);
        };
        assert_eq!(inst.source, source);
        assert_eq!(inst.geom_count, 56, "the stream's id space must survive");
        assert_eq!(inst.visible_groups, vec![0, 1, 2]);

        let Some(MujocoComponent::Geom(g)) = &baked.nodes[0].children[0].mujoco else {
            panic!("geom component lost in the bake");
        };
        assert_eq!((g.geom_id, g.group, g.body), (12, 2, 3));
        assert_eq!(g.kind, GeomKind::Mesh);
    }
}
