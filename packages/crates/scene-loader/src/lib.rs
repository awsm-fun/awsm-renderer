//! `populate_awsm_scene` — load an [`awsm_renderer_scene::Scene`] (the runtime bundle's
//! `scene.toml`) into the renderer. The parallel to
//! `awsm_renderer_gltf::populate_gltf`: that loads *foreign* glTF, this loads
//! *our* format. They share the same renderer core — glb meshes in a bundle go
//! through `populate_gltf`'s machinery, primitives regenerate via `awsm-renderer-meshgen`,
//! and our materials / clips bind on top.
//!
//! The headline use is the **round-trip test**: in the MCP-controlled browser
//! session, `export_player_bundle` → `populate_awsm_scene` → screenshot, compared
//! against the source render. The model-test page can load a `.glb` *or* one of
//! our exported bundles this way.
//!
//! Runs as one batched, phased pass (build materials → upload textures → upload
//! meshes → load animation → compile pipelines), reporting each [`LoadPhase`]
//! through a callback. This IS the load transaction: `begin_load` → declare every
//! op in dependency order (transforms before the geometry that references them) →
//! `commit_load`, which dedups, uploads concurrently, finalizes the texture pool,
//! and compiles pipelines ONCE — no per-op commits, no post-hoc re-materialise. The
//! editor's live bulk load mirrors this same renderer transaction (see the editor
//! `node_sync` join-barrier). Handles: the node hierarchy (transforms); **primitive**
//! meshes with their built-in materials; **glb** meshes (`assets/<id>.glb`) AND
//! **skinned** meshes (`assets/<skin.source>.glb`), both fed through
//! `populate_gltf` with [`GltfMaterialSource::Single`] so they take OUR material
//! (no glTF default-mint) and ride the same geometry+skin+morph upload foreign
//! glTF uses; **lights** (shared `light_from_config` + shadow params);
//! **cameras**; textures + custom-WGSL materials; and **animation** — the scene's
//! clips + NLA mixer ([`animation::load_animations`]) lowered against the per-node
//! keys built here. The loader only LOADS the clips; the consumer drives the
//! clock (a player's `update_animations`, or the editor round-trip's playhead
//! pin). Skinned meshes animate: `skin_joints` binds each bone `NodeId` → the rig
//! glb's joint `TransformKey`, and a bone's Transform track resolves to that joint
//! key (so the clips drive the joints the skin reads) — verified end-to-end via a
//! `LoadPlayerBundle` round-trip + driven `update_animations`.
//!
//! Beyond meshes/lights/cameras the loader also materializes the remaining
//! authored [`NodeKind`](awsm_renderer_scene::NodeKind)s: **lines** (fat-line strips,
//! world-baked), **sprites** (unlit / flipbook textured quads, optionally
//! billboarded), **decals** (oriented-cube projections, skipped with a one-time
//! warn when the renderer's `decals` feature is off), and **instances-along-curve**
//! (GPU-instanced copies of a source mesh placed along a `Curve` by arc length).
//! `Curve` / `Group` / `Collider` carry no runtime renderable.
//!
//! **`ParticleEmitter`** materializes into a ready-to-drive instanced billboard
//! (**Design A: loader sets up, game ticks**): the loader builds the emissive quad and
//! GPU instancing at `max_alive` capacity and returns an
//! [`EmitterHandle`](particles::EmitterHandle) in [`NodeHandles::emitter`]; it does
//! NOT simulate. The game ticks an [`awsm_renderer_particles::Simulator`] each frame and
//! pushes the live particles via [`drive_emitter`](particles::drive_emitter) — the
//! same "loads, doesn't drive" boundary as animation. See [`particles`].
//!
//! # Example
//!
//! Load a bundle for a player, drive an authored node, instantiate a prefab, then
//! tear the whole load down for a reload. (Won't run — it needs a live GPU
//! [`AwsmRenderer`]; written `no_run` so it still type-checks.)
//!
//! ```rust,no_run
//! use awsm_renderer_scene_loader::{load_scene_for_player, set_node_visible};
//! use awsm_renderer::{AwsmRenderer, transforms::Transform};
//! use awsm_renderer_scene::{Scene, NodeId, Trs};
//!
//! async fn run(
//!     renderer: &mut AwsmRenderer,
//!     scene: &Scene,
//!     assets: &std::collections::HashMap<String, Vec<u8>>,
//!     some_node: NodeId,
//!     prefab_root: NodeId,
//! ) -> anyhow::Result<()> {
//!     // Load the scene (in-memory `HashMap` satisfies `&impl SceneAssets`).
//!     let loaded = load_scene_for_player(renderer, scene, assets, |_phase| {}).await?;
//!
//!     // Drive an authored node by id: read its transform handle, move it.
//!     if let Some(handles) = loaded.nodes.get(&some_node) {
//!         renderer
//!             .transforms
//!             .set_local(handles.transform, Transform::default())?;
//!         // Hide/show just that node's meshes.
//!         set_node_visible(renderer, handles, false);
//!     }
//!
//!     // Instantiate a prefab at a world transform (cheap GPU-buffer-sharing clone).
//!     if let Some(template) = loaded.prefabs.get(&prefab_root) {
//!         let _instance = template.instantiate(renderer, Trs::default())?;
//!     }
//!
//!     // On reload: free everything the load created (instances are the caller's
//!     // responsibility — tear them down first via `PrefabInstance::teardown`).
//!     loaded.teardown(renderer);
//!     Ok(())
//! }
//! ```

pub mod animation;
pub mod assets;
pub mod camera;
pub mod dynamic;
pub mod environment;
pub mod light;
pub mod material;
pub mod particles;
pub mod texture;

pub use assets::SceneAssets;
pub use particles::{drive_emitter, EmitterHandle};

use std::collections::HashMap;

use animation::AnimResolveMaps;
use anyhow::{anyhow, Result};
use awsm_renderer::animation::AnimationClipKey;
use awsm_renderer::cameras::CameraKey;
use awsm_renderer::decals::DecalKey;
use awsm_renderer::lights::LightKey;
#[cfg(feature = "lod")]
use awsm_renderer::lod::{LodChain, LodLevel};
use awsm_renderer::materials::unlit::UnlitMaterial;
use awsm_renderer::materials::{Material, MaterialAlphaMode, MaterialKey};
use awsm_renderer::meshes::MeshKey;
use awsm_renderer::pipeline_scheduler::CompileProgress;
use awsm_renderer::raw_mesh::RawMeshData;
use awsm_renderer::render_passes::lines::LineKey;
use awsm_renderer::transforms::{Transform, TransformKey};
use awsm_renderer::{AwsmRenderer, LoadPhase};
use awsm_renderer_core::texture::mipmap::MipmapTextureKind;
use awsm_renderer_gltf::loader::GltfLoader;
use awsm_renderer_gltf::{AwsmRendererGltfExt, GltfMaterialSource, PopulateGltfOpts};
use awsm_renderer_scene::{
    mesh_glb_filename, AssetId, AssetSource, CameraConfig, CurveDef, DecalConfig, EditorNode,
    InstancerDef, InstancesAlongCurveDef, LightConfig, LineDef, MaterialInstance, MaterialShading,
    NodeId, NodeKind, ParticleEmitterDef, RuntimeMesh, Scene, SpriteDef, Trs, ASSETS_DIR,
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
    /// `renderer.set_mesh_hidden(key, true)` over these (or [`set_node_visible`]).
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
    /// `Some` for `ParticleEmitter` nodes — the ready-to-drive
    /// [`EmitterHandle`](particles::EmitterHandle). The loader built the instanced
    /// billboard but does NOT simulate; the game ticks it every frame via
    /// [`drive_emitter`](particles::drive_emitter) (Design A: loader sets up, game
    /// ticks).
    pub emitter: Option<EmitterHandle>,
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
    /// Every renderer [`MeshKey`] the load created — the static world's meshes
    /// **and** every hidden prefab-template mesh. Retained for the model-test
    /// round-trip and consumed by [`teardown`](Self::teardown).
    pub meshes: Vec<MeshKey>,
    /// Every [`LightKey`] the load inserted. Freed by [`teardown`](Self::teardown)
    /// via [`AwsmRenderer::remove_light`].
    pub lights: Vec<LightKey>,
    /// Animation clips inserted into `renderer.animations` (the scene's
    /// `StoredAnimation`s lowered to runtime clip groups). Tracked so a host can
    /// remove them on the next load — like meshes/lights, they live outside any
    /// per-node tracking. The mixer is rebuilt wholesale on each load.
    pub clips: Vec<AnimationClipKey>,
    /// Every [`LineKey`] the load created (one per materialized `Line` node).
    /// Freed by [`teardown`](Self::teardown). Tracked here (not just on
    /// `NodeHandles`) so teardown frees lines without re-walking `nodes`.
    pub lines: Vec<LineKey>,
    /// Every [`DecalKey`] the load created (one per materialized `Decal` node,
    /// when the renderer's `decals` feature is on). Freed by
    /// [`teardown`](Self::teardown).
    pub decals: Vec<DecalKey>,
    /// Every [`TransformKey`] the load inserted — the static world's per-node
    /// transforms **plus** each prefab template's hidden scratch + per-node
    /// transforms. Freed last by [`teardown`](Self::teardown) (after the meshes
    /// rooted under them). Collected from `maps.transforms` + prefab capture.
    pub transforms: Vec<TransformKey>,
    /// `NodeId → ` this mesh node's material palette, each variant fully
    /// built in Phase 1 into a ready [`MaterialKey`] (inline def lowered,
    /// textures fetched + bound, pipelines in the same Phase-4 compile batch).
    /// In authored order, carrying each variant's stable id + display name so
    /// a game addresses looks by NAME and swaps with a plain
    /// [`AwsmRenderer::set_mesh_material`]. The SELECTED variant is already
    /// the mesh's starting material. Nodes with empty palettes are absent.
    pub node_material_variants: HashMap<NodeId, Vec<LoadedMaterialVariant>>,
    /// Skeleton bone `NodeId → ` the rig glb's baked joint [`TransformKey`]
    /// the skin actually reads (the player-grade skin bridge — see
    /// `AnimResolveMaps::skin_joints`). A game that drives a skinned rig
    /// per frame writes THESE keys (via `renderer.transforms.set_local`),
    /// not the bone's own scene transform key. Bones of every skinned
    /// mesh in the static world are included; prefab-template rigs are
    /// not (instantiate + resolve per instance instead).
    pub skin_joints: HashMap<NodeId, TransformKey>,
    /// Mesh/skinned nodes → the material key built for them (the same map
    /// the loader's BuiltinParam animation targets resolve against).
    /// Complements [`node_material_variants`](Self::node_material_variants)
    /// for direct "the node's current material" addressing when a game
    /// drives per-node material params (uniforms, emissive) at runtime.
    pub node_materials: HashMap<NodeId, MaterialKey>,
    /// Every [`TransformKey`] under which the load minted GPU **instance rows**
    /// (`enable_mesh_instancing_opaque` transform lists + per-instance colour
    /// attributes) — one per materialized `InstancesAlongCurve` / `Instancer`
    /// node. The rows live in `renderer.instances`, outside the mesh/transform
    /// stores, so [`teardown`](Self::teardown) frees them explicitly.
    pub instanced_transforms: Vec<TransformKey>,
}

/// One pre-built entry of a mesh's material palette — see
/// [`LoadedScene::node_material_variants`].
#[derive(Clone, Debug)]
pub struct LoadedMaterialVariant {
    /// The variant's stable id (`MaterialVariant::id`).
    pub id: awsm_renderer_scene::VariantId,
    /// The variant's display name (`MaterialVariant::name`).
    pub name: String,
    /// The ready-to-assign renderer material.
    pub key: MaterialKey,
}

/// A prefab-root subtree, materialized **once** (hidden) as a reusable template,
/// then cheaply cloned into live instances via [`Self::instantiate`].
///
/// The template is *replayable structural metadata*, not a live set of handles:
/// it records every node's authored local transform, its parent within the
/// subtree, and the hidden template [`MeshKey`]s to duplicate. Instancing walks
/// that metadata to insert a fresh transform per node and
/// `duplicate_mesh_with_transform` the template meshes — each duplicate shares
/// the template's GPU geometry + material buffers, so an instance costs a handful
/// of transform slots + mesh-instance records, not a re-upload.
///
/// Opaque by contract: the fields are private; inspect the shape via
/// [`root_id`](Self::root_id) / [`node_ids`](Self::node_ids).
///
/// **Coverage:** mesh-bearing prefab nodes are replayed per instance — `Mesh`
/// (primitive + bare-geometry glb), `SkinnedMesh` (rig glb), and `Sprite` — sharing
/// the template's GPU buffers via `duplicate_mesh_with_transform`. `Light` /
/// `Camera` / `Line` / `Decal` / `ParticleEmitter` / `InstancesAlongCurve` nodes
/// *inside* a prefab are **also** re-created per instance now (A.3): each gets a
/// fresh per-instance key (lines/decals re-baked into the instance's world transform;
/// the decal texture index is resolved at capture; an emitter rebuilds its instanced
/// billboard; an `InstancesAlongCurve` bakes its curve placement at capture and a
/// second pass in `instantiate` enables instancing on the instance's own duplicated
/// source mesh; an explicit `Instancer` builds its referenced mesh ASSET as its
/// own hidden template mesh at capture and the same second pass enables
/// instancing on the instance's own duplicate). A **nested** prefab child is captured as its own template in
/// [`LoadedScene::prefabs`] — never inlined into its parent.
#[derive(Debug)]
pub struct PrefabTemplate {
    /// The prefab-root [`NodeId`] (the node authored with `prefab == true`).
    root: NodeId,
    /// The subtree in DFS pre-order (every parent precedes its children) so
    /// [`instantiate`](Self::instantiate) can wire each new transform under its
    /// already-inserted parent in a single forward pass.
    nodes: Vec<PrefabNode>,
}

/// One node of a captured [`PrefabTemplate`] subtree: enough to replay it into a
/// fresh instance.
#[derive(Debug)]
struct PrefabNode {
    /// The authored [`NodeId`] (reproduced verbatim on every instance, so callers
    /// can address instance nodes by the same id as the template).
    id: NodeId,
    /// The node's authored **local** transform. The subtree root's local is
    /// replaced by the caller's `world_trs` at instantiate time (anchoring the
    /// instance); every other node keeps its authored local under its parent.
    local: Trs,
    /// Parent **within this subtree** (`None` for the subtree root). Mapped through
    /// the per-instance `NodeId → TransformKey` table during instantiate.
    parent: Option<NodeId>,
    /// The hidden template [`MeshKey`]s this node produced (one per primitive). An
    /// instance `duplicate_mesh_with_transform`s each under the node's fresh
    /// transform. Empty for non-mesh nodes.
    template_meshes: Vec<MeshKey>,
    /// The non-mesh renderable this node replays per instance (A.3). Captured at
    /// load time (decal texture resolved then, since `instantiate` has no assets)
    /// so [`PrefabTemplate::instantiate`] can re-create a fresh light / camera /
    /// line / decal per instance. [`PrefabReplay::None`] for mesh / group nodes.
    replay: PrefabReplay,
}

/// The non-mesh renderable a [`PrefabNode`] re-creates on each instance (A.3).
///
/// Mesh-bearing nodes share GPU buffers cheaply (`duplicate_mesh_with_transform`),
/// but a light / camera / line / decal is a distinct renderer resource per
/// instance — so the template captures enough to *replay* it. Decal textures are
/// resolved to a flat pool index at capture time because
/// [`PrefabTemplate::instantiate`] runs without the asset bytes.
#[derive(Debug, Clone)]
enum PrefabReplay {
    /// Mesh / group / curve node — nothing extra to replay (the transform, and any
    /// template meshes, are handled separately).
    None,
    /// A `ParticleEmitter` — its instanced billboard is rebuilt per instance (a
    /// fresh [`EmitterHandle`] recorded on the instance node, ready for the game to
    /// drive), so a prefab containing an emitter emits, not just contributes a
    /// transform.
    ParticleEmitter(ParticleEmitterDef),
    /// A `Light` — re-inserted and bound to the instance transform.
    Light(LightConfig),
    /// A `Camera` — re-registered in the renderer camera store.
    Camera(CameraConfig),
    /// A `Line` — its authored (local) points re-baked into the instance's world
    /// transform, then a fresh strip added.
    Line(LineDef),
    /// A `Decal` — re-inserted at the instance's world transform with the texture
    /// pool index + alpha resolved at capture time.
    Decal { texture_index: u32, alpha: f32 },
    /// An `InstancesAlongCurve` — the curve placement is baked at capture (the curve
    /// node's control points are static in the template, so `instantiate`, which is
    /// asset-free, needs nothing live). A SECOND pass in `instantiate` then enables
    /// instancing on this instance's OWN duplicated source mesh, so every prefab copy
    /// carries its own instanced row.
    InstancesAlongCurve {
        transforms: Vec<Transform>,
        source_node: NodeId,
        per_instance_colors: Vec<[f32; 4]>,
    },
    /// An explicit `Instancer` — self-contained (unlike
    /// [`Self::InstancesAlongCurve`] it references no other node): the referenced
    /// mesh ASSET is built as this node's own hidden template mesh at capture
    /// (see the `Instancer` arm of [`build_node_meshes`]), so `instantiate`
    /// (asset-free) just duplicates it like any mesh node. The same second pass
    /// then enables instancing on the instance's OWN duplicated mesh with the
    /// authored transforms + colours — mirroring [`materialize_instancer`].
    /// The def's `shadow`/`lod` flags are carried on the scene node but not
    /// applied here, exactly matching the live path (documented follow-on).
    Instancer {
        transforms: Vec<Transform>,
        per_instance_colors: Vec<[f32; 4]>,
    },
}

/// Replay a prefab node's non-mesh renderable into a fresh per-instance resource
/// (A.3), recording the produced key onto `handles`. `tk` is the instance node's
/// transform; `world` its composed world matrix (lines/decals are world-space).
///
/// Best-effort: a failed line/decal insert is warned and skipped (the instance's
/// other nodes still materialize). Async pipeline warm-ups the live arms perform
/// are intentionally omitted — `instantiate` is sync and the renderer's normal
/// per-frame drive compiles line/shadow pipelines (or a prior load already did).
/// Clone a skinned prefab node's joint skeleton into a fresh per-instance copy
/// so the instance deforms independently of the template (whose skeleton is a
/// single shared glb loaded once at capture — see the `SkinnedMesh` arm of
/// [`build_node_meshes`]). Returns `template_joint_TransformKey → fresh_TransformKey`.
///
/// Each fresh joint copies its template joint's LOCAL transform. Parent wiring:
/// a joint whose template parent is ALSO a template joint → parented to that
/// parent's clone (preserving the skeleton hierarchy); a skeleton-**root** joint
/// (template parent is the glb root / an armature above the joints, i.e. NOT in
/// the joint set) → parented under `instance_root`, so driving the instance's
/// placement transform moves the whole rig and its skinned mesh. Assumes any
/// armature node between the root joint and the glb root is identity (true for
/// the rigs this ships) — a non-identity intermediate would drop its offset.
fn clone_skin_skeleton(
    renderer: &mut AwsmRenderer,
    template_joints: &[TransformKey],
    instance_root: TransformKey,
) -> HashMap<TransformKey, TransformKey> {
    let joint_set: std::collections::HashSet<TransformKey> =
        template_joints.iter().copied().collect();
    let mut fresh: HashMap<TransformKey, TransformKey> =
        HashMap::with_capacity(template_joints.len());
    // Pass 1: mint a fresh transform per joint (local copied; parent fixed next).
    for &jt in template_joints {
        let local = renderer
            .transforms
            .get_local(jt)
            .cloned()
            .unwrap_or_default();
        let new_tk = renderer.transforms.insert(local, Some(instance_root));
        fresh.insert(jt, new_tk);
    }
    // Pass 2: rewire parents now that every clone exists.
    for &jt in template_joints {
        let new_tk = fresh[&jt];
        let new_parent = match renderer.transforms.get_parent(jt).ok() {
            Some(p) if joint_set.contains(&p) => fresh.get(&p).copied(),
            _ => Some(instance_root),
        };
        renderer.transforms.set_parent(new_tk, new_parent);
    }
    fresh
}

fn replay_prefab_node(
    renderer: &mut AwsmRenderer,
    replay: &PrefabReplay,
    tk: TransformKey,
    world: Mat4,
    handles: &mut NodeHandles,
) {
    match replay {
        PrefabReplay::None => {}
        // Applied in `instantiate`'s post-loop second pass (needs the source node's
        // duplicated mesh, which only exists once the whole forest is duplicated).
        PrefabReplay::InstancesAlongCurve { .. } | PrefabReplay::Instancer { .. } => {}
        PrefabReplay::ParticleEmitter(def) => {
            // Rebuild the instanced billboard for this instance — the same sync
            // builder the main path uses (no simulation). The game drives it via the
            // recorded handle; `PrefabInstance::teardown` frees its mesh + transform.
            match particles::build_emitter(renderer, def, tk, world) {
                Ok(handle) => handles.emitter = Some(handle),
                Err(err) => {
                    tracing::warn!("prefab replay: ParticleEmitter build failed: {err}")
                }
            }
        }
        PrefabReplay::Light(cfg) => {
            // Seed pos/dir from the composed world transform; binding to `tk` lets
            // the light re-derive them each frame (the seed only matters pre-bind).
            let pos = world.w_axis.truncate();
            let dir = world.transform_vector3(Vec3::NEG_Z).normalize_or_zero();
            let lt = light::light_from_config(cfg, pos, dir);
            let shadow = light::light_shadow_params_from_config(cfg.shadow());
            if let Ok(k) = renderer.insert_light(lt, Some(shadow)) {
                renderer.lights.bind_transform(k, tk);
                handles.light = Some(k);
            }
        }
        PrefabReplay::Camera(cfg) => {
            let ck = renderer
                .cameras
                .insert(camera::camera_params_from_config(cfg));
            handles.camera = Some(ck);
            handles.camera_config = Some(cfg.clone());
        }
        PrefabReplay::Line(def) => {
            if def.points.len() < 2 {
                return;
            }
            // Bake the authored (local) points into the instance's world transform,
            // exactly as the live `materialize_line` bakes `node_world`.
            let positions: Vec<Vec3> = def
                .points
                .iter()
                .map(|p| world.transform_point3(Vec3::from_array(p.pos)))
                .collect();
            let colors: Vec<Vec4> = def
                .points
                .iter()
                .map(|p| Vec4::from_array(p.color))
                .collect();
            match renderer.add_line_strip(&positions, &colors, def.width_px, def.depth_test_always)
            {
                Ok(Some(k)) => handles.line = Some(k),
                Ok(None) => {}
                Err(err) => tracing::warn!("prefab instantiate: add_line_strip failed: {err}"),
            }
        }
        PrefabReplay::Decal {
            texture_index,
            alpha,
        } => {
            use awsm_renderer::decals::AwsmDecalError;
            match renderer.insert_decal(world, *texture_index, *alpha) {
                Ok(k) => handles.decal = Some(k),
                Err(AwsmDecalError::FeatureNotEnabled) => {}
                Err(err) => tracing::warn!("prefab instantiate: insert_decal failed: {err:?}"),
            }
        }
    }
}

