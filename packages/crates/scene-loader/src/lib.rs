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
//!
//! Beyond meshes/lights/cameras the loader also materializes the remaining
//! authored [`NodeKind`](awsm_scene::NodeKind)s: **lines** (fat-line strips,
//! world-baked), **sprites** (unlit / flipbook textured quads, optionally
//! billboarded), **decals** (oriented-cube projections, skipped with a one-time
//! warn when the renderer's `decals` feature is off), and **instances-along-curve**
//! (GPU-instanced copies of a source mesh placed along a `Curve` by arc length).
//! `Curve` / `Group` / `Collider` carry no runtime renderable.
//!
//! **`ParticleEmitter` is a documented gap**: the loader has no renderer particle
//! pass to drive, so emitter nodes are cleanly skipped (one-time warn). Particles
//! are owned by the game, which simulates them and updates its own per-frame
//! mesh/line buffers; the loader only carries the authored node's transform.

pub mod animation;
pub mod assets;
pub mod camera;
pub mod dynamic;
pub mod light;
pub mod material;
pub mod texture;

pub use assets::SceneAssets;

use std::collections::HashMap;

use animation::AnimResolveMaps;
use anyhow::{anyhow, Result};
use awsm_renderer::animation::AnimationClipKey;
use awsm_renderer::cameras::CameraKey;
use awsm_renderer::decals::DecalKey;
use awsm_renderer::lights::LightKey;
use awsm_renderer::materials::unlit::UnlitMaterial;
use awsm_renderer::materials::{Material, MaterialAlphaMode, MaterialKey};
use awsm_renderer::meshes::MeshKey;
use awsm_renderer::render_passes::lines::LineKey;
use awsm_renderer::raw_mesh::RawMeshData;
use awsm_renderer::transforms::{Transform, TransformKey};
use awsm_renderer::{AwsmRenderer, LoadPhase};
use awsm_renderer_core::texture::mipmap::MipmapTextureKind;
use awsm_renderer_gltf::loader::GltfLoader;
use awsm_renderer_gltf::{AwsmRendererGltfExt, GltfMaterialSource, PopulateGltfOpts};
use awsm_scene::{
    mesh_glb_filename, AssetId, AssetSource, CameraConfig, CurveDef, DecalConfig, EditorNode,
    InstancesAlongCurveDef, LineDef, MaterialInstance, MaterialShading, NodeId, NodeKind,
    RuntimeMesh, Scene, SpriteDef, Trs, ASSETS_DIR,
};
use glam::{Mat4, Quat, Vec3, Vec4};

/// The renderer handles a single materialized scene node produced, keyed back to
/// its [`NodeId`] in [`LoadedScene::nodes`]. A player drives named nodes every
/// frame through these: read/set the `transform`, hide via the `meshes`, attach a
/// camera rig to a `camera`/`camera_config`, etc. Non-applicable fields are empty
/// / `None` (e.g. a `Light` node has an empty `meshes` + a `Some(light)`).
#[derive(Clone, Debug, Default)]
pub struct NodeHandles {
    /// The node's local transform key (always present — every node gets one).
    /// Drive it via `renderer.transforms.set_local(handle.transform, ..)`.
    pub transform: TransformKey,
    /// Every renderer mesh this node produced (a glb node destructures into one
    /// key per primitive). Empty for non-mesh nodes. Hide the whole node with
    /// `renderer.meshes.set_mesh_hidden(key, true)` over these.
    pub meshes: Vec<MeshKey>,
    /// `Some` for `Light` nodes — the inserted [`LightKey`].
    pub light: Option<LightKey>,
    /// `Some` for `Camera` nodes — the registered renderer [`CameraKey`].
    pub camera: Option<CameraKey>,
    /// `Some` for `Camera` nodes — the authored [`CameraConfig`], handed to the
    /// consumer's camera rig (projection/near/far/behavior) alongside `camera`.
    pub camera_config: Option<CameraConfig>,
    /// `Some` for `Line` nodes — the inserted [`LineKey`].
    pub line: Option<LineKey>,
    /// `Some` for `Decal` nodes (only when the renderer's `decals` feature is on;
    /// otherwise the decal is cleanly skipped at load).
    pub decal: Option<DecalKey>,
}

