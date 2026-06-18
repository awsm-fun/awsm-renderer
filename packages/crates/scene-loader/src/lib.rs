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
//! **`ParticleEmitter`** materializes into a ready-to-drive instanced billboard
//! (**Design A: loader sets up, game ticks**): the loader builds the emissive quad
//! + GPU instancing at `max_alive` capacity and returns an
//! [`EmitterHandle`](particles::EmitterHandle) in [`NodeHandles::emitter`]; it does
//! NOT simulate. The game ticks an [`awsm_particles::Simulator`] each frame and
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
//! use awsm_scene_loader::{load_scene_for_player, set_node_visible};
//! use awsm_renderer::{AwsmRenderer, transforms::Transform};
//! use awsm_scene::{Scene, NodeId, Trs};
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
use awsm_renderer::materials::unlit::UnlitMaterial;
use awsm_renderer::materials::{Material, MaterialAlphaMode, MaterialKey};
use awsm_renderer::meshes::MeshKey;
use awsm_renderer::raw_mesh::RawMeshData;
use awsm_renderer::render_passes::lines::LineKey;
use awsm_renderer::transforms::{Transform, TransformKey};
use awsm_renderer::{AwsmRenderer, LoadPhase};
use awsm_renderer_core::texture::mipmap::MipmapTextureKind;
use awsm_renderer_gltf::loader::GltfLoader;
use awsm_renderer_gltf::{AwsmRendererGltfExt, GltfMaterialSource, PopulateGltfOpts};
use awsm_scene::{
    mesh_glb_filename, AssetId, AssetSource, CameraConfig, CurveDef, DecalConfig, EditorNode,
    InstancesAlongCurveDef, LightConfig, LineDef, MaterialInstance, MaterialShading, NodeId,
    NodeKind, RuntimeMesh, Scene, SpriteDef, Trs, ASSETS_DIR,
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
/// `Camera` / `Line` / `Decal` nodes *inside* a prefab are **also** re-created per
/// instance now (A.3): each gets a fresh per-instance key (lines/decals re-baked
/// into the instance's world transform; the decal texture index is resolved at
/// capture). Still **not** replayed: `InstancesAlongCurve` (its own
/// instancing-on-a-source-mesh shape) and `ParticleEmitter` (the emitter handle
/// isn't threaded through `PrefabInstance` yet) — both contribute only their
/// transform (documented follow-ons). A **nested** prefab child is captured as its
/// own template in [`LoadedScene::prefabs`] — never inlined into its parent.
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
    /// Mesh / group / curve / particle node — nothing extra to replay (the
    /// transform, and any template meshes, are handled separately).
    None,
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
}

