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
//! Runs as one batched, phased pass (build materials → upload textures → upload
//! meshes → load animation → compile pipelines), reporting each [`LoadPhase`]
//! through a callback. Handles: the node hierarchy (transforms); **primitive**
//! meshes with their built-in materials; **glb** meshes (`assets/<id>.glb`) AND
//! **skinned** meshes (`assets/<skin.source>.glb`), both fed through
//! `populate_gltf` with [`GltfMaterialSource::Single`] so they take OUR material
//! (no glTF default-mint) and ride the same geometry+skin+morph upload foreign
//! glTF uses; **lights** (shared `light_from_config` + shadow params);
//! **cameras**; textures + custom-WGSL materials; and **animation** — the scene's
//! clips + NLA mixer ([`animation::load_animations`]) lowered against the per-node
//! keys built here. The loader only LOADS the clips; the consumer drives the
//! clock (a player's `update_animations`, or the editor round-trip's playhead
//! pin). Remaining follow-on: driving a skinned mesh's rig glb joints from our
//! Transform tracks (skin correspondence — the rig still poses at bind pose, and
//! its bone tracks currently target the scene bone nodes, not the glb joints).

pub mod animation;
pub mod camera;
pub mod dynamic;
pub mod light;
pub mod material;
pub mod texture;

use std::collections::HashMap;

use animation::AnimResolveMaps;
use anyhow::{anyhow, Result};
use awsm_renderer::animation::AnimationClipKey;
use awsm_renderer::lights::LightKey;
use awsm_renderer::materials::unlit::UnlitMaterial;
use awsm_renderer::materials::{Material, MaterialAlphaMode, MaterialKey};
use awsm_renderer::meshes::MeshKey;
use awsm_renderer::raw_mesh::RawMeshData;
use awsm_renderer::transforms::{Transform, TransformKey};
use awsm_renderer::{AwsmRenderer, LoadPhase};
use awsm_renderer_core::texture::mipmap::MipmapTextureKind;
use awsm_renderer_gltf::loader::GltfLoader;
use awsm_renderer_gltf::{AwsmRendererGltfExt, GltfMaterialSource, PopulateGltfOpts};
use awsm_scene::{
    mesh_glb_filename, AssetId, AssetSource, EditorNode, MaterialInstance, MaterialShading, NodeId,
    NodeKind, RuntimeMesh, Scene, Trs, ASSETS_DIR,
};
use glam::{Quat, Vec3};

/// The renderer resources `populate_awsm_scene` created, returned so a host can
/// tear the loaded scene back down — these inserts live OUTSIDE any per-node
/// tracking the host keeps, so without removing them they'd leak/ghost on the
/// next load. Only the *visible* resources (meshes + lights) are tracked;
/// orphaned transforms / materials are invisible and cleared on a full renderer
/// rebuild.
#[derive(Default, Debug)]
pub struct LoadedScene {
    pub meshes: Vec<MeshKey>,
    pub lights: Vec<LightKey>,
    /// Animation clips inserted into `renderer.animations` (the scene's
    /// `StoredAnimation`s lowered to runtime clip groups). Tracked so a host can
    /// remove them on the next load — like meshes/lights, they live outside any
    /// per-node tracking. The mixer is rebuilt wholesale on each load.
    pub clips: Vec<AnimationClipKey>,
}