/// The renderer resources `populate_awsm_scene` / [`load_scene_for_player`]
/// created, returned so a host can drive and tear down the loaded scene.
///
/// [`nodes`](Self::nodes) is the player-grade addition: a `NodeId → `
/// [`NodeHandles`] map of the **static** (non-prefab) world, so a game can drive
/// authored nodes by id every frame. [`prefabs`](Self::prefabs) holds prefab-root
/// templates to instantiate on demand. The flat [`meshes`](Self::meshes) /
/// [`lights`](Self::lights) / [`clips`](Self::clips) vecs are retained for
/// back-compat (the model-test round-trip) and for teardown.
#[derive(Default, Debug)]
pub struct LoadedScene {
    /// `NodeId → ` [`NodeHandles`] for every materialized **non-prefab** node —
    /// the live static world the player drives. (Prefab-root subtrees are in
    /// [`prefabs`](Self::prefabs) instead, materialized hidden.)
    pub nodes: HashMap<NodeId, NodeHandles>,
    /// Prefab-root `NodeId → ` template. Each is materialized once (hidden) and
    /// instantiated cheaply on demand via [`PrefabTemplate::instantiate`].
    pub prefabs: HashMap<NodeId, PrefabTemplate>,
    pub meshes: Vec<MeshKey>,
    pub lights: Vec<LightKey>,
    /// Animation clips inserted into `renderer.animations` (the scene's
    /// `StoredAnimation`s lowered to runtime clip groups). Tracked so a host can
    /// remove them on the next load — like meshes/lights, they live outside any
    /// per-node tracking. The mixer is rebuilt wholesale on each load.
    pub clips: Vec<AnimationClipKey>,
}

/// A prefab-root subtree, materialized once (hidden) as a reusable template.
/// Cheaply cloned into live instances via [`Self::instantiate`]. Opaque: holds
/// the hidden materialized handles + the per-node metadata an instance needs.
///
/// (B4 fills in the instancing body; B1 introduces the type so [`LoadedScene`]
/// can carry the map.)
#[derive(Default, Debug)]
pub struct PrefabTemplate {
    /// Per-node handles of the hidden template subtree (kept for B4 instancing +
    /// so callers can inspect the template's shape). Same `NodeId`s the instances
    /// reproduce. (Populated + consumed by B4's `instantiate`.)
    #[allow(dead_code)]
    pub(crate) nodes: HashMap<NodeId, NodeHandles>,
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
    let custom = dynamic::register_custom_materials(renderer, scene, assets).await;

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
            glam::Mat4::IDENTITY,
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

    // ── Phase 3a: commit any textures staged while materializing ──────────────
    // Phase 2's `finalize_gpu_textures` covers material textures (built in Phase
    // 1). `Sprite` / `Decal` nodes resolve their textures HERE in Phase 3 (their
    // material isn't in `collect_renderables`), so re-finalize to upload anything
    // newly staged. Idempotent — a no-op when no sprite/decal added a texture.
    renderer.finalize_gpu_textures().await?;