/// Replay a prefab node's non-mesh renderable into a fresh per-instance resource
/// (A.3), recording the produced key onto `handles`. `tk` is the instance node's
/// transform; `world` its composed world matrix (lines/decals are world-space).
///
/// Best-effort: a failed line/decal insert is warned and skipped (the instance's
/// other nodes still materialize). Async pipeline warm-ups the live arms perform
/// are intentionally omitted — `instantiate` is sync and the renderer's normal
/// per-frame drive compiles line/shadow pipelines (or a prior load already did).
fn replay_prefab_node(
    renderer: &mut AwsmRenderer,
    replay: &PrefabReplay,
    tk: TransformKey,
    world: Mat4,
    handles: &mut NodeHandles,
) {
    match replay {
        PrefabReplay::None => {}
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
            let ck = renderer.cameras.insert(camera::camera_params_from_config(cfg));
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
            let colors: Vec<Vec4> = def.points.iter().map(|p| Vec4::from_array(p.color)).collect();
            match renderer.add_line_strip(&positions, &colors, def.width_px, def.depth_test_always) {
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
            let mut mesh_keys = Vec::with_capacity(pn.template_meshes.len());
            for &template_key in &pn.template_meshes {
                let new_key = renderer.duplicate_mesh_with_transform(template_key, tk)?;
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

        let root = root_tk.ok_or_else(|| anyhow!("prefab template has no root node"))?;
        Ok(PrefabInstance { root, nodes })
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
    /// replayed [`LightKey`] / [`LineKey`] / [`DecalKey`] (A.3), then every per-node
    /// [`TransformKey`] (meshes/lights/lines/decals first, transforms last). The
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
        }
        for handles in self.nodes.values() {
            renderer.transforms.remove(handles.transform);
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
/// `Group`s to its descendants) has its meshes loaded hidden and its lines/decals
/// skipped; its light, if any, is still inserted (a documented minor gap — a
/// hidden node's light still emits — toggled later via [`set_node_visible`] only
/// for meshes).
pub async fn load_scene_for_player(
    renderer: &mut AwsmRenderer,
    scene: &Scene,
    assets: &impl SceneAssets,
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
    // Meshes of a hidden node are `set_mesh_hidden(true)`; lines/decals are SKIPPED
    // entirely (cleaner than minting then hiding — the renderer has no per-line/
    // -decal hide toggle). Lights of a hidden node are still inserted (known minor
    // gap: a hidden node's light still emits — see `populate_awsm_scene` docs).
    let effective_visible = parent_effective_visible && node.visible;
    // Prefab root: capture the whole subtree as a hidden, reusable template and
    // return BEFORE inserting any transform — so neither this node nor its
    // descendants enter the static world (`loaded.nodes` / `maps`). Instances are
    // minted later, on demand, via `PrefabTemplate::instantiate`.
    if node.prefab {
        let tmpl = capture_prefab(
            renderer,
            scene,
            node,
            None,
            assets,
            &maps.node_materials,
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
            // Skip a hidden node's line entirely — the renderer has no per-line
            // hide toggle, so not creating it is the cleanest way to honor
            // `visible == false` (documented on `populate_awsm_scene`).
            if effective_visible {
                materialize_line(renderer, def, node.id, node_world, maps).await?;
            }
        }
        NodeKind::Sprite(def) => {
            materialize_sprite(renderer, assets, def, node.id, tk, maps, loaded).await?;
        }
        NodeKind::Decal(cfg) => {
            // Skip a hidden node's decal entirely (no per-decal hide toggle).
            if effective_visible {
                materialize_decal(renderer, assets, cfg, node.id, node_world, maps).await?;
            }
        }
        NodeKind::InstancesAlongCurve(def) => {
            materialize_instances_along_curve(renderer, scene, def, maps)?;
        }
        // A bare `Curve` is data-only: it emits no renderer node. It's consumed
        // by `InstancesAlongCurve` (and sweeps at bake time), which look the curve
        // up directly from `scene` by `NodeId` — no per-node renderer resource.
        NodeKind::Curve(_) => {}
        // A.1 (Design A): the loader builds the emitter's instanced billboard
        // (ready to drive) and hands back an `EmitterHandle`; it does NOT simulate.
        // The game ticks an `awsm_particles::Simulator` each frame and pushes the
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
/// **Non-mesh replay (A.3):** `Light` / `Camera` / `Line` / `Decal` nodes capture
/// a [`PrefabReplay`] alongside their transform (the decal texture resolved to a
/// pool index here, while assets are available), so [`PrefabTemplate::instantiate`]
/// re-creates each as a fresh per-instance resource. `InstancesAlongCurve` /
/// `ParticleEmitter` inside a prefab still contribute only their transform (see
/// [`PrefabTemplate`]).
#[allow(clippy::too_many_arguments)]
async fn capture_prefab(
    renderer: &mut AwsmRenderer,
    scene: &Scene,
    node: &EditorNode,
    parent: Option<NodeId>,
    assets: &impl SceneAssets,
    node_materials: &HashMap<NodeId, MaterialKey>,
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
                scene,
                n,
                None,
                assets,
                node_materials,
                placeholder,
                loaded,
            ))
            .await?;
            loaded.prefabs.insert(n.id, tmpl);
            continue;
        }
        let mat = node_materials.get(&n.id).copied().unwrap_or(placeholder);
        // Build this node's meshes (hidden) under the scratch transform; non-mesh
        // kinds yield an empty vec (their transform is still recorded).
        let template_meshes =
            build_node_meshes(renderer, scene, n, scratch, mat, assets, true).await?;
        // A.3: capture the non-mesh renderable to replay per instance. The decal
        // texture is resolved NOW (assets are available here; `instantiate` is
        // asset-free). Light/Camera/Line carry their authored config verbatim.
        let replay = match &n.kind {
            NodeKind::Light(cfg) => PrefabReplay::Light(cfg.clone()),
            NodeKind::Camera(cfg) => PrefabReplay::Camera(cfg.clone()),
            NodeKind::Line(def) => PrefabReplay::Line(def.clone()),
            NodeKind::Decal(cfg) => PrefabReplay::Decal {
                texture_index: resolve_decal_texture_index(renderer, assets, cfg).await,
                alpha: cfg.alpha,
            },
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
/// `SkinnedMesh` (rig glb), and `Sprite`; every other [`NodeKind`] yields no mesh
/// (an empty vec). When `hidden` is set, each produced mesh is hidden immediately
/// (the prefab-template case) — the caller's instances un-hide their duplicates.
///
/// Does NOT touch `maps` / `loaded` / progress: it returns the keys so the caller
/// records them where they belong (live arms push into `maps`/`loaded`; the
/// prefab path stores them on the template). Sprites build their own material
/// here (sprites aren't in Phase-1 `collect_renderables`), so `mat` is ignored
/// for the `Sprite` arm.
async fn build_node_meshes(
    renderer: &mut AwsmRenderer,
    scene: &Scene,
    node: &EditorNode,
    tk: TransformKey,
    mat: MaterialKey,
    assets: &impl SceneAssets,
    hidden: bool,
) -> Result<Vec<MeshKey>> {
    let mut keys: Vec<MeshKey> = Vec::new();
    match &node.kind {
        NodeKind::Mesh { mesh, .. } => {
            if let Some(entry) = scene.assets.get(mesh.0) {
                match &entry.source {
                    AssetSource::Mesh(RuntimeMesh::Primitive(shape)) => {
                        let md = awsm_meshgen::primitive_mesh(shape);
                        let key = renderer.add_raw_mesh(mesh_data_to_raw(md), tk, mat)?;
                        keys.push(key);
                    }
                    AssetSource::Mesh(RuntimeMesh::Glb) => {
                        let (glb_keys, _) = load_glb_under(
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
            // Self-placing rig glb (rooted at None, like the live arm). For a
            // prefab template the joint binding is omitted (skin-correspondence is
            // a follow-on even outside prefabs); the rig poses at bind pose.
            let (glb_keys, _) =
                load_glb_under(renderer, assets, &mesh_glb_filename(skin.source), None, mat)
                    .await?;
            keys.extend(glb_keys);
        }
        NodeKind::Sprite(def) => {
            let key = build_sprite_mesh(renderer, assets, def, tk).await?;
            keys.push(key);
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
    let key = build_sprite_mesh(renderer, assets, def, tk).await?;
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
    assets: &impl SceneAssets,
    def: &SpriteDef,
    tk: TransformKey,
) -> Result<MeshKey> {
    use awsm_renderer::materials::flipbook::{FlipBookMaterial, FlipBookMode};
    use awsm_renderer::materials::unlit::UnlitMaterial;
    use awsm_renderer::meshes::mesh::BillboardMode as RBillboard;
    use awsm_renderer_core::texture::mipmap::MipmapTextureKind;
    use awsm_scene::{BillboardMode, FlipBookModeDef, SpriteAlphaMode};

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
            texture::load_texture(renderer, assets, t, true, MipmapTextureKind::Albedo).await
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

    let texture_index = resolve_decal_texture_index(renderer, assets, cfg).await;

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
/// samples (`array_index * 64 + layer_index`). `None` (no texture, failed load,
/// or not pooled) → index `0`, matching the editor bridge. Shared by the live
/// [`materialize_decal`] arm and prefab capture ([`capture_prefab`], which must
/// resolve at load time because [`PrefabTemplate::instantiate`] has no assets).
async fn resolve_decal_texture_index(
    renderer: &mut AwsmRenderer,
    assets: &impl SceneAssets,
    cfg: &DecalConfig,
) -> u32 {
    match &cfg.texture {
        Some(t) => {
            match texture::load_texture(renderer, assets, t, true, MipmapTextureKind::Albedo).await {
                Some(mt) => renderer
                    .textures
                    .get_entry(mt.key)
                    .map(|e| {
                        (e.array_index as u32) * DECAL_POOL_LAYERS_PER_ARRAY + e.layer_index as u32
                    })
                    .unwrap_or(0),
                None => 0,
            }
        }
        None => 0,
    }
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
/// our material-source semantics outside the full scene pass.
pub async fn load_glb_under(
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
    build_node_meshes(renderer, scene, node, tk, material, assets, false).await
}

fn trs_to_transform(trs: &Trs) -> Transform {
    Transform {
        translation: Vec3::from_array(trs.translation),
        rotation: Quat::from_array(trs.rotation),
        scale: Vec3::from_array(trs.scale),
    }
}

/// Convert an [`awsm_meshgen::MeshData`] (the procedural/primitive mesh builder's
/// output) into the renderer's [`RawMeshData`] upload struct — positions, normals,
/// UV0, colors, and indices pass through; UV1 is always `None` (meshgen primitives
/// carry a single UV set). Public (R4) so a host can feed a meshgen primitive into
/// `renderer.add_raw_mesh` with the same conversion the loader uses.
pub fn mesh_data_to_raw(md: awsm_meshgen::MeshData) -> RawMeshData {
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
    use awsm_scene::{EditorNode, NodeId, NodeKind};

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
}