/// Load a runtime [`Scene`] into the renderer as one batched, phased pass.
/// Returns the [`LoadedScene`] handles for later teardown.
///
/// `assets` maps bundle-relative paths (e.g. `assets/<id>.glb`, `assets/<id>.png`)
/// to their bytes — the in-memory file set the bundle exporter produces, so the
/// round-trip never touches disk. `on_phase` is invoked at each
/// [`LoadPhase`](awsm_renderer::LoadPhase) boundary (and through the pipeline
/// compile) so a host can show live progress; pass `|_| {}` to ignore it.
///
/// The phases (in order) are why this is efficient for the player's typical
/// "load a bundle then render" case:
/// 1. **Build materials** — lower every node's authored material to a renderer
///    `Material` and insert it once, producing a ready `MaterialKey`. Built here
///    so meshes — including glb meshes via [`GltfMaterialSource::Single`] —
///    reference a ready key instead of letting the glTF loader mint (and compile
///    a pipeline for) a throwaway default that we'd then replace.
/// 2. **Upload textures** — one batched `finalize_gpu_textures` for the whole
///    scene, not once per glb.
/// 3. **Upload meshes** — transforms + geometry (+ skins) + lights, each mesh
///    handed its already-built `MaterialKey`.
/// 4. **Compile pipelines** — one drive-to-ready (`wait_for_pipelines_ready`)
///    for the whole scene's materials + shadows, so the first frame draws
///    everything rather than trickling pipelines across frames.
pub async fn populate_awsm_scene(
    renderer: &mut AwsmRenderer,
    scene: &Scene,
    assets: &HashMap<String, Vec<u8>>,
    mut on_phase: impl FnMut(LoadPhase),
) -> Result<LoadedScene> {
    // ── Phase 0: register custom-WGSL materials ──────────────────────────────
    // Build + register each custom material (material.json + wgsl) once; nodes
    // assigned one resolve to its shader id below. Built-in materials have no
    // folder, so they're skipped here and lower via their inline MaterialDef.
    let custom = dynamic::register_custom_materials(renderer, scene, assets);

    // ── Phase 1: build materials ──────────────────────────────────────────────
    // The missing-material sentinel (magenta) for unassigned meshes.
    let placeholder = insert_placeholder_material(renderer);
    // The key maps the animation resolver consults (filled across phases): node
    // material keys here, transform/light/camera/mesh keys while materializing.
    let mut maps = AnimResolveMaps::default();
    // Per-node material key. A built-in assignment's `inline` is a faithful,
    // complete MaterialDef (seeded from the shared variant at assign time), so
    // the player lowers it directly. NOT deduped by asset id: two nodes assigned
    // the same library material carry different per-mesh `inline` uniforms, so
    // they are distinct renderer materials.
    let renderables = collect_renderables(&scene.nodes);
    let total = renderables.len();
    for (i, (id, material)) in renderables.iter().enumerate() {
        on_phase(LoadPhase::BuildingMaterials { done: i, total });
        let key = resolve_material(renderer, material.as_ref(), placeholder, assets, &custom).await;
        maps.node_materials.insert(*id, key);
        // A custom-WGSL asset's first built key is the one a Uniform track drives
        // (an asset assigned to N nodes mints N keys; mirror the editor's
        // first-match `material_key_for_shader`).
        if let Some(inst) = material.as_ref() {
            if custom.contains_key(&inst.asset) {
                maps.custom_materials.entry(inst.asset).or_insert(key);
            }
        }
    }
    on_phase(LoadPhase::BuildingMaterials { done: total, total });
    // The custom-WGSL asset → shader-id table (Phase 0) feeds Uniform resolution.
    maps.custom_shaders = custom;

    // ── Phase 2: upload textures (one batch across the whole scene) ───────────
    on_phase(LoadPhase::UploadingTextures);
    renderer.finalize_gpu_textures().await?;

    // ── Phase 3: upload meshes (geometry + skins) + lights ────────────────────
    let mut loaded = LoadedScene::default();
    let mut uploaded = 0usize;
    for node in &scene.nodes {
        materialize(
            renderer,
            scene,
            node,
            None,
            assets,
            &mut maps,
            placeholder,
            &mut on_phase,
            &mut uploaded,
            total,
            &mut loaded,
        )
        .await?;
    }

    // ── Phase 3b: load animation clips + the NLA mixer ────────────────────────
    // Now that every node's transform / material / light / camera / mesh key
    // exists, lower the scene's clips + mixer against them and insert into the
    // renderer. The loader only LOADS animation; the consumer drives the clock
    // (`update_animations` each frame, or the editor round-trip's playhead pin).
    loaded.clips = animation::load_animations(renderer, scene, &maps);

    // ── Phase 4: compile pipelines to ready (materials + shadows) ─────────────
    renderer
        .wait_for_pipelines_ready_with_progress(|cp| on_phase(LoadPhase::CompilingPipelines(cp)))
        .await?;
    Ok(loaded)
}