impl PrefabTemplate {
    /// The prefab-root [`NodeId`] (the node authored with `prefab == true`).
    pub fn root_id(&self) -> NodeId {
        self.root
    }

    /// Every [`NodeId`] in the template subtree (root first, then descendants in
    /// DFS pre-order) — the same ids an instance reproduces in
    /// [`PrefabInstance::nodes`].
    pub fn node_ids(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.nodes.iter().map(|n| n.id)
    }

    /// Instantiate the template into a fresh, live [`PrefabInstance`] anchored at
    /// `world_trs`.
    ///
    /// Walks the template in DFS pre-order (parents first), inserting a new
    /// [`TransformKey`] per node — the **subtree root** gets `world_trs` (its
    /// authored local is *replaced*, anchoring the instance in the world), every
    /// other node keeps its authored local under its already-inserted parent. Each
    /// node's hidden template meshes are `duplicate_mesh_with_transform`d under its
    /// new transform — the duplicates **share** the template's GPU geometry +
    /// material buffers (the cheap part) and are explicitly un-hidden (a duplicate
    /// inherits the hidden template's flag).
    ///
    /// Two calls produce two `PrefabInstance`s with independent transforms + mesh
    /// keys over the same shared GPU buffers. `Light` / `Camera` / `Line` / `Decal`
    /// prefab nodes are replayed into fresh per-instance resources (A.3); other
    /// non-mesh nodes contribute only their transform (see [`PrefabTemplate`]
    /// coverage / follow-on notes).
    pub fn instantiate(
        &self,
        renderer: &mut AwsmRenderer,
        world_trs: Trs,
    ) -> Result<PrefabInstance> {
        // template NodeId → freshly-inserted instance TransformKey.
        let mut tk_for: HashMap<NodeId, TransformKey> = HashMap::with_capacity(self.nodes.len());
        // template NodeId → composed world matrix within THIS instance. `transforms
        // .insert` only seeds a node's world with its local until a later `update()`
        // folds in ancestors — but line/decal replay needs the resolved world NOW
        // (a line bakes world-space points; a decal takes a world `Mat4`), so we
        // accumulate it by hand exactly like the live `materialize` recursion.
        let mut world_for: HashMap<NodeId, Mat4> = HashMap::with_capacity(self.nodes.len());
        let mut nodes: HashMap<NodeId, NodeHandles> = HashMap::with_capacity(self.nodes.len());
        let mut root_tk: Option<TransformKey> = None;
        // ONE cloned skeleton shared across every skinned part-node of this
        // instance (template joint TransformKey → instance clone). A multi-part
        // rig (e.g. BodyShell/Accent/JointDark/Visor) skins all its parts to the
        // SAME template skeleton, so we clone it ONCE (not per part) — otherwise
        // each part gets its own parallel skeleton (4× the joints) and the
        // authored clips, which target ONE skeleton, can't drive them coherently.
        let mut shared_skeleton: HashMap<TransformKey, TransformKey> = HashMap::new();
        // Flattened per-instance cloned skin joints (see
        // `PrefabInstance::skin_joints`) — the host's handle for posing/animation.
        let mut skin_joints: Vec<TransformKey> = Vec::new();

        for pn in &self.nodes {
            // Root anchors at world_trs (replacing its authored local); others keep
            // their authored local under their (already-inserted) parent.
            let (local, parent_tk) = match pn.parent {
                None => (trs_to_transform(&world_trs), None),
                Some(parent_id) => (trs_to_transform(&pn.local), tk_for.get(&parent_id).copied()),
            };
            let world = match pn.parent {
                None => local.to_matrix(),
                Some(parent_id) => {
                    world_for.get(&parent_id).copied().unwrap_or(Mat4::IDENTITY) * local.to_matrix()
                }
            };
            let tk = renderer.transforms.insert(local, parent_tk);
            tk_for.insert(pn.id, tk);
            world_for.insert(pn.id, world);
            if pn.parent.is_none() {
                root_tk = Some(tk);
            }

            // Duplicate the hidden template meshes under the fresh transform and
            // un-hide each duplicate (it inherits the template's hidden flag).
            //
            // SKINNED meshes can't be plainly duplicated: `duplicate_mesh_with_
            // transform` shares the source resource + its single `skin_key`, so
            // every instance would bind to the ONE template skeleton and render
            // collapsed. For a skinned node we instead clone its skeleton ONCE
            // (shared across the node's skinned primitives), register a fresh
            // per-instance skin over the clones, and duplicate each primitive
            // onto that skin — so the instance deforms + moves independently.
            let mut mesh_keys = Vec::with_capacity(pn.template_meshes.len());
            let skinned: Vec<_> = pn
                .template_meshes
                .iter()
                .filter_map(|&mk| {
                    let sk = renderer.meshes.mesh_skin_key(mk).flatten()?;
                    let joints = renderer.meshes.mesh_skin_joint_transforms(mk)?;
                    Some((mk, sk, joints))
                })
                .collect();
            if !skinned.is_empty() {
                // Union of this node's skinned-primitive joints.
                let mut union: Vec<TransformKey> = Vec::new();
                let mut seen = std::collections::HashSet::new();
                for (_, _, joints) in &skinned {
                    for &j in joints {
                        if seen.insert(j) {
                            union.push(j);
                        }
                    }
                }
                // Clone only the joints NOT already in the instance's shared
                // skeleton (the first skinned part clones the whole rig; sibling
                // parts that skin to the SAME joints reuse it → one skeleton per
                // instance). A genuinely distinct skeleton (different joints) still
                // gets cloned and merged in. Parent the clones under the instance
                // ROOT so driving `root` moves the whole rig; a skinned mesh's own
                // node transform doesn't affect its skin (the joints do).
                let missing: Vec<TransformKey> = union
                    .iter()
                    .copied()
                    .filter(|j| !shared_skeleton.contains_key(j))
                    .collect();
                if !missing.is_empty() {
                    let anchor = root_tk.unwrap_or(tk);
                    let clones = clone_skin_skeleton(renderer, &missing, anchor);
                    for (t, c) in clones {
                        shared_skeleton.insert(t, c);
                        skin_joints.push(c);
                    }
                }
            }
            for &template_key in &pn.template_meshes {
                let new_key = match skinned.iter().find(|(mk, _, _)| *mk == template_key) {
                    Some((_, tsk, joints)) => {
                        let instance_joints: Vec<TransformKey> = joints
                            .iter()
                            .filter_map(|jt| shared_skeleton.get(jt).copied())
                            .collect();
                        let instance_skin =
                            renderer.clone_skin_for_joints(*tsk, instance_joints)?;
                        renderer.duplicate_skinned_mesh_with_skin(
                            template_key,
                            tk,
                            instance_skin,
                        )?
                    }
                    None => renderer.duplicate_mesh_with_transform(template_key, tk)?,
                };
                renderer.set_mesh_hidden(new_key, false)?;
                mesh_keys.push(new_key);
            }

            let mut handles = NodeHandles {
                transform: tk,
                meshes: mesh_keys,
                ..Default::default()
            };
            // A.3: replay this node's non-mesh renderable into a fresh per-instance
            // resource. Pipeline compiles that the live arms `await`
            // (`ensure_line_pipelines_compiled` / `ensure_shadow_pipelines_compiled`)
            // are skipped — `instantiate` is sync; the renderer's normal per-frame
            // pipeline drive compiles them (or a prior load with a line/caster did).
            replay_prefab_node(renderer, &pn.replay, tk, world, &mut handles);

            nodes.insert(pn.id, handles);
        }

        // SECOND PASS — instancing replays: now that every node's mesh is duplicated,
        // enable instancing on each instance's OWN mesh copy. An
        // `InstancesAlongCurve` targets its captured SOURCE node's duplicate (a
        // curve + source + instances trio inside the prefab); an explicit
        // `Instancer` targets its own node's duplicate (its referenced mesh asset
        // was built as its template mesh at capture). Best-effort: a missing
        // source is warned + skipped (just that row doesn't appear); the rest
        // still instantiate. Every minted instance row's `TransformKey` is
        // recorded in `instanced_transforms` so `teardown` frees the rows.
        let mut instanced_transforms: Vec<TransformKey> = Vec::new();
        for pn in &self.nodes {
            let (mesh_node, transforms, per_instance_colors) = match &pn.replay {
                PrefabReplay::InstancesAlongCurve {
                    transforms,
                    source_node,
                    per_instance_colors,
                } => (*source_node, transforms, per_instance_colors),
                PrefabReplay::Instancer {
                    transforms,
                    per_instance_colors,
                } => (pn.id, transforms, per_instance_colors),
                _ => continue,
            };
            if transforms.is_empty() {
                continue;
            }
            let Some(&source_mesh) = nodes.get(&mesh_node).and_then(|h| h.meshes.first()) else {
                tracing::warn!(
                    "prefab: instancing node {:?} has no duplicated mesh in this \
                     instance — instancing skipped",
                    mesh_node
                );
                continue;
            };
            let transform_key = match renderer.meshes.get(source_mesh) {
                Ok(m) => m.transform_key,
                Err(_) => continue,
            };
            if let Err(err) = renderer.enable_mesh_instancing_opaque(source_mesh, transforms) {
                tracing::warn!("prefab: enable_mesh_instancing_opaque failed: {err}");
                continue;
            }
            instanced_transforms.push(transform_key);
            if !per_instance_colors.is_empty() {
                let attrs: Vec<awsm_renderer::instances::InstanceAttr> =
                    expand_instance_colors(per_instance_colors, transforms.len())
                        .into_iter()
                        .map(|c| {
                            awsm_renderer::instances::InstanceAttr::from_rgba_alpha_size(
                                c, 1.0, 1.0,
                            )
                        })
                        .collect();
                if let Err(err) = renderer.set_mesh_instance_attrs(transform_key, &attrs) {
                    tracing::warn!("prefab: per-instance colours failed: {err}");
                }
            }
        }

        let root = root_tk.ok_or_else(|| anyhow!("prefab template has no root node"))?;
        Ok(PrefabInstance {
            root,
            nodes,
            skin_joints,
            joint_remap: shared_skeleton,
            instanced_transforms,
        })
    }
}

/// A live, cheaply-cloned instance of a [`PrefabTemplate`] — fresh transforms +
/// duplicated meshes (sharing the template's GPU buffers), addressable by the
/// template's authored [`NodeId`]s.
///
/// Two [`instantiate`](PrefabTemplate::instantiate) calls yield two
/// `PrefabInstance`s with **independent** [`root`](Self::root) transforms and
/// independent mesh keys, but the same underlying GPU geometry/material — move one
/// instance's `root` and the others stay put.
#[derive(Clone, Debug)]
pub struct PrefabInstance {
    /// The instance's root [`TransformKey`] (anchored at the `world_trs` passed to
    /// [`instantiate`](PrefabTemplate::instantiate)). Drive the whole instance by
    /// moving this transform.
    pub root: TransformKey,
    /// `NodeId → ` [`NodeHandles`] for every node in the instance, keyed by the
    /// **template's** authored ids. Mesh-bearing nodes carry their fresh visible
    /// mesh keys; non-mesh nodes carry only their transform (see
    /// [`PrefabTemplate`] coverage notes).
    pub nodes: HashMap<NodeId, NodeHandles>,
    /// Every per-instance **cloned skin joint** [`TransformKey`] — ONE shared
    /// skeleton per instance (a multi-part rig skins all its parts to it). A host
    /// can pose the instance by writing these joints' locals and reading their
    /// bind pose via `get_local` / `get_world`. Empty for a non-skinned prefab.
    pub skin_joints: Vec<TransformKey>,
    /// **Template joint `TransformKey` → this instance's cloned joint.** The
    /// authored clips target the TEMPLATE's joints (bound in the loader's skinned
    /// arm); this map lets a host RETARGET a clip onto THIS instance — clone the
    /// clip's channels remapping each `AnimationTarget::Transform(templateTK)`
    /// through this map — so the scene's animations play per-instance. Empty for
    /// a non-skinned prefab.
    pub joint_remap: HashMap<TransformKey, TransformKey>,
    /// Every [`TransformKey`] under which `instantiate`'s second pass minted GPU
    /// **instance rows** (`enable_mesh_instancing_opaque` transform lists +
    /// per-instance colour attributes) — one entry per replayed
    /// `InstancesAlongCurve` / `Instancer` node. Tracked so
    /// [`teardown`](Self::teardown) frees the rows (they live in
    /// `renderer.instances`, outside the mesh/transform stores). Empty for a
    /// prefab without instancing nodes.
    pub instanced_transforms: Vec<TransformKey>,
}

impl LoadedScene {
    /// Free every renderer resource this load created, so a reload doesn't leak.
    /// **Consumes** the scene.
    ///
    /// Frees, in order: every [`MeshKey`] in [`meshes`](Self::meshes) (the static
    /// world's meshes **and** every hidden prefab-template mesh) via
    /// `renderer.remove_mesh`; every [`LightKey`] via
    /// [`AwsmRenderer::remove_light`]; every [`AnimationClipKey`] via
    /// `renderer.animations.remove_clip`; every [`LineKey`] /
    /// [`DecalKey`] via [`AwsmRenderer::remove_line`] /
    /// [`AwsmRenderer::remove_decal`]; and finally every [`TransformKey`] (the
    /// static world's per-node transforms **plus** each prefab template's scratch
    /// transform) via `renderer.transforms.remove` — transforms last, after the
    /// meshes rooted under them.
    ///
    /// **Not** freed (caller's responsibility): resources created **after** the
    /// load from a [`PrefabTemplate`]. A live [`PrefabInstance`] mints its own
    /// fresh transforms + duplicated meshes at
    /// [`instantiate`](PrefabTemplate::instantiate) time, which this load never saw
    /// — tear an instance down with [`PrefabInstance::teardown`] before calling
    /// this (this frees the hidden *templates*, but a duplicate that outlives its
    /// template's GPU buffers is undefined).
    pub fn teardown(self, renderer: &mut AwsmRenderer) {
        for mesh in self.meshes {
            renderer.remove_mesh(mesh);
        }
        for light in self.lights {
            renderer.remove_light(light);
        }
        for clip in self.clips {
            renderer.animations.remove_clip(clip);
        }
        for line in self.lines {
            renderer.remove_line(line);
        }
        for decal in self.decals {
            renderer.remove_decal(decal);
        }
        // Instance rows minted by InstancesAlongCurve / Instancer nodes live in
        // `renderer.instances`, keyed by the instanced mesh's TransformKey —
        // `remove_mesh` above does not free them, so a load/teardown cycle
        // would leak one transform-list + attribute slice per instancing node.
        for tk in self.instanced_transforms {
            renderer.instances.transform_remove(tk);
            renderer.instances.attribute_remove(tk);
        }
        // Transforms last — meshes/lights bound to them are already gone.
        for tk in self.transforms {
            renderer.transforms.remove(tk);
        }
    }
}

impl PrefabInstance {
    /// Free the fresh transforms + duplicated meshes this instance created (the
    /// resources [`instantiate`](PrefabTemplate::instantiate) minted, which the
    /// owning [`LoadedScene::teardown`] does not track). **Consumes** the instance.
    ///
    /// Removes every duplicated [`MeshKey`] across the instance's nodes, then the
    /// replayed [`LightKey`] / [`LineKey`] / [`DecalKey`] / emitter billboard (A.3), then every per-node
    /// [`TransformKey`] **and** every cloned skeleton joint in
    /// [`skin_joints`](Self::skin_joints) (meshes/lights/lines/decals first, transforms last),
    /// **and** every GPU instance row in
    /// [`instanced_transforms`](Self::instanced_transforms) (transform lists +
    /// colour attributes minted by the instancing second pass). The
    /// shared template GPU buffers stay alive — they belong to the still-loaded
    /// template (freed by [`LoadedScene::teardown`]); only this instance's
    /// duplicates + replayed resources + transform slots are released. (Replayed
    /// `Camera`s are not freed — the renderer camera store has no remove, matching
    /// the static loader, which also never frees cameras.)
    pub fn teardown(self, renderer: &mut AwsmRenderer) {
        for handles in self.nodes.values() {
            for &mesh in &handles.meshes {
                renderer.remove_mesh(mesh);
            }
            if let Some(light) = handles.light {
                renderer.remove_light(light);
            }
            if let Some(line) = handles.line {
                renderer.remove_line(line);
            }
            if let Some(decal) = handles.decal {
                renderer.remove_decal(decal);
            }
            // A replayed emitter owns its own billboard mesh + sub-transform.
            if let Some(emitter) = &handles.emitter {
                renderer.remove_mesh(emitter.mesh);
                renderer.transforms.remove(emitter.instance_transform);
            }
        }
        for handles in self.nodes.values() {
            renderer.transforms.remove(handles.transform);
        }
        // The cloned skeleton is per-instance too: `instantiate` minted a fresh
        // TransformKey per joint (see `clone_skeleton`), and those live outside
        // `nodes` — freeing only the node transforms leaks a full skeleton per
        // spawn/despawn cycle.
        for jt in self.skin_joints {
            renderer.transforms.remove(jt);
        }
        // Instance rows minted by the second pass (InstancesAlongCurve /
        // Instancer replays) live in `renderer.instances`, keyed by the
        // instanced mesh's TransformKey — removing the mesh + transform above
        // does NOT free them, so a spawn/despawn cycle would leak one
        // transform-list + attribute slice per instancing node.
        for tk in self.instanced_transforms {
            renderer.instances.transform_remove(tk);
            renderer.instances.attribute_remove(tk);
        }
    }
}

/// Toggle the **mesh** visibility of a previously-loaded node (from
/// [`LoadedScene::nodes`] or a [`PrefabInstance`]) — sets
/// `renderer.set_mesh_hidden(k, !visible)` for every [`MeshKey`] in
/// `handles.meshes` (per-key errors are ignored).
///
/// **Mesh-only:** a `Light` / `Line` / `Decal` / `Camera` node is *not* toggled by
/// this helper (the renderer has no per-light/-line/-decal hide toggle today —
/// honoring `visible` for those at load is done by skipping them; runtime toggling
/// of those node kinds is a follow-on). For a mesh node (incl. sprites) it hides /
/// shows the whole node.
pub fn set_node_visible(renderer: &mut AwsmRenderer, handles: &NodeHandles, visible: bool) {
    for &k in &handles.meshes {
        let _ = renderer.set_mesh_hidden(k, !visible);
    }
}

/// Schema [`awsm_renderer_scene::PostProcessConfig`] → runtime
/// [`awsm_renderer::post_process::PostProcessing`]. THE single mapping — shared
/// by the player load path ([`populate_awsm_scene`]) and the editor's live
/// `settings_sync`, so an authored tonemapper/bloom/DoF/exposure lowers
/// identically in both (the round-trip premise, same as lights/cameras).
pub fn post_process_to_renderer(
    pp: &awsm_renderer_scene::PostProcessConfig,
) -> awsm_renderer::post_process::PostProcessing {
    use awsm_renderer::post_process::ToneMapping;
    use awsm_renderer_scene::ToneMappingConfig as T;
    awsm_renderer::post_process::PostProcessing {
        tonemapping: match pp.tonemapping {
            T::None => ToneMapping::None,
            T::KhronosNeutralPbr => ToneMapping::KhronosNeutralPbr,
            T::Aces => ToneMapping::Aces,
        },
        bloom: pp.bloom,
        dof: pp.dof,
        exposure: pp.exposure,
        bloom_threshold: pp.bloom_threshold,
        bloom_knee: pp.bloom_knee,
        bloom_intensity: pp.bloom_intensity,
        bloom_scatter: pp.bloom_scatter,
        ssr: awsm_renderer::post_process::Ssr {
            enabled: pp.ssr.enabled,
            intensity: pp.ssr.intensity,
            max_distance: pp.ssr.max_distance,
            thickness: pp.ssr.thickness,
            max_steps: pp.ssr.max_steps,
            spread_cutoff: pp.ssr.spread_cutoff,
            edge_fade: pp.ssr.edge_fade,
            resolution_scale: pp.ssr.resolution_scale,
            temporal: pp.ssr.temporal,
            temporal_weight: pp.ssr.temporal_weight,
            debug: pp.ssr.debug,
        },
    }
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
    on_phase: impl FnMut(LoadPhase),
) -> Result<LoadedScene> {
    // The `&HashMap<String, Vec<u8>>` satisfies `&impl SceneAssets` via the blanket
    // impl in `assets`, so this is a thin forward to the generic player entry.
    load_scene_for_player(renderer, scene, assets, on_phase).await
}

