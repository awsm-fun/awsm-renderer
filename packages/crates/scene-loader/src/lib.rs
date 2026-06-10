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
//! Status: this cut materializes the node hierarchy (transforms), **primitive**
//! meshes (regenerated from params) with their **built-in materials** (the
//! shared `material_to_renderer` conversion, texture-less), and **glb** meshes
//! (the per-mesh `assets/<id>.glb` fed through `populate_gltf`, rooted under the
//! scene node's transform — the geometry+skin+morph path foreign glTF already
//! uses). The remaining arms — texture binding, custom-WGSL materials, glb-mesh
//! material reassignment, lights, cameras, standalone skins, and our animation
//! clips — are staged follow-ons (each marked below).

pub mod material;

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use awsm_renderer::materials::unlit::UnlitMaterial;
use awsm_renderer::materials::{Material, MaterialAlphaMode, MaterialKey};
use awsm_renderer::raw_mesh::RawMeshData;
use awsm_renderer::transforms::{Transform, TransformKey};
use awsm_renderer::AwsmRenderer;
use awsm_renderer_gltf::loader::GltfLoader;
use awsm_renderer_gltf::AwsmRendererGltfExt;
use awsm_scene::{
    mesh_glb_filename, AssetSource, EditorNode, MaterialInstance, NodeKind, RuntimeMesh, Scene,
    Trs, ASSETS_DIR,
};
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
    // The missing-material sentinel for unassigned meshes (and glb meshes until
    // their material reassignment lands). Built-in-assigned meshes resolve their
    // own key via `resolve_material`.
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

    if let NodeKind::Mesh { mesh, material, .. } = &node.kind {
        if let Some(entry) = scene.assets.get(mesh.0) {
            match &entry.source {
                AssetSource::Mesh(RuntimeMesh::Primitive(shape)) => {
                    let mat = resolve_material(renderer, material.as_ref(), placeholder);
                    let md = awsm_meshgen::primitive_mesh(shape);
                    renderer.add_raw_mesh(mesh_data_to_raw(md), tk, mat)?;
                }
                // Geometry (+ skin / morph) glb: feed the in-memory bytes through
                // the exact path foreign glTF uses, rooted under `tk` so the scene
                // node's TRS applies on top of the glb's identity node.
                //
                // Follow-on: the node's assigned material. `populate_gltf` mints
                // its own keys from the glb's (absent → default) materials; binding
                // our `material` over them means reassigning each primitive's
                // material key from the returned `GltfPopulateContext`.
                AssetSource::Mesh(RuntimeMesh::Glb) => {
                    let key = format!("{ASSETS_DIR}/{}", mesh_glb_filename(mesh.0));
                    let bytes = assets
                        .get(&key)
                        .ok_or_else(|| anyhow!("bundle is missing mesh glb `{key}`"))?;
                    let data = GltfLoader::from_glb_bytes(bytes).await?.into_data(None)?;
                    renderer.populate_gltf_under(data, None, Some(tk)).await?;
                }
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

/// Resolve a mesh node's assigned material to a renderer key.
///
/// A built-in assignment's `inline` is a faithful, complete `MaterialDef` — it's
/// seeded from the shared variant when the material is assigned, and per-mesh
/// edits only touch uniform fields — so the player lowers it directly via the
/// shared [`material::material_to_renderer`] (the same conversion the editor's
/// live render uses). Texture binding and custom-WGSL materials are follow-ons;
/// an unassigned node (`None`) renders the magenta placeholder.
fn resolve_material(
    renderer: &mut AwsmRenderer,
    instance: Option<&MaterialInstance>,
    placeholder: MaterialKey,
) -> MaterialKey {
    match instance {
        Some(inst) => renderer.materials.insert(
            material::material_to_renderer(&inst.inline),
            &renderer.textures,
            &renderer.dynamic_materials,
            &renderer.extras_pool,
        ),
        None => placeholder,
    }
}

/// A magenta unlit placeholder for unassigned meshes (and glb meshes until their
/// material reassignment lands).
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