    // ── Assemble per-NodeId handles (R1) ──────────────────────────────────────
    // Every materialized node owns a transform; attach whatever mesh/light/camera
    // keys it produced. This is the player-grade map the loader used to discard
    // (it lived only in the private `AnimResolveMaps`). Prefab separation (B4)
    // will later route prefab-root subtrees into `loaded.prefabs` instead.
    for (&node_id, &tk) in &maps.transforms {
        loaded.nodes.insert(
            node_id,
            NodeHandles {
                transform: tk,
                meshes: maps.node_meshes.get(&node_id).cloned().unwrap_or_default(),
                light: maps.lights.get(&node_id).copied(),
                camera: maps.cameras.get(&node_id).copied(),
                camera_config: maps.camera_configs.get(&node_id).cloned(),
                line: maps.lines.get(&node_id).copied(),
                decal: maps.decals.get(&node_id).copied(),
            },
        );
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
    parent_world: glam::Mat4,
    assets: &impl SceneAssets,
    maps: &mut AnimResolveMaps,
    placeholder: MaterialKey,
    on_phase: &mut dyn FnMut(LoadPhase),
    uploaded: &mut usize,
    total: usize,
    loaded: &mut LoadedScene,
) -> Result<()> {
    let local = trs_to_transform(&node.transform);
    // World matrix for THIS node, composed up the chain by hand. We can't rely on
    // `renderer.transforms.get_world(tk)` here: `transforms.insert` seeds a node's
    // world matrix with its *local* matrix and only the later `update()` pass folds
    // in ancestors. Line / Decal nodes need the resolved world transform *now*
    // (the fat-line API bakes world-space points; a decal takes a world `Mat4`), so
    // we accumulate it through the recursion instead.
    let node_world = parent_world * local.to_matrix();
    let tk = renderer.transforms.insert(local, parent);
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
                        maps.node_meshes.entry(node.id).or_default().push(key);
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
                        maps.node_meshes
                            .entry(node.id)
                            .or_default()
                            .extend(keys.iter().copied());
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
            maps.node_meshes
                .entry(node.id)
                .or_default()
                .extend(keys.iter().copied());
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
            // Keep the authored config too, so the player's camera rig gets the
            // projection/behavior (exposed via NodeHandles.camera_config).
            maps.camera_configs.insert(node.id, cfg.clone());
        }
        NodeKind::Line(def) => {
            materialize_line(renderer, def, node.id, node_world, maps).await?;
        }
        NodeKind::Sprite(def) => {
            materialize_sprite(renderer, assets, def, node.id, tk, maps, loaded).await?;
        }
        NodeKind::Decal(cfg) => {
            materialize_decal(renderer, assets, cfg, node.id, node_world, maps).await?;
        }
        NodeKind::InstancesAlongCurve(def) => {
            materialize_instances_along_curve(renderer, scene, def, maps)?;
        }
        // A bare `Curve` is data-only: it emits no renderer node. It's consumed
        // by `InstancesAlongCurve` (and sweeps at bake time), which look the curve
        // up directly from `scene` by `NodeId` — no per-node renderer resource.
        NodeKind::Curve(_) => {}
        // B3: clean-skip — the loader has no renderer particle pass to drive.
        // The game owns particle simulation (it updates its own per-frame
        // mesh/line buffers), so materializing an emitter here would render
        // nothing. Warned once (see `warn_particle_skip`) + documented on
        // `populate_awsm_scene`.
        NodeKind::ParticleEmitter(_) => warn_particle_skip(),
        // `Group` (pure transform parent) and `Collider` (editor-only wireframe;
        // no runtime renderable) need nothing further here. `Mesh` /
        // `SkinnedMesh` / `Light` / `Camera` are handled by the arms above.
        NodeKind::Group | NodeKind::Collider(_) => {}
    }

    for child in &node.children {
        Box::pin(materialize(
            renderer,
            scene,
            child,
            Some(tk),
            node_world,
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

/// Materialize a [`NodeKind::Line`] into a renderer fat-line strip.
///
/// The screen-space fat-line API ([`AwsmRenderer::add_line_strip`]) takes
/// world-space points with no transform of its own, so we bake the node's world
/// transform (`node_world`, accumulated through the materialize recursion) into
/// each authored [`LinePoint::pos`](awsm_scene::LinePoint) before handing them
/// over. Colours pass through verbatim. Records the [`LineKey`] into
/// `maps.lines` so the `NodeHandles` assembly can wire `NodeHandles.line`.
///
/// Compiles the line pipelines once (idempotent — `ensure_line_pipelines_compiled`
/// early-returns after the first compile), so the strip draws on the first frame
/// rather than warn-skipping until the next pipeline-ready drive.
async fn materialize_line(
    renderer: &mut AwsmRenderer,
    def: &LineDef,
    node_id: NodeId,
    node_world: Mat4,
    maps: &mut AnimResolveMaps,
) -> Result<()> {
    if def.points.len() < 2 {
        // Fewer than 2 points has no segment to draw; `add_line_strip` would
        // return `None` anyway. Skip without minting a (never-drawn) entry.
        return Ok(());
    }
    let positions: Vec<Vec3> = def
        .points
        .iter()
        .map(|p| node_world.transform_point3(Vec3::from_array(p.pos)))
        .collect();
    let colors: Vec<Vec4> = def
        .points
        .iter()
        .map(|p| Vec4::from_array(p.color))
        .collect();
    if let Some(key) =
        renderer.add_line_strip(&positions, &colors, def.width_px, def.depth_test_always)?
    {
        maps.lines.insert(node_id, key);
        // Drive the (cold-boot-lazy) line pipeline compile now; idempotent.
        renderer.ensure_line_pipelines_compiled().await?;
    }
    Ok(())
}

/// Materialize a [`NodeKind::Sprite`] into a textured quad rooted under the node's
/// transform `tk`.
///
/// Geometry is `awsm_meshgen::sprite_quad` (a unit XY quad facing +Z) scaled by
/// `def.size`. The material is **Unlit** (tint + optional texture) when
/// `def.flipbook` is `None`, or a **FlipBook** material sampling `def.texture` as
/// an N×M atlas when `Some` — both bind the texture into their base-color /
/// atlas slot exactly like the editor's sprite bridge. Records the mesh key into
/// `maps.meshes` + `maps.node_meshes` so the `NodeHandles` assembly + the
/// morph-target animation path pick it up.
///
/// Billboarding: when `def.billboard != BillboardMode::None`, sets the renderer
/// mesh's billboard mode via the existing
/// [`AwsmRenderer::set_mesh_billboard_mode`] (the `Mesh.billboard_mode` field the
/// vertex shader already reads — see `apply_vertex.wgsl`). `None` leaves the quad
/// world-aligned as authored.
async fn materialize_sprite(
    renderer: &mut AwsmRenderer,
    assets: &impl SceneAssets,
    def: &SpriteDef,
    node_id: NodeId,
    tk: TransformKey,
    maps: &mut AnimResolveMaps,
    loaded: &mut LoadedScene,
) -> Result<()> {
    use awsm_renderer::materials::flipbook::{FlipBookMaterial, FlipBookMode};
    use awsm_renderer::materials::unlit::UnlitMaterial;
    use awsm_renderer::meshes::mesh::BillboardMode as RBillboard;
    use awsm_scene::{BillboardMode, FlipBookModeDef, SpriteAlphaMode};
    use awsm_renderer_core::texture::mipmap::MipmapTextureKind;

    let alpha = match def.alpha_mode {
        SpriteAlphaMode::Opaque => MaterialAlphaMode::Opaque,
        SpriteAlphaMode::Mask { cutoff_x1000 } => MaterialAlphaMode::Mask {
            cutoff: cutoff_x1000 as f32 / 1000.0,
        },
        SpriteAlphaMode::Blend => MaterialAlphaMode::Blend,
    };
    // The sprite atlas / texture is colour data → sRGB + albedo mips, like a
    // base-color slot. `None` keeps the slot unbound (a flat-tint sprite).
    let tex = match &def.texture {
        Some(t) => texture::load_texture(renderer, assets, t, true, MipmapTextureKind::Albedo).await,
        None => None,
    };

    let material = match &def.flipbook {
        Some(fb) => {
            let mut m = FlipBookMaterial::new(alpha, true);
            m.tint = def.tint;
            m.cols = fb.cols.max(1);
            m.rows = fb.rows.max(1);
            m.frame_count = fb.frame_count.max(1);
            m.fps = fb.fps;
            m.time_offset = fb.time_offset;
            m.mode = match fb.mode {
                FlipBookModeDef::Loop => FlipBookMode::Loop,
                FlipBookModeDef::PingPong => FlipBookMode::PingPong,
                FlipBookModeDef::Clamp => FlipBookMode::Clamp,
                FlipBookModeDef::Once => FlipBookMode::Once,
            };
            m.flip_y = fb.flip_y;
            m.atlas_tex = tex;
            Material::FlipBook(Box::new(m))
        }
        None => {
            let mut m = UnlitMaterial::new(alpha, true);
            m.base_color_factor = def.tint;
            m.base_color_tex = tex;
            Material::Unlit(m)
        }
    };
    let mat = renderer.materials.insert(
        material,
        &renderer.textures,
        &renderer.dynamic_materials,
        &renderer.extras_pool,
    );

    let md = awsm_meshgen::sprite_quad(def.size[0], def.size[1]);
    let key = renderer.add_raw_mesh(mesh_data_to_raw(md), tk, mat)?;

    let rbillboard = match def.billboard {
        BillboardMode::None => RBillboard::None,
        BillboardMode::YAxis => RBillboard::YAxis,
        BillboardMode::Full => RBillboard::Full,
    };
    if !matches!(rbillboard, RBillboard::None) {
        renderer.set_mesh_billboard_mode(key, rbillboard)?;
    }

    maps.meshes.entry(node_id).or_insert(key);
    maps.node_meshes.entry(node_id).or_default().push(key);
    loaded.meshes.push(key);
    Ok(())
}

/// Materialize a [`NodeKind::Decal`] into a renderer projection decal.
///
/// The decal is an oriented unit cube in world space — the node's `node_world`
/// matrix (accumulated through the recursion) supplies position / orientation /
/// size directly, matching the editor's `materialize_decal` (which reads the
/// node's world matrix). Records the [`DecalKey`] into `maps.decals` for
/// `NodeHandles.decal`.
///
/// Texture wiring: the renderer's decal `texture_index` is a *flat* texture-pool
/// index (`array_index * 64 + layer_index`, per the decal shader's hard-coded
/// 64-layers-per-array convention). When `cfg.texture` resolves to a pooled
/// texture we derive that index from `renderer.textures.get_entry`; otherwise we
/// fall back to index `0` (the editor's own decal bridge always passes `0` — it
/// does not wire decal textures at all — so an untextured decal here matches the
/// editor exactly). When the renderer's `decals` feature is off, `insert_decal`
/// returns [`AwsmDecalError::FeatureNotEnabled`]; we warn once and skip.
async fn materialize_decal(
    renderer: &mut AwsmRenderer,
    assets: &impl SceneAssets,
    cfg: &DecalConfig,
    node_id: NodeId,
    node_world: Mat4,
    maps: &mut AnimResolveMaps,
) -> Result<()> {
    use awsm_renderer::decals::AwsmDecalError;
    use awsm_renderer_core::texture::mipmap::MipmapTextureKind;

    // Resolve the decal texture to a flat pool index, mirroring the decal
    // shader's `array_index * 64 + layer_index` packing. `None` (no texture, or
    // it failed to load / isn't pooled) → index 0, as the editor bridge does.
    let texture_index = match &cfg.texture {
        Some(t) => {
            match texture::load_texture(renderer, assets, t, true, MipmapTextureKind::Albedo).await {
                Some(mt) => renderer
                    .textures
                    .get_entry(mt.key)
                    .map(|e| (e.array_index as u32) * DECAL_POOL_LAYERS_PER_ARRAY + e.layer_index as u32)
                    .unwrap_or(0),
                None => 0,
            }
        }
        None => 0,
    };

    match renderer.insert_decal(node_world, texture_index, cfg.alpha) {
        Ok(key) => {
            maps.decals.insert(node_id, key);
        }
        Err(AwsmDecalError::FeatureNotEnabled) => warn_decal_feature_off(),
        Err(err) => tracing::warn!("scene-loader: insert_decal failed: {err:?}"),
    }
    Ok(())
}

/// Layers-per-texture-array assumed by the decal shader's flat-index unpacking
/// (`texture_index % 64`, `texture_index / 64` in `material_decal_wgsl`). The
/// scene loader packs the decal `texture_index` with the same constant so a
/// resolved decal texture lands on the layer the shader samples.
const DECAL_POOL_LAYERS_PER_ARRAY: u32 = 64;

/// Materialize a [`NodeKind::InstancesAlongCurve`]: place copies of a source
/// node's mesh along a Catmull-Rom curve via GPU instancing.
///
/// Looks the `curve_node` up directly in `scene` (a [`NodeKind::Curve`]) and the
/// `source_node`'s already-materialized first mesh key up in `maps.meshes`
/// (`source_node` must be materialized before this node — true when it precedes
/// the instances node in DFS order, which the typical authoring layout
/// satisfies; resolved best-effort otherwise). Samples the curve by arc length,
/// dropping a copy every `spacing` units, offsetting `side_offset` along the
/// frame normal and (when `orient_to_tangent`) rotating +Z to the tangent. Hands
/// the resulting `Vec<Transform>` to
/// [`AwsmRenderer::enable_mesh_instancing_opaque`](awsm_renderer::AwsmRenderer).
///
/// Limitations (documented best-effort): the source node's *local* transform is
/// not re-composed into each instance (the curve frame fully defines placement);
/// `per_instance_colors` and the per-instance `shadow` config are not yet applied
/// (the opaque instancing path takes transforms only) — both are follow-ons.
fn materialize_instances_along_curve(
    renderer: &mut AwsmRenderer,
    scene: &Scene,
    def: &InstancesAlongCurveDef,
    maps: &mut AnimResolveMaps,
) -> Result<()> {
    let Some(curve) = find_curve(&scene.nodes, def.curve_node) else {
        tracing::warn!(
            "scene-loader: InstancesAlongCurve references missing/non-curve node {:?}",
            def.curve_node
        );
        return Ok(());
    };
    let Some(&source_mesh) = maps.meshes.get(&def.source_node) else {
        // The source isn't materialized (yet) — e.g. it follows this node in DFS
        // order, or isn't a mesh-bearing node. Best-effort skip with a warn.
        tracing::warn!(
            "scene-loader: InstancesAlongCurve source node {:?} has no materialized mesh \
             (must precede the instances node) — skipped",
            def.source_node
        );
        return Ok(());
    };

    let transforms = curve_instance_transforms(curve, def);
    if transforms.is_empty() {
        return Ok(());
    }
    if let Err(err) = renderer.enable_mesh_instancing_opaque(source_mesh, &transforms) {
        tracing::warn!("scene-loader: enable_mesh_instancing_opaque failed: {err}");
    }
    Ok(())
}

/// Find a [`NodeKind::Curve`]'s [`CurveDef`] by `NodeId` anywhere in the tree.
fn find_curve(nodes: &[EditorNode], id: NodeId) -> Option<&CurveDef> {
    for n in nodes {
        if n.id == id {
            if let NodeKind::Curve(def) = &n.kind {
                return Some(def);
            }
        }
        if let Some(found) = find_curve(&n.children, id) {
            return Some(found);
        }
    }
    None
}

/// Sample `curve` by arc length and build one [`Transform`] per placed instance,
/// spacing copies `def.spacing` apart, offsetting `def.side_offset` along the
/// frame normal, and (when `def.orient_to_tangent`) orienting +Z to the tangent.
fn curve_instance_transforms(curve: &CurveDef, def: &InstancesAlongCurveDef) -> Vec<Transform> {
    use awsm_curves::{Curve3, FrameSequence};

    let points: Vec<Vec3> = curve
        .control_points
        .iter()
        .map(|p| Vec3::from_array(*p))
        .collect();
    if points.len() < 2 {
        return Vec::new();
    }
    let mut crom = awsm_curves::CatmullRomCurve::new(points, curve.closed);
    crom.tension = curve.tension;

    let sample_count = curve.sample_count.max(2) as usize;
    let total_len = crom.total_length(sample_count);
    let spacing = def.spacing.max(1.0e-3);
    if total_len <= 0.0 {
        return Vec::new();
    }
    // A parallel-transport frame set gives a stable normal for `side_offset` +
    // a tangent for `orient_to_tangent` (Z+ → tangent, Y+ → normal).
    let frames = FrameSequence::parallel_transport(&crom, sample_count, Vec3::Y);

    // Walk arc length in `spacing` steps, mapping each arc-distance to a frame by
    // its normalized parameter (uniform-`t` frames; good enough for placement).
    let mut out = Vec::new();
    let mut dist = 0.0_f32;
    while dist <= total_len + 1.0e-4 {
        let t = (dist / total_len).clamp(0.0, 1.0);
        let frame_pos = (t * (sample_count - 1) as f32).round() as usize;
        let frame = &frames.frames[frame_pos.min(frames.frames.len() - 1)];
        let position = frame.position + frame.normal * def.side_offset;
        let rotation = if def.orient_to_tangent {
            frame.rotation()
        } else {
            Quat::IDENTITY
        };
        out.push(Transform {
            translation: position,
            rotation,
            scale: Vec3::ONE,
        });
        dist += spacing;
    }
    out
}

/// One-time warn that `ParticleEmitter` nodes aren't rendered by the loader.
fn warn_particle_skip() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !WARNED.swap(true, Ordering::Relaxed) {
        tracing::warn!(
            "ParticleEmitter not rendered by the loader: no renderer particle pass; the game \
             drives particles via its own per-frame mesh/line updates"
        );
    }
}

/// One-time warn that a `Decal` node was skipped because the renderer's `decals`
/// feature is off.
fn warn_decal_feature_off() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !WARNED.swap(true, Ordering::Relaxed) {
        tracing::warn!(
            "scene-loader: Decal node skipped — the renderer's `decals` feature is off, so the \
             per-decal GPU pass doesn't exist (would render as 'decal missing')"
        );
    }
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
    assets: &impl SceneAssets,
    leaf: &str,
    parent: Option<TransformKey>,
    material: MaterialKey,
) -> Result<(Vec<MeshKey>, HashMap<usize, TransformKey>)> {
    let key = format!("{ASSETS_DIR}/{leaf}");
    let bytes = assets
        .fetch(&key)
        .await
        .map_err(|_| anyhow!("bundle is missing mesh glb `{key}`"))?;
    // The bundle's glb is geometry-only (materials stripped), so `populate_gltf`
    // would decide every primitive Opaque and build no transparency geometry —
    // but we apply OUR material via `Single`. If that material is transparent the
    // transparency pass would then fail (`TransparencyGeometryBufferNotFound`), so
    // override the geometry kind from our material (per-load — the same glb asset
    // can be shared by nodes with different materials).
    use awsm_renderer_gltf::data::GltfGeometryOverride;
    let transparent = renderer.materials.is_transparency_pass(material);
    let geometry_override = if transparent {
        GltfGeometryOverride::Transparent
    } else {
        GltfGeometryOverride::FromMaterial
    };
    let hints = awsm_renderer_gltf::data::GltfDataHints::default()
        .with_geometry_override(geometry_override);
    let data = GltfLoader::from_glb_bytes(&bytes)
        .await?
        .into_data(Some(hints))?;
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
    let (keys, node_index_transforms): (Vec<MeshKey>, HashMap<usize, TransformKey>) = {
        let lookups = ctx.key_lookups.lock().unwrap();
        // The renderer mesh keys this glb produced (one per primitive), so the host
        // can remove them on teardown.
        let keys = lookups.all_mesh_keys.values().flatten().copied().collect();
        // glb node index → baked transform key — the skinned-mesh arm binds each
        // skeleton joint (by its clean-glb node index) to drive the skin.
        (keys, lookups.node_index_to_transform.clone())
    };
    // A transparent mesh is built with transparency geometry only (above), so it
    // must NOT enter the shadow pass — that pass draws from VISIBILITY geometry
    // (`shadows/render_pass.rs`), which transparent meshes lack →
    // `VisibilityGeometryBufferNotFound`. Matches `MeshShadowConfig::
    // TRANSPARENT_DEFAULT` (transparent = no cast / no receive); the bundle's
    // geometry-only glb carries no per-mesh shadow flags, so set them here.
    if transparent {
        for &k in &keys {
            let _ = renderer.set_mesh_shadow_flags(
                k,
                awsm_renderer::shadows::MeshShadowFlags {
                    cast: false,
                    receive: false,
                },
            );
        }
    }
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
        uvs1: None,
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
    assets: &impl SceneAssets,
    custom: &HashMap<AssetId, awsm_materials::MaterialShaderId>,
) -> MaterialKey {
    let Some(inst) = instance else {
        return placeholder;
    };
    // Custom-WGSL assignment: the asset resolved to a registered shader (Phase 0).
    // Build a Material::Custom (defaults + uniform overrides); `inline` is ignored.
    if let Some(&shader_id) = custom.get(&inst.asset) {
        if let Some(mat) = dynamic::build_custom_material(renderer, shader_id, inst, assets).await {
            // Upload the instance's per-slot buffer-override words into the extras
            // pool BEFORE insert (insert packs `MaterialData.<slot>_offset` from
            // `extras_pool.slice_for`, so the slice must exist first).
            renderer.upload_dynamic_material_buffers(&mat);
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
            // KHR-extension texture slots (the factors are already mapped by
            // `material_to_pbr`; bind their textures the same way the editor does).
            bind_extension_textures(renderer, assets, def, &mut pbr).await;
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

/// Bind the KHR-extension texture slots on a PBR material from the bundle,
/// mirroring the editor's `apply_extension_textures`. The extension *factors* are
/// already mapped by [`material::material_to_pbr`] (so each `pbr.<ext>` is `Some`
/// iff the material carries it); here we bind the *textures*. `color_tex` slots
/// are colour data (sRGB + albedo mips); normal maps use the normal mip kind; the
/// rest are linear data (metallic-roughness mips).
async fn bind_extension_textures(
    renderer: &mut AwsmRenderer,
    assets: &impl SceneAssets,
    def: &awsm_scene::MaterialDef,
    pbr: &mut awsm_renderer::materials::pbr::PbrMaterial,
) {
    use MipmapTextureKind as K;
    let ext = &def.extensions;
    if let (Some(e), Some(p)) = (ext.specular.as_ref(), pbr.specular.as_mut()) {
        if let Some(t) = &e.tex {
            p.tex = texture::load_texture(renderer, assets, t, false, K::MetallicRoughness).await;
        }
        if let Some(t) = &e.color_tex {
            p.color_tex = texture::load_texture(renderer, assets, t, true, K::Albedo).await;
        }
    }
    if let (Some(e), Some(p)) = (ext.transmission.as_ref(), pbr.transmission.as_mut()) {
        if let Some(t) = &e.tex {
            p.tex = texture::load_texture(renderer, assets, t, false, K::MetallicRoughness).await;
        }
    }
    if let (Some(e), Some(p)) = (
        ext.diffuse_transmission.as_ref(),
        pbr.diffuse_transmission.as_mut(),
    ) {
        if let Some(t) = &e.tex {
            p.tex = texture::load_texture(renderer, assets, t, false, K::MetallicRoughness).await;
        }
        if let Some(t) = &e.color_tex {
            p.color_tex = texture::load_texture(renderer, assets, t, true, K::Albedo).await;
        }
    }
    if let (Some(e), Some(p)) = (ext.volume.as_ref(), pbr.volume.as_mut()) {
        if let Some(t) = &e.thickness_tex {
            p.thickness_tex =
                texture::load_texture(renderer, assets, t, false, K::MetallicRoughness).await;
        }
    }
    if let (Some(e), Some(p)) = (ext.clearcoat.as_ref(), pbr.clearcoat.as_mut()) {
        if let Some(t) = &e.tex {
            p.tex = texture::load_texture(renderer, assets, t, false, K::MetallicRoughness).await;
        }
        if let Some(t) = &e.roughness_tex {
            p.roughness_tex =
                texture::load_texture(renderer, assets, t, false, K::MetallicRoughness).await;
        }
        if let Some(t) = &e.normal_tex {
            p.normal_tex = texture::load_texture(renderer, assets, t, false, K::Normal).await;
        }
    }
    if let (Some(e), Some(p)) = (ext.sheen.as_ref(), pbr.sheen.as_mut()) {
        if let Some(t) = &e.color_tex {
            p.color_tex = texture::load_texture(renderer, assets, t, true, K::Albedo).await;
        }
        if let Some(t) = &e.roughness_tex {
            p.roughness_tex =
                texture::load_texture(renderer, assets, t, false, K::MetallicRoughness).await;
        }
    }
    if let (Some(e), Some(p)) = (ext.anisotropy.as_ref(), pbr.anisotropy.as_mut()) {
        if let Some(t) = &e.tex {
            p.tex = texture::load_texture(renderer, assets, t, false, K::Normal).await;
        }
    }
    if let (Some(e), Some(p)) = (ext.iridescence.as_ref(), pbr.iridescence.as_mut()) {
        if let Some(t) = &e.tex {
            p.tex = texture::load_texture(renderer, assets, t, false, K::MetallicRoughness).await;
        }
        if let Some(t) = &e.thickness_tex {
            p.thickness_tex =
                texture::load_texture(renderer, assets, t, false, K::MetallicRoughness).await;
        }
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