/// Load a runtime [`Scene`] into the renderer for a **player** — the generic,
/// asset-source-agnostic entry behind [`populate_awsm_scene`]. Identical batched,
/// phased pass; takes `assets: &impl `[`SceneAssets`] so a player can stream bundle
/// bytes from disk/network/embed, not just an in-memory `HashMap`.
///
/// Returns a [`LoadedScene`] whose [`nodes`](LoadedScene::nodes) map drives the
/// static world by [`NodeId`] and whose [`prefabs`](LoadedScene::prefabs) are
/// instantiated on demand. Tear the whole load down with
/// [`LoadedScene::teardown`]. See [`populate_awsm_scene`] for the per-phase
/// rationale (the two share this body), and the crate-level docs for an example.
///
/// Visibility (B5): a node authored `visible == false` (propagated through
/// `Group`s to its descendants) has its meshes loaded hidden and its
/// lines/decals/**lights** skipped (a hidden node no longer emits its light).
/// Runtime toggling via [`set_node_visible`] applies to meshes only; re-showing a
/// skipped line/decal/light at runtime needs the renderer to grow a per-kind hide
/// toggle (a follow-on).
pub async fn load_scene_for_player(
    renderer: &mut AwsmRenderer,
    scene: &Scene,
    assets: &impl SceneAssets,
    mut on_phase: impl FnMut(LoadPhase),
) -> Result<LoadedScene> {
    // ── Asset prefetch ───────────────────────────────────────────────────────
    // Wrap the source so the bundle files we can ENUMERATE up front — mesh
    // glbs, their LOD manifests, the environment cubemaps — are fetched
    // concurrently now and served from memory when the (serial, renderer-
    // holding) walk below consumes them. The wrapper also dedupes: a glb
    // shared by N nodes downloads once, not N times. Texture images aren't
    // seeded here — they have their own decode-level prefetch below.
    let assets = &crate::assets::PrefetchedAssets::new(assets);
    {
        let mut paths: Vec<String> = scene
            .environment
            .ktx_asset_ids()
            .into_iter()
            .map(awsm_renderer_scene::env_ktx_path)
            .collect();
        collect_prefetch_paths(&scene.nodes, scene, &mut paths);
        assets
            .seed(paths, |done, total| {
                on_phase(LoadPhase::FetchingAssets { done, total })
            })
            .await;
    }

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

    // ── Texture prefetch ─────────────────────────────────────────────────────
    // Fetch + decode every unique texture image the scene's materials (and
    // their variants) reference, CONCURRENTLY — network fetches and the
    // browser's `createImageBitmap` decode threads overlap instead of
    // serializing one slot at a time through the material walk below. On a
    // deployed bundle this step dominates the load, so it reports live
    // per-image progress. Anything the collector misses (sprite atlases,
    // decal textures) falls back to `load_texture`'s on-demand path — same
    // result, just without the overlap. The cache also dedupes pool staging:
    // an image shared by N material slots uploads ONCE per color semantics
    // instead of N times.
    let mut cache = texture::TextureCache::new(scene);
    {
        let mut seen = std::collections::HashSet::new();
        let mut ids: Vec<awsm_renderer_scene::AssetId> = Vec::new();
        let mut collect = |inst: &MaterialInstance| {
            if custom.contains_key(&inst.asset) {
                // Custom-WGSL assignment: only its texture overrides bind images.
                for tref in inst.texture_overrides.values() {
                    if seen.insert(tref.asset) {
                        ids.push(tref.asset);
                    }
                }
            } else {
                for tref in inst.inline.texture_refs() {
                    if seen.insert(tref.asset) {
                        ids.push(tref.asset);
                    }
                }
            }
        };
        for (_, material) in &renderables {
            if let Some(inst) = material {
                collect(inst);
            }
        }
        for (_, variants) in collect_material_variants(&scene.nodes) {
            for v in variants {
                collect(&v.instance);
            }
        }
        cache
            .prefetch(assets, ids, |done, total| {
                on_phase(LoadPhase::FetchingTextures { done, total })
            })
            .await;
    }
    let cache = &mut cache;

    for (i, (id, material)) in renderables.iter().enumerate() {
        on_phase(LoadPhase::BuildingMaterials { done: i, total });
        let key = resolve_material(renderer, cache, *material, placeholder, assets, &custom).await;
        maps.node_materials.insert(*id, key);
        // A custom-WGSL asset's first built key is the one a Uniform track drives
        // (an asset assigned to N nodes mints N keys; mirror the editor's
        // first-match `material_key_for_shader`).
        if let Some(inst) = material {
            if custom.contains_key(&inst.asset) {
                maps.custom_materials.entry(inst.asset).or_insert(key);
            }
        }
    }
    // The material palette (`NodeKind::material_variants`): build EVERY variant
    // into a ready key exactly like the selected one, so the game can swap
    // looks with `set_mesh_material` alone. Built in Phase 1 with everything
    // else so pipelines compile in the same Phase-4 batch (no first-swap
    // hitch). (The selected variant is built twice — once here, once as the
    // node's starting material above; the texture cache dedupes the heavy
    // work.)
    let mut node_material_variants: HashMap<NodeId, Vec<LoadedMaterialVariant>> = HashMap::new();
    for (id, variants) in collect_material_variants(&scene.nodes) {
        let mut entries = Vec::with_capacity(variants.len());
        for v in variants {
            let key = resolve_material(
                renderer,
                cache,
                Some(&v.instance),
                placeholder,
                assets,
                &custom,
            )
            .await;
            entries.push(LoadedMaterialVariant {
                id: v.id,
                name: v.name.clone(),
                key,
            });
        }
        node_material_variants.insert(id, entries);
    }
    on_phase(LoadPhase::BuildingMaterials { done: total, total });
    // The custom-WGSL asset → shader-id table (Phase 0) feeds Uniform resolution.
    maps.custom_shaders = custom;

    // ── Phase 2: meshes are staged below; texture finalize + pipeline compile
    //    both happen in ONE `commit_load` at Phase 4 (the load transaction's
    //    single compile point), so there is no separate texture-finalize here.

    // ── Phase 3: upload meshes (geometry + skins) + lights ────────────────────
    let mut loaded = LoadedScene {
        node_material_variants,
        ..LoadedScene::default()
    };
    let mut uploaded = 0usize;
    for node in &scene.nodes {
        materialize(
            renderer,
            cache,
            scene,
            node,
            None,
            glam::Mat4::IDENTITY,
            // Roots have no parent to inherit from — start visible; each node's own
            // `visible` flag (and ancestors') then gates the subtree.
            true,
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

    // (Textures staged here by `Sprite` / `Decal` nodes are committed by the
    // single Phase-4 `commit_load` below — no separate finalize needed.)

    // ── Assemble per-NodeId handles (R1) ──────────────────────────────────────
    // Every materialized node owns a transform; attach whatever mesh/light/camera
    // keys it produced. This is the player-grade map the loader used to discard
    // (it lived only in the private `AnimResolveMaps`). Prefab separation (B4)
    // will later route prefab-root subtrees into `loaded.prefabs` instead.
    for (&node_id, &tk) in &maps.transforms {
        // Track every static-world transform for teardown (prefab template
        // transforms are tracked separately in `capture_prefab`).
        loaded.transforms.push(tk);
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
                emitter: maps.emitters.get(&node_id).cloned(),
            },
        );
    }
    // Track lines + decals for teardown (they live outside per-node tracking like
    // meshes/lights). `maps.lights` is already mirrored into `loaded.lights` as
    // lights are inserted; lines/decals are gathered here from the resolved maps.
    loaded.lines.extend(maps.lines.values().copied());
    loaded.decals.extend(maps.decals.values().copied());
    // Surface the skin bridge + per-node material keys to the consumer:
    // games driving skinned rigs / material params per frame resolve
    // through these (see the field docs on `LoadedScene`).
    loaded.skin_joints = maps.skin_joints.clone();
    loaded.node_materials = maps.node_materials.clone();

    // ── Phase 3b: load animation clips + the NLA mixer ────────────────────────
    // Now that every node's transform / material / light / camera / mesh key
    // exists, lower the scene's clips + mixer against them and insert into the
    // renderer. The loader only LOADS animation; the consumer drives the clock
    // (`update_animations` each frame, or the editor round-trip's playhead pin).
    loaded.clips = animation::load_animations(renderer, scene, &maps);

    // ── Environment: apply skybox + IBL BEFORE the Phase-4 compile so
    //    IBL-sampling materials compile against the final environment (mirrors
    //    the editor's `env_sync::apply_initial` running before the first paint).
    //    FATAL on failure: silently falling back to the built-in default renders
    //    the whole scene with the wrong lighting, which reads as "the player is
    //    broken" and hides real problems — a missing/corrupt/cache-poisoned
    //    `assets/<id>.ktx2` for a file-based environment, or GPU/context trouble
    //    for a procedural one. Every scene has an environment config (absent ⇒
    //    built-in default), so there is no legitimate can't-apply case to
    //    degrade through.
    environment::apply_environment(renderer, &scene.environment, assets)
        .await
        .map_err(|err| {
            err.context("scene environment failed to load — refusing to silently render with the default environment")
        })?;

    // ── Post-processing: apply the scene's authored tonemapping / bloom / DoF /
    //    exposure. Placed BEFORE the Phase-4 commit like the environment, so the
    //    effects/display pipelines it selects compile in the same batch. `#[serde
    //    (default)]` matches the renderer defaults, so pre-schema bundles no-op.
    //    Non-fatal like the environment apply.
    if let Err(err) = renderer
        .set_post_processing(post_process_to_renderer(&scene.post_process))
        .await
    {
        tracing::warn!("post-processing apply failed, using renderer default: {err}");
    }

    // ── Phase 4: THE commit — finalize the texture pool ONCE + compile every
    //    pipeline the scene needs, against the now-final content. This is the
    //    load transaction's single compile point (`commit_load`), replacing the
    //    old hand-rolled finalize×2 + `wait_for_pipelines_ready`. Maps the
    //    unified `LoadingStats` back onto the loader's coarse `LoadPhase`.
    renderer
        .commit_load(|stats| {
            use awsm_renderer::loading::LoadPhase as P;
            match stats.phase {
                P::UploadingGeometry => on_phase(LoadPhase::UploadingMeshes {
                    done: stats.geometry_uploaded,
                    total: stats.geometry_total,
                }),
                P::FinalizingTextures => on_phase(LoadPhase::UploadingTextures {
                    done: stats.textures_uploaded,
                    total: stats.textures_total,
                }),
                P::PreparingMaterials => on_phase(LoadPhase::PreparingMaterials),
                P::Compiling => on_phase(LoadPhase::CompilingPipelines(CompileProgress {
                    materials_pending: stats.pipelines_pending,
                    materials_ready: stats.pipelines_ready,
                    materials_failed: stats.pipelines_failed,
                    in_flight_subcompiles: stats.in_flight_subcompiles,
                })),
                P::Idle | P::Ready => {}
            }
        })
        .await?;
    Ok(loaded)
}

/// Walk the node tree collecting every bundle path the materialize walk below
/// will fetch that we can ENUMERATE up front — mesh / skinned-rig glbs and
/// (with the `lod` feature, for opted-in nodes) their LOD manifests — for
/// [`assets::PrefetchedAssets`] seeding. Path derivations mirror the
/// consumption sites exactly ([`load_glb_under`], [`load_static_lod_chain`],
/// [`load_skinned_lod_chain`]); a drifted path is only a cache miss, never a
/// wrong load. Cluster meshes are skipped — their pages stream on demand by
/// design.
fn collect_prefetch_paths(nodes: &[EditorNode], scene: &Scene, paths: &mut Vec<String>) {
    #[cfg(feature = "lod")]
    let manifest_path = |asset_id: String| {
        format!(
            "{ASSETS_DIR}/{}",
            awsm_renderer_lod_bake::lod_manifest_filename(&asset_id)
        )
    };
    for node in nodes {
        match &node.kind {
            NodeKind::Mesh { mesh, .. } => {
                if let Some(entry) = scene.assets.get(mesh.0) {
                    if matches!(entry.source, AssetSource::Mesh(RuntimeMesh::Glb)) {
                        paths.push(format!("{ASSETS_DIR}/{}", mesh_glb_filename(mesh.0)));
                        #[cfg(feature = "lod")]
                        if node_lod_enabled(&node.kind) {
                            paths.push(manifest_path(mesh.0.to_string()));
                        }
                    }
                }
            }
            NodeKind::SkinnedMesh { skin, .. } => {
                paths.push(format!("{ASSETS_DIR}/{}", mesh_glb_filename(skin.source)));
                #[cfg(feature = "lod")]
                if node_lod_enabled(&node.kind) {
                    paths.push(manifest_path(skin.source.to_string()));
                }
            }
            // An explicit instancer fetches its mesh asset's glb exactly like a
            // Mesh node (same derivation as `materialize_instancer`).
            NodeKind::Instancer(def) => {
                if let Some(entry) = scene.assets.get(def.mesh.0) {
                    if matches!(entry.source, AssetSource::Mesh(RuntimeMesh::Glb)) {
                        paths.push(format!("{ASSETS_DIR}/{}", mesh_glb_filename(def.mesh.0)));
                    }
                }
            }
            _ => {}
        }
        collect_prefetch_paths(&node.children, scene, paths);
    }
}

/// Walk the tree collecting each mesh node's material palette (see
/// `NodeKind::material_variants`). Companion to [`collect_renderables`];
/// nodes with empty palettes are skipped.
fn collect_material_variants(
    nodes: &[EditorNode],
) -> Vec<(NodeId, &Vec<awsm_renderer_scene::MaterialVariant>)> {
    let mut out = Vec::new();
    fn walk<'a>(
        nodes: &'a [EditorNode],
        out: &mut Vec<(NodeId, &'a Vec<awsm_renderer_scene::MaterialVariant>)>,
    ) {
        for n in nodes {
            if let Some(variants) = n.kind.material_variants() {
                if !variants.is_empty() {
                    out.push((n.id, variants));
                }
            }
            walk(&n.children, out);
        }
    }
    walk(nodes, &mut out);
    out
}