/// Flatten the tree (DFS) to the renderable nodes that carry a material —
/// `Mesh` and `SkinnedMesh` — as `(node id, &material)`. Used to build every
/// material up front (Phase 1) and to size the mesh-upload progress.
fn collect_renderables(nodes: &[EditorNode]) -> Vec<(NodeId, &Option<MaterialInstance>)> {
    let mut out = Vec::new();
    fn walk<'a>(nodes: &'a [EditorNode], out: &mut Vec<(NodeId, &'a Option<MaterialInstance>)>) {
        for n in nodes {
            match &n.kind {
                NodeKind::Mesh { material, .. } | NodeKind::SkinnedMesh { material, .. } => {
                    out.push((n.id, material));
                }
                _ => {}
            }
            walk(&n.children, out);
        }
    }
    walk(nodes, &mut out);
    out
}

#[allow(clippy::too_many_arguments)]
async fn materialize(
    renderer: &mut AwsmRenderer,
    scene: &Scene,
    node: &EditorNode,
    parent: Option<TransformKey>,
    assets: &HashMap<String, Vec<u8>>,
    maps: &mut AnimResolveMaps,
    placeholder: MaterialKey,
    on_phase: &mut dyn FnMut(LoadPhase),
    uploaded: &mut usize,
    total: usize,
    loaded: &mut LoadedScene,
) -> Result<()> {
    let tk = renderer
        .transforms
        .insert(trs_to_transform(&node.transform), parent);
    // Record this node's transform key for animation Transform tracks.
    maps.transforms.insert(node.id, tk);
    // The material key built for this node in Phase 1 (placeholder if unassigned
    // or — defensively — somehow unbuilt).
    let mat = maps
        .node_materials
        .get(&node.id)
        .copied()
        .unwrap_or(placeholder);

    match &node.kind {
        NodeKind::Mesh { mesh, .. } => {
            if let Some(entry) = scene.assets.get(mesh.0) {
                match &entry.source {
                    AssetSource::Mesh(RuntimeMesh::Primitive(shape)) => {
                        let md = awsm_meshgen::primitive_mesh(shape);
                        let key = renderer.add_raw_mesh(mesh_data_to_raw(md), tk, mat)?;
                        maps.meshes.entry(node.id).or_insert(key);
                        loaded.meshes.push(key);
                    }
                    AssetSource::Mesh(RuntimeMesh::Glb) => {
                        // Bare geometry glb (single identity node) — root it UNDER
                        // the scene node's transform, which is what places it.
                        let (keys, _) = load_glb_under(
                            renderer,
                            assets,
                            &mesh_glb_filename(mesh.0),
                            Some(tk),
                            mat,
                        )
                        .await?;
                        if let Some(&first) = keys.first() {
                            maps.meshes.entry(node.id).or_insert(first);
                        }
                        loaded.meshes.extend(keys);
                    }
                    // A Mesh node always references an AssetSource::Mesh; other
                    // source kinds (Filename / Url / Material / Texture) can't be a
                    // mesh asset — ignore defensively.
                    _ => {}
                }
            }
            *uploaded += 1;
            on_phase(LoadPhase::UploadingMeshes {
                done: *uploaded,
                total,
            });
        }
        // A skinned mesh's whole rig glb (skeleton + mesh + skin + morph,
        // re-exported clean at export) loads keyed by the skin source. Unlike a
        // bare Mesh(Glb), the rig glb carries the original glTF's FULL hierarchy —
        // including its root basis-conversion node (e.g. RiggedSimple's `Z_UP`) —
        // so it is SELF-PLACING. We root it at the renderer root (`None`), exactly
        // as the editor's own import does (`populate_gltf` with parent=None).
        // Rooting it under the scene node's `tk` would double-apply that root
        // rotation, because scene.toml ALSO mirrors the `Z_UP` node — the cause of
        // the "skinned mesh loads lying on its side" bug. (Composing a user's
        // *repositioning* of the rig + driving the skin from our scene-node bones
        // is the remaining skin-correspondence follow-on; the glb poses at bind
        // pose for now.)
        NodeKind::SkinnedMesh { skin, .. } => {
            let (keys, node_index_transforms) =
                load_glb_under(renderer, assets, &mesh_glb_filename(skin.source), None, mat)
                    .await?;
            if let Some(&first) = keys.first() {
                maps.meshes.entry(node.id).or_insert(first);
            }
            // Bind each skeleton bone (NodeId) → the rig glb's baked joint
            // transform (by the joint's clean-glb node index), so our clips'
            // Transform tracks drive the joints the skin reads. (Empty `joints`
            // for legacy projects → no binding → bind-pose, as before.)
            for j in &skin.joints {
                if let Some(&tk) = node_index_transforms.get(&(j.index as usize)) {
                    maps.skin_joints.insert(j.node, tk);
                }
            }
            loaded.meshes.extend(keys);
            *uploaded += 1;
            on_phase(LoadPhase::UploadingMeshes {
                done: *uploaded,
                total,
            });
        }
        NodeKind::Light(cfg) => {
            // Same derivation as the editor bridge's `apply_light`: position from
            // the node translation, forward from rotating local -Z. Bind the
            // light to its transform so a moved/rotated light re-derives pos/dir.
            let pos = Vec3::from_array(node.transform.translation);
            let dir = (Quat::from_array(node.transform.rotation) * Vec3::NEG_Z).normalize_or_zero();
            let lt = light::light_from_config(cfg, pos, dir);
            let shadow = light::light_shadow_params_from_config(cfg.shadow());
            let casts = shadow.cast;
            if let Ok(k) = renderer.insert_light(lt, Some(shadow)) {
                renderer.lights.bind_transform(k, tk);
                maps.lights.insert(node.id, k);
                loaded.lights.push(k);
            }
            // Compile shadow pipelines on the first caster (no-op once compiled).
            if casts {
                renderer.ensure_shadow_pipelines_compiled().await?;
            }
        }
        NodeKind::Camera(cfg) => {
            // Register the camera's projection params in the renderer (under its
            // transform `tk`). A player's camera controller picks which camera
            // drives the view + reads `tk` for position; the editor round-trip
            // uses its own free camera, so this just makes the camera node load.
            let ck = renderer
                .cameras
                .insert(camera::camera_params_from_config(cfg));
            maps.cameras.insert(node.id, ck);
        }
        // Follow-on: our-clip (animation) wiring.
        _ => {}
    }

    for child in &node.children {
        Box::pin(materialize(
            renderer,
            scene,
            child,
            Some(tk),
            assets,
            maps,
            placeholder,
            on_phase,
            uploaded,
            total,
            loaded,
        ))
        .await?;
    }
    Ok(())
}

