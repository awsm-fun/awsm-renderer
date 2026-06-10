//! `populate_awsm_scene` — load an [`awsm_scene::Scene`] (the runtime bundle's
//! `scene.toml`) into the renderer. The parallel to
//! `awsm_renderer_gltf::populate_gltf`: that loads *foreign* glTF, this loads
//! *our* format. They share the same renderer core — glb meshes in a bundle go
//! through `populate_gltf`'s machinery, primitives regenerate via `awsm-meshgen`,
//! and our materials / clips bind on top.
//!
//! The headline use is the **round-trip test**: in the MCP-controlled browser
//! session, `export_player_bundle` → `populate_awsm_scene` → screenshot, compared
//! against the source render. The model-test page can load a `.glb` *or* one of
//! our exported bundles this way.
//!
//! Status: this first cut materializes the node hierarchy (transforms) +
//! **primitive** meshes (regenerated from params). The remaining arms —
//! `RuntimeMesh::Glb` (reuse `populate_gltf` on `assets/<id>.glb`), real material
//! binding (currently a magenta placeholder), lights, cameras, skins, and our
//! animation clips — are staged follow-ons (each marked below).

use std::collections::HashMap;

use anyhow::Result;
use awsm_renderer::materials::unlit::UnlitMaterial;
use awsm_renderer::materials::{Material, MaterialAlphaMode, MaterialKey};
use awsm_renderer::raw_mesh::RawMeshData;
use awsm_renderer::transforms::{Transform, TransformKey};
use awsm_renderer::AwsmRenderer;
use awsm_scene::{AssetSource, EditorNode, NodeKind, RuntimeMesh, Scene, Trs};
use glam::{Quat, Vec3};

/// Load a runtime [`Scene`] into the renderer. `assets` maps bundle-relative
/// paths (e.g. `assets/<id>.glb`, `assets/<id>.png`) to their bytes — the in-
/// memory file set the bundle exporter produces, so the round-trip never touches
/// disk. Builds the node hierarchy + meshes; see the module docs for what's
/// staged.
pub async fn populate_awsm_scene(
    renderer: &mut AwsmRenderer,
    scene: &Scene,
    assets: &HashMap<String, Vec<u8>>,
) -> Result<()> {
    // A shared placeholder material until real material binding lands (follow-on:
    // resolve each node's `MaterialInstance` + `scene.custom_materials`).
    let placeholder = insert_placeholder_material(renderer);
    for node in &scene.nodes {
        materialize(renderer, scene, node, None, assets, placeholder).await?;
    }
    Ok(())
}

async fn materialize(
    renderer: &mut AwsmRenderer,
    scene: &Scene,
    node: &EditorNode,
    parent: Option<TransformKey>,
    assets: &HashMap<String, Vec<u8>>,
    placeholder: MaterialKey,
) -> Result<()> {
    let tk = renderer
        .transforms
        .insert(trs_to_transform(&node.transform), parent);

    if let NodeKind::Mesh { mesh, .. } = &node.kind {
        if let Some(entry) = scene.assets.get(mesh.0) {
            match &entry.source {
                AssetSource::Mesh(RuntimeMesh::Primitive(shape)) => {
                    let md = awsm_meshgen::primitive_mesh(shape);
                    renderer.add_raw_mesh(mesh_data_to_raw(md), tk, placeholder)?;
                }
                // Follow-on: load `assets/<mesh.0>.glb` from `assets` and feed it
                // through `populate_gltf`'s mesh/skin upload (reusing the exact
                // path foreign glTF uses), then place it under `tk`.
                AssetSource::Mesh(RuntimeMesh::Glb) => {}
                _ => {}
            }
        }
    }
    // Follow-on: Light / Camera / SkinnedMesh arms + our-clip wiring.

    for child in &node.children {
        Box::pin(materialize(
            renderer,
            scene,
            child,
            Some(tk),
            assets,
            placeholder,
        ))
        .await?;
    }
    Ok(())
}

fn trs_to_transform(trs: &Trs) -> Transform {
    Transform {
        translation: Vec3::from_array(trs.translation),
        rotation: Quat::from_array(trs.rotation),
        scale: Vec3::from_array(trs.scale),
    }
}

fn mesh_data_to_raw(md: awsm_meshgen::MeshData) -> RawMeshData {
    RawMeshData {
        positions: md.positions,
        normals: md.normals,
        uvs: md.uvs,
        colors: md.colors,
        indices: md.indices,
    }
}

/// A magenta unlit placeholder until real material binding lands.
fn insert_placeholder_material(renderer: &mut AwsmRenderer) -> MaterialKey {
    let mut m = UnlitMaterial::new(MaterialAlphaMode::Opaque, false);
    m.base_color_factor = [1.0, 0.0, 1.0, 1.0];
    renderer.materials.insert(
        Material::Unlit(m),
        &renderer.textures,
        &renderer.dynamic_materials,
        &renderer.extras_pool,
    )
}