/// Flatten the tree (DFS) to the renderable nodes that carry a material
/// palette — `Mesh` and `SkinnedMesh` — as `(node id, selected variant's
/// instance)`. Used to build every mesh's STARTING material up front (Phase 1)
/// and to size the mesh-upload progress.
fn collect_renderables(nodes: &[EditorNode]) -> Vec<(NodeId, Option<&MaterialInstance>)> {
    let mut out = Vec::new();
    fn walk<'a>(nodes: &'a [EditorNode], out: &mut Vec<(NodeId, Option<&'a MaterialInstance>)>) {
        for n in nodes {
            match &n.kind {
                NodeKind::Mesh { .. } | NodeKind::SkinnedMesh { .. } => {
                    out.push((n.id, n.kind.selected_material()));
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
    cache: &mut texture::TextureCache,
    scene: &Scene,
    node: &EditorNode,
    parent: Option<TransformKey>,
    parent_world: glam::Mat4,
    parent_effective_visible: bool,
    assets: &impl SceneAssets,
    maps: &mut AnimResolveMaps,
    placeholder: MaterialKey,
    on_phase: &mut dyn FnMut(LoadPhase),
    uploaded: &mut usize,
    total: usize,
    loaded: &mut LoadedScene,
) -> Result<()> {
    // Visibility (B5) propagates down the hierarchy: a node is effectively visible
    // only if it AND every ancestor is. A `Group`/`Mesh`/etc. authored
    // `visible == false` thus hides all descendants (the flag rides the recursion).
    // Meshes of a hidden node are `set_mesh_hidden(true)`; lines/decals/lights are
    // SKIPPED entirely (cleaner than minting then hiding — the renderer has no
    // per-line/-decal/-light hide toggle), so a hidden node does not emit its light.
    let effective_visible = parent_effective_visible && node.visible;
    // Prefab root: capture the whole subtree as a hidden, reusable template and
    // return BEFORE inserting any transform — so neither this node nor its
    // descendants enter the static world (`loaded.nodes` / `maps`). Instances are
    // minted later, on demand, via `PrefabTemplate::instantiate`.
    if node.prefab {
        let tmpl = capture_prefab(
            renderer,
            cache,
            scene,
            node,
            None,
            assets,
            maps,
            placeholder,
            loaded,
        )
        .await?;
        loaded.prefabs.insert(node.id, tmpl);
        return Ok(());
    }

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
                        let md = awsm_renderer_meshgen::primitive_mesh(shape);
                        let key = renderer.add_raw_mesh(mesh_data_to_raw(md), tk, mat)?;
                        maps.meshes.entry(node.id).or_insert(key);
                        maps.node_meshes.entry(node.id).or_default().push(key);
                        loaded.meshes.push(key);
                    }
                    AssetSource::Mesh(RuntimeMesh::Glb) => {
                        // Cluster LOD (Phase B): when `virtual_geometry` is on and
                        // this static asset has a baked cluster DAG, render its
                        // finest cut and skip the base glb. `None` (vg off / no
                        // cluster data) falls through to the base glb path.
                        let cluster_key =
                            load_cluster_lod(renderer, assets, &mesh.0.to_string(), tk, mat)
                                .await?;
                        if let Some(ckey) = cluster_key {
                            maps.meshes.entry(node.id).or_insert(ckey);
                            maps.node_meshes.entry(node.id).or_default().push(ckey);
                            loaded.meshes.push(ckey);
                        } else {
                            // Bare geometry glb (single identity node) — root it
                            // UNDER the scene node's transform, which is what places it.
                            let (keys, _, _) = load_glb_under(
                                renderer,
                                assets,
                                &mesh_glb_filename(mesh.0),
                                Some(tk),
                                mat,
                            )
                            .await?;
                            if let Some(&first) = keys.first() {
                                maps.meshes.entry(node.id).or_insert(first);
                                // Discrete LOD (static): load + register this
                                // asset's simplified level chain (hidden — drawn
                                // only when a per-frame selection reroutes to it).
                                // No-op unless the `lod` feature is on, the bundle
                                // carries a manifest, AND this node opts in.
                                if node_lod_enabled(&node.kind) {
                                    load_static_lod_chain(
                                        renderer,
                                        assets,
                                        &mesh.0.to_string(),
                                        first,
                                        tk,
                                        mat,
                                    )
                                    .await?;
                                }
                            }
                            maps.node_meshes
                                .entry(node.id)
                                .or_default()
                                .extend(keys.iter().copied());
                            loaded.meshes.extend(keys);
                        }
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
        // the "skinned mesh loads lying on its side" bug. The skin DOES animate:
        // `skin_joints` (built below) binds each bone `NodeId` → the rig glb's joint
        // `TransformKey`, and `resolve_target` drives those joints from our clip
        // tracks (VERIFIED 2026-06-20 — a LoadPlayerBundle round-trip + a driven
        // `update_animations` deforms the Fox). The only remaining nuance is
        // composing a user's *repositioning* of the whole rig (it self-places at the
        // renderer root, so moving the scene node doesn't move it) — separate from
        // the now-working joint animation.
        // Pre-baked cluster ("nanite") mesh — fetch its baked DAG side file and
        // materialize through the bounded cluster pipeline (same path the editor's
        // view-only import uses). A no-op stub when the `lod` feature is off.
        #[cfg(feature = "lod")]
        NodeKind::ClusterMesh { cluster, .. } => {
            let id = cluster.source.0.to_string();
            let path = format!(
                "{ASSETS_DIR}/{}",
                awsm_renderer_lod_bake::cluster_mesh_filename(&id)
            );
            if let Ok(bytes) = assets.fetch(&path).await {
                match serde_json::from_slice::<awsm_renderer_lod_bake::ClusterMesh>(&bytes) {
                    Ok(cm) => {
                        if let Some(key) =
                            materialize_cluster_mesh(renderer, &cm, &id, tk, mat).await?
                        {
                            maps.meshes.entry(node.id).or_insert(key);
                            maps.node_meshes.entry(node.id).or_default().push(key);
                            loaded.meshes.push(key);
                        }
                    }
                    Err(e) => tracing::warn!("cluster mesh `{path}`: unreadable: {e}"),
                }
            } else {
                tracing::warn!("cluster mesh asset `{path}` not found");
            }
        }
        #[cfg(not(feature = "lod"))]
        NodeKind::ClusterMesh { .. } => {}
        NodeKind::SkinnedMesh { skin, .. } => {
            // Multi-mesh, multi-instance rigs: several sibling
            // `SkinnedMesh` nodes share one rig glb (same `skin.source`
            // + joints), and the same rig can be PLACED more than once
            // (two dancers). Load the rig ONCE per placed instance —
            // keyed by (glb leaf, first joint's scene NodeId, which is
            // shared by the siblings but distinct across placements) —
            // parented under THIS node's transform so the authored
            // scene placement applies, then rebind each scene node's
            // material onto its own glb node's primitives.
            let rig_key = (
                mesh_glb_filename(skin.source),
                skin.joints.first().map(|j| j.node).unwrap_or(node.id),
            );
            if !maps.rig_cache.contains_key(&rig_key) {
                let (all_keys, node_index_transforms, node_index_meshes) =
                    load_glb_under(renderer, assets, &rig_key.0, Some(tk), mat).await?;
                // Bind each skeleton bone (scene NodeId) → THIS rig
                // instance's baked joint transform (by the joint's
                // clean-glb node index), so clips / a game host drive
                // the joints the skin reads.
                let mut joints_resolved = 0usize;
                for j in &skin.joints {
                    if let Some(&jtk) = node_index_transforms.get(&(j.index as usize)) {
                        maps.skin_joints.insert(j.node, jtk);
                        joints_resolved += 1;
                    }
                }
                if joints_resolved < skin.joints.len() {
                    let mut available: Vec<usize> = node_index_transforms.keys().copied().collect();
                    available.sort_unstable();
                    tracing::warn!(
                        "skinned mesh `{}`: only {}/{} skin joints resolved — joint indices {:?} vs glb node indices {:?}",
                        node.name,
                        joints_resolved,
                        skin.joints.len(),
                        skin.joints.iter().map(|j| j.index).collect::<Vec<_>>(),
                        available
                    );
                }
                loaded.meshes.extend(all_keys.iter().copied());
                maps.rig_cache
                    .insert(rig_key.clone(), (node_index_transforms, node_index_meshes));
            }
            let (node_index_transforms, node_index_meshes) = maps
                .rig_cache
                .get(&rig_key)
                .cloned()
                .expect("rig cache entry just ensured");

            // This scene node's own primitive within the shared rig.
            // `all_mesh_keys` is keyed by glTF MESH index (a multi-part
            // rig is one glTF mesh with one primitive per part), so
            // flatten in key order and select by `skin.primitive_index`.
            let flat: Vec<MeshKey> = {
                let mut mesh_indices: Vec<usize> = node_index_meshes.keys().copied().collect();
                mesh_indices.sort_unstable();
                mesh_indices
                    .iter()
                    .flat_map(|i| node_index_meshes[i].iter().copied())
                    .collect()
            };
            // `primitive_index: None` (legacy single-part rigs) takes
            // every primitive.
            let keys: Vec<MeshKey> = match skin.primitive_index {
                Some(pi) => flat.get(pi as usize).map(|k| vec![*k]).unwrap_or_default(),
                None => flat.clone(),
            };
            if keys.is_empty() {
                tracing::warn!(
                    "skinned mesh `{}`: primitive {:?} not found (rig produced {} primitives)",
                    node.name,
                    skin.primitive_index,
                    flat.len()
                );
            }
            // The rig loaded with the FIRST sibling's material — rebind
            // this node's own.
            for &k in &keys {
                let _ = renderer.set_mesh_material(k, mat);
            }
            if let Some(&first) = keys.first() {
                maps.meshes.entry(node.id).or_insert(first);
            }
            maps.node_meshes
                .entry(node.id)
                .or_default()
                .extend(keys.iter().copied());
            // Discrete LOD (skinned/morph): unchanged, per node opt-in.
            if let (Some(&base_key), true) = (keys.first(), node_lod_enabled(&node.kind)) {
                load_skinned_lod_chain(
                    renderer,
                    assets,
                    &skin.source.to_string(),
                    base_key,
                    &node_index_transforms,
                    mat,
                )
                .await?;
            }
            *uploaded += 1;
            on_phase(LoadPhase::UploadingMeshes {
                done: *uploaded,
                total,
            });
        }
        NodeKind::Light(cfg) => {
            // Skip a hidden node's light entirely — the renderer has no per-light
            // hide toggle, so not inserting it is the cleanest way to honor
            // `visible == false` (matching lines/decals; documented on
            // `populate_awsm_scene`). So a hidden node no longer emits its light.
            if effective_visible {
                // Same derivation as the editor bridge's `apply_light`: position from
                // the node translation, forward from rotating local -Z. Bind the
                // light to its transform so a moved/rotated light re-derives pos/dir.
                let pos = Vec3::from_array(node.transform.translation);
                let dir =
                    (Quat::from_array(node.transform.rotation) * Vec3::NEG_Z).normalize_or_zero();
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
            // Skip a hidden node's line entirely — the renderer has no per-line
            // hide toggle, so not creating it is the cleanest way to honor
            // `visible == false` (documented on `populate_awsm_scene`).
            if effective_visible {
                materialize_line(renderer, def, node.id, node_world, maps).await?;
            }
        }
        NodeKind::Sprite(def) => {
            materialize_sprite(renderer, cache, assets, def, node.id, tk, maps, loaded).await?;
        }
        NodeKind::Decal(cfg) => {
            // Skip a hidden node's decal entirely (no per-decal hide toggle).
            if effective_visible {
                materialize_decal(renderer, cache, assets, cfg, node.id, node_world, maps).await?;
            }
        }
        NodeKind::InstancesAlongCurve(def) => {
            materialize_instances_along_curve(renderer, scene, def, maps, loaded)?;
        }
        // Explicit instancer: upload the referenced mesh ASSET once (same
        // acquisition as the `Mesh` arm) and instance it with the node's
        // authored transform list — one geometry upload, one instance buffer.
        NodeKind::Instancer(def) => {
            materialize_instancer(
                renderer,
                cache,
                scene,
                assets,
                def,
                node.id,
                tk,
                placeholder,
                maps,
                loaded,
            )
            .await?;
        }
        // A bare `Curve` is data-only: it emits no renderer node. It's consumed
        // by `InstancesAlongCurve` (and sweeps at bake time), which look the curve
        // up directly from `scene` by `NodeId` — no per-node renderer resource.
        NodeKind::Curve(_) => {}
        // A.1 (Design A): the loader builds the emitter's instanced billboard
        // (ready to drive) and hands back an `EmitterHandle`; it does NOT simulate.
        // The game ticks an `awsm_renderer_particles::Simulator` each frame and pushes the
        // result via `drive_emitter` — the same "loads, doesn't drive" contract as
        // animation. Skip a hidden emitter (no per-mesh hide toggle would help once
        // the game drives it; cleanest is to not build it).
        NodeKind::ParticleEmitter(def) => {
            if effective_visible {
                match particles::build_emitter(renderer, def, tk, node_world) {
                    Ok(handle) => {
                        // Track the billboard mesh + its instance transform for
                        // teardown, then record the handle for the NodeHandles
                        // assembly (and the morph/anim maps don't touch emitters).
                        loaded.meshes.push(handle.mesh);
                        loaded.transforms.push(handle.instance_transform);
                        maps.emitters.insert(node.id, handle);
                    }
                    Err(err) => {
                        tracing::warn!("scene-loader: ParticleEmitter build failed: {err}");
                    }
                }
            }
        }
        // `Group` (pure transform parent) and `Collider` (editor-only wireframe;
        // no runtime renderable) need nothing further here. `Mesh` /
        // `SkinnedMesh` / `Light` / `Camera` are handled by the arms above.
        NodeKind::Group | NodeKind::Collider(_) => {}
    }

    // Honor `visible == false` for this node's meshes (sprites included): hide
    // every mesh key just added for it. Lines/decals were already skipped above;
    // lights are intentionally still emitting (documented gap).
    if !effective_visible {
        if let Some(keys) = maps.node_meshes.get(&node.id) {
            for &k in keys {
                let _ = renderer.set_mesh_hidden(k, true);
            }
        }
    }

    for child in &node.children {
        Box::pin(materialize(
            renderer,
            cache,
            scene,
            child,
            Some(tk),
            node_world,
            effective_visible,
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

/// Capture a prefab-root subtree as a reusable [`PrefabTemplate`].
///
/// Walks the subtree in DFS pre-order, materializing each node's meshes **hidden**
/// (so the template never draws) and recording a [`PrefabNode`] per node — its
/// authored local transform, its parent within the subtree, and the hidden
/// template mesh keys. The template meshes are inserted under a single shared
/// scratch [`TransformKey`] (placement doesn't matter — they're hidden and only
/// ever duplicated under fresh instance transforms), reusing the same mesh-build
/// paths the normal `Mesh` / `SkinnedMesh` / `Sprite` arms use via
/// [`build_node_meshes`].
///
/// `parent` is the parent **`NodeId` within the subtree** (`None` for the root).
///
/// **Nested prefab:** a child authored with `prefab == true` is captured as its
/// OWN [`PrefabTemplate`] into `loaded.prefabs` and is NOT inlined here — the
/// recursion stops at it (its descendants belong to the nested template).
///
/// **Non-mesh replay (A.3):** `Light` / `Camera` / `Line` / `Decal` /
/// `ParticleEmitter` / `InstancesAlongCurve` / `Instancer` nodes capture a [`PrefabReplay`]
/// alongside their transform (the decal texture resolved to a pool index here, the
/// curve placement baked here — both while assets / the scene are available), so
/// [`PrefabTemplate::instantiate`] re-creates each as a fresh per-instance resource
/// (instancing is wired in `instantiate`'s second pass; see [`PrefabTemplate`]).
#[allow(clippy::too_many_arguments)]
async fn capture_prefab(
    renderer: &mut AwsmRenderer,
    cache: &mut texture::TextureCache,
    scene: &Scene,
    node: &EditorNode,
    parent: Option<NodeId>,
    assets: &impl SceneAssets,
    // The shared resolve maps — capture binds skinned prefabs' bones into
    // `maps.skin_joints` (so the scene's clips, lowered AFTER capture, resolve to
    // the template's joints) and shares the rig via `maps.rig_cache`.
    maps: &mut AnimResolveMaps,
    placeholder: MaterialKey,
    loaded: &mut LoadedScene,
) -> Result<PrefabTemplate> {
    debug_assert!(
        parent.is_none(),
        "prefab root capture starts at the subtree root"
    );
    // Pure structural plan first (DFS pre-order, parent wiring, nested-prefab
    // boundaries) — unit-tested independently of the GPU mesh build below.
    let layout = prefab_subtree_layout(node);

    // A single hidden scratch transform anchors every template mesh; instances
    // never reuse it (they duplicate the meshes under their own transforms).
    let scratch = renderer.transforms.insert(Transform::default(), None);

    let mut nodes = Vec::with_capacity(layout.len());
    for step in &layout {
        let n = step.node;
        if step.nested_prefab {
            // Nested prefab → captured as its OWN template; not inlined here.
            let tmpl = Box::pin(capture_prefab(
                renderer,
                cache,
                scene,
                n,
                None,
                assets,
                &mut *maps,
                placeholder,
                loaded,
            ))
            .await?;
            loaded.prefabs.insert(n.id, tmpl);
            continue;
        }
        // An `Instancer` node isn't in Phase-1 `collect_renderables` (it carries
        // no material palette), so `node_materials` has no entry for it — build
        // the same flat-default material the live `materialize_instancer` uses
        // instead of falling through to the magenta placeholder.
        let mat = match &n.kind {
            NodeKind::Instancer(_) => {
                instancer_default_material(
                    renderer,
                    cache,
                    assets,
                    placeholder,
                    &maps.custom_shaders,
                )
                .await
            }
            _ => maps
                .node_materials
                .get(&n.id)
                .copied()
                .unwrap_or(placeholder),
        };
        // Build this node's meshes (hidden) under the scratch transform; non-mesh
        // kinds yield an empty vec (their transform is still recorded).
        let template_meshes = build_node_meshes(
            renderer,
            cache,
            scene,
            n,
            scratch,
            mat,
            assets,
            Some(&mut *maps),
            true,
        )
        .await?;
        // A.3: capture the non-mesh renderable to replay per instance. The decal
        // texture is resolved NOW (assets are available here; `instantiate` is
        // asset-free). Light/Camera/Line carry their authored config verbatim.
        let replay = match &n.kind {
            NodeKind::Light(cfg) => PrefabReplay::Light(cfg.clone()),
            NodeKind::Camera(cfg) => PrefabReplay::Camera(cfg.clone()),
            NodeKind::Line(def) => PrefabReplay::Line(def.clone()),
            NodeKind::Decal(cfg) => PrefabReplay::Decal {
                texture_index: resolve_decal_texture_index(renderer, cache, assets, cfg).await,
                alpha: cfg.alpha,
            },
            NodeKind::ParticleEmitter(def) => PrefabReplay::ParticleEmitter(def.clone()),
            // Bake the curve placement now (control points are static) — `instantiate`
            // wires instancing onto the instance's own source-mesh copy in a 2nd pass.
            NodeKind::InstancesAlongCurve(def) => match find_curve(&scene.nodes, def.curve_node) {
                Some(curve) => PrefabReplay::InstancesAlongCurve {
                    transforms: curve_instance_transforms(curve, def),
                    source_node: def.source_node,
                    per_instance_colors: def.per_instance_colors.clone(),
                },
                None => PrefabReplay::None,
            },
            // Explicit instancer: the referenced mesh asset was built as this
            // node's own hidden template mesh above (`build_node_meshes`
            // Instancer arm); capture the authored placements verbatim so
            // `instantiate`'s second pass wires instancing onto the duplicate.
            NodeKind::Instancer(def) => instancer_prefab_replay(def),
            _ => PrefabReplay::None,
        };
        nodes.push(PrefabNode {
            id: n.id,
            local: n.transform,
            parent: step.parent,
            template_meshes,
            replay,
        });
    }

    // Track the template's hidden meshes + scratch transform on `loaded` so
    // `teardown` frees them (the templates never enter `loaded.nodes`/`maps`).
    for pn in &nodes {
        loaded.meshes.extend(pn.template_meshes.iter().copied());
    }
    loaded.transforms.push(scratch);

    Ok(PrefabTemplate {
        root: node.id,
        nodes,
    })
}

/// One node of a [`prefab_subtree_layout`] plan: the node, its parent within the
/// subtree (`None` for the root), and whether it is a **nested** prefab boundary
/// (captured as its own template, descendants excluded from this plan).
struct PrefabLayoutStep<'a> {
    node: &'a EditorNode,
    parent: Option<NodeId>,
    nested_prefab: bool,
}

/// Compute the capture plan for a prefab-root subtree: a DFS pre-order list
/// (parents before children) of every node, its in-subtree parent, and whether it
/// is a nested-prefab boundary.
///
/// The traversal *stops descending* at a nested prefab (a non-root child with
/// `prefab == true`): that child appears in the plan flagged `nested_prefab` (so
/// [`capture_prefab`] captures it as its own template) but its descendants do NOT
/// — they belong to the nested template. Pure (no renderer) so it is unit-tested.
fn prefab_subtree_layout(root: &EditorNode) -> Vec<PrefabLayoutStep<'_>> {
    fn walk<'a>(
        node: &'a EditorNode,
        parent: Option<NodeId>,
        is_root: bool,
        out: &mut Vec<PrefabLayoutStep<'a>>,
    ) {
        // A non-root node flagged prefab is a nested boundary: record it, stop.
        if !is_root && node.prefab {
            out.push(PrefabLayoutStep {
                node,
                parent,
                nested_prefab: true,
            });
            return;
        }
        out.push(PrefabLayoutStep {
            node,
            parent,
            nested_prefab: false,
        });
        for child in &node.children {
            walk(child, Some(node.id), false, out);
        }
    }
    let mut out = Vec::new();
    walk(root, None, true, &mut out);
    out
}

/// Build the renderer meshes for one node under `tk` with material `mat`, the
/// shared mesh-construction path used by both the live `materialize` arms and
/// prefab [`capture_prefab`]. Covers `Mesh` (primitive + bare-geometry glb),
/// `SkinnedMesh` (rig glb), `Sprite`, and `Instancer` (its referenced mesh
/// ASSET, built once — placements are the prefab replay's job); every other
/// [`NodeKind`] yields no mesh
/// (an empty vec). When `hidden` is set, each produced mesh is hidden immediately
/// (the prefab-template case) — the caller's instances un-hide their duplicates.
///
/// Does NOT touch `maps` / `loaded` / progress: it returns the keys so the caller
/// records them where they belong (live arms push into `maps`/`loaded`; the
/// prefab path stores them on the template). Sprites build their own material
/// here (sprites aren't in Phase-1 `collect_renderables`), so `mat` is ignored
/// for the `Sprite` arm.
#[allow(clippy::too_many_arguments)]
async fn build_node_meshes(
    renderer: &mut AwsmRenderer,
    cache: &mut texture::TextureCache,
    scene: &Scene,
    node: &EditorNode,
    tk: TransformKey,
    mat: MaterialKey,
    assets: &impl SceneAssets,
    // `Some` from prefab capture (needs the rig cache + skin-joint binding so a
    // skinned prefab shares ONE skeleton and its clips find their joints); `None`
    // from the standalone public `materialize_node_mesh` path (no anim context).
    maps: Option<&mut AnimResolveMaps>,
    hidden: bool,
) -> Result<Vec<MeshKey>> {
    let mut keys: Vec<MeshKey> = Vec::new();
    match &node.kind {
        NodeKind::Mesh { mesh, .. } => {
            if let Some(entry) = scene.assets.get(mesh.0) {
                match &entry.source {
                    AssetSource::Mesh(RuntimeMesh::Primitive(shape)) => {
                        let md = awsm_renderer_meshgen::primitive_mesh(shape);
                        let key = renderer.add_raw_mesh(mesh_data_to_raw(md), tk, mat)?;
                        keys.push(key);
                    }
                    AssetSource::Mesh(RuntimeMesh::Glb) => {
                        let (glb_keys, _, _) = load_glb_under(
                            renderer,
                            assets,
                            &mesh_glb_filename(mesh.0),
                            Some(tk),
                            mat,
                        )
                        .await?;
                        keys.extend(glb_keys);
                    }
                    _ => {}
                }
            }
        }
        NodeKind::SkinnedMesh { skin, .. } => {
            // Mirror the live `materialize` SkinnedMesh arm so a skinned PREFAB
            // shares ONE skeleton across its sibling part-nodes and binds its
            // bones so the authored clips find their joints. Several sibling
            // `SkinnedMesh` nodes share one rig glb (same `skin.source` + joints):
            // load it ONCE (keyed in `rig_cache` by glb leaf + first joint node),
            // bind each bone `NodeId → baked joint TransformKey`, and select each
            // node's OWN primitive via `skin.primitive_index`. Loading per-node
            // (the old path) minted a duplicate skeleton per part AND bound no
            // joints → the clips had no targets and the rig sat at bind pose.
            if let Some(maps) = maps {
                let rig_key = (
                    mesh_glb_filename(skin.source),
                    skin.joints.first().map(|j| j.node).unwrap_or(node.id),
                );
                if !maps.rig_cache.contains_key(&rig_key) {
                    let (all_keys, node_index_transforms, node_index_meshes) =
                        load_glb_under(renderer, assets, &rig_key.0, Some(tk), mat).await?;
                    // Bind each skeleton bone (scene NodeId) → this rig's baked
                    // joint transform (by the joint's clean-glb node index), so a
                    // bone's Transform track resolves to the joint the skin reads.
                    let mut joints_resolved = 0usize;
                    for j in &skin.joints {
                        if let Some(&jtk) = node_index_transforms.get(&(j.index as usize)) {
                            maps.skin_joints.insert(j.node, jtk);
                            joints_resolved += 1;
                        }
                    }
                    if joints_resolved < skin.joints.len() {
                        tracing::warn!(
                            "skinned prefab `{}`: only {}/{} skin joints resolved",
                            node.name,
                            joints_resolved,
                            skin.joints.len(),
                        );
                    }
                    // Hidden template meshes: `all_keys` are set hidden by the
                    // caller's post-loop pass (they land in the returned `keys`).
                    let _ = all_keys;
                    maps.rig_cache
                        .insert(rig_key.clone(), (node_index_transforms, node_index_meshes));
                }
                let (_node_index_transforms, node_index_meshes) = maps
                    .rig_cache
                    .get(&rig_key)
                    .cloned()
                    .expect("rig cache entry just ensured");
                // `node_index_meshes` is keyed by glTF MESH index (a multi-part
                // rig is one glTF mesh, one primitive per part): flatten in key
                // order and select this node's primitive.
                let flat: Vec<MeshKey> = {
                    let mut mesh_indices: Vec<usize> = node_index_meshes.keys().copied().collect();
                    mesh_indices.sort_unstable();
                    mesh_indices
                        .iter()
                        .flat_map(|i| node_index_meshes[i].iter().copied())
                        .collect()
                };
                let selected: Vec<MeshKey> = match skin.primitive_index {
                    Some(pi) => flat.get(pi as usize).map(|k| vec![*k]).unwrap_or_default(),
                    None => flat.clone(),
                };
                if selected.is_empty() {
                    tracing::warn!(
                        "skinned prefab `{}`: primitive {:?} not found (rig has {} primitives)",
                        node.name,
                        skin.primitive_index,
                        flat.len()
                    );
                }
                // The rig loaded with the FIRST sibling's material — rebind this
                // node's own onto its selected primitive.
                for &k in &selected {
                    let _ = renderer.set_mesh_material(k, mat);
                }
                keys.extend(selected);
            } else {
                // No anim context (public `materialize_node_mesh`): naive load.
                let (glb_keys, _, _) =
                    load_glb_under(renderer, assets, &mesh_glb_filename(skin.source), None, mat)
                        .await?;
                keys.extend(glb_keys);
            }
        }
        NodeKind::Sprite(def) => {
            let key = build_sprite_mesh(renderer, cache, assets, def, tk).await?;
            keys.push(key);
        }
        // Explicit instancer: build the referenced mesh ASSET under `tk`,
        // exactly like the `Mesh` arm — the same geometry acquisition as the
        // live [`materialize_instancer`] (a procedural primitive regenerates
        // from params; a baked asset loads its single-mesh glb). The authored
        // instance placements are NOT applied here — the prefab path captures
        // them as a [`PrefabReplay::Instancer`] and wires instancing onto the
        // duplicate of THIS mesh in `instantiate`'s second pass. A nil mesh ref
        // builds nothing (a valid, not-yet-wired authored state; `assets.get`
        // on the nil id misses).
        NodeKind::Instancer(def) => {
            if let Some(entry) = scene.assets.get(def.mesh.0) {
                match &entry.source {
                    AssetSource::Mesh(RuntimeMesh::Primitive(shape)) => {
                        let md = awsm_renderer_meshgen::primitive_mesh(shape);
                        let key = renderer.add_raw_mesh(mesh_data_to_raw(md), tk, mat)?;
                        keys.push(key);
                    }
                    AssetSource::Mesh(RuntimeMesh::Glb) => {
                        let (glb_keys, _, _) = load_glb_under(
                            renderer,
                            assets,
                            &mesh_glb_filename(def.mesh.0),
                            Some(tk),
                            mat,
                        )
                        .await?;
                        keys.extend(glb_keys);
                    }
                    _ => {}
                }
            }
        }
        // Non-mesh kinds: no geometry to share. Their transform is still recorded
        // by the caller; `Light`/`Camera`/`Line`/`Decal` replay per instance is
        // captured separately as a `PrefabReplay` (A.3, see `capture_prefab`).
        _ => {}
    }
    if hidden {
        for &k in &keys {
            renderer.set_mesh_hidden(k, true)?;
        }
    }
    Ok(keys)
}

/// Materialize a [`NodeKind::Line`] into a renderer fat-line strip.
///
/// The screen-space fat-line API ([`AwsmRenderer::add_line_strip`]) takes
/// world-space points with no transform of its own, so we bake the node's world
/// transform (`node_world`, accumulated through the materialize recursion) into
/// each authored [`LinePoint::pos`](awsm_renderer_scene::LinePoint) before handing them
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
/// Geometry is `awsm_renderer_meshgen::sprite_quad` (a unit XY quad facing +Z) scaled by
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
#[allow(clippy::too_many_arguments)]
async fn materialize_sprite(
    renderer: &mut AwsmRenderer,
    cache: &mut texture::TextureCache,
    assets: &impl SceneAssets,
    def: &SpriteDef,
    node_id: NodeId,
    tk: TransformKey,
    maps: &mut AnimResolveMaps,
    loaded: &mut LoadedScene,
) -> Result<()> {
    let key = build_sprite_mesh(renderer, cache, assets, def, tk).await?;
    maps.meshes.entry(node_id).or_insert(key);
    maps.node_meshes.entry(node_id).or_default().push(key);
    loaded.meshes.push(key);
    Ok(())
}

/// Build a sprite's Unlit/FlipBook material + textured quad under `tk`, returning
/// the mesh key. Shared by the live `Sprite` arm ([`materialize_sprite`]) and
/// prefab capture ([`build_node_meshes`]); the caller records the key. Sprites
/// build their own material here (they aren't in Phase-1 `collect_renderables`).
async fn build_sprite_mesh(
    renderer: &mut AwsmRenderer,
    cache: &mut texture::TextureCache,
    assets: &impl SceneAssets,
    def: &SpriteDef,
    tk: TransformKey,
) -> Result<MeshKey> {
    use awsm_renderer::materials::flipbook::{FlipBookMaterial, FlipBookMode};
    use awsm_renderer::materials::unlit::UnlitMaterial;
    use awsm_renderer::meshes::mesh::BillboardMode as RBillboard;
    use awsm_renderer_core::texture::mipmap::MipmapTextureKind;
    use awsm_renderer_scene::{BillboardMode, FlipBookModeDef, SpriteAlphaMode};

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
        Some(t) => {
            texture::load_texture(renderer, cache, assets, t, true, MipmapTextureKind::Albedo).await
        }
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

    let md = awsm_renderer_meshgen::sprite_quad(def.size[0], def.size[1]);
    let key = renderer.add_raw_mesh(mesh_data_to_raw(md), tk, mat)?;

    let rbillboard = match def.billboard {
        BillboardMode::None => RBillboard::None,
        BillboardMode::YAxis => RBillboard::YAxis,
        BillboardMode::Full => RBillboard::Full,
    };
    if !matches!(rbillboard, RBillboard::None) {
        renderer.set_mesh_billboard_mode(key, rbillboard)?;
    }

    Ok(key)
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
/// index (`array_index * stride + layer_index`, where `stride` is the device
/// `max_texture_array_layers` — the decal shader unpacks with the same value, A.4;
/// see [`resolve_decal_texture_index`]). When `cfg.texture` resolves to a pooled
/// texture we derive that index from `renderer.textures.get_entry`; otherwise we
/// fall back to index `0` (the editor's own decal bridge always passes `0` — it
/// does not wire decal textures at all — so an untextured decal here matches the
/// editor exactly). When the renderer's `decals` feature is off, `insert_decal`
/// returns [`AwsmDecalError::FeatureNotEnabled`]; we warn once and skip.
async fn materialize_decal(
    renderer: &mut AwsmRenderer,
    cache: &mut texture::TextureCache,
    assets: &impl SceneAssets,
    cfg: &DecalConfig,
    node_id: NodeId,
    node_world: Mat4,
    maps: &mut AnimResolveMaps,
) -> Result<()> {
    use awsm_renderer::decals::AwsmDecalError;

    let texture_index = resolve_decal_texture_index(renderer, cache, assets, cfg).await;

    match renderer.insert_decal(node_world, texture_index, cfg.alpha) {
        Ok(key) => {
            maps.decals.insert(node_id, key);
        }
        Err(AwsmDecalError::FeatureNotEnabled) => warn_decal_feature_off(),
        Err(err) => tracing::warn!("scene-loader: insert_decal failed: {err:?}"),
    }
    Ok(())
}

/// Resolve a decal's texture to the flat texture-pool index the decal shader
/// samples (`array_index * stride + layer_index`, `stride` =
/// [`decal_texture_index_stride`](awsm_renderer::decals::decal_texture_index_stride),
/// A.4). `None` (no texture, failed load, or not pooled) → index `0`, matching the
/// editor bridge. Shared by the live
/// [`materialize_decal`] arm and prefab capture ([`capture_prefab`], which must
/// resolve at load time because [`PrefabTemplate::instantiate`] has no assets).
async fn resolve_decal_texture_index(
    renderer: &mut AwsmRenderer,
    cache: &mut texture::TextureCache,
    assets: &impl SceneAssets,
    cfg: &DecalConfig,
) -> u32 {
    // A.4: pack with the SAME stride the decal shader unpacks with — the device
    // `max_texture_array_layers`, via the renderer's single-source-of-truth helper
    // (no longer a hard-coded `64`, which mis-sampled once a pool array exceeded 64
    // layers). Read before the `&mut` texture load (Copy → borrow ends).
    let stride = awsm_renderer::decals::decal_texture_index_stride(&renderer.gpu);
    match &cfg.texture {
        Some(t) => {
            match texture::load_texture(renderer, cache, assets, t, true, MipmapTextureKind::Albedo)
                .await
            {
                Some(mt) => renderer
                    .textures
                    .get_entry(mt.key)
                    .map(|e| (e.array_index as u32) * stride + e.layer_index as u32)
                    .unwrap_or(0),
                None => 0,
            }
        }
        None => 0,
    }
}

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
/// Per-instance **colours** (`per_instance_colors`) are applied via
/// [`AwsmRenderer::set_mesh_instance_attrs`] (A.2) — expanded to the placed count,
/// repeating the last authored value when the list is shorter (the def's
/// documented semantics).
///
/// Limitations (documented best-effort): the source node's *local* transform is
/// not re-composed into each instance (the curve frame fully defines placement);
/// the per-instance `shadow` config is not applied — shadow cast/receive is a
/// **mesh-level** flag (shared by every instance, since instancing reuses the
/// source mesh), so honoring the curve's `shadow` would overwrite the *source
/// node's own* authored shadow flags; left as a documented follow-on (needs a true
/// per-instance shadow flag in the renderer).
fn materialize_instances_along_curve(
    renderer: &mut AwsmRenderer,
    scene: &Scene,
    def: &InstancesAlongCurveDef,
    maps: &mut AnimResolveMaps,
    loaded: &mut LoadedScene,
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
    // The transform key instancing is keyed under — also the per-instance attribute
    // key. Grab it before the mutable instancing call (Copy, so the borrow ends).
    let transform_key = renderer.meshes.get(source_mesh)?.transform_key;
    if let Err(err) = renderer.enable_mesh_instancing_opaque(source_mesh, &transforms) {
        tracing::warn!("scene-loader: enable_mesh_instancing_opaque failed: {err}");
        return Ok(());
    }
    // Track the minted instance rows so `LoadedScene::teardown` frees them.
    loaded.instanced_transforms.push(transform_key);
    // A.2: apply per-instance colour overrides via the same per-instance attribute
    // path the particle emitter uses. `set_mesh_instance_attrs` requires exactly one
    // attr per placed transform, so expand `per_instance_colors` to the placed count,
    // repeating the last value when the authored list is shorter (the def's
    // documented semantics). Empty list → leave the default white tint untouched.
    if !def.per_instance_colors.is_empty() {
        let attrs: Vec<awsm_renderer::instances::InstanceAttr> =
            expand_instance_colors(&def.per_instance_colors, transforms.len())
                .into_iter()
                .map(|c| awsm_renderer::instances::InstanceAttr::from_rgba_alpha_size(c, 1.0, 1.0))
                .collect();
        if let Err(err) = renderer.set_mesh_instance_attrs(transform_key, &attrs) {
            tracing::warn!("scene-loader: curve per-instance colours failed: {err}");
        }
    }
    Ok(())
}

/// Materialize a [`NodeKind::Instancer`]: upload the referenced mesh **asset**
/// once and draw it with the node's authored per-instance transforms via GPU
/// instancing — ONE geometry upload + one instance buffer
/// ([`AwsmRenderer::enable_mesh_instancing_opaque`]).
///
/// Geometry acquisition mirrors the `Mesh` arm exactly (a procedural primitive
/// regenerates from params; a baked asset loads `assets/<id>.glb` — prefetched
/// by [`collect_prefetch_paths`]). Unlike [`materialize_instances_along_curve`]
/// (which reuses a *source node's* already-materialized mesh), the instancer is
/// self-contained: it references the asset directly, so it needs no other node
/// in the scene. Instances render with a default material (matching the
/// editor's bridge, which uploads the instancer flat-default); per-instance
/// **colors** apply via [`AwsmRenderer::set_mesh_instance_attrs`], expanded to
/// the instance count with the last value repeated (the def's documented
/// semantics). A nil mesh ref or an empty transform list renders nothing (a
/// valid, not-yet-wired authored state).
#[allow(clippy::too_many_arguments)]
async fn materialize_instancer(
    renderer: &mut AwsmRenderer,
    cache: &mut texture::TextureCache,
    scene: &Scene,
    assets: &impl SceneAssets,
    def: &InstancerDef,
    node_id: NodeId,
    tk: TransformKey,
    placeholder: MaterialKey,
    maps: &mut AnimResolveMaps,
    loaded: &mut LoadedScene,
) -> Result<()> {
    if def.mesh.0.is_nil() || def.transforms.is_empty() {
        return Ok(());
    }
    let Some(entry) = scene.assets.get(def.mesh.0) else {
        tracing::warn!(
            "scene-loader: Instancer references missing mesh asset {} — skipped",
            def.mesh.0
        );
        return Ok(());
    };
    let mat =
        instancer_default_material(renderer, cache, assets, placeholder, &maps.custom_shaders)
            .await;

    // Same geometry acquisition as the `Mesh` arm (minus the LOD/cluster
    // chains — the instanced draw is one mesh by design).
    let mesh_key = match &entry.source {
        AssetSource::Mesh(RuntimeMesh::Primitive(shape)) => {
            let md = awsm_renderer_meshgen::primitive_mesh(shape);
            Some(renderer.add_raw_mesh(mesh_data_to_raw(md), tk, mat)?)
        }
        AssetSource::Mesh(RuntimeMesh::Glb) => {
            let (keys, _, _) = load_glb_under(
                renderer,
                assets,
                &mesh_glb_filename(def.mesh.0),
                Some(tk),
                mat,
            )
            .await?;
            // A baked mesh asset is a single-mesh glb (one asset = one glb,
            // written by the bundle bake); instance its first (only) key.
            keys.first().copied()
        }
        // An Instancer always references an AssetSource::Mesh; other source
        // kinds can't be a mesh asset — ignore defensively (mirrors `Mesh`).
        _ => None,
    };
    let Some(key) = mesh_key else {
        return Ok(());
    };
    maps.meshes.entry(node_id).or_insert(key);
    maps.node_meshes.entry(node_id).or_default().push(key);
    loaded.meshes.push(key);

    let transforms: Vec<Transform> = def.transforms.iter().map(trs_to_transform).collect();
    // The per-instance attribute key (also read by `set_mesh_instance_attrs`).
    let transform_key = renderer.meshes.get(key)?.transform_key;
    if let Err(err) = renderer.enable_mesh_instancing_opaque(key, &transforms) {
        tracing::warn!("scene-loader: instancer enable_mesh_instancing_opaque failed: {err}");
        return Ok(());
    }
    // Track the minted instance rows so `LoadedScene::teardown` frees them.
    loaded.instanced_transforms.push(transform_key);
    if !def.per_instance_colors.is_empty() {
        let attrs: Vec<awsm_renderer::instances::InstanceAttr> =
            expand_instance_colors(&def.per_instance_colors, transforms.len())
                .into_iter()
                .map(|c| awsm_renderer::instances::InstanceAttr::from_rgba_alpha_size(c, 1.0, 1.0))
                .collect();
        if let Err(err) = renderer.set_mesh_instance_attrs(transform_key, &attrs) {
            tracing::warn!("scene-loader: instancer per-instance colours failed: {err}");
        }
    }
    Ok(())
}

/// Capture an explicit `Instancer` node's [`PrefabReplay`]: the authored
/// placements + colours verbatim (its mesh asset becomes the node's own hidden
/// template mesh via `build_node_meshes`, so `instantiate` needs no assets).
/// A nil mesh ref or an empty transform list is a valid "not wired up yet"
/// authored state — nothing to replay ([`PrefabReplay::None`]), matching
/// [`materialize_instancer`]'s live guard. Pure, so it is unit-tested natively
/// (see `prefab_tests`).
fn instancer_prefab_replay(def: &InstancerDef) -> PrefabReplay {
    if def.mesh.0.is_nil() || def.transforms.is_empty() {
        return PrefabReplay::None;
    }
    PrefabReplay::Instancer {
        transforms: def.transforms.iter().map(trs_to_transform).collect(),
        per_instance_colors: def.per_instance_colors.clone(),
    }
}

/// The flat-default material an explicit `Instancer` renders with (the editor's
/// instancer bridge renders flat-default too; the kind carries no material
/// palette). Built through the same resolve path as node materials so its
/// pipeline compiles in the Phase-4 batch. Shared by [`materialize_instancer`]
/// (the live load) and [`capture_prefab`] (prefab templates) so an instancer
/// inside a prefab renders identically to a top-level one.
async fn instancer_default_material(
    renderer: &mut AwsmRenderer,
    cache: &mut texture::TextureCache,
    assets: &impl SceneAssets,
    placeholder: MaterialKey,
    custom_shaders: &HashMap<AssetId, awsm_renderer_materials::MaterialShaderId>,
) -> MaterialKey {
    let default_inst = MaterialInstance {
        asset: AssetId::new(),
        inline: Default::default(),
        uniform_overrides: Default::default(),
        texture_overrides: Default::default(),
        buffer_overrides: Default::default(),
    };
    resolve_material(
        renderer,
        cache,
        Some(&default_inst),
        placeholder,
        assets,
        custom_shaders,
    )
    .await
}

/// Expand authored `per_instance_colors` to exactly `count` entries, repeating the
/// last value when the list is shorter (the [`InstancesAlongCurveDef`] documented
/// semantics) and truncating when longer. Caller guarantees `colors` is non-empty.
fn expand_instance_colors(colors: &[[f32; 4]], count: usize) -> Vec<[f32; 4]> {
    let last = *colors.last().expect("non-empty per_instance_colors");
    (0..count)
        .map(|i| colors.get(i).copied().unwrap_or(last))
        .collect()
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
    use awsm_renderer_curves::{Curve3, FrameSequence};

    let points: Vec<Vec3> = curve
        .control_points
        .iter()
        .map(|p| Vec3::from_array(*p))
        .collect();
    if points.len() < 2 {
        return Vec::new();
    }
    let mut crom = awsm_renderer_curves::CatmullRomCurve::new(points, curve.closed);
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
///
/// Returns `(mesh keys, glb-node-index → baked-transform key)` — the latter lets a
/// skinned-mesh consumer bind each skeleton joint (by its clean-glb node index) to
/// drive the skin. Public (R4) so a host can load an individual bundle glb with
/// Cluster LOD (Phase B), **finest-cut** render: when `virtual_geometry` is on
/// and the asset has a baked cluster DAG (`<id>.clusters.bin`), render its finest
/// cut — the level-0 clusters, which reconstruct the source geometry exactly —
/// and return its key so the caller renders it instead of the base glb. Returns
/// `None` (→ caller loads the base) when vg is off or there's no cluster data.
///
/// This validates the bake→emit→load→render path end-to-end. The GPU per-cluster
/// LOD cut + compaction (B.2 proper) will replace the static finest cut with a
/// per-frame on-device selection over the same pages.
/// Cluster-streaming residency budget (Phase 5): cap the cluster render mesh `M`
/// to this many triangles when `cluster_streaming` is on, so a multi-million-tri
/// asset loads instead of overflowing the GPU pool (the exploded vertex buffer is
/// `M`'s dominant cost at 56 B / index). Tunable.
#[cfg(feature = "lod")]
const CLUSTER_STREAMING_BUDGET_TRIS: usize = 1_000_000;

/// Select the resident cluster set for capped streaming (Phase 5).
///
/// Keeps the **coarsest** clusters (largest `lod_error`) up to `budget_tris`,
/// then clamps each resident **leaf** (a resident cluster with no resident DAG
/// child) `lod_error` to 0 so the per-cluster cut still covers all budgets below
/// the residency frontier — i.e. close-up stays watertight at the capped detail —
/// and remaps each resident cluster's `first_index` into the compacted output
/// `m_indices`. Returns `(gpu_pages, m_indices)` ready to upload. When the mesh
/// already fits the budget (the common case — `budget_tris == usize::MAX` with the
/// flag off, or a small mesh), returns every cluster with `cm.indices` verbatim,
/// so the result is byte-identical to the non-streaming path.
///
/// Over budget, the resident set is the finest **complete-antichain** cut that
/// fits (a uniform-error-threshold cut over the DAG, which the bake guarantees is
/// crack-free) — a SOFT budget. This is watertight at every camera distance, unlike
/// the older hard-tri cap that left a partial frontier and seamed. Per-frame paging
/// (Gap B) later restores *within-mesh* camera-driven detail on top of this.
#[cfg(feature = "lod")]
fn select_resident_clusters(
    cm: &awsm_renderer_lod_bake::ClusterMesh,
    budget_tris: usize,
) -> (
    Vec<awsm_renderer::cluster_lod::ClusterPage>,
    Vec<u32>,
    Vec<usize>,
) {
    let to_page = |c: &awsm_renderer_lod_bake::ClusterPage, first_index: u32, lod_error: f32| {
        awsm_renderer::cluster_lod::ClusterPage {
            center: c.center,
            radius: c.radius,
            lod_error,
            parent_error: c.parent_error,
            lod_bounds_center: c.lod_bounds_center,
            lod_bounds_radius: c.lod_bounds_radius,
            parent_bounds_center: c.parent_bounds_center,
            parent_bounds_radius: c.parent_bounds_radius,
            first_index,
            index_count: c.index_count,
        }
    };
    if cm.indices.len() / 3 <= budget_tris {
        let pages = cm
            .clusters
            .iter()
            .map(|c| to_page(c, c.first_index, c.lod_error))
            .collect();
        let ids = (0..cm.clusters.len()).collect();
        return (pages, cm.indices.clone(), ids);
    }
    // Pick a COMPLETE-ANTICHAIN frontier, not a hard-tri partial cut. The runtime
    // per-cluster cut at a uniform error threshold `T` selects exactly
    // `{c : c.lod_error <= T < c.parent_error}` — a crack-free antichain, because
    // the bake guarantees each root→leaf path's `[lod_error, parent_error)`
    // intervals tile `[0, ∞)` (one cluster per path contains `T`). A *partial*
    // frontier (the old hard-tri cap, which cut off mid-level) borders coarser-only
    // regions and tears; a whole antichain never does. So: evaluate the cut at each
    // candidate threshold and keep the FINEST (smallest `T` ⇒ most detail) whose
    // triangle count fits the budget — a SOFT budget (we may undershoot to stay
    // whole). Candidate thresholds are the clusters' own `lod_error` breakpoints
    // (the level boundaries); ascending `T` ⇒ monotonically coarser/cheaper cuts.
    let mut thresholds: Vec<f32> = cm.clusters.iter().map(|c| c.lod_error).collect();
    thresholds.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    thresholds.dedup();

    let budget_idx = budget_tris * 3;
    let cut_at = |t: f32| -> (usize, Vec<usize>) {
        let mut idx = 0usize;
        let mut sel = Vec::new();
        for (i, c) in cm.clusters.iter().enumerate() {
            if c.lod_error <= t && t < c.parent_error {
                idx += c.index_count as usize;
                sel.push(i);
            }
        }
        (idx, sel)
    };

    // Finest threshold whose whole cut fits; fall back to the coarsest cut (the
    // roots — always tiny) if even that overflows, so we never return empty.
    let mut chosen: Option<Vec<usize>> = None;
    for &t in &thresholds {
        let (idx, sel) = cut_at(t);
        if idx <= budget_idx && !sel.is_empty() {
            chosen = Some(sel);
            break;
        }
    }
    let sel = chosen.unwrap_or_else(|| cut_at(*thresholds.last().unwrap()).1);

    // Emit the frontier as an always-drawn cut: `lod_error = 0` and
    // `parent_error = MAX` so the runtime selects every resident cluster at any
    // camera distance (the resident set is the *only* representation — never
    // refined past it, never coarsened below it). The antichain tiles the surface,
    // so the draw is watertight at all distances and bounded by the budget.
    let mut m_indices: Vec<u32> = Vec::new();
    let mut pages = Vec::new();
    for &i in &sel {
        let c = &cm.clusters[i];
        let first_index = m_indices.len() as u32;
        let s = c.first_index as usize;
        m_indices.extend_from_slice(&cm.indices[s..s + c.index_count as usize]);
        let mut page = to_page(c, first_index, 0.0);
        page.parent_error = f32::MAX;
        pages.push(page);
    }
    // `sel` is the chosen antichain's cm-cluster ids, in the SAME order as `pages`
    // (⇒ as the page-pool slots), so the caller can seed `slot_cluster`.
    (pages, m_indices, sel)
}

/// Exploded vertices per page slot: a bake cluster is ≤128 triangles, and the
/// raster geometry is exploded (3 unique verts / triangle), so a slot must hold
/// ≤ 384 verts. Fixed so every slot is interchangeable (the basis of paging).
#[cfg(feature = "lod")]
const CLUSTER_PAGE_VERTS: usize = 384;

/// Default page-pool capacity (slots) for dynamic paging (Gap B). 8192 slots ×
/// 384 verts × 56 B ≈ 168 MB — the VRAM-budget knob. A host can size it; this is
/// the default ceiling the working set evicts against.
// Used by the fixed-pool + LRU eviction path (Gap B step 3/4); step 2 sizes the
// pool to the (bounded) resident count.
#[allow(dead_code)]
#[cfg(feature = "lod")]
const CLUSTER_PAGE_POOL_SLOTS: usize = 8192;

/// Default residency budget (triangles) when `cluster_paging` is on (Gap B step 2).
/// The page pool pads each resident cluster to a fixed `CLUSTER_PAGE_VERTS` slot, so
/// the resident set must stay small enough that `slots * page_verts * vertex_stride`
/// fits well under the GPU buffer cap (~512 MB). 30k tris → a few-hundred to ~2k
/// resident clusters ⇒ a slot buffer comfortably in budget. `?streambudget=N`
/// overrides. (Camera-driven streaming later refines detail within this pool.)
#[cfg(feature = "lod")]
const CLUSTER_PAGING_BUDGET_TRIS: usize = 30_000;

/// How many cluster meshes may be resident at their FULL per-mesh budget before the
/// global residency cap starts throttling later ones. The per-mesh budgets above
/// each bound ONE mesh's GPU pool; this bounds the SUM across all resident cluster
/// meshes (`per_mesh_budget * this`) so total VRAM stays bounded no matter how many
/// nanite meshes a scene loads. Few-mesh scenes are unaffected (each gets its full
/// budget until the sum reaches the cap); the uncapped path (budget == `MAX`, i.e.
/// streaming + paging both off) stays uncapped, so the shipped path is unchanged.
#[cfg(feature = "lod")]
const GLOBAL_RESIDENCY_MESH_MULTIPLE: usize = 8;

/// A static page-pool residency plan: which slot each cluster occupies (`-1` =
/// not resident / overflowed) plus occupancy stats. This is the CPU side of the
/// Gap-B page pool — the fixed-slot indirection that lets clusters stream in/out
/// independently. Step 1 builds + validates it; the GPU upload + cut-shader read
/// of `resident` + dynamic swap land in the following steps.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(feature = "lod")]
struct PagePoolPlan {
    /// `resident[cluster_id]` = slot index, or `-1` if not resident.
    resident: Vec<i32>,
    /// Slots actually occupied (= residents that fit).
    slots_used: usize,
    /// Resident clusters that didn't fit `pool_slots` (working set too big for
    /// the VRAM budget — these fall back to a coarser resident ancestor at runtime).
    overflow: usize,
}

/// Assign each resident cluster a page-pool slot, in order, up to `pool_slots`.
/// Pure + deterministic (the GPU-free core of paging, unit-tested without a device).
#[cfg(feature = "lod")]
fn plan_page_pool(
    cluster_count: usize,
    resident_cluster_ids: &[usize],
    pool_slots: usize,
) -> PagePoolPlan {
    let mut resident = vec![-1i32; cluster_count];
    let mut slots_used = 0usize;
    let mut overflow = 0usize;
    for &c in resident_cluster_ids {
        if c >= cluster_count {
            continue; // defensive: ignore out-of-range ids
        }
        if slots_used < pool_slots {
            resident[c] = slots_used as i32;
            slots_used += 1;
        } else {
            overflow += 1;
        }
    }
    PagePoolPlan {
        resident,
        slots_used,
        overflow,
    }
}

/// Pack each resident cluster's geometry into a fixed `page_verts`-sized page-pool
/// slot (Gap B step 2). Returns `(slot_indices, source_indices)`:
/// - `slot_indices`: the vertex-attribute index buffer for the cluster render mesh
///   `M`, padded so slot `s` occupies `[s*page_verts, s*page_verts+page_verts)`;
///   a cluster's `index_count` indices sit at the slot start, the remainder repeats
///   the first (a degenerate vert that is never indexed by the draw). The renderer
///   explodes `M` in this order, so slot `s`'s exploded verts are self-contained
///   and swappable independently — the basis of paging.
/// - `source_indices`: per-cluster-contiguous (page order, so each page's existing
///   `first_index` still addresses it), with **slot-relative values**
///   `s*page_verts + k` that the compaction copies into the draw stream.
///
/// Under full residency (`resident[c] = c`'s slot, no `-1`) the drawn triangle set
/// is unchanged — only the vertex buffer is slot-laid-out + padded — so the render
/// is identical; eviction later overwrites a slot without touching its neighbours.
/// `page_spans[c] = (first_index, index_count)` of cluster `c` in `m_indices`
/// (each `ClusterPage`'s span). Decoupled from `ClusterPage` so it's testable
/// without constructing GPU page structs.
#[cfg(feature = "lod")]
fn build_slot_geometry(
    page_spans: &[(u32, u32)],
    m_indices: &[u32],
    resident: &[i32],
    page_verts: usize,
) -> (Vec<u32>, Vec<u32>) {
    // Allocate `num_slots` slots (slot ids are 0..num_slots-1 for the contiguous
    // identity residency the loader builds). BOTH buffers are SLOT-ALIGNED: slot `s`
    // owns `[s*page_verts, (s+1)*page_verts)` in each. That lets the per-frame
    // streamer rewrite ONE slot's geometry + draw-indices independently (Gap B
    // 20b-iv). A page's `first_index` is therefore `slot*page_verts` (set by the
    // caller), and the compaction reads its first `index_count` entries.
    let num_slots = resident.iter().filter(|&&s| s >= 0).count();
    let mut slot_indices = vec![0u32; num_slots * page_verts];
    let mut source_indices = vec![0u32; num_slots * page_verts];
    for (c, &(first_index, index_count)) in page_spans.iter().enumerate() {
        let slot = resident[c];
        if slot < 0 {
            continue;
        }
        let base = slot as usize * page_verts;
        let f = first_index as usize;
        let ic = (index_count as usize).min(page_verts);
        let pad = if ic > 0 { m_indices[f] } else { 0 };
        for k in 0..page_verts {
            slot_indices[base + k] = if k < ic { m_indices[f + k] } else { pad };
            // Slot-relative draw index for k<ic; the slot's remaining region is
            // padding the page (index_count=ic) never reads — point it at the slot base.
            source_indices[base + k] = (base + if k < ic { k } else { 0 }) as u32;
        }
    }
    (slot_indices, source_indices)
}

/// DAG group key: exact f32 bits of a (sphere center+radius, error) — used to match
/// a cluster's `parent_*` to another cluster's `lod_*` (the bake assigns the same
/// group sphere/error to both sides, so exact-bits compares).
#[cfg(feature = "lod")]
type DagGroupKey = [u32; 5];

/// For each cluster `F` (by index), the finer clusters whose group produced it —
/// `F`'s refinement set for dynamic paging (Gap B step 3). The DAG is GROUP-based:
/// clusters created by one group share `F`'s lod key and flip together (crack-free),
/// so a finer cluster `c` is a child of `F` iff `c`'s PARENT key == `F`'s LOD key.
/// Refining `F` streams its whole finer group in. Keyed (decoupled from `ClusterPage`)
/// so it's unit-testable without GPU structs. Build on the ORIGINAL bake clusters
/// (the resident pages' lod/parent errors are clamped to 0/MAX).
#[allow(dead_code)] // wired into the step-3 stream/refine path
#[cfg(feature = "lod")]
fn cluster_finer_group(lod_keys: &[DagGroupKey], parent_keys: &[DagGroupKey]) -> Vec<Vec<usize>> {
    use std::collections::HashMap;
    let mut by_lod: HashMap<DagGroupKey, Vec<usize>> = HashMap::new();
    for (i, k) in lod_keys.iter().enumerate() {
        by_lod.entry(*k).or_default().push(i);
    }
    let mut children = vec![Vec::new(); lod_keys.len()];
    for (c, pk) in parent_keys.iter().enumerate() {
        if let Some(parents) = by_lod.get(pk) {
            for &f in parents {
                children[f].push(c);
            }
        }
    }
    children
}

/// CPU-driven streaming step (Gap B step 3, CPU-driven design): decide which
/// clusters to page into which slots this frame. The CPU has the camera + DAG, so
/// it runs the cut itself (cheap at our scale — ≤~80k clusters for a 5–10M-tri
/// asset) and diffs the `desired` resident set against the current residency,
/// rather than a GPU feedback/readback loop (which only pays off at 100s-of-millions
/// of clusters). Reuses free slots first, then evicts the COLDEST non-desired slots
/// (LRU), within a per-step upload cap so a big camera jump doesn't hitch.
///
/// Pure + deterministic (ties broken by slot index) ⇒ unit-testable without a GPU.
/// Returns `loads: (cluster_id, slot)` to writeBuffer this step. Covers both the
/// stream-in (step 3d) and LRU eviction (step 4) decisions.
#[allow(dead_code)] // wired into the per-frame paging update next
#[cfg(feature = "lod")]
fn plan_stream_evict(
    desired: &[bool],     // per cluster: in the camera's cut this frame?
    resident: &[i32],     // cluster_id -> slot, or -1 (absent)
    slot_cluster: &[i32], // slot -> cluster_id, or -1 (free)
    slot_last_used: &[u64],
    max_loads: usize,
) -> Vec<(usize, usize)> {
    // Wanted = desired but not yet resident.
    let wanted: Vec<usize> = (0..desired.len())
        .filter(|&c| desired[c] && resident[c] < 0)
        .collect();
    // Recycle order: free slots (lowest index first), then non-desired resident
    // slots coldest-first (LRU). A desired slot is never recycled.
    let mut free: Vec<usize> = (0..slot_cluster.len())
        .filter(|&s| slot_cluster[s] < 0)
        .collect();
    free.sort_unstable();
    let mut evictable: Vec<usize> = (0..slot_cluster.len())
        .filter(|&s| {
            let c = slot_cluster[s];
            c >= 0 && !desired[c as usize]
        })
        .collect();
    evictable.sort_by_key(|&s| (slot_last_used[s], s)); // coldest first, deterministic
    let mut recycle = free.into_iter().chain(evictable);

    let mut loads = Vec::new();
    for &c in wanted.iter().take(max_loads) {
        match recycle.next() {
            Some(slot) => loads.push((c, slot)),
            None => break, // pool full of desired-resident clusters; nothing to recycle
        }
    }
    loads
}

/// LOD-off stub: no cluster data is loaded, so the caller falls through to the
/// base glb (every instance draws its base mesh).
#[cfg(not(feature = "lod"))]
async fn load_cluster_lod(
    _renderer: &mut AwsmRenderer,
    _assets: &impl SceneAssets,
    _asset_id: &str,
    _tk: TransformKey,
    _mat: MaterialKey,
) -> Result<Option<MeshKey>> {
    Ok(None)
}

#[cfg(feature = "lod")]
async fn load_cluster_lod(
    renderer: &mut AwsmRenderer,
    assets: &impl SceneAssets,
    asset_id: &str,
    tk: TransformKey,
    mat: MaterialKey,
) -> Result<Option<MeshKey>> {
    if !renderer.features().virtual_geometry {
        return Ok(None);
    }
    let path = format!(
        "{ASSETS_DIR}/{}",
        awsm_renderer_lod_bake::cluster_mesh_filename(asset_id)
    );
    let Ok(bytes) = assets.fetch(&path).await else {
        return Ok(None);
    };
    let cm: awsm_renderer_lod_bake::ClusterMesh = match serde_json::from_slice(&bytes) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("cluster LOD: unreadable `{path}`: {e}");
            return Ok(None);
        }
    };
    materialize_cluster_mesh(renderer, &cm, asset_id, tk, mat).await
}

/// Materialize a parsed [`awsm_renderer_lod_bake::ClusterMesh`] into a bounded
/// cluster-LOD render mesh on the renderer (uploads the cluster pages, caps the
/// resident set to the active streaming/paging budget, builds the page pool when
/// paging, and registers the cluster render mesh `M`). This is the post-fetch core
/// shared by the player load path ([`load_cluster_lod`]) AND the editor's
/// pre-baked nanite import — the editor fetches the `.clusters.bin` its own way
/// and calls this directly. Returns the render mesh key, or `None` when
/// `virtual_geometry` is off or the cluster mesh is empty. `asset_label` is only
/// used for the diagnostic log line.
#[cfg(feature = "lod")]
pub async fn materialize_cluster_mesh(
    renderer: &mut AwsmRenderer,
    cm: &awsm_renderer_lod_bake::ClusterMesh,
    asset_label: &str,
    tk: TransformKey,
    mat: MaterialKey,
) -> Result<Option<MeshKey>> {
    if !renderer.features().virtual_geometry {
        return Ok(None);
    }
    if cm.positions.is_empty() || cm.clusters.is_empty() {
        return Ok(None);
    }
    // Backstop against a malformed DAG (hand-authored / third-party / corrupted
    // `.clusters.bin`): refuse to materialize rather than read out-of-bounds
    // vertices or draw garbage. The bake's own output always passes.
    if let Err(e) = cm.validate() {
        tracing::error!(
            "cluster mesh '{asset_label}': malformed DAG ({e}) — refusing to materialize"
        );
        return Ok(None);
    }

    // Phase B GPU cut (B.2): hand the cluster pages to the GPU cut pass. No-op
    // unless `virtual_geometry` built the pass; coexists with the per-instance
    // discrete cut below (which the GPU per-cluster dispatch will eventually
    // replace). The lod-bake `ROOT_PARENT_ERROR` (f32::MAX) rides through as a
    // huge value, so roots always pass the cut's upper bound on-device.
    // Phase 5 streaming residency: cap M's geometry to a triangle budget when
    // `cluster_streaming` is on, so a multi-million-tri asset loads instead of
    // overflowing the GPU pool. Off ⇒ budget = MAX ⇒ every cluster resident with
    // `cm.indices` verbatim, byte-identical to the non-streaming path.
    // `cluster_paging` (Gap B) implies a residency budget too: the page pool packs
    // each resident cluster into a fixed CLUSTER_PAGE_VERTS slot (build_slot_geometry
    // below), so the resident set MUST stay small enough that the padded slot buffer
    // fits VRAM (full residency = ~1 GiB > the 512 MB GPU buffer cap — see
    // NORTHSTAR-GAPS). A conservative default keeps the bounded antichain comfortably
    // under the cap; `?streambudget=N` overrides it.
    let per_mesh_budget = if renderer.features().cluster_paging {
        renderer
            .features()
            .cluster_streaming_budget
            .unwrap_or(CLUSTER_PAGING_BUDGET_TRIS)
    } else if renderer.features().cluster_streaming {
        renderer
            .features()
            .cluster_streaming_budget
            .unwrap_or(CLUSTER_STREAMING_BUDGET_TRIS)
    } else {
        usize::MAX
    };
    // Global residency cap: bound the SUM across all resident cluster meshes, not
    // just each one. A later mesh's budget shrinks by what's already resident, so
    // total VRAM stays bounded at `per_mesh_budget * GLOBAL_RESIDENCY_MESH_MULTIPLE`
    // regardless of mesh count. Few-mesh scenes are unaffected (the subtraction
    // leaves the full per-mesh budget); the uncapped path (`MAX`) stays uncapped, so
    // the shipped streaming/paging-off path is byte-identical.
    let budget = if per_mesh_budget == usize::MAX {
        usize::MAX
    } else {
        let global_cap = per_mesh_budget.saturating_mul(GLOBAL_RESIDENCY_MESH_MULTIPLE);
        let already_resident = renderer.cluster_resident_tris_total();
        per_mesh_budget.min(global_cap.saturating_sub(already_resident))
    };
    // No global budget left for this mesh — refuse to materialize (renders nothing)
    // rather than allocate an over-budget pool. Rare: only past the global cap.
    if budget == 0 {
        tracing::warn!(
            "cluster LOD: {asset_label} skipped — global residency budget exhausted \
             ({} tris already resident, per-mesh budget {per_mesh_budget})",
            renderer.cluster_resident_tris_total()
        );
        return Ok(None);
    }
    let (gpu_pages, m_indices, resident_cluster_ids) = select_resident_clusters(cm, budget);
    let resident_tris = m_indices.len() / 3;
    let capped = m_indices.len() < cm.indices.len();
    let resident_page_count = gpu_pages.len();

    // The cluster-LOD GPU state is keyed by the render mesh `M`, but `M`'s key only
    // exists after `add_raw_mesh` (below) — which needs `m_geometry_indices` from
    // this block. So the block computes the upload payload and DEFERS the actual
    // upload until `m_key` is known (right after `add_raw_mesh`).
    enum ClusterUploads {
        Paging {
            gpu_pages_pool: Vec<awsm_renderer::cluster_lod::ClusterPage>,
            slot_source: Vec<u32>,
            resident_pool: Vec<i32>,
            init: awsm_renderer::render_passes::cluster_lod::ClusterPagingInit,
        },
        Simple {
            gpu_pages: Vec<awsm_renderer::cluster_lod::ClusterPage>,
            identity_indices: Vec<u32>,
        },
    }

    // The compaction's `source_indices` is the EXPLODED raster index space, not
    // `cm.indices`: the renderer explodes geometry (pack_visibility_bytes) so
    // triangle t's corners are exploded vertices [3t,3t+1,3t+2]. Cluster pages are
    // triangle-aligned (and remapped contiguously into m_indices above), so the
    // raster indices for a page [F,C) are exactly the identity range [F,F+C) —
    // feed identity and the unchanged compaction emits a drawable stream into M's
    // exploded buffer.
    // Geometry layout for the cluster render mesh M + compaction `source_indices`:
    //  - NON-PAGING (shipped): contiguous M; identity source (page [F,C) ⇒ [F,F+C)).
    //  - PAGING (Gap B step 2): pack the BOUNDED resident set (capped by the paging
    //    budget above) into a fixed CLUSTER_PAGE_VERTS slot pool sized to exactly the
    //    resident count (every cluster gets a slot ⇒ no overflow ⇒ crack-free), so
    //    slots are independently swappable for streaming. The drawn cut equals the
    //    capped frontier (like `?streambudget`), crack-free. Slot buffer =
    //    resident*page_verts verts — kept under the GPU cap by the conservative
    //    paging budget. Upload the resident table (identity) the cut variant reads.
    let (m_geometry_indices, uploads): (Vec<u32>, ClusterUploads) = if renderer
        .features()
        .cluster_paging
    {
        // Gap B 20b-iv-b-1: a page pool with HEADROOM. The load-time frontier occupies
        // slots `0..frontier` (clamped, drawn); the pool has `pool_slots > frontier`
        // total so the per-frame streamer (20b-iv-b-2) can stream finer clusters into
        // the SPARE slots BEFORE evicting the coarse ones (crack-free transitions).
        // Spare slots/pages are non-drawn (resident `-1` ⇒ the cut's paging variant
        // skips them; index_count 0). The pool is bounded (the VRAM cap): the drawn
        // cut + the streamed working set both stay within `pool_slots`.
        let frontier = gpu_pages.len();
        let pool_slots = (frontier * 2).max(frontier + 1);
        let resident_ids: Vec<usize> = (0..frontier).collect();
        let plan = plan_page_pool(frontier, &resident_ids, frontier.max(1));
        let page_spans: Vec<(u32, u32)> = gpu_pages
            .iter()
            .map(|p| (p.first_index, p.index_count))
            .collect();
        let (mut slot_indices, mut slot_source) =
            build_slot_geometry(&page_spans, &m_indices, &plan.resident, CLUSTER_PAGE_VERTS);
        // Pad M + slot-aligned source_indices to `pool_slots` (spare slots = empty
        // exploded verts + unused source region, never drawn; filled on stream-in).
        slot_indices.resize(pool_slots * CLUSTER_PAGE_VERTS, 0);
        slot_source.resize(pool_slots * CLUSTER_PAGE_VERTS, 0);
        // Pad the GPU pages + resident table to `pool_slots`: spare pages draw nothing
        // (index_count 0) and are skipped by the cut (resident `-1`).
        let spare_page = awsm_renderer::cluster_lod::ClusterPage {
            center: [0.0; 3],
            radius: 0.0,
            lod_error: 0.0,
            parent_error: 0.0,
            lod_bounds_center: [0.0; 3],
            lod_bounds_radius: 0.0,
            parent_bounds_center: [0.0; 3],
            parent_bounds_radius: 0.0,
            first_index: 0,
            index_count: 0,
        };
        let mut gpu_pages_pool = gpu_pages.clone();
        gpu_pages_pool.resize(pool_slots, spare_page);
        // source_indices is now SLOT-ALIGNED ⇒ each frontier page's draw span starts at
        // its slot: first_index = slot*PAGE_VERTS (was the m_indices offset). Residency
        // is identity here (resident[c]=c), so page c lives in slot c.
        for (c, page) in gpu_pages_pool.iter_mut().take(frontier).enumerate() {
            page.first_index = (c * CLUSTER_PAGE_VERTS) as u32;
        }
        let mut resident_pool = plan.resident.clone();
        resident_pool.resize(pool_slots, -1);
        // Arm the Gap-B paging manager (step 20a/20b-iii) with: the FULL un-clamped
        // DAG (the bake's real `[lod_error, parent_error)` per cluster — NOT the
        // clamped frontier `gpu_pages`) for the per-frame CPU cut; the CPU geometry
        // (positions/normals/indices) the per-frame streamer (20b-iv) gathers a
        // slot's exploded verts from; and the INITIAL residency seed
        // `slot_cluster[slot] = resident_cluster_ids[slot]` (the load-time frontier,
        // in slot order — `build_slot_geometry` packs `gpu_pages`/`resident_cluster_ids`
        // in order, so slot s holds full-DAG cluster `resident_cluster_ids[s]`). Only
        // the `cluster_paging` path reaches here ⇒ shipped path unaffected. The GPU
        // upload above is UNCHANGED from step 2 (the watertight frontier) — this slice
        // only enriches CPU manager state; per-frame streaming lands in 20b-iv.
        let original_pages: Vec<awsm_renderer::cluster_lod::ClusterPage> = cm
            .clusters
            .iter()
            .map(|c| awsm_renderer::cluster_lod::ClusterPage {
                center: c.center,
                radius: c.radius,
                lod_error: c.lod_error,
                parent_error: c.parent_error,
                lod_bounds_center: c.lod_bounds_center,
                lod_bounds_radius: c.lod_bounds_radius,
                parent_bounds_center: c.parent_bounds_center,
                parent_bounds_radius: c.parent_bounds_radius,
                first_index: c.first_index,
                index_count: c.index_count,
            })
            .collect();
        // Manager residency seed: slot_cluster has `pool_slots` entries — the frontier
        // clusters in slots `0..frontier`, the rest `-1` (free, available to stream into).
        let mut slot_cluster: Vec<i32> = resident_cluster_ids.iter().map(|&c| c as i32).collect();
        slot_cluster.resize(pool_slots, -1);
        let init = awsm_renderer::render_passes::cluster_lod::ClusterPagingInit {
            pages: original_pages,
            positions: cm.positions.clone(),
            normals: cm.normals.clone(),
            indices: cm.indices.clone(),
            slot_cluster,
            page_verts: CLUSTER_PAGE_VERTS,
        };
        tracing::info!(
            "cluster paging (Gap B): page pool — {} slots × {} verts/slot = {} slot verts ({} resident tris capped to budget {}); cut draws the capped frontier crack-free",
            plan.slots_used,
            CLUSTER_PAGE_VERTS,
            slot_indices.len(),
            resident_tris,
            budget,
        );
        (
            slot_indices,
            ClusterUploads::Paging {
                gpu_pages_pool,
                slot_source,
                resident_pool,
                init,
            },
        )
    } else {
        let identity_indices: Vec<u32> = (0..m_indices.len() as u32).collect();
        (
            m_indices,
            ClusterUploads::Simple {
                gpu_pages,
                identity_indices,
            },
        )
    };

    // Cluster render mesh M = the (resident) cluster geometry as an ordinary mesh,
    // so its exploded vertex buffer is in m_indices triangle order with
    // vertex_attribute_indices = m_indices (the normal material path). M is the
    // node's visible mesh; the GPU per-cluster cut's compacted indirect stream
    // draws into M's buffer (the geometry-pass override in mesh.rs keys on
    // `cluster_lod.state(mesh_key)`), so detail varies WITHIN the mesh by
    // per-cluster distance. (load_cluster_lod is only reached when
    // virtual_geometry is on.)
    let m_raw = RawMeshData {
        positions: cm.positions.clone(),
        normals: (!cm.normals.is_empty()).then(|| cm.normals.clone()),
        uv_sets: if cm.uvs.is_empty() {
            vec![]
        } else {
            vec![cm.uvs.clone()]
        },
        colors: (!cm.colors.is_empty()).then(|| cm.colors.clone()),
        indices: m_geometry_indices,
        tangents: None,
        skin: None,
        morph: None,
    };
    let m_key = renderer.add_raw_mesh(m_raw, tk, mat)?;
    // Now that M exists, apply the deferred GPU uploads keyed by `m_key` so SEVERAL
    // cluster meshes can be resident at once (each owns its cut/compaction state).
    match uploads {
        ClusterUploads::Paging {
            gpu_pages_pool,
            slot_source,
            resident_pool,
            init,
        } => {
            renderer.upload_cluster_pages(
                m_key,
                &gpu_pages_pool,
                &slot_source,
                resident_tris as u32,
            )?;
            renderer.upload_cluster_resident(m_key, &resident_pool)?;
            renderer.init_cluster_paging(m_key, init);
        }
        ClusterUploads::Simple {
            gpu_pages,
            identity_indices,
        } => {
            renderer.upload_cluster_pages(
                m_key,
                &gpu_pages,
                &identity_indices,
                resident_tris as u32,
            )?;
        }
    }
    tracing::info!(
        "cluster LOD (GPU): {asset_label} {} clusters ({} resident), render mesh M = {} tris{}, per-cluster cut drives draw",
        cm.cluster_count(),
        resident_page_count,
        resident_tris,
        if capped {
            // Print the EFFECTIVE budget actually applied (URL override or the
            // default), not the const — they differ when `?streambudget=N` is set.
            format!(
                " (CAPPED from {} — streaming residency budget {})",
                cm.indices.len() / 3,
                budget
            )
        } else {
            String::new()
        }
    );
    Ok(Some(m_key))
}

/// Whether a node opts in to LOD (its per-mesh `MeshLodConfig.enabled`). The
/// asset is baked with levels if *any* referencing node is enabled, so the
/// runtime must re-check the per-node toggle: a LOD-off instance pins level 0.
fn node_lod_enabled(kind: &NodeKind) -> bool {
    kind.mesh_lod().map(|l| l.enabled).unwrap_or(false)
}

/// Load + register the discrete-LOD level chain for a **static** mesh asset.
///
/// Each level glb (`<id>.lod{N}.glb`) is loaded under the same transform `tk` and
/// material `mat` as the base, so a level renders co-located with it; the level
/// meshes are set hidden (they draw only when the per-frame selection reroutes to
/// them) and the chain is registered on `renderer.lod` keyed by `base_key`.
///
/// A no-op when the `lod` feature is off, or when the mesh has no `.lod.toml`
/// manifest (most meshes — below the bake floor or LOD-disabled). Skinned/morph
/// LOD selection runs on a separate path (shared skeleton) and is not loaded here.
/// LOD-off stub.
#[cfg(not(feature = "lod"))]
async fn load_static_lod_chain(
    _renderer: &mut AwsmRenderer,
    _assets: &impl SceneAssets,
    _asset_id: &str,
    _base_key: MeshKey,
    _tk: TransformKey,
    _mat: MaterialKey,
) -> Result<()> {
    Ok(())
}

#[cfg(feature = "lod")]
async fn load_static_lod_chain(
    renderer: &mut AwsmRenderer,
    assets: &impl SceneAssets,
    asset_id: &str,
    base_key: MeshKey,
    tk: TransformKey,
    mat: MaterialKey,
) -> Result<()> {
    if !renderer.features().lod {
        return Ok(());
    }
    let manifest_path = format!(
        "{ASSETS_DIR}/{}",
        awsm_renderer_lod_bake::lod_manifest_filename(asset_id)
    );
    // Missing manifest = this mesh has no LOD; not an error.
    let Ok(bytes) = assets.fetch(&manifest_path).await else {
        return Ok(());
    };
    let manifest: awsm_renderer_lod_bake::MeshLodManifest = match std::str::from_utf8(&bytes)
        .ok()
        .and_then(|s| toml::from_str(s).ok())
    {
        Some(m) => m,
        None => {
            tracing::warn!("lod: ignoring unreadable manifest `{manifest_path}`");
            return Ok(());
        }
    };

    let mut levels = Vec::with_capacity(manifest.levels.len());
    for lvl in &manifest.levels {
        let leaf = awsm_renderer_lod_bake::lod_level_filename(asset_id, lvl.index);
        let (keys, _, _) = load_glb_under(renderer, assets, &leaf, Some(tk), mat).await?;
        let Some(&level_key) = keys.first() else {
            continue;
        };
        // Levels are off by default — only the selected level is shown each frame.
        for &k in &keys {
            let _ = renderer.set_mesh_hidden(k, true);
        }
        levels.push(LodLevel {
            mesh_key: level_key,
            error: lvl.error,
        });
    }
    if !levels.is_empty() {
        renderer.lod.register(
            base_key,
            LodChain {
                levels,
                bounds_radius: manifest.bounds_radius,
                ..Default::default()
            },
        );
    }
    Ok(())
}

/// Load + register the discrete-LOD level chain for a **skinned / morph** mesh.
///
/// Each level rig glb (`<source>.lod{N}.glb`) carries its own skeleton, but the
/// animation clips only drive the *base* rig's joints. So instead of loading the
/// level glb wholesale (which would make a second, undriven skeleton), this
/// extracts each level mesh node's geometry + skin + morph and rebuilds it with
/// `add_raw_mesh`, **rebinding** its skin to the BASE rig's joint transforms
/// (mapped through `node_index_transforms` — valid because every level shares the
/// base's joint node indices, both coming from `reexport_clean_scene` of the same
/// source). The level meshes thus deform with the base; they're set hidden and
/// the chain is registered keyed by `base_key`.
///
/// No-op when `lod` is off / no manifest. Scoped to the common single-mesh-node
/// skinned case: the chain is keyed on `base_key` and tracks each level's first
/// mesh node (multi-mesh skinned LOD is a follow-up).
/// LOD-off stub.
#[cfg(not(feature = "lod"))]
async fn load_skinned_lod_chain(
    _renderer: &mut AwsmRenderer,
    _assets: &impl SceneAssets,
    _source_id: &str,
    _base_key: MeshKey,
    _node_index_transforms: &HashMap<usize, TransformKey>,
    _mat: MaterialKey,
) -> Result<()> {
    Ok(())
}

#[cfg(feature = "lod")]
async fn load_skinned_lod_chain(
    renderer: &mut AwsmRenderer,
    assets: &impl SceneAssets,
    source_id: &str,
    base_key: MeshKey,
    node_index_transforms: &HashMap<usize, TransformKey>,
    mat: MaterialKey,
) -> Result<()> {
    use awsm_renderer::meshes::buffer_info::MeshBufferGeometryMorphInfo;
    use awsm_renderer::raw_mesh::{RawMeshData, RawMorph, RawSkin};

    if !renderer.features().lod {
        return Ok(());
    }
    let manifest_path = format!(
        "{ASSETS_DIR}/{}",
        awsm_renderer_lod_bake::lod_manifest_filename(source_id)
    );
    let Ok(bytes) = assets.fetch(&manifest_path).await else {
        return Ok(());
    };
    let manifest: awsm_renderer_lod_bake::MeshLodManifest = match std::str::from_utf8(&bytes)
        .ok()
        .and_then(|s| toml::from_str(s).ok())
    {
        Some(m) => m,
        None => {
            tracing::warn!("lod: ignoring unreadable skinned manifest `{manifest_path}`");
            return Ok(());
        }
    };

    let root = renderer.transforms.root_node;
    let mut levels = Vec::with_capacity(manifest.levels.len());
    for lvl in &manifest.levels {
        let leaf = awsm_renderer_lod_bake::lod_level_filename(source_id, lvl.index);
        let key = format!("{ASSETS_DIR}/{leaf}");
        let Ok(glb) = assets.fetch(&key).await else {
            continue;
        };
        // Parse once; rebuild every mesh node bound to the base's transforms.
        let data = GltfLoader::from_glb_bytes(&glb).await?.into_data(None)?;
        let mut level_first: Option<MeshKey> = None;
        let node_indices: Vec<usize> = data
            .doc
            .nodes()
            .filter(|n| n.mesh().is_some())
            .map(|n| n.index())
            .collect();
        for node_index in node_indices {
            let Some(extracted) = awsm_renderer_glb_export::extract_node_mesh(
                &data.doc,
                &data.buffers.raw,
                node_index as u32,
                None,
            ) else {
                continue;
            };
            // Rebind skin joints (rig node index → base transform).
            let raw_skin = match extracted.skin.as_ref() {
                Some(s) => {
                    let mut joints = Vec::with_capacity(s.joint_node_indices.len());
                    let mut ok = true;
                    for rig_idx in &s.joint_node_indices {
                        match node_index_transforms.get(rig_idx) {
                            Some(&tk) => joints.push(tk),
                            None => {
                                ok = false;
                                break;
                            }
                        }
                    }
                    if !ok {
                        None
                    } else {
                        Some(RawSkin {
                            joints,
                            inverse_bind_matrices: s
                                .inverse_bind_matrices
                                .iter()
                                .map(glam::Mat4::from_cols_array)
                                .collect(),
                            set_count: 1,
                            index_weights: s.packed_index_weights(),
                        })
                    }
                }
                None => None,
            };
            let vertex_count = extracted.mesh.positions.len();
            let raw_morph = extracted.morph.as_ref().map(|m| {
                let values = m.packed_values(vertex_count);
                RawMorph {
                    info: MeshBufferGeometryMorphInfo {
                        targets_len: m.targets_len(),
                        vertex_stride_size: m.vertex_stride_size(),
                        values_size: values.len(),
                    },
                    weights: m.weights_bytes(),
                    values,
                }
            });
            let raw = RawMeshData {
                positions: extracted.mesh.positions,
                normals: extracted.mesh.normals,
                uv_sets: extracted.mesh.uvs,
                colors: extracted.mesh.colors,
                indices: extracted.mesh.indices,
                // Authored tangents from the rig glb → used verbatim (correct basis +
                // skips MikkTSpace); `None` ⇒ regenerate as before.
                tangents: extracted.tangents,
                skin: raw_skin,
                morph: raw_morph,
            };
            // Self-placing under the renderer root, exactly like the base rig.
            let tk = renderer.transforms.insert(Transform::default(), Some(root));
            let Ok(mk) = renderer.add_raw_mesh(raw, tk, mat) else {
                continue;
            };
            let _ = renderer.set_mesh_hidden(mk, true);
            if level_first.is_none() {
                level_first = Some(mk);
            }
        }
        if let Some(mesh_key) = level_first {
            levels.push(LodLevel {
                mesh_key,
                error: lvl.error,
            });
        }
    }
    if !levels.is_empty() {
        renderer.lod.register(
            base_key,
            LodChain {
                levels,
                bounds_radius: manifest.bounds_radius,
                ..Default::default()
            },
        );
    }
    Ok(())
}

/// What [`load_glb_under`] hands back per glb: the renderer mesh keys it
/// produced (one per primitive), plus the glb-node-index → transform-key and
/// glb-node-index → mesh-keys lookups the skinned-mesh arm rebinds with.
type LoadedGlb = (
    Vec<MeshKey>,
    HashMap<usize, TransformKey>,
    HashMap<usize, Vec<MeshKey>>,
);

/// our material-source semantics outside the full scene pass.
pub async fn load_glb_under(
    renderer: &mut AwsmRenderer,
    assets: &impl SceneAssets,
    leaf: &str,
    parent: Option<TransformKey>,
    material: MaterialKey,
) -> Result<LoadedGlb> {
    let key = format!("{ASSETS_DIR}/{leaf}");
    let bytes = assets
        .fetch(&key)
        .await
        .map_err(|_| anyhow!("bundle is missing mesh glb `{key}`"))?;
    // The bundle's glb is geometry-only (materials stripped) and we apply OUR
    // material via `Single`. The geometry KIND (visibility vs transparency) is no
    // longer baked at decode — the renderer derives it at commit from the bound
    // material, so a transparent material correctly gets
    // transparency geometry with no per-load override. The same glb asset can be
    // shared by nodes with different materials, each resolving its own kind.
    let transparent = renderer.materials.is_transparency_pass(material);
    let data = GltfLoader::from_glb_bytes(&bytes).await?.into_data(None)?;
    let ctx = renderer
        .populate_gltf_with(
            data,
            PopulateGltfOpts {
                scene: None,
                parent_transform: parent,
                material_source: GltfMaterialSource::Single(material),
            },
        )
        .await?;
    let (keys, node_index_transforms, node_index_meshes): LoadedGlb = {
        let lookups = ctx.key_lookups.lock().unwrap();
        // The renderer mesh keys this glb produced (one per primitive), so the host
        // can remove them on teardown.
        let keys = lookups.all_mesh_keys.values().flatten().copied().collect();
        // glb node index → baked transform key — the skinned-mesh arm binds each
        // skeleton joint (by its clean-glb node index) to drive the skin.
        // glb node index → its mesh keys — the skinned-mesh arm rebinds each
        // scene node's material onto its own glb node's primitives when a
        // multi-mesh rig loads once for several scene skinned-mesh nodes.
        (
            keys,
            lookups.node_index_to_transform.clone(),
            lookups.all_mesh_keys.clone(),
        )
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
    Ok((keys, node_index_transforms, node_index_meshes))
}

/// Materialize just one node's renderable **mesh(es)** with the given `material`,
/// outside the full scene pass — the public (R4) wrapper over the loader's shared
/// mesh-build path ([`build_node_meshes`]).
///
/// Inserts a fresh [`TransformKey`] from `node.transform` (rooted at the renderer
/// root) and builds the node's geometry under it, returning the visible
/// [`MeshKey`]s. Covers the mesh-bearing kinds the Mesh/SkinnedMesh/Sprite arms
/// handle — `Mesh` (primitive + bare-geometry glb), `SkinnedMesh` (self-placing rig
/// glb, rooted at the renderer root regardless of the inserted transform), and
/// `Sprite` (which builds its own material, ignoring `material`); every other
/// [`NodeKind`] returns an empty vec. The caller owns the returned keys (and the
/// transform via the meshes) for later teardown — this does not track them on any
/// [`LoadedScene`].
pub async fn materialize_node_mesh(
    renderer: &mut AwsmRenderer,
    scene: &Scene,
    node: &EditorNode,
    assets: &impl SceneAssets,
    material: MaterialKey,
) -> Result<Vec<MeshKey>> {
    let tk = renderer
        .transforms
        .insert(trs_to_transform(&node.transform), None);
    // Public API: no caller-visible cache — a fresh per-call one still dedupes
    // and on-demand-fetches within this node's own slots.
    let mut cache = texture::TextureCache::new(scene);
    build_node_meshes(
        renderer, &mut cache, scene, node, tk, material, assets, None, false,
    )
    .await
}

fn trs_to_transform(trs: &Trs) -> Transform {
    Transform {
        translation: Vec3::from_array(trs.translation),
        rotation: Quat::from_array(trs.rotation),
        scale: Vec3::from_array(trs.scale),
    }
}

/// Convert an [`awsm_renderer_meshgen::MeshData`] (the procedural/primitive mesh builder's
/// output) into the renderer's [`RawMeshData`] upload struct — positions, normals,
/// colors, indices, and ALL UV sets pass through (both carry N TEXCOORD sets now).
/// Public (R4) so a host can feed a meshgen primitive into `renderer.add_raw_mesh`
/// with the same conversion the loader uses.
pub fn mesh_data_to_raw(md: awsm_renderer_meshgen::MeshData) -> RawMeshData {
    RawMeshData {
        positions: md.positions,
        normals: md.normals,
        uv_sets: md.uvs,
        colors: md.colors,
        indices: md.indices,
        ..Default::default()
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
    cache: &mut texture::TextureCache,
    instance: Option<&MaterialInstance>,
    placeholder: MaterialKey,
    assets: &impl SceneAssets,
    custom: &HashMap<AssetId, awsm_renderer_materials::MaterialShaderId>,
) -> MaterialKey {
    let Some(inst) = instance else {
        return placeholder;
    };
    // Custom-WGSL assignment: the asset resolved to a registered shader (Phase 0).
    // Build a Material::Custom (defaults + uniform overrides); `inline` is ignored.
    if let Some(&shader_id) = custom.get(&inst.asset) {
        if let Some(mat) =
            dynamic::build_custom_material(renderer, cache, shader_id, inst, assets).await
        {
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
                    texture::load_texture(renderer, cache, assets, t, true, K::Albedo).await;
            }
            if let Some(t) = &def.metallic_roughness_texture {
                pbr.metallic_roughness_tex =
                    texture::load_texture(renderer, cache, assets, t, false, K::MetallicRoughness)
                        .await;
            }
            if let Some(t) = &def.normal_texture {
                pbr.normal_tex =
                    texture::load_texture(renderer, cache, assets, t, false, K::Normal).await;
            }
            if let Some(t) = &def.occlusion_texture {
                pbr.occlusion_tex =
                    texture::load_texture(renderer, cache, assets, t, false, K::Occlusion).await;
            }
            if let Some(t) = &def.emissive_texture {
                pbr.emissive_tex =
                    texture::load_texture(renderer, cache, assets, t, true, K::Emissive).await;
            }
            // KHR-extension texture slots (the factors are already mapped by
            // `material_to_pbr`; bind their textures the same way the editor does).
            bind_extension_textures(renderer, cache, assets, def, &mut pbr).await;
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
    cache: &mut texture::TextureCache,
    assets: &impl SceneAssets,
    def: &awsm_renderer_scene::MaterialDef,
    pbr: &mut awsm_renderer::materials::pbr::PbrMaterial,
) {
    use MipmapTextureKind as K;
    let ext = &def.extensions;
    if let (Some(e), Some(p)) = (ext.specular.as_ref(), pbr.specular.as_mut()) {
        if let Some(t) = &e.tex {
            p.tex = texture::load_texture(renderer, cache, assets, t, false, K::MetallicRoughness)
                .await;
        }
        if let Some(t) = &e.color_tex {
            p.color_tex = texture::load_texture(renderer, cache, assets, t, true, K::Albedo).await;
        }
    }
    if let (Some(e), Some(p)) = (ext.transmission.as_ref(), pbr.transmission.as_mut()) {
        if let Some(t) = &e.tex {
            p.tex = texture::load_texture(renderer, cache, assets, t, false, K::MetallicRoughness)
                .await;
        }
    }
    if let (Some(e), Some(p)) = (
        ext.diffuse_transmission.as_ref(),
        pbr.diffuse_transmission.as_mut(),
    ) {
        if let Some(t) = &e.tex {
            p.tex = texture::load_texture(renderer, cache, assets, t, false, K::MetallicRoughness)
                .await;
        }
        if let Some(t) = &e.color_tex {
            p.color_tex = texture::load_texture(renderer, cache, assets, t, true, K::Albedo).await;
        }
    }
    if let (Some(e), Some(p)) = (ext.volume.as_ref(), pbr.volume.as_mut()) {
        if let Some(t) = &e.thickness_tex {
            p.thickness_tex =
                texture::load_texture(renderer, cache, assets, t, false, K::MetallicRoughness)
                    .await;
        }
    }
    if let (Some(e), Some(p)) = (ext.clearcoat.as_ref(), pbr.clearcoat.as_mut()) {
        if let Some(t) = &e.tex {
            p.tex = texture::load_texture(renderer, cache, assets, t, false, K::MetallicRoughness)
                .await;
        }
        if let Some(t) = &e.roughness_tex {
            p.roughness_tex =
                texture::load_texture(renderer, cache, assets, t, false, K::MetallicRoughness)
                    .await;
        }
        if let Some(t) = &e.normal_tex {
            p.normal_tex =
                texture::load_texture(renderer, cache, assets, t, false, K::Normal).await;
        }
    }
    if let (Some(e), Some(p)) = (ext.sheen.as_ref(), pbr.sheen.as_mut()) {
        if let Some(t) = &e.color_tex {
            p.color_tex = texture::load_texture(renderer, cache, assets, t, true, K::Albedo).await;
        }
        if let Some(t) = &e.roughness_tex {
            p.roughness_tex =
                texture::load_texture(renderer, cache, assets, t, false, K::MetallicRoughness)
                    .await;
        }
    }
    if let (Some(e), Some(p)) = (ext.anisotropy.as_ref(), pbr.anisotropy.as_mut()) {
        if let Some(t) = &e.tex {
            p.tex = texture::load_texture(renderer, cache, assets, t, false, K::Normal).await;
        }
    }
    if let (Some(e), Some(p)) = (ext.iridescence.as_ref(), pbr.iridescence.as_mut()) {
        if let Some(t) = &e.tex {
            p.tex = texture::load_texture(renderer, cache, assets, t, false, K::MetallicRoughness)
                .await;
        }
        if let Some(t) = &e.thickness_tex {
            p.thickness_tex =
                texture::load_texture(renderer, cache, assets, t, false, K::MetallicRoughness)
                    .await;
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

#[cfg(test)]
mod prefab_tests {
    //! Prefab capture/instancing is exercised here through its pure structural
    //! core, [`prefab_subtree_layout`] — the production traversal `capture_prefab`
    //! drives (DFS pre-order, parent wiring, nested-prefab boundaries).
    //!
    //! The full `populate_awsm_scene` → `instantiate` round-trip is NOT unit-tested
    //! natively: it needs a live `AwsmRenderer`, which requires a GPU/WebGPU device
    //! (the renderer runs on wasm). That path is covered by the browser round-trip
    //! harness instead; unit-testing it here would block on an un-unit-testable GPU
    //! dependency, so we test the part we can pin down without a device.
    use super::{expand_instance_colors, prefab_subtree_layout};
    use awsm_renderer_scene::{EditorNode, NodeId, NodeKind};

    #[test]
    fn instance_colors_repeat_last_when_short_and_truncate_when_long() {
        let red = [1.0, 0.0, 0.0, 1.0];
        let green = [0.0, 1.0, 0.0, 1.0];
        // Shorter than count → last value (green) repeats to fill.
        let out = expand_instance_colors(&[red, green], 4);
        assert_eq!(out, vec![red, green, green, green]);
        // Longer than count → truncated to count.
        let out = expand_instance_colors(&[red, green, red], 2);
        assert_eq!(out, vec![red, green]);
        // Exact length → identity.
        let out = expand_instance_colors(&[red, green], 2);
        assert_eq!(out, vec![red, green]);
    }

    fn node(id: NodeId, prefab: bool, children: Vec<EditorNode>) -> EditorNode {
        EditorNode {
            id,
            name: String::new(),
            transform: Default::default(),
            kind: NodeKind::Group,
            locked: false,
            visible: true,
            prefab,
            children,
        }
    }

    // The layout is the plan `capture_prefab` replays: root first (parent None),
    // then descendants in DFS pre-order, each wired to its in-subtree parent.
    #[test]
    fn layout_is_dfs_preorder_with_parent_wiring() {
        // root ── child1 ── grandchild
        //      └─ child2
        let (root, child1, grandchild, child2) =
            (NodeId::new(), NodeId::new(), NodeId::new(), NodeId::new());
        let tree = node(
            root,
            true,
            vec![
                node(child1, false, vec![node(grandchild, false, vec![])]),
                node(child2, false, vec![]),
            ],
        );

        let layout = prefab_subtree_layout(&tree);
        let plan: Vec<_> = layout
            .iter()
            .map(|s| (s.node.id, s.parent, s.nested_prefab))
            .collect();

        assert_eq!(
            plan,
            vec![
                (root, None, false),
                (child1, Some(root), false),
                (grandchild, Some(child1), false),
                (child2, Some(root), false),
            ]
        );
    }

    // A nested prefab child is recorded as a boundary (so `capture_prefab` captures
    // it as its OWN template) and its descendants are NOT inlined into the parent.
    #[test]
    fn nested_prefab_is_a_boundary_and_excludes_its_descendants() {
        // root ── nested(prefab) ── deep
        let (root, nested, deep) = (NodeId::new(), NodeId::new(), NodeId::new());
        let tree = node(
            root,
            true,
            vec![node(nested, true, vec![node(deep, false, vec![])])],
        );

        let layout = prefab_subtree_layout(&tree);
        let ids: Vec<_> = layout.iter().map(|s| s.node.id).collect();

        // `deep` belongs to the nested template, not this plan.
        assert_eq!(ids, vec![root, nested]);
        let nested_step = layout.iter().find(|s| s.node.id == nested).unwrap();
        assert!(nested_step.nested_prefab);
        assert_eq!(nested_step.parent, Some(root));
        // The root is never itself a nested boundary.
        assert!(!layout[0].nested_prefab);
    }

    // The explicit-instancer replay capture (`instancer_prefab_replay`): authored
    // placements + colours are carried verbatim (Trs → renderer Transform), and
    // the two valid "not wired up yet" authored states — nil mesh ref / empty
    // transform list — capture nothing (PrefabReplay::None), matching the live
    // `materialize_instancer` guard.
    #[test]
    fn instancer_replay_captures_placements_and_skips_unwired_defs() {
        use super::{instancer_prefab_replay, PrefabReplay};
        use awsm_renderer_scene::{
            instances::InstancerDef, primitive::MeshRef, transform::Trs, AssetId,
        };

        let wired = InstancerDef {
            mesh: MeshRef(AssetId::new()),
            transforms: vec![
                Trs {
                    translation: [1.0, 2.0, 3.0],
                    ..Trs::IDENTITY
                },
                Trs {
                    translation: [-4.0, 0.0, 0.0],
                    scale: [2.0, 2.0, 2.0],
                    ..Trs::IDENTITY
                },
            ],
            per_instance_colors: vec![[1.0, 0.0, 0.0, 1.0]],
            ..Default::default()
        };
        match instancer_prefab_replay(&wired) {
            PrefabReplay::Instancer {
                transforms,
                per_instance_colors,
            } => {
                assert_eq!(transforms.len(), 2);
                assert_eq!(transforms[0].translation.to_array(), [1.0, 2.0, 3.0]);
                assert_eq!(transforms[1].scale.to_array(), [2.0, 2.0, 2.0]);
                assert_eq!(per_instance_colors, vec![[1.0, 0.0, 0.0, 1.0]]);
            }
            other => panic!("expected PrefabReplay::Instancer, got {other:?}"),
        }

        // Nil mesh ref → nothing to replay.
        let nil_mesh = InstancerDef {
            transforms: wired.transforms.clone(),
            ..Default::default()
        };
        assert!(matches!(
            instancer_prefab_replay(&nil_mesh),
            PrefabReplay::None
        ));

        // Empty transform list → nothing to replay.
        let no_placements = InstancerDef {
            mesh: wired.mesh,
            ..Default::default()
        };
        assert!(matches!(
            instancer_prefab_replay(&no_placements),
            PrefabReplay::None
        ));
    }
}

#[cfg(all(test, feature = "lod"))]
mod cluster_streaming_tests {
    //! Phase 5 capped-residency selection ([`select_resident_clusters`]) — the
    //! pure CPU core that bounds the cluster render mesh `M` to a triangle budget.
    //! The GPU upload/draw around it needs a device (browser round-trip), but the
    //! resident-set choice + `first_index` remap + leaf clamp are device-free.
    use super::{
        build_slot_geometry, cluster_finer_group, plan_page_pool, plan_stream_evict,
        select_resident_clusters,
    };

    #[test]
    fn stream_evict_fills_free_slots_first() {
        // clusters 0 & 1 wanted (desired, absent), 2 free slots, none resident.
        let loads = plan_stream_evict(&[true, true, false], &[-1, -1, -1], &[-1, -1], &[0, 0], 10);
        assert_eq!(loads, vec![(0, 0), (1, 1)]); // free slots 0,1 in order
    }

    #[test]
    fn stream_evict_evicts_coldest_non_desired_when_full() {
        // cluster 0 wanted; both slots full of NON-desired (c1 in slot0 hot, c2 in slot1 cold).
        let loads = plan_stream_evict(
            &[true, false, false],
            &[-1, 0, 1], // c1->slot0, c2->slot1
            &[1, 2],     // slot0->c1, slot1->c2
            &[5, 2],     // slot1 colder
            10,
        );
        assert_eq!(loads, vec![(0, 1)]); // evict coldest slot1, load cluster 0 there
    }

    #[test]
    fn stream_evict_honours_max_loads_and_skips_resident() {
        // 4 clusters all desired; c3 already resident (slot0). 3 free slots, cap 2.
        let loads = plan_stream_evict(
            &[true, true, true, true],
            &[-1, -1, -1, 0],
            &[3, -1, -1, -1],
            &[9, 0, 0, 0],
            2,
        );
        assert_eq!(loads, vec![(0, 1), (1, 2)]); // cap honoured; resident c3 skipped
    }
    use awsm_renderer_lod_bake::{
        build_cluster_dag, ClusterMesh, ClusterPage, DagOptions, ROOT_PARENT_ERROR,
    };

    #[test]
    fn finer_group_links_synthetic_two_level_dag() {
        // C0, C1 leaves (lod K0,K1; parent = group KG); C2 root (lod KG; parent ROOT).
        let k0 = [0u32, 0, 0, 0, 0];
        let k1 = [1u32, 0, 0, 0, 0];
        let kg = [9u32, 9, 9, 9, 9];
        let kroot = [u32::MAX; 5];
        let lod = vec![k0, k1, kg];
        let par = vec![kg, kg, kroot];
        let ch = cluster_finer_group(&lod, &par);
        assert_eq!(ch[0], Vec::<usize>::new()); // leaf: no finer group
        assert_eq!(ch[1], Vec::<usize>::new());
        assert_eq!(ch[2], vec![0, 1]); // root's finer group = both leaves (the group)
    }

    #[test]
    fn finer_group_links_real_dag_cover_all_non_roots() {
        let (pos, idx) = uv_sphere(24, 16);
        let dag = build_cluster_dag(&pos, &idx, &DagOptions::default());
        let key = |e: f32, c: [f32; 3], r: f32| {
            [
                e.to_bits(),
                c[0].to_bits(),
                c[1].to_bits(),
                c[2].to_bits(),
                r.to_bits(),
            ]
        };
        let lod: Vec<_> = dag
            .clusters
            .iter()
            .map(|c| key(c.lod_error, c.lod_bounds_center, c.lod_bounds_radius))
            .collect();
        let par: Vec<_> = dag
            .clusters
            .iter()
            .map(|c| {
                key(
                    c.parent_error,
                    c.parent_bounds_center,
                    c.parent_bounds_radius,
                )
            })
            .collect();
        let ch = cluster_finer_group(&lod, &par);
        assert!(
            ch.iter().any(|g| !g.is_empty()),
            "DAG must have refinement groups"
        );
        // Every non-root cluster is the finer-child of >=1 cluster (valid inverse).
        let mut is_child = vec![false; dag.clusters.len()];
        for g in &ch {
            for &c in g {
                is_child[c] = true;
            }
        }
        for (i, c) in dag.clusters.iter().enumerate() {
            if c.parent_error < ROOT_PARENT_ERROR {
                assert!(
                    is_child[i],
                    "non-root cluster {i} must be in some finer-group"
                );
            }
        }
    }

    #[test]
    fn slot_geometry_packs_padded_slots_and_preserves_triangles() {
        // 2 clusters (1 tri each), full residency, tiny PAGE_VERTS=4 (ic=3 + 1 pad).
        let spans = [(0u32, 3u32), (3, 3)]; // (first_index, index_count) per cluster
        let m_indices = vec![10u32, 11, 12, 20, 21, 22];
        let resident = vec![0i32, 1];
        let (slot_indices, source_indices) = build_slot_geometry(&spans, &m_indices, &resident, 4);
        // slot 0 = C0 verts + pad(first=10); slot 1 = C1 verts + pad(first=20).
        assert_eq!(slot_indices, vec![10, 11, 12, 10, 20, 21, 22, 20]);
        // source is SLOT-ALIGNED now (slot s owns [s*4, s*4+4)), slot-relative values;
        // the per-slot pad entry (k>=ic) points at the slot base (never read by the cut).
        assert_eq!(source_indices, vec![0, 1, 2, 0, 4, 5, 6, 4]);
        // Round-trip: each page's source span is [slot*page_verts, +index_count) (the
        // loader sets page.first_index = slot*page_verts); mapped through slot_indices
        // it reconstructs the cluster's ORIGINAL position indices.
        for (c, &(f, ic)) in spans.iter().enumerate() {
            let (f, ic) = (f as usize, ic as usize);
            let slot_first = resident[c] as usize * 4;
            let drawn: Vec<u32> = source_indices[slot_first..slot_first + ic]
                .iter()
                .map(|&s| slot_indices[s as usize])
                .collect();
            assert_eq!(drawn, m_indices[f..f + ic].to_vec());
        }
    }

    #[test]
    fn page_pool_assigns_one_slot_per_resident_cluster() {
        // 5 clusters, 3 resident (ids 0,2,4), ample pool → each gets a distinct
        // slot in order; non-resident stay -1; no overflow.
        let plan = plan_page_pool(5, &[0, 2, 4], 8);
        assert_eq!(plan.resident, vec![0, -1, 1, -1, 2]);
        assert_eq!(plan.slots_used, 3);
        assert_eq!(plan.overflow, 0);
    }

    #[test]
    fn page_pool_overflows_past_capacity() {
        // 4 residents but only 2 slots → first 2 get slots, rest overflow.
        let plan = plan_page_pool(4, &[0, 1, 2, 3], 2);
        assert_eq!(plan.slots_used, 2);
        assert_eq!(plan.overflow, 2);
        assert_eq!(plan.resident, vec![0, 1, -1, -1]);
    }

    #[test]
    fn page_pool_ignores_out_of_range_ids() {
        // Defensive: an id >= cluster_count is skipped, not panicking / OOB.
        let plan = plan_page_pool(2, &[0, 9, 1], 8);
        assert_eq!(plan.resident, vec![0, 1]);
        assert_eq!(plan.slots_used, 2);
        assert_eq!(plan.overflow, 0);
    }

    // --- North-star gap markers (docs/nanite-lod.md) ---
    // These #[ignore]d tests keep the unmet A2/A3/A6 claims visible in `cargo test`
    // (run with `--ignored` to see them fail). Replace each with a real assertion
    // as the behavior lands; delete when docs/nanite-lod.md is fully met.

    /// A2 — dynamic camera-driven streaming residency. VERIFIED ON-DEVICE (iter 38,
    /// `?vg&paging`, browser un-frozen): a genuine multi-million-triangle asset
    /// (1,081,344-tri source → 2,393,468-tri DAG / 51,753 clusters) pages through the
    /// player cluster path with the render mesh M **CAPPED to 29,850 tris** (budget
    /// 30,000) in a **bounded ~83 MB page pool** (3,862 slots); the per-frame
    /// stream/evict cut is camera-driven and crack-free (watertight): far desired=509
    /// draw=4,908 tris → zoom-IN desired=1,260 draw=14,650 (rises) → zoom-OUT
    /// desired=381 draw=3,860 (falls), with NO per-frame heap allocations (iter 36).
    /// See docs/nanite-lod.md.
    ///
    /// This asserts the CPU invariant underpinning the bounded-VRAM claim: the
    /// resident render mesh M's triangle count is capped by the residency BUDGET,
    /// **independent of source size** — a much larger source DAG yields the same
    /// capped M (so VRAM tracks the budget, not the asset).
    #[test]
    fn a2_residency_is_bounded_by_budget_not_source() {
        let budget = 2_000usize;
        let m_tris = |long: usize, lat: usize| -> (usize, usize) {
            let (pos, indices) = uv_sphere(long, lat);
            let dag = build_cluster_dag(&pos, &indices, &DagOptions::default());
            let cm = ClusterMesh::from_dag(&dag, pos, vec![], vec![], vec![]);
            let full = cm.indices.len() / 3;
            let (_pages, m_indices, _ids) = select_resident_clusters(&cm, budget);
            (full, m_indices.len() / 3)
        };
        let (full_small, m_small) = m_tris(48, 32);
        let (full_big, m_big) = m_tris(96, 64); // ~4x the source DAG
        assert!(
            full_big > full_small * 2,
            "test setup: big DAG ({full_big}) should dwarf small ({full_small})"
        );
        // The capped resident M stays within the budget for BOTH — VRAM is bounded by
        // the budget, not the source. (Soft budget: a complete antichain may undershoot
        // but must never exceed.)
        assert!(
            m_small <= budget,
            "small source M={m_small} exceeded budget {budget}"
        );
        assert!(
            m_big <= budget,
            "big source M={m_big} exceeded budget {budget}"
        );
        // The 4x-larger source does NOT inflate the resident set beyond the budget.
        assert!(
            m_big <= budget && m_small <= budget,
            "residency must track the budget, not source size (m_small={m_small}, m_big={m_big}, budget={budget})"
        );
    }

    /// A3 — the drawn cut is bounded by the screen-space error budget, NOT the source
    /// triangle count. Adding FINER levels below the cut (more source detail) leaves
    /// the selected antichain unchanged at a fixed budget, so the cut size stays flat
    /// as source scales. (On-device confirmation, iter 30, fixed camera @ dist 4, 1px:
    /// source 142,456 tris → drawn 1700; source 583,768 tris [4.1×] → drawn 1696 — the
    /// drawn cut is flat while source grows 4×.)
    #[test]
    fn a3_cut_bounded_by_screen_not_source() {
        use awsm_renderer::cluster_lod::{select_cut, ClusterPage};
        let page = |lod: f32, parent: f32, first_index: u32| ClusterPage {
            center: [0.0; 3],
            radius: 1.0,
            lod_error: lod,
            parent_error: parent,
            lod_bounds_center: [0.0; 3],
            lod_bounds_radius: 1.0,
            parent_bounds_center: [0.0; 3],
            parent_bounds_radius: 1.0,
            first_index,
            index_count: 384,
        };
        // "Coarse" surface = a complete antichain of 4 regions at error interval
        // [1, ∞). At a budget threshold T=1.5 every region's coarse cluster is the
        // one selected ⇒ cut = 4.
        let coarse: Vec<ClusterPage> = (0..4).map(|r| page(1.0, f32::INFINITY, r * 384)).collect();
        let mut cut = Vec::new();
        select_cut(&coarse, 1.5, &mut cut);
        assert_eq!(cut.len(), 4, "coarse DAG: 4 regions selected at T=1.5");

        // "Refined" = the SAME 4 regions PLUS finer children under each (lots more
        // SOURCE: 4 children/region in [0,1), then 4 grandchildren/child in [0,0.5)).
        // 4 coarse + 16 children + 64 grandchildren = 84 clusters (21× the source).
        let mut refined = coarse.clone();
        for r in 0..4u32 {
            for c in 0..4u32 {
                refined.push(page(0.0, 1.0, 10_000 + r * 100 + c)); // child: [0,1)
                for g in 0..4u32 {
                    refined.push(page(0.0, 0.5, 50_000 + r * 1000 + c * 10 + g));
                    // [0,0.5)
                }
            }
        }
        assert_eq!(refined.len(), 84, "refined DAG has 21× the source clusters");

        // At the SAME budget T=1.5, the finer levels (intervals below 1.5) are NOT
        // selected — only the 4 coarse clusters are. The cut is identical: bounded by
        // the budget (screen-space error), invariant to the source size.
        select_cut(&refined, 1.5, &mut cut);
        assert_eq!(
            cut.len(),
            4,
            "refined DAG: cut stays 4 at T=1.5 despite 21× the source — bounded by \
             screen-space error budget, not source size"
        );
    }

    /// A6 — the multi-million-triangle benchmark TABLE is recorded. VERIFIED + committed
    /// (iter 39): `docs/nanite-lod-benchmark.md` records, on a genuine 1,081,344-tri
    /// source (2,393,468-tri DAG / 51,753 clusters) through the player cluster path:
    /// bounded VRAM (~83 MB pool, M capped to 29,850 tris) and bounded draw (cut 4,908–14,835
    /// tris = 0.2–0.6% of the DAG, scaling with viewport height + camera, independent of
    /// width/source), plus per-pass CPU-encode timings + frame time (8.3 ms ≈ 120 FPS,
    /// vsync-capped). This test pins the doc to those verified figures so the table can't
    /// silently drift or vanish.
    #[test]
    fn a6_benchmark_table_recorded() {
        const BENCH: &str = include_str!("../../../../docs/nanite-lod-benchmark.md");
        for needle in [
            "1,081,344", // source tris
            "2,393,468", // full DAG tris
            "29,850",    // resident render mesh M (capped)
            "83 MB",     // bounded page pool VRAM
            "14,835",    // near-camera drawn cut @ 1392×746
            "4,908",     // far-camera drawn cut
        ] {
            assert!(
                BENCH.contains(needle),
                "A6 benchmark table missing verified figure `{needle}` — \
                 docs/nanite-lod-benchmark.md must record the multi-M-tri bench"
            );
        }
    }
    use std::collections::HashMap;

    // --- A1 capped crack-free helpers (mirror the bake's dag.rs test harness) ---

    /// UV sphere: geometrically closed, non-watertight by index (seam + poles).
    fn uv_sphere(long: usize, lat: usize) -> (Vec<[f32; 3]>, Vec<u32>) {
        use std::f32::consts::PI;
        const TAU: f32 = 2.0 * PI;
        let mut pos = Vec::new();
        for la in 0..=lat {
            let theta = (la as f32 / lat as f32) * PI;
            let (st, ct) = (theta.sin(), theta.cos());
            for lo in 0..=long {
                let phi = (lo as f32 / long as f32) * TAU;
                pos.push([st * phi.cos(), ct, st * phi.sin()]);
            }
        }
        let stride = long + 1;
        let mut idx = Vec::new();
        for la in 0..lat {
            for lo in 0..long {
                let a = (la * stride + lo) as u32;
                let b = (la * stride + lo + 1) as u32;
                let c = ((la + 1) * stride + lo + 1) as u32;
                let d = ((la + 1) * stride + lo) as u32;
                idx.extend_from_slice(&[a, b, c, a, c, d]);
            }
        }
        (pos, idx)
    }

    fn weld_ids(pos: &[[f32; 3]], eps: f32) -> Vec<u32> {
        let mut map: HashMap<(i64, i64, i64), u32> = HashMap::new();
        let q = |v: f32| (v / eps).round() as i64;
        pos.iter()
            .map(|p| {
                let key = (q(p[0]), q(p[1]), q(p[2]));
                let n = map.len() as u32;
                *map.entry(key).or_insert(n)
            })
            .collect()
    }

    /// Hole/boundary edges (used by exactly one welded, non-degenerate triangle).
    fn boundary_edges(tris: &[[u32; 3]], weld: &[u32]) -> usize {
        let mut edges: HashMap<(u32, u32), u32> = HashMap::new();
        for t in tris {
            let w = [
                weld[t[0] as usize],
                weld[t[1] as usize],
                weld[t[2] as usize],
            ];
            if w[0] == w[1] || w[1] == w[2] || w[0] == w[2] {
                continue;
            }
            for (a, b) in [(w[0], w[1]), (w[1], w[2]), (w[2], w[0])] {
                let key = if a < b { (a, b) } else { (b, a) };
                *edges.entry(key).or_insert(0) += 1;
            }
        }
        edges.values().filter(|&&c| c == 1).count()
    }

    /// A1 (north-star, CAPPED clause): under `?streambudget` the cluster cut must
    /// still be crack-free. The capped resident set, cut at the closest camera
    /// (threshold 0 ⇒ all resident leaves), must weld to a closed surface — a torn
    /// "partial frontier" (a hard tri budget cutting mid-level) shows up as open
    /// edges. Was the on-device subdivided-sphere holes under `?streambudget=8000`;
    /// fixed by selecting a COMPLETE antichain (`select_resident_clusters`, soft
    /// budget) instead of a hard-tri partial frontier.
    #[test]
    fn capped_resident_cut_is_crack_free() {
        let (pos, indices) = uv_sphere(48, 32);
        let weld = weld_ids(&pos, 1e-3);
        let dag = build_cluster_dag(&pos, &indices, &DagOptions::default());
        let cm = ClusterMesh::from_dag(&dag, pos, vec![], vec![], vec![]);

        let full_tris = cm.indices.len() / 3;
        let budget = full_tris / 4; // aggressive: forces a partial frontier today
        let (pages, m_indices, _ids) = select_resident_clusters(&cm, budget);

        // Simulate the runtime per-cluster cut at the closest camera (threshold 0):
        // select resident pages whose error interval contains 0 (the clamped
        // leaves). Reconstruct their triangles and check for holes.
        let mut tris: Vec<[u32; 3]> = Vec::new();
        for p in &pages {
            if p.lod_error <= 0.0 && 0.0 < p.parent_error {
                let s = p.first_index as usize;
                let e = s + p.index_count as usize;
                for c in m_indices[s..e].chunks_exact(3) {
                    tris.push([c[0], c[1], c[2]]);
                }
            }
        }
        assert!(!tris.is_empty(), "capped cut selected no leaves");
        let holes = boundary_edges(&tris, &weld);
        assert_eq!(
            holes, 0,
            "capped cluster cut (budget {budget} tris) tore {holes} hole edge(s) — \
             partial-frontier seam; resident set must be a complete antichain (A1 capped)"
        );
    }

    fn page(lod_error: f32, parent_error: f32, first_index: u32, index_count: u32) -> ClusterPage {
        ClusterPage {
            center: [0.0; 3],
            radius: 1.0,
            lod_error,
            parent_error,
            lod_bounds_center: [0.0; 3],
            lod_bounds_radius: 1.0,
            parent_bounds_center: [0.0; 3],
            parent_bounds_radius: 1.0,
            first_index,
            index_count,
        }
    }

    /// A 2-level DAG: two finest clusters (error 0) simplify into one root
    /// (error 5). indices are slice markers so the remap is checkable.
    fn fixture() -> ClusterMesh {
        ClusterMesh {
            positions: vec![[0.0; 3]; 9],
            normals: vec![],
            uvs: vec![],
            colors: vec![],
            indices: vec![0, 1, 2, 3, 4, 5, 6, 7, 8],
            clusters: vec![
                page(0.0, 5.0, 0, 3),      // C0 leaf
                page(0.0, 5.0, 3, 3),      // C1 leaf
                page(5.0, f32::MAX, 6, 3), // C2 root
            ],
        }
    }

    #[test]
    fn under_budget_is_verbatim_passthrough() {
        let cm = fixture();
        let (pages, m_indices, _ids) = select_resident_clusters(&cm, 100);
        // Every cluster, indices verbatim, nothing remapped or clamped — must be
        // byte-identical to the non-streaming path.
        assert_eq!(m_indices, cm.indices);
        assert_eq!(pages.len(), 3);
        for (p, c) in pages.iter().zip(cm.clusters.iter()) {
            assert_eq!(p.first_index, c.first_index);
            assert_eq!(p.lod_error, c.lod_error);
        }
    }

    #[test]
    fn cap_to_one_tri_keeps_root_and_clamps_it() {
        let cm = fixture();
        // Budget 1 tri: coarsest-first keeps only the root C2; with no resident
        // child its lod_error clamps to 0 so close-up stays covered.
        let (pages, m_indices, _ids) = select_resident_clusters(&cm, 1);
        assert_eq!(pages.len(), 1);
        assert_eq!(m_indices, vec![6, 7, 8]); // C2's slice, remapped to front
        assert_eq!(pages[0].first_index, 0);
        assert_eq!(pages[0].lod_error, 0.0); // leaf clamp
        assert_eq!(pages[0].parent_error, f32::MAX);
    }

    #[test]
    fn cap_to_two_tris_selects_the_complete_leaf_antichain() {
        let cm = fixture();
        // Budget 2 tris: the finest complete antichain that fits is BOTH leaves
        // {C0, C1} (2 tris) — NOT the old partial {root C2 + one leaf C0}, which
        // tore (C1's region covered only by the coarse root while C0 was fine).
        // The whole leaf level tiles the surface ⇒ crack-free; budget honoured.
        let (pages, m_indices, _ids) = select_resident_clusters(&cm, 2);
        assert_eq!(pages.len(), 2);
        assert_eq!(m_indices, vec![0, 1, 2, 3, 4, 5]); // C0 then C1, remapped
        for p in &pages {
            assert_eq!(p.lod_error, 0.0); // always-drawn frontier
            assert_eq!(p.parent_error, f32::MAX);
        }
    }
}