/// Load a glb (`assets/<leaf>`) rooted under `parent` (or the renderer root when
/// `None`), applying our pre-built `material` to every primitive — no glTF
/// material/texture mint (see [`GltfMaterialSource::Single`]). Texture finalize
/// is deferred to the batched Phase 2. Reuses the exact mesh/skin/morph upload
/// foreign glTF uses.
///
/// `parent`: `Some(tk)` for a bare geometry glb (the scene node's transform
/// places it); `None` for a self-placing rig glb that carries its own root
/// hierarchy (see the SkinnedMesh arm — rooting it under the scene chain would
/// double-apply the glTF's basis-conversion node).
async fn load_glb_under(
    renderer: &mut AwsmRenderer,
    assets: &HashMap<String, Vec<u8>>,
    leaf: &str,
    parent: Option<TransformKey>,
    material: MaterialKey,
) -> Result<(Vec<MeshKey>, HashMap<usize, TransformKey>)> {
    let key = format!("{ASSETS_DIR}/{leaf}");
    let bytes = assets
        .get(&key)
        .ok_or_else(|| anyhow!("bundle is missing mesh glb `{key}`"))?;
    let data = GltfLoader::from_glb_bytes(bytes).await?.into_data(None)?;
    let ctx = renderer
        .populate_gltf_with(
            data,
            PopulateGltfOpts {
                scene: None,
                parent_transform: parent,
                material_source: GltfMaterialSource::Single(material),
                finalize_textures: false,
            },
        )
        .await?;
    let lookups = ctx.key_lookups.lock().unwrap();
    // The renderer mesh keys this glb produced (one per primitive), so the host
    // can remove them on teardown.
    let keys = lookups.all_mesh_keys.values().flatten().copied().collect();
    // glb node index → baked transform key — the skinned-mesh arm binds each
    // skeleton joint (by its clean-glb node index) to drive the skin.
    let node_index_transforms = lookups.node_index_to_transform.clone();
    Ok((keys, node_index_transforms))
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
/// shared [`material`] conversion. For a **PBR** material this also binds the five
/// standard texture slots from the bundle's `assets/<id>.png` (mirroring the
/// editor's `apply_textures`); Unlit/Toon are texture-less (as in the editor).
/// Custom-WGSL materials are a follow-on; an unassigned node (`None`) renders the
/// magenta placeholder.
async fn resolve_material(
    renderer: &mut AwsmRenderer,
    instance: Option<&MaterialInstance>,
    placeholder: MaterialKey,
    assets: &HashMap<String, Vec<u8>>,
    custom: &HashMap<AssetId, awsm_materials::MaterialShaderId>,
) -> MaterialKey {
    let Some(inst) = instance else {
        return placeholder;
    };
    // Custom-WGSL assignment: the asset resolved to a registered shader (Phase 0).
    // Build a Material::Custom (defaults + uniform overrides); `inline` is ignored.
    if let Some(&shader_id) = custom.get(&inst.asset) {
        if let Some(mat) = dynamic::build_custom_material(renderer, shader_id, inst, assets).await {
            return renderer.materials.insert(
                mat,
                &renderer.textures,
                &renderer.dynamic_materials,
                &renderer.extras_pool,
            );
        }
        return placeholder;
    }
    let def = &inst.inline;
    let material = match def.shading {
        MaterialShading::Pbr => {
            let alpha = material::alpha_mode_of(def);
            let mut pbr = material::material_to_pbr(def, alpha, None);
            // Bind each enabled standard PBR texture slot. sRGB for color data
            // (base-color / emissive), linear for the rest.
            use MipmapTextureKind as K;
            if let Some(t) = &def.base_color_texture {
                pbr.base_color_tex =
                    texture::load_texture(renderer, assets, t, true, K::Albedo).await;
            }
            if let Some(t) = &def.metallic_roughness_texture {
                pbr.metallic_roughness_tex =
                    texture::load_texture(renderer, assets, t, false, K::MetallicRoughness).await;
            }
            if let Some(t) = &def.normal_texture {
                pbr.normal_tex = texture::load_texture(renderer, assets, t, false, K::Normal).await;
            }
            if let Some(t) = &def.occlusion_texture {
                pbr.occlusion_tex =
                    texture::load_texture(renderer, assets, t, false, K::Occlusion).await;
            }
            if let Some(t) = &def.emissive_texture {
                pbr.emissive_tex =
                    texture::load_texture(renderer, assets, t, true, K::Emissive).await;
            }
            Material::Pbr(Box::new(pbr))
        }
        _ => material::material_to_renderer(def),
    };
    renderer.materials.insert(
        material,
        &renderer.textures,
        &renderer.dynamic_materials,
        &renderer.extras_pool,
    )
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
