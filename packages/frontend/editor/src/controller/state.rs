use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;

use awsm_web_shared::prelude::{Mutable, MutableVec, Toast};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::spawn_local;

use super::animation::{find_clip, CustomAnimation as CA};
use super::custom_material::{find_material, CustomMaterial as CM};
use super::*;
use crate::engine::scene::{mutate, AssetId, NodeId, NodeKind, Scene};
use crate::error::EditorResult;
use awsm_editor_protocol::{
    AssetEntry, AssetSource as SceneAssetSource, MaterialDef, ModifierStack, ProceduralTextureDef,
    TextureDef,
};
use std::sync::Arc;

thread_local! {
    static CONTROLLER: OnceCell<EditorController> = const { OnceCell::new() };
    /// The cross-tab relay channel. `None` until `init`, or if the browser
    /// lacks `BroadcastChannel` (cross-tab then simply disabled — the editor still
    /// works). Every non-tab-local dispatched command is posted here; other tabs
    /// apply it. `BroadcastChannel` does not deliver to the posting context, so
    /// there is no echo to guard against.
    static SYNC_CHANNEL: RefCell<Option<web_sys::BroadcastChannel>> = const { RefCell::new(None) };
}

thread_local! {
    /// In-process ring buffer of editor notices (toasts) — surfaced over MCP via
    /// `ConsoleLogs` so a driver can see runtime errors otherwise stuck in the
    /// browser. Capped; oldest dropped.
    static CONSOLE_LOG: RefCell<std::collections::VecDeque<(String, String)>> =
        const { RefCell::new(std::collections::VecDeque::new()) };
}

const CONSOLE_LOG_CAP: usize = 200;

/// Append an editor notice to the console-log ring buffer (level + message).
pub(crate) fn record_console_log(level: &str, msg: &str) {
    CONSOLE_LOG.with(|b| {
        let mut b = b.borrow_mut();
        if b.len() >= CONSOLE_LOG_CAP {
            b.pop_front();
        }
        b.push_back((level.to_string(), msg.to_string()));
    });
}

/// Install the controller singleton. Call once at boot, before mounting the UI.
pub fn init() {
    CONTROLLER.with(|c| {
        let _ = c.set(EditorController::new());
    });
    init_cross_tab_sync();
    // Mirror every toast into the console-log ring buffer (MCP `get_console_logs`).
    awsm_web_shared::prelude::set_toast_log_hook(|kind, msg| {
        use awsm_web_shared::prelude::ToastKind;
        let level = match kind {
            ToastKind::Info => "info",
            ToastKind::Warning => "warning",
            ToastKind::Error => "error",
        };
        record_console_log(level, msg);
        // Push the noteworthy notices to the agent (editor → agent channel).
        if matches!(kind, ToastKind::Warning | ToastKind::Error) {
            crate::remote::notify_event(awsm_editor_protocol::EditorEvent {
                kind: "toast".to_string(),
                level: Some(level.to_string()),
                message: Some(msg.to_string()),
                nodes: None,
            });
        }
    });
}

/// Wire the cross-tab relay: a `BroadcastChannel` whose incoming commands
/// are applied through the same `dispatch`/`apply` seam (replay path — no
/// re-broadcast, no undo record). Two tabs on the same project thus stay in
/// lock-step on every clip/track/keyframe/mixer edit + the shared playhead, while
/// each keeps its own camera / selection / mode (`is_tab_local`, not broadcast).
fn init_cross_tab_sync() {
    let bc = match web_sys::BroadcastChannel::new("awsm-editor-sync") {
        Ok(bc) => bc,
        Err(_) => return, // unsupported → cross-tab disabled; editor unaffected
    };
    let on_message =
        Closure::<dyn FnMut(web_sys::MessageEvent)>::new(|e: web_sys::MessageEvent| {
            let Some(json) = e.data().as_string() else {
                return;
            };
            match serde_json::from_str::<EditorCommand>(&json) {
                Ok(cmd) => spawn_local(async move {
                    // Remote replay: straight to `apply` (dispatch would re-broadcast
                    // + record undo). The returned inverse is discarded.
                    let _ = controller().apply_remote(cmd).await;
                }),
                Err(err) => tracing::warn!("cross-tab: undecodable command: {err}"),
            }
        });
    bc.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    on_message.forget(); // handler lives for the app's lifetime
    SYNC_CHANNEL.with(|c| *c.borrow_mut() = Some(bc));
}

/// A cheap clone of the controller singleton (all fields are `Mutable`/`Rc`).
pub fn controller() -> EditorController {
    CONTROLLER.with(|c| c.get().expect("controller not initialized").clone())
}

/// The command/query authority. Clone is cheap — every field is a shared handle.
#[derive(Clone)]
pub struct EditorController {
    /// The live, reactive scene tree (the canonical scene state).
    pub scene: Arc<Scene>,
    /// Ordered selection (last = primary/anchor). Set via `SetSelection`.
    pub selected: Mutable<Vec<NodeId>>,
    /// Read-only **vertex-selection highlight**: `Some((node, indices))` marks
    /// those vertices of that node for a viewport overlay (no geometry edit).
    /// Set via `SetVertexSelection`; `None` (or an empty `indices`) = no
    /// highlight. Transient session-local view state (not undoable / persisted).
    pub vertex_selection: Mutable<Option<(NodeId, Vec<u32>)>>,
    pub mode: Mutable<EditorMode>,
    pub project_name: Mutable<String>,
    pub dirty: Mutable<bool>,
    pub missing_assets: Mutable<Vec<String>>,
    pub can_undo: Mutable<bool>,
    pub can_redo: Mutable<bool>,
    /// Bumps only when a `SetKind` changes a node's **structural** shape (the
    /// shape/shading/projection/light *variant*, not a numeric value). The
    /// inspector rebuilds on this so a discrete toggle (PBR↔Unlit, Persp↔Ortho)
    /// refreshes which rows exist — while a continuous numeric scrub, which
    /// keeps the structure key constant, never tears out the field being dragged.
    pub structure_rev: Mutable<u64>,
    /// Whether the Content Browser bottom drawer is expanded. Pure view state
    /// (not project/undo state), held here so the ribbon toggle, the drawer, and
    /// the workspace layout share one source of truth.
    pub content_browser_open: Mutable<bool>,
    /// Which camera the viewport renders through. `None` = the free built-in
    /// editor camera (orbit/pan/zoom). `Some(node)` = a scene `Camera` node — the
    /// view is locked to that camera's transform + config and orbit/pan/zoom do
    /// nothing. This is *per-window* view state (not a synced command), so two
    /// windows can look through different cameras at the same scene.
    pub active_camera: Mutable<Option<NodeId>>,
    /// The asset selected in the Content Browser, if any. When `Some`, the right
    /// rail shows the Asset Inspector instead of the node inspector. Set via the
    /// transient `SetAssetSelection` command.
    pub asset_selection: Mutable<Option<AssetId>>,
    /// The custom WGSL materials authored in the Material-mode Studio.
    /// Reactive — the Studio edits their bodies/slots live.
    pub custom_materials: MutableVec<Arc<CM>>,
    /// The material the Studio is currently editing.
    pub current_material: Mutable<Option<AssetId>>,
    /// The animation clips authored in Animation mode (mirrors `custom_materials`).
    /// Reactive — the studio edits their tracks/keys live.
    pub custom_animations: MutableVec<Arc<CA>>,
    /// The clip Animation mode is currently editing/playing.
    pub current_clip: Mutable<Option<AssetId>>,
    /// The transport playhead, in **seconds** (shared across synced tabs).
    pub playhead: Mutable<f64>,
    /// Whether the transport is playing.
    pub playing: Mutable<bool>,
    /// The display frame rate (frames⇄seconds in the ruler).
    pub anim_fps: Mutable<u32>,
    /// Solo-subtree focus: only tracks under this node advance.
    pub anim_solo_root: Mutable<Option<NodeId>>,
    /// The selected timeline element (track / keyframe).
    pub anim_selection: Mutable<Option<AnimSel>>,
    /// The NLA mixer document (layers / strips / masks / weights, by clip id).
    pub anim_mixer: Mutable<MixerDoc>,
    /// Monotonic revision bumped by `apply` whenever a command
    /// [`EditorCommand::affects_animation`] — the single signal the bridge
    /// observes to debounced-re-lower the renderer. Routing every lowering-
    /// affecting edit through ONE counter (rather than per-field signal
    /// observers) guarantees no edit silently skips a re-lower.
    pub anim_revision: Mutable<u32>,
    /// Monotonic revision bumped by `apply` whenever a command
    /// [`EditorCommand::affects_mesh`] — the single signal the `mesh_sync` bridge
    /// observes to re-materialize captured-mesh geometry that changed without a
    /// node-kind change (`SetMeshData`). Mirrors [`Self::anim_revision`].
    pub mesh_revision: Mutable<u32>,
    /// Which timeline editor the dock shows (Dope / Curves / Mixer).
    pub anim_view: Mutable<AnimView>,
    /// Whether the ⌘K command palette is open (view state).
    pub cmdk_open: Mutable<bool>,
    /// Editor (view-only) settings — viewport toggles, units, etc. Not saved
    /// into the project file.
    pub settings: Settings,
    /// Whether the Settings drawer is open.
    pub settings_open: Mutable<bool>,
    /// Inverses of applied commands, newest last (the undo log).
    undo: Rc<RefCell<Vec<EditorCommand>>>,
    /// Inverses popped by undo, re-appliable by redo.
    redo: Rc<RefCell<Vec<EditorCommand>>>,
    /// Count of in-flight (or debounce-scheduled) material compiles. The
    /// `WaitRenderSettled` query waits for this to reach zero — plus the
    /// renderer's own pipeline scheduler to drain and a frame to present — so an
    /// MCP `set_material_wgsl → screenshot` doesn't race the ~400ms recompile.
    pub(crate) compile_pending: Rc<Cell<u32>>,
}

/// Editor view-only settings (viewport toggles + units). Reactive; each field is
/// a shared `Mutable`. Not persisted into the project file.
#[derive(Clone)]
pub struct Settings {
    pub grid: Mutable<bool>,
    pub gizmo: Mutable<bool>,
    /// Show the pickable light-icon HUD markers (one per light node).
    pub light_gizmos: Mutable<bool>,
    /// Show the skeleton bone-line overlay on skinned rigs.
    pub skeleton_viz: Mutable<bool>,
    pub msaa: Mutable<bool>,
    pub heatmap: Mutable<bool>,
    pub snap: Mutable<bool>,
    pub units: Mutable<String>,
    /// Built-in editor view camera projection: `true` = orthographic, `false` =
    /// perspective. Kept authoritative by the `SetCameraProjection` handler, so the
    /// viewport toggle/keyboard shortcut and any MCP-driven change stay in sync.
    pub editor_ortho: Mutable<bool>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            grid: Mutable::new(true),
            gizmo: Mutable::new(true),
            light_gizmos: Mutable::new(true),
            skeleton_viz: Mutable::new(true),
            msaa: Mutable::new(true),
            heatmap: Mutable::new(false),
            snap: Mutable::new(false),
            units: Mutable::new("meters".to_string()),
            editor_ortho: Mutable::new(false),
        }
    }
}

impl EditorController {
    fn new() -> Self {
        Self {
            scene: Scene::new(),
            selected: Mutable::new(Vec::new()),
            vertex_selection: Mutable::new(None),
            mode: Mutable::new(EditorMode::default()),
            project_name: Mutable::new("untitled.awsm".to_string()),
            dirty: Mutable::new(false),
            missing_assets: Mutable::new(Vec::new()),
            can_undo: Mutable::new(false),
            can_redo: Mutable::new(false),
            structure_rev: Mutable::new(0),
            content_browser_open: Mutable::new(false),
            active_camera: Mutable::new(None),
            asset_selection: Mutable::new(None),
            custom_materials: MutableVec::new(),
            current_material: Mutable::new(None),
            custom_animations: MutableVec::new(),
            current_clip: Mutable::new(None),
            playhead: Mutable::new(0.0),
            playing: Mutable::new(false),
            anim_fps: Mutable::new(30),
            anim_solo_root: Mutable::new(None),
            anim_selection: Mutable::new(None),
            anim_mixer: Mutable::new(MixerDoc::default()),
            anim_revision: Mutable::new(0),
            mesh_revision: Mutable::new(0),
            anim_view: Mutable::new(AnimView::default()),
            cmdk_open: Mutable::new(false),
            settings: Settings::default(),
            settings_open: Mutable::new(false),
            undo: Rc::new(RefCell::new(Vec::new())),
            redo: Rc::new(RefCell::new(Vec::new())),
            compile_pending: Rc::new(Cell::new(0)),
        }
    }

    /// The single entry point. UI handlers build a command and dispatch it here;
    /// async because some commands await the renderer / FS / network.
    pub async fn dispatch(&self, cmd: EditorCommand) -> EditorResult<()> {
        // Every command entering through `dispatch` is a *direct user input*
        // (undo/redo replay goes straight to `apply`, bypassing this). Broadcast
        // it for future multi-window / collaboration sync — see `broadcast`.
        self.broadcast(&cmd);
        let transient = cmd.is_transient();
        // Coalesce consecutive continuous edits on the same node (transform
        // drag-scrub, name typing) into one undo step.
        let key = coalesce_key(&cmd);
        let inverse = self.apply(cmd).await?;
        if !transient {
            if let Some(inv) = inverse {
                let skip = key.is_some() && self.undo.borrow().last().and_then(coalesce_key) == key;
                if !skip {
                    self.undo.borrow_mut().push(inv);
                    self.redo.borrow_mut().clear();
                    self.refresh_history_signals();
                }
            }
            self.dirty.set_neq(true);
        }
        Ok(())
    }

    /// Broadcast a direct-input command. Today this only logs `broadcasting
    /// <command>` (the command serialized as JSON — the exact payload a peer
    /// would replay), which is handy for tracing undo/redo and input flow. Later
    /// this will feed a transport so other windows / collaborators apply the same
    /// command — e.g. driving a scene camera from one window's built-in view and
    /// seeing it move in another. Undo/redo deliberately don't broadcast (they
    /// call `apply` directly), so a replay isn't mistaken for a fresh edit.
    fn broadcast(&self, cmd: &EditorCommand) {
        // Per-tab view-local commands (camera / selection / mode) never cross-tab
        // broadcast — a second window keeps its own view.
        if cmd.is_tab_local() {
            return;
        }
        let payload = serde_json::to_string(cmd).unwrap_or_else(|_| format!("{cmd:?}"));
        tracing::trace!("broadcasting {payload}");
        SYNC_CHANNEL.with(|c| {
            if let Some(bc) = c.borrow().as_ref() {
                let _ = bc.post_message(&JsValue::from_str(&payload));
            }
        });
    }

    /// Apply a command that arrived from ANOTHER tab via the cross-tab relay.
    /// Goes straight to `apply` — the replay path: it does NOT re-broadcast
    /// (only `dispatch` broadcasts) and does NOT record undo (the inverse is
    /// discarded), so a relayed edit isn't mistaken for a fresh local one.
    async fn apply_remote(&self, cmd: EditorCommand) -> EditorResult<()> {
        let _ = self.apply(cmd).await?;
        Ok(())
    }

    /// Apply a command and, if it changes anything the renderer must re-lower
    /// for animation, bump [`Self::anim_revision`] — the single signal the bridge
    /// debounced-observes. This is the ONE chokepoint every path (`dispatch`,
    /// `apply_remote`, undo, redo) funnels through, so no edit can skip the
    /// re-lower (the stale-channel bug). The actual effect lives in `apply_inner`.
    async fn apply(&self, cmd: EditorCommand) -> EditorResult<Option<EditorCommand>> {
        // A `Batch` applies its sub-commands in order (each a leaf — batches don't
        // nest) and returns a `Batch` of their inverses, reversed, so undo replays
        // them back-to-front as one step. Handled here (not `apply_inner`) so the
        // async fn doesn't recurse into itself.
        if let EditorCommand::Batch(cmds) = cmd {
            let mut inverses = Vec::new();
            for c in cmds {
                let touches_anim = c.affects_animation();
                let touches_mesh = c.affects_mesh();
                if let Some(inv) = Box::pin(self.apply_inner(c)).await? {
                    inverses.push(inv);
                }
                if touches_anim {
                    self.anim_revision.replace_with(|v| v.wrapping_add(1));
                }
                if touches_mesh {
                    self.mesh_revision.replace_with(|v| v.wrapping_add(1));
                }
            }
            inverses.reverse();
            return Ok(Some(EditorCommand::Batch(inverses)));
        }
        let touches_anim = cmd.affects_animation();
        let touches_mesh = cmd.affects_mesh();
        let result = self.apply_inner(cmd).await;
        if touches_anim {
            self.anim_revision.replace_with(|v| v.wrapping_add(1));
        }
        if touches_mesh {
            self.mesh_revision.replace_with(|v| v.wrapping_add(1));
        }
        result
    }

    /// Apply a list of commands as one atomic step that collapses into a single
    /// undo entry. The MCP `dispatch_batch` round-trips here. Each sub-command is
    /// broadcast individually (cross-tab replay), then the combined inverse is
    /// pushed as one `Batch` so undo reverses the whole thing.
    pub async fn dispatch_batch(&self, cmds: Vec<EditorCommand>) -> EditorResult<()> {
        let mut inverses = Vec::new();
        let mut any_recorded = false;
        for cmd in cmds {
            self.broadcast(&cmd);
            let transient = cmd.is_transient();
            let inv = self.apply(cmd).await?;
            if !transient {
                any_recorded = true;
                if let Some(i) = inv {
                    inverses.push(i);
                }
            }
        }
        if !inverses.is_empty() {
            inverses.reverse();
            self.undo.borrow_mut().push(EditorCommand::Batch(inverses));
            self.redo.borrow_mut().clear();
            self.refresh_history_signals();
        }
        if any_recorded {
            self.dirty.set_neq(true);
        }
        Ok(())
    }

    /// Read the current modifier-stack **recipe** off a mesh asset, for the
    /// incremental modifier commands (`AddModifier` / `SetModifier` /
    /// `RemoveModifier`). Errors if the asset isn't a mesh or has no recipe — a
    /// raw captured/converted mesh with `modifiers == None` must get a base via
    /// `SetMeshModifiers` first (synthesizing a `Captured`-self base here would
    /// double-apply the prior bake on the next edit).
    fn mesh_stack(&self, mesh: AssetId) -> EditorResult<ModifierStack> {
        match self
            .scene
            .assets
            .lock()
            .unwrap()
            .get(mesh)
            .map(|e| &e.source)
        {
            Some(SceneAssetSource::Mesh(def)) => Ok(def.stack.clone()),
            _ => Err(crate::error::EditorError::msg(format!(
                "asset {mesh} is not an editable mesh"
            ))),
        }
    }

    /// Replace a mesh asset's modifier-stack **recipe** wholesale: store the new
    /// recipe on the asset, re-evaluate → re-bake the `.mesh.bin` cache, bump the
    /// mesh revision (the bridge re-materializes referencing nodes), and return
    /// the inverse — `SetMeshModifiers(prior_stack)` (re-evaluates to the prior
    /// geometry). Every `MeshDef` carries a mandatory `stack`, so the prior recipe
    /// always exists. The shared body of `SetMeshModifiers` and the incremental
    /// modifier commands. Returns `None` (not undoable) if the asset isn't a mesh.
    fn apply_mesh_stack(&self, mesh: AssetId, stack: ModifierStack) -> Option<EditorCommand> {
        use crate::engine::bridge::mesh_cache;
        // Capture the prior recipe for the inverse, and bail if this asset isn't a
        // mesh.
        let prior_stack = match self
            .scene
            .assets
            .lock()
            .unwrap()
            .get(mesh)
            .map(|e| &e.source)
        {
            Some(SceneAssetSource::Mesh(def)) => def.stack.clone(),
            _ => return None,
        };
        // Store the new recipe on the asset (the recipe lives in the
        // project; the .mesh.bin is a regenerable cache). Snapshot the full def
        // (incl. any authoring overrides) so the re-bake layers them back on.
        let def = {
            let mut assets = self.scene.assets.lock().unwrap();
            match assets.entries.get_mut(&mesh).map(|e| &mut e.source) {
                Some(SceneAssetSource::Mesh(def)) => {
                    def.stack = stack.clone();
                    def.editable = true;
                    def.clone()
                }
                _ => return None,
            }
        };
        // Re-evaluate → re-bake the cache (the bridge re-materializes via
        // the mesh-revision bump in `apply`).
        let baked = crate::controller::mesh_eval::evaluate_def(&self.scene, &def);
        mesh_cache::store_with_id(mesh, mesh_cache::from_mesh_data(baked));
        self.scene.bump_revision();
        // Inverse: restore the prior stack (re-evaluates to prior geometry).
        Some(EditorCommand::SetMeshModifiers {
            mesh,
            stack: prior_stack,
        })
    }

    /// Collapse-before-authoring: make `mesh` *authorable* (index-based per-vertex
    /// authoring on a frozen topology). If the def isn't already a bare
    /// `Captured`-self base with no modifiers, bake the procedural part of the
    /// stack (base + modifiers, WITHOUT the override layer) into a fresh
    /// `Captured(self)` blob and flatten the stack to `{ base: Captured(self),
    /// modifiers: [] }`, freezing topology. The existing `overrides` are kept as
    /// the live layer (they still index into the now-frozen topology), so this is
    /// idempotent and non-destructive. Per-vertex authoring is **terminal**: after
    /// this the procedural params are baked and only the override layer is editable.
    ///
    /// Returns `Ok(true)` if a collapse actually happened (so the caller can fold
    /// the recipe-restore into its undo inverse), `Ok(false)` if already authorable,
    /// and the prior stack (for the inverse) via the out-param. Errors if `mesh`
    /// isn't a mesh asset.
    fn ensure_authorable(&self, mesh: AssetId) -> EditorResult<Option<ModifierStack>> {
        use crate::engine::bridge::mesh_cache;
        use awsm_editor_protocol::MeshRef;
        use awsm_editor_protocol::{MeshBase, ModifierStack};
        let prior_stack = {
            let assets = self.scene.assets.lock().unwrap();
            match assets.get(mesh).map(|e| &e.source) {
                Some(SceneAssetSource::Mesh(def)) => def.stack.clone(),
                _ => {
                    return Err(crate::error::EditorError::msg(format!(
                        "asset {mesh} is not an editable mesh"
                    )))
                }
            }
        };
        // Already authorable: a bare `Captured`-self base with no modifiers.
        if prior_stack.modifiers.is_empty()
            && matches!(prior_stack.base, MeshBase::Captured(r) if r == MeshRef(mesh))
        {
            return Ok(None);
        }
        // Freeze topology: bake the *procedural* part (stack only — overrides
        // stay a live layer that still indexes the frozen verts) into captured
        // bytes, then flatten the recipe to point at those bytes.
        let baked = crate::controller::mesh_eval::evaluate_stack(&self.scene, &prior_stack);
        mesh_cache::store_with_id(mesh, mesh_cache::from_mesh_data(baked));
        {
            let mut assets = self.scene.assets.lock().unwrap();
            if let Some(entry) = assets.entries.get_mut(&mesh) {
                if let SceneAssetSource::Mesh(def) = &mut entry.source {
                    def.stack = ModifierStack {
                        base: MeshBase::Captured(MeshRef(mesh)),
                        modifiers: vec![],
                    };
                    def.editable = true;
                }
            }
        }
        Ok(Some(prior_stack))
    }

    /// Read-modify-write the sparse [`VertexOverrides`] of a mesh def, re-bake the
    /// cache (stack + overrides), bump the scene revision, and return the prior
    /// overrides (for an inverse). The shared body of the per-vertex authoring
    /// commands (paint colors / set normals / sculpt positions). `mutate` receives
    /// the live overrides to insert/replace into; out-of-range indices are
    /// silently ignored at bake time (`apply_overrides`).
    fn apply_vertex_overrides(
        &self,
        mesh: AssetId,
        mutate: impl FnOnce(&mut awsm_editor_protocol::VertexOverrides),
    ) -> EditorResult<awsm_editor_protocol::VertexOverrides> {
        use crate::engine::bridge::mesh_cache;
        // Collapse to a frozen-topology base first (terminal authoring).
        self.ensure_authorable(mesh)?;
        let (prior, def) = {
            let mut assets = self.scene.assets.lock().unwrap();
            match assets.entries.get_mut(&mesh).map(|e| &mut e.source) {
                Some(SceneAssetSource::Mesh(def)) => {
                    let prior = def.overrides.clone();
                    mutate(&mut def.overrides);
                    (prior, def.clone())
                }
                _ => {
                    return Err(crate::error::EditorError::msg(format!(
                        "asset {mesh} is not an editable mesh"
                    )))
                }
            }
        };
        let baked = crate::controller::mesh_eval::evaluate_def(&self.scene, &def);
        mesh_cache::store_with_id(mesh, mesh_cache::from_mesh_data(baked));
        self.scene.bump_revision();
        Ok(prior)
    }

    /// Build the undo inverse for a per-vertex authoring command: restore the
    /// prior `overrides` (a `SetVertexOverrides`), and — if `ensure_authorable`
    /// collapsed the procedural stack — restore the prior stack too, as a `Batch`.
    /// The stack restore runs first on undo (it re-bakes the procedural base);
    /// the overrides restore then re-applies the prior authoring layer.
    fn overrides_inverse(
        &self,
        mesh: AssetId,
        prior: awsm_editor_protocol::VertexOverrides,
        collapse: Option<ModifierStack>,
    ) -> EditorCommand {
        let restore_overrides = EditorCommand::SetVertexOverrides {
            mesh,
            overrides: prior,
        };
        match collapse {
            None => restore_overrides,
            // Order matters: restore the prior overrides FIRST (a no-op collapse,
            // since topology is already frozen), THEN restore the procedural
            // stack — `apply_mesh_stack` re-bakes `stack + overrides`, so the
            // recipe-restore picks up the just-restored authoring layer. (Doing
            // the stack restore first would let the overrides-restore re-collapse
            // it.)
            Some(prior_stack) => EditorCommand::Batch(vec![
                restore_overrides,
                EditorCommand::SetMeshModifiers {
                    mesh,
                    stack: prior_stack,
                },
            ]),
        }
    }

    /// For a procedural-geometry `InsertSpec` (`Primitive` / `Sweep`), mint the
    /// backing `MeshDef` asset (a `ModifierStack` with the matching base), bake
    /// its `.mesh.bin` cache, and build the unified `NodeKind::Mesh` node that
    /// references it. Returns `(mesh_asset_id, node)`; `None` for any other spec
    /// (the caller falls back to a plain `build_insert`).
    ///
    /// The mesh asset id is `AssetId(node_id.0)` — deterministic from the node id
    /// (asset ids and node ids are disjoint keyspaces, so reusing the UUID is
    /// safe) so cross-tab replays produce the same asset and the insert stays
    /// idempotent. Baking the stack now means the node renders the first time it
    /// materializes (a nil-curve Sweep bakes empty until its curve is picked).
    fn build_mesh_insert(
        &self,
        node_id: NodeId,
        spec: &InsertSpec,
    ) -> Option<(AssetId, std::sync::Arc<crate::engine::scene::node::Node>)> {
        use crate::engine::bridge::mesh_cache;
        use crate::engine::scene::node::Node;
        use awsm_editor_protocol::{CapturedSource, MeshDef, MeshRef, PrimitiveShape, Trs};
        use awsm_editor_protocol::{MeshBase, ModifierStack};

        let (label, base, source): (&str, MeshBase, CapturedSource) = match spec {
            InsertSpec::Primitive(shape) => {
                let label = match shape {
                    PrimitiveShape::Plane { .. } => "Plane",
                    PrimitiveShape::Box { .. } => "Box",
                    PrimitiveShape::Sphere { .. } => "Sphere",
                    PrimitiveShape::Cylinder { .. } => "Cylinder",
                    PrimitiveShape::Cone { .. } => "Cone",
                    PrimitiveShape::Torus { .. } => "Torus",
                };
                (
                    label,
                    MeshBase::Primitive(shape.clone()),
                    CapturedSource::Primitive(shape.clone()),
                )
            }
            InsertSpec::Sweep => {
                let def = awsm_editor_protocol::SweepAlongCurveDef::default();
                (
                    "Sweep",
                    MeshBase::Sweep(def.clone()),
                    CapturedSource::Sweep(def),
                )
            }
            _ => return None,
        };

        let mesh_id = AssetId(node_id.0);
        let stack = ModifierStack {
            base,
            modifiers: vec![],
        };
        // Bake the stack now so the node has geometry on first materialize.
        let baked = crate::controller::mesh_eval::evaluate_stack(&self.scene, &stack);
        mesh_cache::store_with_id(mesh_id, mesh_cache::from_mesh_data(baked));
        self.scene.assets.lock().unwrap().entries.insert(
            mesh_id,
            AssetEntry::new(SceneAssetSource::Mesh(MeshDef {
                label: label.to_string(),
                source: Some(source),
                editable: true,
                stack,
                overrides: Default::default(),
            })),
        );

        let mut node = Node::new_with_transform_and_kind(
            label,
            Trs::default(),
            NodeKind::Mesh {
                mesh: MeshRef(mesh_id),
                material: None,
                shadow: Default::default(),
            },
        );
        std::sync::Arc::get_mut(&mut node)
            .expect("freshly built node is sole-owned")
            .id = node_id;
        Some((mesh_id, node))
    }

    /// Apply a command's effect and return its inverse (for the undo log), or
    /// `None` if the command is not undoable. The undoable per-node mutation
    /// commands return `Some(inverse)` here.
    async fn apply_inner(&self, cmd: EditorCommand) -> EditorResult<Option<EditorCommand>> {
        match cmd {
            EditorCommand::SwitchMode { mode } => {
                self.mode.set_neq(mode);
                Ok(None)
            }
            EditorCommand::SetSelection { ids } => {
                // Notify the agent of selection changes (e.g. a human clicking a
                // node in the Outliner) over the push channel.
                crate::remote::notify_event(awsm_editor_protocol::EditorEvent {
                    kind: "selection".to_string(),
                    level: None,
                    message: None,
                    nodes: Some(ids.iter().map(|id| id.to_string()).collect()),
                });
                self.selected.set(ids);
                Ok(None)
            }
            EditorCommand::SetVertexSelection { node, indices } => {
                // Read-only highlight (no geometry mutation). An empty `indices`
                // is normalized to `None` so the bridge's "Some ⇒ draw" path
                // doubles as the clear path.
                self.vertex_selection.set(if indices.is_empty() {
                    None
                } else {
                    Some((node, indices))
                });
                Ok(None)
            }
            // `Batch` is unwrapped in `apply` (so the async fn doesn't recurse);
            // it never reaches here.
            EditorCommand::Batch(_) => Ok(None),
            EditorCommand::SetKind { id, kind } => match mutate::find_by_id(&self.scene, id) {
                Some(node) => {
                    let prev = node.kind.get_cloned();
                    if structure_key(&prev) != structure_key(&kind) {
                        self.structure_rev
                            .set(self.structure_rev.get().wrapping_add(1));
                    }
                    // A Light node owns a renderer `LightKey` that a lowered
                    // `AnimationTarget::Light` channel holds. Editing a light
                    // param rebuilds the light in the bridge (teardown keeps
                    // shadow reallocation correct) and churns that key, so force
                    // a re-lower to refresh the target. (Camera nodes keep a
                    // stable key via an in-place update in the bridge — no
                    // shadow resource to rebuild — so they don't need this.)
                    if matches!(&prev, NodeKind::Light(_))
                        || matches!(kind.as_ref(), NodeKind::Light(_))
                    {
                        self.anim_revision.replace_with(|v| v.wrapping_add(1));
                    }
                    node.kind.set(*kind);
                    self.scene.bump_revision();
                    Ok(Some(EditorCommand::SetKind {
                        id,
                        kind: Box::new(prev),
                    }))
                }
                None => Ok(None),
            },
            EditorCommand::SetTransform { id, transform } => {
                match mutate::find_by_id(&self.scene, id) {
                    Some(node) => {
                        let prev = node.transform.get();
                        node.transform.set(transform);
                        self.scene.bump_revision();
                        Ok(Some(EditorCommand::SetTransform {
                            id,
                            transform: prev,
                        }))
                    }
                    None => Ok(None),
                }
            }
            EditorCommand::Rename { id, name } => match mutate::find_by_id(&self.scene, id) {
                Some(node) => {
                    let prev = node.name.get_cloned();
                    node.name.set(name);
                    self.scene.bump_revision();
                    Ok(Some(EditorCommand::Rename { id, name: prev }))
                }
                None => Ok(None),
            },
            EditorCommand::SetVisible { id, visible } => {
                match mutate::find_by_id(&self.scene, id) {
                    Some(node) => {
                        let prev = node.visible.get();
                        node.visible.set_neq(visible);
                        self.scene.bump_revision();
                        Ok(Some(EditorCommand::SetVisible { id, visible: prev }))
                    }
                    None => Ok(None),
                }
            }
            EditorCommand::SetLocked { id, locked } => match mutate::find_by_id(&self.scene, id) {
                Some(node) => {
                    let prev = node.locked.get();
                    node.locked.set_neq(locked);
                    self.scene.bump_revision();
                    Ok(Some(EditorCommand::SetLocked { id, locked: prev }))
                }
                None => Ok(None),
            },
            EditorCommand::SetPrefab { id, prefab } => match mutate::find_by_id(&self.scene, id) {
                Some(node) => {
                    let prev = node.prefab.get();
                    node.prefab.set_neq(prefab);
                    self.scene.bump_revision();
                    Ok(Some(EditorCommand::SetPrefab { id, prefab: prev }))
                }
                None => Ok(None),
            },
            EditorCommand::Duplicate { id } => match mutate::duplicate_by_id(&self.scene, id) {
                Some(new_id) => {
                    self.scene.bump_revision();
                    self.selected.set(vec![new_id]);
                    Ok(Some(EditorCommand::Delete { id: new_id }))
                }
                None => Ok(None),
            },
            EditorCommand::Reparent {
                id,
                new_parent,
                index,
            } => {
                let old_parent = mutate::find_parent(&self.scene, id).map(|p| p.id);
                let old_index = node_index(&self.scene, id, old_parent);
                if mutate::reparent(&self.scene, id, new_parent, index) {
                    self.scene.bump_revision();
                    Ok(Some(EditorCommand::Reparent {
                        id,
                        new_parent: old_parent,
                        index: old_index,
                    }))
                } else {
                    Ok(None)
                }
            }
            EditorCommand::NewProject => {
                // Project-level reset (clears the undo log — not itself undoable).
                self.scene.nodes.lock_mut().clear();
                self.selected.set(Vec::new());
                *self.scene.assets.lock().unwrap() = Default::default();
                self.scene.bump_revision();
                // Material library.
                self.custom_materials.lock_mut().clear();
                self.current_material.set(None);
                self.asset_selection.set(None);
                // Animation library + mixer + transport (these previously leaked
                // across New Project — clips/mixer/playhead persisted).
                self.custom_animations.lock_mut().clear();
                self.current_clip.set(None);
                self.anim_mixer.set(MixerDoc::default());
                self.anim_selection.set(None);
                self.anim_solo_root.set(None);
                self.playhead.set_neq(0.0);
                self.playing.set_neq(false);
                // Skin bridge mappings (#2) + import templates belong to imported
                // models — drop them.
                crate::engine::bridge::bridge().clear_skin_joints();
                // Remove imported-glTF template meshes from the renderer BEFORE
                // dropping the template metadata: `clear_templates` only clears the
                // map, and the skinned populate copies are template-owned (node
                // teardown deliberately skips them), so without this they ghost.
                clear_untracked_renderer_resources().await;
                crate::engine::bridge::bridge().clear_templates();
                crate::engine::bridge::skinned_bake_cache::clear();
                self.project_name.set("untitled.awsm".to_string());
                self.missing_assets.set(Vec::new());
                // Seed a sane default scene: a key directional light (tilted ~50°
                // by `new_light`) + the built-in skybox/IBL environment, so the
                // first PBR/lit material isn't black out of the box (the §E3 fix —
                // applies to the human editor and MCP alike).
                let light = build_insert(&InsertSpec::Light(
                    awsm_editor_protocol::LightKind::Directional,
                ));
                mutate::insert_under(&self.scene, None, light);
                self.scene
                    .environment
                    .set(awsm_editor_protocol::EnvironmentConfig::default());
                self.scene.bump_revision();
                self.dirty.set_neq(false);
                self.undo.borrow_mut().clear();
                self.redo.borrow_mut().clear();
                self.refresh_history_signals();
                Toast::info("New project");
                Ok(None)
            }
            EditorCommand::LoadPlayerBundle => {
                // Round-trip self-test: bake the open project to an in-memory
                // bundle, reset to empty, then reload it via the player path
                // (`populate_awsm_scene`). Destructive + not undoable — the
                // viewport ends up showing the runtime reload (the scene tree is
                // left empty; reload the project to keep editing). An agent
                // screenshots before/after to compare authored vs runtime render.

                // 1. Bake the CURRENT project — must read it before we clear.
                let files = crate::controller::export::bake_player_bundle(self)
                    .await
                    .map_err(|e| crate::error::EditorError::msg(format!("bake: {e}")))?;
                // 2. Split scene.toml out; the rest is the asset map
                //    (bundle-relative path → bytes) `populate_awsm_scene` reads.
                let mut scene_toml: Option<String> = None;
                let mut assets: std::collections::HashMap<String, Vec<u8>> =
                    std::collections::HashMap::new();
                for f in files {
                    if f.path == awsm_editor_protocol::SCENE_FILE {
                        scene_toml = Some(String::from_utf8_lossy(&f.bytes).into_owned());
                    } else {
                        assets.insert(f.path, f.bytes);
                    }
                }
                let scene_toml = scene_toml
                    .ok_or_else(|| crate::error::EditorError::msg("bundle missing scene.toml"))?;
                let scene = awsm_editor_protocol::scene_from_toml(&scene_toml)
                    .map_err(|e| crate::error::EditorError::msg(format!("scene.toml: {e}")))?;

                // 3. Bare reset to empty — NO default-light seed (the bundle
                //    carries its own light; seeding one would double it). Mirrors
                //    NewProject's clears. Carry the bundle's environment so the
                //    env bridge applies the same skybox/IBL.
                self.scene.nodes.lock_mut().clear();
                self.selected.set(Vec::new());
                *self.scene.assets.lock().unwrap() = Default::default();
                self.custom_materials.lock_mut().clear();
                self.current_material.set(None);
                self.asset_selection.set(None);
                self.custom_animations.lock_mut().clear();
                self.current_clip.set(None);
                self.anim_mixer.set(MixerDoc::default());
                self.anim_selection.set(None);
                self.anim_solo_root.set(None);
                self.playhead.set_neq(0.0);
                self.playing.set_neq(false);
                crate::engine::bridge::bridge().clear_skin_joints();
                // Remove imported-glTF template meshes from the renderer BEFORE
                // dropping the template metadata: `clear_templates` only clears the
                // map, and the skinned populate copies are template-owned (node
                // teardown deliberately skips them), so without this they ghost.
                clear_untracked_renderer_resources().await;
                crate::engine::bridge::bridge().clear_templates();
                crate::engine::bridge::skinned_bake_cache::clear();
                self.missing_assets.set(Vec::new());
                self.scene.environment.set(scene.environment.clone());
                self.scene.bump_revision();

                // 4. Load the bundle into the renderer via the player path. The
                //    bridge's teardown of the old nodes (observer-driven, needs
                //    the renderer lock) runs once we release this guard, removing
                //    only the old keys — populate's fresh keys persist. The
                //    render loop then presents the reload via the free camera.
                {
                    let handle = crate::engine::context::renderer_handle();
                    let mut r = handle.lock().await;
                    // Drop the editor's own clips + mixer (a prior relower may have
                    // populated them from the now-cleared model) so the bundle's
                    // clips don't double up. LoadPlayerBundle doesn't relower (see
                    // `affects_animation`), so nothing repopulates them.
                    r.animations.clear_clips();
                    r.animations.mixer.clear();
                    // Surface each load phase (building materials / uploading
                    // textures / uploading meshes / compiling pipelines N) in the
                    // activity pill — live, because the pill is a reactive signal
                    // and the loader's awaits yield to the event loop.
                    let res =
                        awsm_scene_loader::populate_awsm_scene(&mut r, &scene, &assets, |p| {
                            crate::engine::activity::set_load_phase(Some(p.label()));
                        })
                        .await;
                    crate::engine::activity::set_load_phase(None);
                    let loaded =
                        res.map_err(|e| crate::error::EditorError::msg(format!("populate: {e}")))?;
                    // Track the direct inserts so the NEXT reset removes them
                    // (they're outside the bridge's per-node teardown).
                    set_bundle_resources(loaded.meshes, loaded.lights, loaded.clips);
                }
                self.project_name.set("round-trip.awsm".to_string());
                self.dirty.set_neq(false);
                self.undo.borrow_mut().clear();
                self.redo.borrow_mut().clear();
                self.refresh_history_signals();
                Toast::info("Round-trip: reloaded via populate_awsm_scene");
                Ok(None)
            }
            EditorCommand::Insert { id, spec, parent } => {
                // Idempotent (apply-when-absent): a cross-tab replay or a
                // duplicate caller-minted id is a no-op, so the id stays stable.
                if mutate::find_by_id(&self.scene, id).is_some() {
                    return Ok(None);
                }
                // Procedural-geometry specs (Primitive / Sweep) mint a `MeshDef`
                // asset (a `ModifierStack` with the matching base) + bake its cache,
                // then create a unified `NodeKind::Mesh` referencing it. The mesh
                // asset id is derived deterministically from the node id (disjoint
                // keyspace) so a cross-tab replay produces the same asset id, and
                // the inverse can delete both. Every other spec is a plain insert.
                if let Some((mesh_id, node)) = self.build_mesh_insert(id, &spec) {
                    if mutate::insert_under(&self.scene, parent, node) {
                        self.scene.bump_revision();
                        return Ok(Some(EditorCommand::Batch(vec![
                            EditorCommand::Delete { id },
                            EditorCommand::DeleteAsset { id: mesh_id },
                        ])));
                    }
                    return Ok(None);
                }
                let mut node = build_insert(&spec);
                // Adopt the caller-minted id (build_insert mints a fresh one).
                Arc::get_mut(&mut node)
                    .expect("freshly built node is sole-owned")
                    .id = id;
                if mutate::insert_under(&self.scene, parent, node) {
                    self.scene.bump_revision();
                    Ok(Some(EditorCommand::Delete { id }))
                } else {
                    Ok(None)
                }
            }
            EditorCommand::InsertTree {
                node,
                parent,
                index,
            } => {
                let arc = node_from_spec(&node);
                let id = arc.id;
                // Insert at the captured position so undo lands the subtree back
                // where it was; fall back to append if the slot is gone.
                let ok = match (parent, index) {
                    (None, Some(idx)) => {
                        let mut nodes = self.scene.nodes.lock_mut();
                        let idx = idx.min(nodes.len());
                        nodes.insert_cloned(idx, arc);
                        true
                    }
                    (Some(pid), Some(idx)) => match mutate::find_by_id(&self.scene, pid) {
                        Some(p) => {
                            let mut children = p.children.lock_mut();
                            let idx = idx.min(children.len());
                            children.insert_cloned(idx, arc);
                            true
                        }
                        None => false,
                    },
                    (parent, None) => mutate::insert_under(&self.scene, parent, arc),
                };
                if ok {
                    self.scene.bump_revision();
                    Ok(Some(EditorCommand::Delete { id }))
                } else {
                    Ok(None)
                }
            }
            EditorCommand::Delete { id } => {
                let parent = mutate::find_parent(&self.scene, id).map(|p| p.id);
                let index = node_index(&self.scene, id, parent);
                match mutate::remove_by_id(&self.scene, id) {
                    Some(node) => {
                        let spec = spec_from_node(&node);
                        self.selected.lock_mut().retain(|x| *x != id);
                        self.scene.bump_revision();
                        Ok(Some(EditorCommand::InsertTree {
                            node: Box::new(spec),
                            parent,
                            index,
                        }))
                    }
                    None => Ok(None),
                }
            }
            EditorCommand::AddMaterialAsset { id, shading } => {
                if self.scene.assets.lock().unwrap().entries.contains_key(&id) {
                    return Ok(None);
                }
                let label = self.next_asset_label("Material");
                let def = MaterialDef {
                    label,
                    shading,
                    ..MaterialDef::default()
                };
                self.scene
                    .assets
                    .lock()
                    .unwrap()
                    .entries
                    .insert(id, AssetEntry::new(SceneAssetSource::Material(def)));
                self.scene.bump_revision();
                self.asset_selection.set(Some(id));
                Ok(Some(EditorCommand::DeleteAsset { id }))
            }
            EditorCommand::AddTextureAsset { id, proc } => {
                if self.scene.assets.lock().unwrap().entries.contains_key(&id) {
                    return Ok(None);
                }
                let def = TextureDef::Procedural(default_procedural(proc));
                self.scene
                    .assets
                    .lock()
                    .unwrap()
                    .entries
                    .insert(id, AssetEntry::new(SceneAssetSource::Texture(def)));
                self.scene.bump_revision();
                self.asset_selection.set(Some(id));
                Ok(Some(EditorCommand::DeleteAsset { id }))
            }
            EditorCommand::DeleteAsset { id } => {
                let removed = self.scene.assets.lock().unwrap().entries.remove(&id);
                match removed {
                    Some(entry) => {
                        self.scene.bump_revision();
                        if self.asset_selection.get() == Some(id) {
                            self.asset_selection.set(None);
                        }
                        Ok(Some(EditorCommand::RestoreAsset {
                            id,
                            entry: Box::new(entry),
                        }))
                    }
                    None => Ok(None),
                }
            }
            EditorCommand::RestoreAsset { id, entry } => {
                self.scene.assets.lock().unwrap().entries.insert(id, *entry);
                self.scene.bump_revision();
                Ok(Some(EditorCommand::DeleteAsset { id }))
            }
            EditorCommand::DropSkinning { node } => {
                use awsm_editor_protocol::SkinnedMeshRef;
                let Some(n) = mutate::find_by_id(&self.scene, node) else {
                    return Ok(None);
                };
                let prev = n.kind.get_cloned();
                // Only a SkinnedMesh can be dropped to editable — anything else is
                // a no-op (the UI/MCP layer surfaces a clearer message).
                let (skin, material, shadow): (SkinnedMeshRef, _, _) = match prev.clone() {
                    NodeKind::SkinnedMesh {
                        skin,
                        material,
                        shadow,
                    } => (skin, material, shadow),
                    _ => return Ok(None),
                };
                // Bind-pose geometry stashed at import (no JOINTS/WEIGHTS).
                let Some(mesh) = crate::engine::bridge::skinned_bake_cache::get(
                    skin.source,
                    skin.node_index,
                    skin.primitive_index,
                ) else {
                    Toast::error(
                        "drop_skinning: this skinned mesh's bind-pose geometry isn't \
                         cached (re-import the model in this session)",
                    );
                    return Ok(None);
                };
                // Mint a captured editable Mesh asset from the bind pose and swap
                // the node's kind. Reuses the import bake path (deterministic id =
                // node id), so it persists like any captured mesh.
                let label = n.name.get_cloned();
                let mesh_ref = mint_imported_mesh(node, &label, &mesh, skin.source);
                // Hide the now-orphaned skinned populate copy so it stops rendering
                // (the node now renders its captured bind-pose Mesh instead).
                if let Some(template) = crate::engine::bridge::bridge().get_template(skin.source) {
                    if let Some(tnode) = template.find_by_node_index(skin.node_index) {
                        let keys: Vec<_> = match skin.primitive_index {
                            None => tnode.mesh_keys.clone(),
                            Some(i) => tnode
                                .mesh_keys
                                .get(i as usize)
                                .copied()
                                .into_iter()
                                .collect(),
                        };
                        spawn_local(async move {
                            crate::engine::context::with_renderer_mut(move |r| {
                                for mk in keys {
                                    let _ = r.set_mesh_hidden(mk, true);
                                }
                            })
                            .await;
                        });
                    }
                }
                n.kind.set(NodeKind::Mesh {
                    mesh: mesh_ref,
                    material,
                    shadow,
                });
                self.structure_rev
                    .set(self.structure_rev.get().wrapping_add(1));
                self.scene.bump_revision();
                self.dirty.set_neq(true);
                // Inverse restores the prior SkinnedMesh kind (the captured asset is
                // left behind, harmlessly unreferenced).
                Ok(Some(EditorCommand::SetKind {
                    id: node,
                    kind: Box::new(prev),
                }))
            }
            EditorCommand::ConvertToEditableMesh { node, mesh } => {
                // Retired: every procedural node is already a `NodeKind::Mesh`
                // backed by an editable `MeshDef` stack, so there is nothing to
                // convert. Kept as a no-op for protocol/back-compat; the MCP tool
                // echoes the node's existing mesh id instead of the (now ignored)
                // caller-minted `mesh`. Not undoable.
                let _ = (node, mesh);
                Ok(None)
            }
            EditorCommand::SetMeshData { mesh, data } => {
                use crate::engine::bridge::mesh_cache;
                let prior = mesh_cache::get_captured(mesh);
                mesh_cache::store_with_id(mesh, data);
                self.scene.bump_revision();
                // Inverse restores the prior geometry; if there was none (the mesh
                // didn't exist), the edit isn't undoable.
                Ok(prior.map(|data| EditorCommand::SetMeshData { mesh, data }))
            }
            EditorCommand::SetMeshModifiers { mesh, stack } => {
                Ok(self.apply_mesh_stack(mesh, stack))
            }
            EditorCommand::AddModifier { mesh, modifier } => {
                let mut stack = match self.mesh_stack(mesh) {
                    Ok(s) => s,
                    Err(e) => return Err(e),
                };
                stack.modifiers.push(modifier);
                Ok(self.apply_mesh_stack(mesh, stack))
            }
            EditorCommand::SetModifier {
                mesh,
                index,
                modifier,
            } => {
                let mut stack = match self.mesh_stack(mesh) {
                    Ok(s) => s,
                    Err(e) => return Err(e),
                };
                let i = index as usize;
                if i >= stack.modifiers.len() {
                    return Err(crate::error::EditorError::msg(format!(
                        "modifier index {index} out of range (mesh {mesh} has {} modifier(s))",
                        stack.modifiers.len()
                    )));
                }
                stack.modifiers[i] = modifier;
                Ok(self.apply_mesh_stack(mesh, stack))
            }
            EditorCommand::RemoveModifier { mesh, index } => {
                let mut stack = match self.mesh_stack(mesh) {
                    Ok(s) => s,
                    Err(e) => return Err(e),
                };
                let i = index as usize;
                if i >= stack.modifiers.len() {
                    return Err(crate::error::EditorError::msg(format!(
                        "modifier index {index} out of range (mesh {mesh} has {} modifier(s))",
                        stack.modifiers.len()
                    )));
                }
                stack.modifiers.remove(i);
                Ok(self.apply_mesh_stack(mesh, stack))
            }
            EditorCommand::SetVertexPositions {
                mesh,
                indices,
                positions,
            } => {
                // Migrated: write to the sparse `overrides.positions` layer
                // (collapse-then-override) instead of mutating captured bytes —
                // same observable result, now non-destructive + uniform.
                let collapse = self.ensure_authorable(mesh)?;
                let prior = self.apply_vertex_overrides(mesh, |ov| {
                    for (k, &idx) in indices.iter().enumerate() {
                        if let Some(p) = positions.get(k) {
                            ov.positions.insert(idx, *p);
                        }
                    }
                })?;
                Ok(Some(self.overrides_inverse(mesh, prior, collapse)))
            }
            EditorCommand::SoftTransformVertices {
                mesh,
                indices,
                translation,
                falloff,
            } => {
                use crate::engine::bridge::mesh_cache;
                // Resolve the current (post-eval+override) geometry to weight the
                // falloff against, then bake the move into `overrides.positions`.
                let collapse = self.ensure_authorable(mesh)?;
                let Some(cap) = mesh_cache::get_captured(mesh) else {
                    return Ok(None);
                };
                let md = awsm_meshgen::MeshData {
                    positions: cap.positions.clone(),
                    normals: cap.normals.clone(),
                    uvs: cap.uvs.clone(),
                    colors: cap.colors.clone(),
                    indices: cap.indices.clone(),
                };
                let new_positions = awsm_meshgen::edit::soft_transform_positions(
                    &md,
                    &indices,
                    translation,
                    falloff,
                );
                // Only override the verts the falloff actually moved.
                let mut moved = false;
                let prior = self.apply_vertex_overrides(mesh, |ov| {
                    for (i, (old, new)) in cap.positions.iter().zip(&new_positions).enumerate() {
                        if old != new {
                            ov.positions.insert(i as u32, *new);
                            moved = true;
                        }
                    }
                })?;
                if !moved {
                    return Ok(None);
                }
                Ok(Some(self.overrides_inverse(mesh, prior, collapse)))
            }
            EditorCommand::PaintVertexColors {
                mesh,
                indices,
                color,
            } => {
                let collapse = self.ensure_authorable(mesh)?;
                let prior = self.apply_vertex_overrides(mesh, |ov| {
                    for &idx in &indices {
                        ov.colors.insert(idx, color);
                    }
                })?;
                Ok(Some(self.overrides_inverse(mesh, prior, collapse)))
            }
            EditorCommand::SetVertexNormals {
                mesh,
                indices,
                normal,
            } => {
                let collapse = self.ensure_authorable(mesh)?;
                let prior = self.apply_vertex_overrides(mesh, |ov| {
                    for &idx in &indices {
                        ov.normals.insert(idx, normal);
                    }
                })?;
                Ok(Some(self.overrides_inverse(mesh, prior, collapse)))
            }
            EditorCommand::SetVertexOverrides { mesh, overrides } => {
                let collapse = self.ensure_authorable(mesh)?;
                let prior = self.apply_vertex_overrides(mesh, |ov| {
                    *ov = overrides;
                })?;
                Ok(Some(self.overrides_inverse(mesh, prior, collapse)))
            }
            EditorCommand::BakeAll {} => {
                // Project-wide finalize: collapse every Mesh asset's stack.
                let mesh_ids: Vec<AssetId> = {
                    let assets = self.scene.assets.lock().unwrap();
                    assets
                        .entries
                        .iter()
                        .filter_map(|(id, e)| {
                            matches!(e.source, SceneAssetSource::Mesh(_)).then_some(*id)
                        })
                        .collect()
                };
                let mut inverses = Vec::new();
                for mesh in mesh_ids {
                    // Each collapse returns the prior stack (when it fired). The
                    // overrides are unchanged by a bake, so the inverse is just the
                    // stack restore.
                    if let Some(prior_stack) = self.ensure_authorable(mesh)? {
                        // Re-bake so the cache reflects the flattened recipe (incl.
                        // overrides re-applied on the frozen base).
                        let def = {
                            let assets = self.scene.assets.lock().unwrap();
                            match assets.get(mesh).map(|e| &e.source) {
                                Some(SceneAssetSource::Mesh(def)) => def.clone(),
                                _ => continue,
                            }
                        };
                        let baked = crate::controller::mesh_eval::evaluate_def(&self.scene, &def);
                        crate::engine::bridge::mesh_cache::store_with_id(
                            mesh,
                            crate::engine::bridge::mesh_cache::from_mesh_data(baked),
                        );
                        inverses.push(EditorCommand::SetMeshModifiers {
                            mesh,
                            stack: prior_stack,
                        });
                    }
                }
                self.scene.bump_revision();
                if inverses.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(EditorCommand::Batch(inverses)))
                }
            }
            EditorCommand::CollapseMeshStack { mesh } => {
                use crate::engine::bridge::mesh_cache;
                use awsm_editor_protocol::MeshRef;
                use awsm_editor_protocol::{MeshBase, ModifierStack};
                let stack = match self
                    .scene
                    .assets
                    .lock()
                    .unwrap()
                    .get(mesh)
                    .map(|e| &e.source)
                {
                    Some(SceneAssetSource::Mesh(def)) => def.stack.clone(),
                    _ => return Ok(None),
                };
                // Nothing to collapse if the stack is already a bare capture
                // (a `Captured` base with no modifiers — its bytes are the source).
                if stack.modifiers.is_empty() && matches!(stack.base, MeshBase::Captured(_)) {
                    return Ok(None);
                }
                let Some(prior_bytes) = mesh_cache::get_captured(mesh) else {
                    return Ok(None);
                };
                // Bake the current stack, then flatten the recipe to a bare capture
                // of this same asset's bytes — the baked geometry becomes the
                // source of truth (no recipe left to re-evaluate).
                let baked = crate::controller::mesh_eval::evaluate_stack(&self.scene, &stack);
                {
                    let mut assets = self.scene.assets.lock().unwrap();
                    if let Some(entry) = assets.entries.get_mut(&mesh) {
                        if let SceneAssetSource::Mesh(def) = &mut entry.source {
                            def.stack = ModifierStack {
                                base: MeshBase::Captured(MeshRef(mesh)),
                                modifiers: vec![],
                            };
                        }
                    }
                }
                mesh_cache::store_with_id(mesh, mesh_cache::from_mesh_data(baked));
                self.scene.bump_revision();
                // Undo restores the recipe (re-evaluates) then the exact prior bytes.
                Ok(Some(EditorCommand::Batch(vec![
                    EditorCommand::SetMeshModifiers { mesh, stack },
                    EditorCommand::SetMeshData {
                        mesh,
                        data: prior_bytes,
                    },
                ])))
            }
            EditorCommand::SetAssetSelection { id } => {
                self.asset_selection.set(id);
                Ok(None)
            }
            EditorCommand::AddCustomMaterial { id } => {
                if find_material(&self.custom_materials, id).is_some() {
                    return Ok(None);
                }
                let n = self.custom_materials.lock_ref().len() + 1;
                let mat = CM::new(id, format!("New Material {n}"));
                self.custom_materials.lock_mut().push_cloned(mat.clone());
                self.current_material.set(Some(id));
                // Usable immediately — compile now + recompile (debounced) on edit.
                spawn_auto_register(mat);
                self.scene.bump_revision();
                self.dirty.set_neq(true);
                Ok(None)
            }
            EditorCommand::AddBuiltinMaterial { id, shading } => {
                if find_material(&self.custom_materials, id).is_some() {
                    return Ok(None);
                }
                let n = self.custom_materials.lock_ref().len() + 1;
                let label = match shading {
                    awsm_editor_protocol::MaterialShading::Pbr => "PBR",
                    awsm_editor_protocol::MaterialShading::Unlit => "Unlit",
                    awsm_editor_protocol::MaterialShading::Toon { .. } => "Toon",
                };
                let mat = CM::new_builtin(id, format!("{label} Material {n}"), shading);
                self.custom_materials.lock_mut().push_cloned(mat.clone());
                self.current_material.set(Some(id));
                // Re-materialize assigned meshes when its variant settings change.
                spawn_builtin_resync(mat);
                self.scene.bump_revision();
                self.dirty.set_neq(true);
                Ok(None)
            }
            EditorCommand::DeleteCustomMaterial { id } => {
                self.custom_materials.lock_mut().retain(|m| m.id != id);
                if self.current_material.get() == Some(id) {
                    let next = self.custom_materials.lock_ref().first().map(|m| m.id);
                    self.current_material.set(next);
                }
                self.scene.bump_revision();
                self.dirty.set_neq(true);
                Ok(None)
            }
            EditorCommand::SetCurrentMaterial { id } => {
                self.current_material.set(id);
                Ok(None)
            }
            EditorCommand::RegisterMaterial { id } => {
                if let Some(mat) = find_material(&self.custom_materials, id) {
                    let was = mat.registered.get();
                    let name = mat.name.get_cloned();
                    // `register_material` records diagnostics (syntax + GPU/naga)
                    // on the material and flips `registered`, so MCP's
                    // `MaterialDiagnostics` query reads the truth either way.
                    compile_begin();
                    let ok = register_material(&mat).await;
                    compile_end();
                    if ok {
                        Toast::info(if was {
                            format!("Recompiled \u{201c}{name}\u{201d} \u{2014} bucket refreshed.")
                        } else {
                            format!("Registered \u{201c}{name}\u{201d}.")
                        });
                    }
                }
                Ok(None)
            }
            EditorCommand::SetCustomMaterialWgsl { id, wgsl } => {
                // Replace a custom (dynamic-WGSL) material's source. Setting the
                // live `wgsl` field triggers the controller-owned auto-register
                // observer (`spawn_auto_register`), which recompiles + re-
                // materializes — so this works headlessly (no Studio UI). Inverse:
                // restore the prior source.
                match find_material(&self.custom_materials, id) {
                    Some(mat) => {
                        let prev = mat.wgsl.get_cloned();
                        mat.wgsl.set(wgsl);
                        mark_material_draft(&mat);
                        self.dirty.set_neq(true);
                        Ok(Some(EditorCommand::SetCustomMaterialWgsl {
                            id,
                            wgsl: prev,
                        }))
                    }
                    None => Ok(None),
                }
            }
            EditorCommand::SetCustomMaterialAlphaWgsl { id, wgsl } => {
                // Replace a MASK material's 2nd alpha-only WGSL window. Setting
                // the live `alpha_wgsl` field marks the material a draft + bumps
                // the recompile rev (via mark_material_draft), so the
                // auto-register observer recompiles the masked variant.
                match find_material(&self.custom_materials, id) {
                    Some(mat) => {
                        let prev = mat.alpha_wgsl.get_cloned();
                        mat.alpha_wgsl.set(wgsl);
                        mark_material_draft(&mat);
                        self.dirty.set_neq(true);
                        Ok(Some(EditorCommand::SetCustomMaterialAlphaWgsl {
                            id,
                            wgsl: prev,
                        }))
                    }
                    None => Ok(None),
                }
            }
            EditorCommand::AssignMaterial { node, material } => {
                match mutate::find_by_id(&self.scene, node) {
                    Some(n) => {
                        let prev = n.kind.get_cloned();
                        // The node's prior assignment (if any) — used to carry the
                        // existing per-mesh inline store forward when reassigning.
                        let prior = node_material_ref(&prev).cloned();
                        // Assigning a material adopts its *defaults* (the full
                        // uniform surface — factors, extension params, Toon knobs,
                        // cutoff) into this mesh's inline store, so the mesh starts
                        // looking like the material; the user then customizes
                        // per-mesh from there. (A dynamic material has no built-in
                        // defaults → keep the existing inline, which it ignores;
                        // fall back to the node's prior inline, else a default.)
                        // Id-keyed assignment: store the material's stable id (so
                        // renaming it never orphans this mesh). Validate the id
                        // exists in the custom-material list. `None` clears the
                        // assignment → magenta.
                        let instance = material
                            .filter(|id| find_material(&self.custom_materials, *id).is_some())
                            .map(|id| {
                                let inline = find_material(&self.custom_materials, id)
                                    .and_then(|m| m.builtin.get_cloned())
                                    .or_else(|| prior.as_ref().map(|p| p.inline.clone()))
                                    .unwrap_or_default();
                                awsm_editor_protocol::dynamic_material::MaterialInstance {
                                    asset: id,
                                    inline,
                                    uniform_overrides: Default::default(),
                                    texture_overrides: Default::default(),
                                    buffer_overrides: Default::default(),
                                }
                            });
                        let next = match prev.clone() {
                            // The sole procedural-geometry node: one material slot.
                            NodeKind::Mesh { mesh, shadow, .. } => NodeKind::Mesh {
                                mesh,
                                material: instance,
                                shadow,
                            },
                            // A skinned import carries the same one-material slot.
                            NodeKind::SkinnedMesh { skin, shadow, .. } => NodeKind::SkinnedMesh {
                                skin,
                                material: instance,
                                shadow,
                            },
                            _ => return Ok(None),
                        };
                        n.kind.set(next);
                        // The material section's structure changes (built-in
                        // knobs ↔ dynamic link ↔ none), so refresh the inspector.
                        self.structure_rev
                            .set(self.structure_rev.get().wrapping_add(1));
                        self.scene.bump_revision();
                        Ok(Some(EditorCommand::SetKind {
                            id: node,
                            kind: Box::new(prev),
                        }))
                    }
                    None => Ok(None),
                }
            }
            EditorCommand::CopyMaterialInstance { from, to } => {
                let (Some(src), Some(dst)) = (
                    mutate::find_by_id(&self.scene, from),
                    mutate::find_by_id(&self.scene, to),
                ) else {
                    return Ok(None);
                };
                // The source node's material slot (geometry kinds only).
                let src_slot = match src.kind.get_cloned() {
                    NodeKind::Mesh { material, .. } => material,
                    NodeKind::SkinnedMesh { material, .. } => material,
                    _ => return Ok(None),
                };
                let prev = dst.kind.get_cloned();
                // Build the next dst kind by replacing only its material slot.
                let (next, dst_mat) = match prev.clone() {
                    NodeKind::Mesh {
                        mesh,
                        material: dst_mat,
                        shadow,
                    } => (
                        NodeKind::Mesh {
                            mesh,
                            material: src_slot.clone(),
                            shadow,
                        },
                        dst_mat,
                    ),
                    NodeKind::SkinnedMesh {
                        skin,
                        material: dst_mat,
                        shadow,
                    } => (
                        NodeKind::SkinnedMesh {
                            skin,
                            material: src_slot.clone(),
                            shadow,
                        },
                        dst_mat,
                    ),
                    _ => return Ok(None),
                };
                // Only copy between meshes that reference the same material.
                if src_slot.as_ref().map(|i| i.asset) != dst_mat.as_ref().map(|i| i.asset) {
                    return Ok(None);
                }
                // Copy the whole instance (inline uniforms + override maps).
                dst.kind.set(next);
                self.structure_rev
                    .set(self.structure_rev.get().wrapping_add(1));
                self.scene.bump_revision();
                Ok(Some(EditorCommand::SetKind {
                    id: to,
                    kind: Box::new(prev),
                }))
            }
            EditorCommand::SetCustomMaterialAlphaMode { id, mode } => {
                match find_material(&self.custom_materials, id) {
                    Some(mat) => {
                        let prev = custom_alpha_of(&mat);
                        match mode {
                            awsm_editor_protocol::CustomAlphaMode::Opaque => {
                                mat.alpha.set_neq(AlphaMode::Opaque);
                            }
                            awsm_editor_protocol::CustomAlphaMode::Mask { cutoff } => {
                                mat.alpha.set_neq(AlphaMode::Mask);
                                mat.cutoff.set_neq(cutoff);
                            }
                            awsm_editor_protocol::CustomAlphaMode::Blend => {
                                mat.alpha.set_neq(AlphaMode::Blend);
                            }
                        }
                        mark_material_draft(&mat);
                        self.dirty.set_neq(true);
                        Ok(Some(EditorCommand::SetCustomMaterialAlphaMode {
                            id,
                            mode: prev,
                        }))
                    }
                    None => Ok(None),
                }
            }
            EditorCommand::SetCustomMaterialDoubleSided { id, double_sided } => {
                match find_material(&self.custom_materials, id) {
                    Some(mat) => {
                        let prev = mat.double_sided.get();
                        mat.double_sided.set_neq(double_sided);
                        mark_material_draft(&mat);
                        self.dirty.set_neq(true);
                        Ok(Some(EditorCommand::SetCustomMaterialDoubleSided {
                            id,
                            double_sided: prev,
                        }))
                    }
                    None => Ok(None),
                }
            }
            EditorCommand::SetCustomMaterialDebugColor { id, hex } => {
                match find_material(&self.custom_materials, id) {
                    Some(mat) => {
                        let prev = mat.color.get_cloned();
                        mat.color.set_neq(hex);
                        // Debug color is preview-only — no recompile needed, but it
                        // is project state, so flag dirty.
                        self.dirty.set_neq(true);
                        Ok(Some(EditorCommand::SetCustomMaterialDebugColor {
                            id,
                            hex: prev,
                        }))
                    }
                    None => Ok(None),
                }
            }
            EditorCommand::SetCustomMaterialLayout {
                id,
                uniforms,
                textures,
                buffers,
            } => match find_material(&self.custom_materials, id) {
                Some(mat) => {
                    let prev = EditorCommand::SetCustomMaterialLayout {
                        id,
                        uniforms: slots_to_specs(&mat.uniforms.get_cloned()),
                        textures: slots_to_specs(&mat.textures.get_cloned()),
                        buffers: slots_to_specs(&mat.buffers.get_cloned()),
                    };
                    mat.uniforms.set(specs_to_slots(&uniforms));
                    mat.textures.set(specs_to_slots(&textures));
                    mat.buffers.set(specs_to_slots(&buffers));
                    mark_material_draft(&mat);
                    self.dirty.set_neq(true);
                    Ok(Some(prev))
                }
                None => Ok(None),
            },
            EditorCommand::SetCustomMaterialShaderIncludes { id, includes } => {
                match find_material(&self.custom_materials, id) {
                    Some(mat) => {
                        let prev = mat.shader_includes.get_cloned();
                        mat.shader_includes.set(validate_keys(
                            &includes,
                            custom_material::SHADER_INCLUDE_KEYS,
                        ));
                        mark_material_draft(&mat);
                        self.dirty.set_neq(true);
                        Ok(Some(EditorCommand::SetCustomMaterialShaderIncludes {
                            id,
                            includes: prev,
                        }))
                    }
                    None => Ok(None),
                }
            }
            EditorCommand::SetCustomMaterialFragmentInputs { id, inputs } => {
                match find_material(&self.custom_materials, id) {
                    Some(mat) => {
                        let prev = mat.fragment_inputs.get_cloned();
                        mat.fragment_inputs
                            .set(validate_keys(&inputs, custom_material::FRAGMENT_INPUT_KEYS));
                        mark_material_draft(&mat);
                        self.dirty.set_neq(true);
                        Ok(Some(EditorCommand::SetCustomMaterialFragmentInputs {
                            id,
                            inputs: prev,
                        }))
                    }
                    None => Ok(None),
                }
            }
            EditorCommand::SetMaterialUniform {
                material,
                name,
                value,
            } => match find_material(&self.custom_materials, material) {
                Some(mat) => {
                    let mut slots = mat.uniforms.get_cloned();
                    let Some(slot) = slots.iter_mut().find(|s| s.name == name) else {
                        return Ok(None);
                    };
                    let prev = slot.val.clone();
                    slot.val = value;
                    mat.uniforms.set(slots);
                    mark_material_draft(&mat);
                    self.dirty.set_neq(true);
                    Ok(Some(EditorCommand::SetMaterialUniform {
                        material,
                        name,
                        value: prev,
                    }))
                }
                None => Ok(None),
            },
            EditorCommand::SetBuiltinParam { node, param, value } => {
                match mutate::find_by_id(&self.scene, node) {
                    Some(n) => {
                        let prev = n.kind.get_cloned();
                        let mut next = prev.clone();
                        let patched = patch_builtin_param(&mut next, param, &value);
                        if !patched {
                            return Ok(None);
                        }
                        n.kind.set(next);
                        self.scene.bump_revision();
                        Ok(Some(EditorCommand::SetKind {
                            id: node,
                            kind: Box::new(prev),
                        }))
                    }
                    None => Ok(None),
                }
            }
            EditorCommand::SetBuiltinTexture {
                node,
                slot,
                texture,
            } => match mutate::find_by_id(&self.scene, node) {
                Some(n) => {
                    let prev = n.kind.get_cloned();
                    let mut next = prev.clone();
                    if !patch_builtin_texture(&mut next, slot, texture) {
                        return Ok(None);
                    }
                    n.kind.set(next);
                    self.scene.bump_revision();
                    Ok(Some(EditorCommand::SetKind {
                        id: node,
                        kind: Box::new(prev),
                    }))
                }
                None => Ok(None),
            },
            EditorCommand::SetLightParam { node, param, value } => {
                match mutate::find_by_id(&self.scene, node) {
                    Some(n) => {
                        let prev = n.kind.get_cloned();
                        let NodeKind::Light(mut cfg) = prev.clone() else {
                            return Ok(None);
                        };
                        if !patch_light_param(&mut cfg, param, &value) {
                            return Ok(None);
                        }
                        // A light edit churns the renderer LightKey a lowered
                        // animation channel holds — force a re-lower (same as the
                        // SetKind light path).
                        self.anim_revision.replace_with(|v| v.wrapping_add(1));
                        n.kind.set(NodeKind::Light(cfg));
                        self.scene.bump_revision();
                        Ok(Some(EditorCommand::SetKind {
                            id: node,
                            kind: Box::new(prev),
                        }))
                    }
                    None => Ok(None),
                }
            }
            EditorCommand::SetFrameTime { seconds } => {
                crate::engine::context::with_renderer_mut(move |r| r.set_time_source(seconds))
                    .await;
                Ok(None)
            }
            EditorCommand::ClearFrameTime => {
                crate::engine::context::with_renderer_mut(|r| r.clear_time_source()).await;
                Ok(None)
            }
            EditorCommand::SetMorphWeight { node, index, value } => {
                // Live renderer poke (transient — see the protocol doc comment):
                // node → materialized mesh(es) → geometry AND material morph
                // buffers, weights[index] = value. Mirrors what a morph animation
                // track write does per frame (animations.rs), so the preview is
                // exactly what playback would produce. Out-of-range index or a
                // morph-less node is a silent no-op; read back via `MorphData`.
                let meshes = renderer_meshes_for_node(node);
                crate::engine::context::with_renderer_mut(move |r| {
                    for mesh in meshes {
                        if let Some(key) = r.meshes.geometry_morph_key_for_mesh(mesh) {
                            let _ = r.meshes.morphs.geometry.update_morph_weights_with(
                                key,
                                |weights| {
                                    if let Some(w) = weights.get_mut(index as usize) {
                                        *w = value;
                                    }
                                },
                            );
                        }
                        if let Some(key) = r.meshes.material_morph_key_for_mesh(mesh) {
                            let _ = r.meshes.morphs.material.update_morph_weights_with(
                                key,
                                |weights| {
                                    if let Some(w) = weights.get_mut(index as usize) {
                                        *w = value;
                                    }
                                },
                            );
                        }
                    }
                })
                .await;
                Ok(None)
            }
            EditorCommand::SetMaterialTexture {
                node,
                slot,
                texture,
            } => match mutate::find_by_id(&self.scene, node) {
                Some(n) => {
                    let prev = n.kind.get_cloned();
                    let mut next = prev.clone();
                    if !patch_material_texture(&mut next, &slot, texture) {
                        return Ok(None);
                    }
                    n.kind.set(next);
                    self.scene.bump_revision();
                    Ok(Some(EditorCommand::SetKind {
                        id: node,
                        kind: Box::new(prev),
                    }))
                }
                None => Ok(None),
            },
            EditorCommand::SetMaterialBuffer { node, slot, data } => {
                match mutate::find_by_id(&self.scene, node) {
                    Some(n) => {
                        let prev = n.kind.get_cloned();
                        let mut next = prev.clone();
                        if !patch_material_buffer(&mut next, &slot, data) {
                            return Ok(None);
                        }
                        n.kind.set(next);
                        self.scene.bump_revision();
                        Ok(Some(EditorCommand::SetKind {
                            id: node,
                            kind: Box::new(prev),
                        }))
                    }
                    None => Ok(None),
                }
            }
            EditorCommand::SetEnvironment { env } => {
                let prev = self.scene.environment.get_cloned();
                self.scene.environment.set(env);
                self.scene.bump_revision();
                Ok(Some(EditorCommand::SetEnvironment { env: prev }))
            }
            EditorCommand::SnapCameraToAxis { axis } => {
                use std::f32::consts::PI;
                // Just under ±90° for top/bottom to dodge the look-at gimbal.
                let top = PI / 2.0 - 0.001;
                let (yaw, pitch) = match axis {
                    CameraAxis::PosZ => (0.0, 0.0),
                    CameraAxis::NegZ => (PI, 0.0),
                    CameraAxis::PosX => (PI / 2.0, 0.0),
                    CameraAxis::NegX => (-PI / 2.0, 0.0),
                    CameraAxis::PosY => (0.0, top),
                    CameraAxis::NegY => (0.0, -top),
                };
                crate::engine::context::try_with_camera_mut(|c| c.snap_to(yaw, pitch));
                Ok(None)
            }
            EditorCommand::ResetCamera => {
                crate::engine::context::try_with_camera_mut(|c| c.reset_default());
                Ok(None)
            }
            EditorCommand::SetCameraOrbit {
                yaw,
                pitch,
                radius,
                look_at,
            } => {
                crate::engine::context::try_with_camera_mut(|c| {
                    c.set_orbit(yaw, pitch, radius, glam::Vec3::from_array(look_at))
                });
                Ok(None)
            }
            EditorCommand::SetCameraProjection { perspective, fov_y } => {
                use awsm_web_shared::util::free_camera::ProjectionMode;
                crate::engine::context::try_with_camera_mut(|c| {
                    if let Some(f) = fov_y {
                        c.set_fov_y(f);
                    }
                    c.set_projection_mode(if perspective {
                        ProjectionMode::Perspective
                    } else {
                        ProjectionMode::Orthographic
                    });
                });
                // Mirror into the reactive flag so the viewport toggle / shortcut
                // reflect the current mode regardless of who changed it (incl. MCP).
                self.settings.editor_ortho.set_neq(!perspective);
                Ok(None)
            }
            EditorCommand::FrameNode { node, padding } => {
                // Prefer the renderer's LIVE world AABB (union over the node's
                // materialized meshes) — same policy as the NodeBounds query;
                // the scene-side local box is a unit-cube fallback for
                // populate-baked SkinnedMesh nodes, which made frame_node aim
                // at nothing on imported rigs. Resolve meshes BEFORE the
                // renderer lock (renderer_meshes_for_node locks bridge nodes).
                let local =
                    mutate::find_by_id(&self.scene, node).map(|n| local_aabb(&n.kind.get_cloned()));
                let Some((lmin, lmax)) = local else {
                    return Ok(None);
                };
                let meshes = renderer_meshes_for_node(node);
                let tk = {
                    let b = crate::engine::bridge::bridge();
                    let nodes = b.nodes.lock().unwrap();
                    nodes.get(&node).map(|n| n.transform_key)
                };
                crate::engine::context::with_renderer_mut(move |r| {
                    let live = meshes
                        .iter()
                        .filter_map(|mk| {
                            r.meshes
                                .get(*mk)
                                .ok()
                                .and_then(|mesh| mesh.world_aabb.clone())
                        })
                        .reduce(|mut acc, b| {
                            acc.extend(&b);
                            acc
                        });
                    let aabb = live.unwrap_or_else(|| {
                        let world = tk
                            .and_then(|tk| r.transforms.get_world(tk).ok().copied())
                            .unwrap_or(glam::Mat4::IDENTITY);
                        let (wmin, wmax) = transform_aabb(world, lmin, lmax);
                        awsm_renderer::bounds::Aabb::new(
                            glam::Vec3::from_array(wmin),
                            glam::Vec3::from_array(wmax),
                        )
                    });
                    crate::engine::context::try_with_camera_mut(|c| {
                        c.frame_aabb(aabb, 1.0 + padding.max(0.0))
                    });
                })
                .await;
                Ok(None)
            }
            EditorCommand::LoadProjectFromUrl { base_url } => {
                match persistence::load_project_from_url(self, &base_url).await {
                    Ok(()) => {
                        self.undo.borrow_mut().clear();
                        self.redo.borrow_mut().clear();
                        self.refresh_history_signals();
                        self.dirty.set_neq(false);
                        Toast::info("Project loaded");
                    }
                    Err(e) => Toast::error(format!("Load failed: {e}")),
                }
                Ok(None)
            }
            EditorCommand::ImportModelFromUrl { url } => {
                let _activity =
                    crate::engine::activity::begin_activity("Inserting model — uploading to GPU…");
                self.finish_model_import(crate::engine::bridge::gltf::import(&url).await);
                Ok(None)
            }
            EditorCommand::ImportModelFromFile { name, url } => {
                let _activity =
                    crate::engine::activity::begin_activity("Inserting model — uploading to GPU…");
                let result = crate::engine::bridge::gltf::import_file(&name, &url).await;
                // The blob: object URL was minted just for this load; release it.
                let _ = web_sys::Url::revoke_object_url(&url);
                self.finish_model_import(result);
                Ok(None)
            }
            EditorCommand::ImportTextureFromUrl { id, url } => {
                // Idempotent: skip if this id already exists (cross-tab replay).
                if self.scene.assets.lock().unwrap().entries.contains_key(&id) {
                    return Ok(None);
                }
                let _activity = crate::engine::activity::begin_activity(
                    "Importing texture — uploading to GPU…",
                );
                match crate::engine::bridge::material::import_texture_url(id, &url).await {
                    Ok(()) => {
                        let name = url
                            .rsplit('/')
                            .next()
                            .filter(|s| !s.is_empty())
                            .unwrap_or("texture")
                            .to_string();
                        self.scene.assets.lock().unwrap().entries.insert(
                            id,
                            AssetEntry::new(SceneAssetSource::Texture(TextureDef::Raster {
                                display_name: name,
                            })),
                        );
                        self.scene.bump_revision();
                        self.asset_selection.set(Some(id));
                        self.dirty.set_neq(true);
                        Toast::info("Imported texture");
                        Ok(Some(EditorCommand::DeleteAsset { id }))
                    }
                    // Fail loudly — the MCP tool surfaces this as an error, not a
                    // silent `ok`.
                    Err(e) => Err(crate::error::EditorError::msg(format!(
                        "texture import failed: {e}"
                    ))),
                }
            }
            EditorCommand::ImportKtxEnvFromUrl { id, url } => {
                // Idempotent (cross-tab replay): skip if this id already exists.
                if self.scene.assets.lock().unwrap().entries.contains_key(&id) {
                    return Ok(None);
                }
                // Register a URL-sourced cubemap asset; the env-sync bridge
                // fetches + decodes the KTX bytes when `SetEnvironment` applies a
                // config that references this id (see `env_sync::load_ktx_by_id`'s
                // `AssetSource::Url` arm). No GPU upload here — unlike a raster
                // texture, the cubemap is materialized lazily at apply time.
                self.scene
                    .assets
                    .lock()
                    .unwrap()
                    .entries
                    .insert(id, AssetEntry::new(SceneAssetSource::Url(url)));
                self.scene.bump_revision();
                self.dirty.set_neq(true);
                Ok(Some(EditorCommand::DeleteAsset { id }))
            }
            // ───────────────────── Animation: clip lifecycle ─────────────────
            EditorCommand::AddClip { id } => {
                // Idempotent: a cross-tab relay replays this; if the clip id
                // already exists (or a self-echo slips through) it's a no-op.
                if find_clip(&self.custom_animations, id).is_none() {
                    let n = self.custom_animations.lock_ref().len() + 1;
                    let clip = CA::new(id, format!("Clip {n}"));
                    self.custom_animations.lock_mut().push_cloned(clip);
                    Toast::info("Created clip");
                }
                self.current_clip.set(Some(id));
                self.dirty.set_neq(true);
                Ok(None)
            }
            EditorCommand::DeleteClip { id } => {
                self.custom_animations.lock_mut().retain(|c| c.id != id);
                if self.current_clip.get() == Some(id) {
                    let next = self.custom_animations.lock_ref().first().map(|c| c.id);
                    self.current_clip.set(next);
                }
                self.dirty.set_neq(true);
                Ok(None)
            }
            EditorCommand::DuplicateClip { id } => {
                let src = find_clip(&self.custom_animations, id);
                if let Some(src) = src {
                    let new_id = AssetId::new();
                    let mut stored = animation::stored_from_live(&src);
                    stored.id = new_id;
                    stored.name = format!("{} copy", stored.name);
                    let clone = animation::stored_to_live(&stored);
                    self.custom_animations.lock_mut().push_cloned(clone);
                    self.current_clip.set(Some(new_id));
                    self.dirty.set_neq(true);
                }
                Ok(None)
            }
            EditorCommand::SetCurrentClip { id } => {
                self.current_clip.set(id);
                Ok(None)
            }
            // ───────────────────── Animation: clip props ─────────────────────
            EditorCommand::RenameClip { id, name } => {
                match find_clip(&self.custom_animations, id) {
                    Some(c) => {
                        let prev = c.name.get_cloned();
                        c.name.set(name);
                        self.dirty.set_neq(true);
                        Ok(Some(EditorCommand::RenameClip { id, name: prev }))
                    }
                    None => Ok(None),
                }
            }
            EditorCommand::SetClipDuration { id, duration } => {
                match find_clip(&self.custom_animations, id) {
                    Some(c) => {
                        let prev = c.duration.get();
                        c.duration.set(duration.max(0.0));
                        self.dirty.set_neq(true);
                        Ok(Some(EditorCommand::SetClipDuration { id, duration: prev }))
                    }
                    None => Ok(None),
                }
            }
            EditorCommand::SetClipLoop { id, loop_style } => {
                match find_clip(&self.custom_animations, id) {
                    Some(c) => {
                        let prev = c.loop_style.get();
                        c.loop_style.set(loop_style);
                        self.dirty.set_neq(true);
                        Ok(Some(EditorCommand::SetClipLoop {
                            id,
                            loop_style: prev,
                        }))
                    }
                    None => Ok(None),
                }
            }
            EditorCommand::SetClipSpeed { id, speed } => {
                match find_clip(&self.custom_animations, id) {
                    Some(c) => {
                        let prev = c.speed.get();
                        c.speed.set(speed);
                        self.dirty.set_neq(true);
                        Ok(Some(EditorCommand::SetClipSpeed { id, speed: prev }))
                    }
                    None => Ok(None),
                }
            }
            EditorCommand::SetClipDirection { id, direction } => {
                match find_clip(&self.custom_animations, id) {
                    Some(c) => {
                        let prev = c.direction.get();
                        c.direction.set(direction);
                        self.dirty.set_neq(true);
                        Ok(Some(EditorCommand::SetClipDirection {
                            id,
                            direction: prev,
                        }))
                    }
                    None => Ok(None),
                }
            }
            EditorCommand::SetClipColor { id, color } => {
                match find_clip(&self.custom_animations, id) {
                    Some(c) => {
                        let prev = c.color.get_cloned();
                        c.color.set(color);
                        self.dirty.set_neq(true);
                        Ok(Some(EditorCommand::SetClipColor { id, color: prev }))
                    }
                    None => Ok(None),
                }
            }
            // ───────────────────── Animation: tracks ─────────────────────────
            EditorCommand::AddTrack { clip, target } => {
                match find_clip(&self.custom_animations, clip) {
                    Some(c) => {
                        let key = animation::target_key(&target);
                        let track = animation::Track::new(target);
                        let index = c.tracks.lock_ref().len();
                        c.tracks.lock_mut().push_cloned(track);
                        self.dirty.set_neq(true);
                        Toast::info(format!("Added track {key}"));
                        Ok(Some(EditorCommand::DeleteTrack { clip, track: index }))
                    }
                    None => Ok(None),
                }
            }
            EditorCommand::DeleteTrack { clip, track } => {
                match find_clip(&self.custom_animations, clip) {
                    Some(c) => {
                        let removed = {
                            let tracks = c.tracks.lock_ref();
                            tracks
                                .get(track)
                                .map(|t| animation::stored_track_from_live(t))
                        };
                        match removed {
                            Some(st) => {
                                c.tracks.lock_mut().remove(track);
                                self.dirty.set_neq(true);
                                Ok(Some(EditorCommand::RestoreTrack {
                                    clip,
                                    index: track,
                                    track: Box::new(st),
                                }))
                            }
                            None => Ok(None),
                        }
                    }
                    None => Ok(None),
                }
            }
            EditorCommand::RestoreTrack { clip, index, track } => {
                match find_clip(&self.custom_animations, clip) {
                    Some(c) => {
                        let live = animation::stored_track_to_live(&track);
                        let mut tracks = c.tracks.lock_mut();
                        let i = index.min(tracks.len());
                        tracks.insert_cloned(i, live);
                        drop(tracks);
                        self.dirty.set_neq(true);
                        Ok(Some(EditorCommand::DeleteTrack { clip, track: index }))
                    }
                    None => Ok(None),
                }
            }
            EditorCommand::SetTrackSampler {
                clip,
                track,
                sampler,
            } => match find_track(&self.custom_animations, clip, track) {
                Some(t) => {
                    let prev = t.sampler.get();
                    t.sampler.set(sampler);
                    self.dirty.set_neq(true);
                    Ok(Some(EditorCommand::SetTrackSampler {
                        clip,
                        track,
                        sampler: prev,
                    }))
                }
                None => Ok(None),
            },
            EditorCommand::SetTrackMute { clip, track, mute } => {
                match find_track(&self.custom_animations, clip, track) {
                    Some(t) => {
                        let prev = t.mute.get();
                        t.mute.set_neq(mute);
                        self.dirty.set_neq(true);
                        Ok(Some(EditorCommand::SetTrackMute {
                            clip,
                            track,
                            mute: prev,
                        }))
                    }
                    None => Ok(None),
                }
            }
            EditorCommand::SetTrackSolo { clip, track, solo } => {
                match find_track(&self.custom_animations, clip, track) {
                    Some(t) => {
                        let prev = t.solo.get();
                        t.solo.set_neq(solo);
                        self.dirty.set_neq(true);
                        Ok(Some(EditorCommand::SetTrackSolo {
                            clip,
                            track,
                            solo: prev,
                        }))
                    }
                    None => Ok(None),
                }
            }
            // ───────────────────── Animation: keyframes ──────────────────────
            EditorCommand::AddKeyframe {
                clip,
                track,
                t,
                value,
            } => match find_track(&self.custom_animations, clip, track) {
                Some(tr) => {
                    let interp = animation::sampler_to_interp(tr.sampler.get());
                    let mut times = tr.times.lock_mut();
                    let mut keys = tr.keys.lock_mut();
                    // Replace an existing key at (almost) the same time, else insert
                    // sorted.
                    if let Some(i) = times.iter().position(|&x| (x - t).abs() < 1.0e-9) {
                        let prev = keys[i].clone();
                        keys[i] = animation::new_keyframe(value, interp);
                        drop(times);
                        drop(keys);
                        self.dirty.set_neq(true);
                        return Ok(Some(EditorCommand::SetKeyframe {
                            clip,
                            track,
                            index: i,
                            t: None,
                            value: Some(prev.value),
                            interp: Some(prev.interp),
                            in_tangent: Some(prev.in_tangent),
                            out_tangent: Some(prev.out_tangent),
                        }));
                    }
                    let pos = times.iter().position(|&x| x > t).unwrap_or(times.len());
                    times.insert(pos, t);
                    keys.insert(pos, animation::new_keyframe(value, interp));
                    drop(times);
                    drop(keys);
                    self.dirty.set_neq(true);
                    Ok(Some(EditorCommand::DeleteKeyframe {
                        clip,
                        track,
                        index: pos,
                    }))
                }
                None => Ok(None),
            },
            EditorCommand::DeleteKeyframe { clip, track, index } => {
                match find_track(&self.custom_animations, clip, track) {
                    Some(tr) => {
                        let mut times = tr.times.lock_mut();
                        let mut keys = tr.keys.lock_mut();
                        if index >= times.len() || index >= keys.len() {
                            return Ok(None);
                        }
                        let t = times.remove(index);
                        let kf = keys.remove(index);
                        drop(times);
                        drop(keys);
                        self.dirty.set_neq(true);
                        Ok(Some(EditorCommand::InsertKeyframe {
                            clip,
                            track,
                            index,
                            t,
                            key: Box::new(kf),
                        }))
                    }
                    None => Ok(None),
                }
            }
            EditorCommand::InsertKeyframe {
                clip,
                track,
                index,
                t,
                key,
            } => match find_track(&self.custom_animations, clip, track) {
                Some(tr) => {
                    let mut times = tr.times.lock_mut();
                    let mut keys = tr.keys.lock_mut();
                    let i = index.min(times.len());
                    times.insert(i, t);
                    keys.insert(i, *key);
                    drop(times);
                    drop(keys);
                    self.dirty.set_neq(true);
                    Ok(Some(EditorCommand::DeleteKeyframe { clip, track, index }))
                }
                None => Ok(None),
            },
            EditorCommand::SetKeyframe {
                clip,
                track,
                index,
                t,
                value,
                interp,
                in_tangent,
                out_tangent,
            } => match find_track(&self.custom_animations, clip, track) {
                Some(tr) => {
                    let mut times = tr.times.lock_mut();
                    let mut keys = tr.keys.lock_mut();
                    if index >= keys.len() {
                        return Ok(None);
                    }
                    let prev_kf = keys[index].clone();
                    let prev_t = times.get(index).copied();
                    if let Some(new_t) = t {
                        if let Some(slot) = times.get_mut(index) {
                            *slot = new_t;
                        }
                    }
                    if let Some(v) = value {
                        keys[index].value = v;
                    }
                    if let Some(i) = interp {
                        keys[index].interp = i;
                    }
                    if let Some(it) = in_tangent {
                        keys[index].in_tangent = it;
                    }
                    if let Some(ot) = out_tangent {
                        keys[index].out_tangent = ot;
                    }
                    drop(times);
                    drop(keys);
                    self.dirty.set_neq(true);
                    Ok(Some(EditorCommand::SetKeyframe {
                        clip,
                        track,
                        index,
                        t: t.and(prev_t),
                        value: value.map(|_| prev_kf.value),
                        interp: interp.map(|_| prev_kf.interp),
                        in_tangent: in_tangent.map(|_| prev_kf.in_tangent),
                        out_tangent: out_tangent.map(|_| prev_kf.out_tangent),
                    }))
                }
                None => Ok(None),
            },
            // ───────────────────── Animation: transport ──────────────────────
            EditorCommand::SetPlayhead { t } => {
                self.playhead.set_neq(t.max(0.0));
                Ok(None)
            }
            EditorCommand::SetPlaying { on } => {
                self.playing.set_neq(on);
                Ok(None)
            }
            EditorCommand::StepPlayhead { kind } => {
                let dur = self
                    .current_clip
                    .get()
                    .and_then(|id| find_clip(&self.custom_animations, id))
                    .map(|c| c.duration.get())
                    .unwrap_or(0.0);
                let cur = self.playhead.get();
                let next = match kind {
                    animation::StepKind::Home => 0.0,
                    animation::StepKind::End => dur,
                    animation::StepKind::Prev => self.adjacent_keyframe_time(cur, false),
                    animation::StepKind::Next => self.adjacent_keyframe_time(cur, true),
                };
                self.playhead.set_neq(next.clamp(0.0, dur.max(0.0)));
                Ok(None)
            }
            EditorCommand::SetAnimFps { fps } => {
                self.anim_fps.set_neq(fps.max(1));
                Ok(None)
            }
            EditorCommand::SetSoloRoot { id } => {
                self.anim_solo_root.set(id);
                Ok(None)
            }
            EditorCommand::SetAnimSelection { sel } => {
                self.anim_selection.set(sel);
                Ok(None)
            }
            EditorCommand::SetAnimView { view } => {
                self.anim_view.set_neq(view);
                Ok(None)
            }
            // ───────────────────── Animation: mixer (NLA) ────────────────────
            EditorCommand::AddLayer => {
                let mut doc = self.anim_mixer.get_cloned();
                let index = doc.layers.len();
                doc.layers.push(animation::LayerDoc::default());
                self.anim_mixer.set(doc);
                self.dirty.set_neq(true);
                Toast::info("Added layer");
                Ok(Some(EditorCommand::DeleteLayer { layer: index }))
            }
            EditorCommand::DeleteLayer { layer } => {
                let mut doc = self.anim_mixer.get_cloned();
                if layer >= doc.layers.len() {
                    return Ok(None);
                }
                let removed = doc.layers.remove(layer);
                self.anim_mixer.set(doc);
                self.dirty.set_neq(true);
                Ok(Some(EditorCommand::RestoreLayer {
                    layer,
                    doc: Box::new(removed),
                }))
            }
            EditorCommand::RestoreLayer { layer, doc } => {
                let mut mixer = self.anim_mixer.get_cloned();
                let i = layer.min(mixer.layers.len());
                mixer.layers.insert(i, *doc);
                self.anim_mixer.set(mixer);
                self.dirty.set_neq(true);
                Ok(Some(EditorCommand::DeleteLayer { layer }))
            }
            EditorCommand::SetLayerMode { layer, mode } => {
                let mut doc = self.anim_mixer.get_cloned();
                match doc.layers.get_mut(layer) {
                    Some(l) => {
                        let prev = l.mode;
                        l.mode = mode;
                        self.anim_mixer.set(doc);
                        self.dirty.set_neq(true);
                        Ok(Some(EditorCommand::SetLayerMode { layer, mode: prev }))
                    }
                    None => Ok(None),
                }
            }
            EditorCommand::SetLayerWeight { layer, weight } => {
                let mut doc = self.anim_mixer.get_cloned();
                match doc.layers.get_mut(layer) {
                    Some(l) => {
                        let prev = l.weight;
                        l.weight = weight.clamp(0.0, 1.0);
                        self.anim_mixer.set(doc);
                        self.dirty.set_neq(true);
                        Ok(Some(EditorCommand::SetLayerWeight {
                            layer,
                            weight: prev,
                        }))
                    }
                    None => Ok(None),
                }
            }
            EditorCommand::SetLayerMask {
                layer,
                nodes,
                include_descendants,
            } => {
                let mut doc = self.anim_mixer.get_cloned();
                match doc.layers.get_mut(layer) {
                    Some(l) => {
                        let prev_nodes = std::mem::replace(&mut l.mask_nodes, nodes);
                        let prev_inc = l.include_descendants;
                        l.include_descendants = include_descendants;
                        self.anim_mixer.set(doc);
                        self.dirty.set_neq(true);
                        Ok(Some(EditorCommand::SetLayerMask {
                            layer,
                            nodes: prev_nodes,
                            include_descendants: prev_inc,
                        }))
                    }
                    None => Ok(None),
                }
            }
            EditorCommand::AddStrip {
                layer,
                clip,
                start,
                len,
            } => {
                let mut doc = self.anim_mixer.get_cloned();
                match doc.layers.get_mut(layer) {
                    Some(l) => {
                        let index = l.strips.len();
                        l.strips.push(animation::StripDoc {
                            clip,
                            start,
                            len,
                            scale: 1.0,
                            repeat: false,
                        });
                        self.anim_mixer.set(doc);
                        self.dirty.set_neq(true);
                        Ok(Some(EditorCommand::DeleteStrip {
                            layer,
                            strip: index,
                        }))
                    }
                    None => Ok(None),
                }
            }
            EditorCommand::DeleteStrip { layer, strip } => {
                let mut doc = self.anim_mixer.get_cloned();
                match doc.layers.get_mut(layer) {
                    Some(l) if strip < l.strips.len() => {
                        let removed = l.strips.remove(strip);
                        self.anim_mixer.set(doc);
                        self.dirty.set_neq(true);
                        Ok(Some(EditorCommand::RestoreStrip {
                            layer,
                            strip,
                            doc: Box::new(removed),
                        }))
                    }
                    _ => Ok(None),
                }
            }
            EditorCommand::RestoreStrip { layer, strip, doc } => {
                let mut mixer = self.anim_mixer.get_cloned();
                match mixer.layers.get_mut(layer) {
                    Some(l) => {
                        let i = strip.min(l.strips.len());
                        l.strips.insert(i, *doc);
                        self.anim_mixer.set(mixer);
                        self.dirty.set_neq(true);
                        Ok(Some(EditorCommand::DeleteStrip { layer, strip }))
                    }
                    None => Ok(None),
                }
            }
            EditorCommand::MoveStrip {
                layer,
                strip,
                start,
            } => {
                let mut doc = self.anim_mixer.get_cloned();
                match doc
                    .layers
                    .get_mut(layer)
                    .and_then(|l| l.strips.get_mut(strip))
                {
                    Some(s) => {
                        let prev = s.start;
                        s.start = start;
                        self.anim_mixer.set(doc);
                        self.dirty.set_neq(true);
                        Ok(Some(EditorCommand::MoveStrip {
                            layer,
                            strip,
                            start: prev,
                        }))
                    }
                    None => Ok(None),
                }
            }
            EditorCommand::TrimStrip {
                layer,
                strip,
                start,
                len,
            } => {
                let mut doc = self.anim_mixer.get_cloned();
                match doc
                    .layers
                    .get_mut(layer)
                    .and_then(|l| l.strips.get_mut(strip))
                {
                    Some(s) => {
                        let (ps, pl) = (s.start, s.len);
                        s.start = start;
                        s.len = len.max(0.0);
                        self.anim_mixer.set(doc);
                        self.dirty.set_neq(true);
                        Ok(Some(EditorCommand::TrimStrip {
                            layer,
                            strip,
                            start: ps,
                            len: pl,
                        }))
                    }
                    None => Ok(None),
                }
            }
            EditorCommand::SetStripRepeat {
                layer,
                strip,
                repeat,
            } => {
                let mut doc = self.anim_mixer.get_cloned();
                match doc
                    .layers
                    .get_mut(layer)
                    .and_then(|l| l.strips.get_mut(strip))
                {
                    Some(s) => {
                        let prev = s.repeat;
                        s.repeat = repeat;
                        self.anim_mixer.set(doc);
                        self.dirty.set_neq(true);
                        Ok(Some(EditorCommand::SetStripRepeat {
                            layer,
                            strip,
                            repeat: prev,
                        }))
                    }
                    None => Ok(None),
                }
            }
        }
    }

    /// The keyframe time nearest to `from` in `next`/prev direction across all
    /// tracks of the active clip (for the transport step buttons). Falls back to
    /// `from` when there's nothing in that direction.
    fn adjacent_keyframe_time(&self, from: f64, forward: bool) -> f64 {
        let Some(clip) = self
            .current_clip
            .get()
            .and_then(|id| find_clip(&self.custom_animations, id))
        else {
            return from;
        };
        let mut best: Option<f64> = None;
        for track in clip.tracks.lock_ref().iter() {
            for &t in track.times.lock_ref().iter() {
                let candidate = if forward {
                    t > from + 1.0e-9
                } else {
                    t < from - 1.0e-9
                };
                if candidate {
                    best = Some(match best {
                        Some(b) if forward => b.min(t),
                        Some(b) => b.max(t),
                        None => t,
                    });
                }
            }
        }
        best.unwrap_or(from)
    }

    /// Shared tail for the two model-import commands. On success, *deconstruct*
    /// the imported glTF into the editor scene tree: every glTF node becomes an
    /// editor node (a `Group` for transform/bone nodes, a `Model` for
    /// mesh-bearing nodes), preserving the hierarchy + local transforms. The
    /// node template is cached under a freshly-minted source-file `AssetId` so
    /// each `Model` node can find + duplicate its meshes (see
    /// `node_sync::materialize_model`). On failure, surface the error.
    fn finish_model_import(&self, result: Result<crate::engine::bridge::gltf::GltfImport, String>) {
        let import = match result {
            Ok(i) => i,
            Err(e) => {
                Toast::error(format!("Import failed: {e}"));
                return;
            }
        };

        if import.template.roots.is_empty() {
            Toast::error("This model contains no nodes to insert");
            return;
        }

        // Bring the imported materials into the **assignable library** (so they
        // can be used on any mesh) and wire them onto the model's meshes — with
        // their textures preserved by reusing the renderer textures populate
        // already uploaded (see `gltf::ExtractedMaterial`). Each glTF material
        // becomes a built-in PBR library material; its textures become texture
        // assets (deduped by baked key) pre-registered to the baked GPU texture.
        use awsm_editor_protocol::MaterialShading;

        let mut tex_for_key: std::collections::HashMap<
            awsm_renderer::textures::TextureKey,
            AssetId,
        > = std::collections::HashMap::new();
        let mut texture_entries: Vec<(AssetId, String)> = Vec::new();
        let mut mat_ids: Vec<AssetId> = Vec::with_capacity(import.materials.len());

        for ex in &import.materials {
            let label = if ex.def.label.is_empty() {
                "Material".to_string()
            } else {
                ex.def.label.clone()
            };
            let mut def = ex.def.clone();
            def.base_color_texture = ensure_import_texture(
                &mut tex_for_key,
                &mut texture_entries,
                ex.textures.base_color,
                &format!("{label} · base color"),
            );
            def.metallic_roughness_texture = ensure_import_texture(
                &mut tex_for_key,
                &mut texture_entries,
                ex.textures.metallic_roughness,
                &format!("{label} · metal/rough"),
            );
            def.normal_texture = ensure_import_texture(
                &mut tex_for_key,
                &mut texture_entries,
                ex.textures.normal,
                &format!("{label} · normal"),
            );
            def.occlusion_texture = ensure_import_texture(
                &mut tex_for_key,
                &mut texture_entries,
                ex.textures.occlusion,
                &format!("{label} · occlusion"),
            );
            def.emissive_texture = ensure_import_texture(
                &mut tex_for_key,
                &mut texture_entries,
                ex.textures.emissive,
                &format!("{label} · emissive"),
            );
            // KHR-extension texture slots (clearcoat normal map, specular colour
            // map, sheen colour map, …): create a texture asset for each + write
            // the TextureRef onto the matching extension field.
            for (slot, baked) in &ex.ext_textures {
                let tref = ensure_import_texture(
                    &mut tex_for_key,
                    &mut texture_entries,
                    Some(*baked),
                    &format!("{label} · {slot}"),
                );
                set_ext_texture(&mut def.extensions, slot, tref);
            }

            // A built-in PBR library material carrying the full variant def.
            let lib_id = AssetId::new();
            let mat = CM::new_builtin(lib_id, label, MaterialShading::Pbr);
            let c = def.base_color;
            mat.color.set(format!(
                "#{:02x}{:02x}{:02x}",
                (c[0].clamp(0.0, 1.0) * 255.0) as u8,
                (c[1].clamp(0.0, 1.0) * 255.0) as u8,
                (c[2].clamp(0.0, 1.0) * 255.0) as u8
            ));
            mat.double_sided.set_neq(def.double_sided);
            mat.builtin.set(Some(def));
            self.custom_materials.lock_mut().push_cloned(mat);
            mat_ids.push(lib_id);
        }

        // Track the source file + the texture assets in the table; record the
        // library material + texture ids on the file entry so `materialize_model`
        // can wire each mesh to its extracted material.
        let img_ids: Vec<AssetId> = texture_entries.iter().map(|(id, _)| *id).collect();
        let asset_id = {
            let mut table = self.scene.assets.lock().unwrap();
            for (id, name) in &texture_entries {
                table.entries.insert(
                    *id,
                    AssetEntry::new(SceneAssetSource::Texture(TextureDef::Raster {
                        display_name: name.clone(),
                    })),
                );
            }
            let id = AssetId::new();
            let mut entry =
                AssetEntry::new(SceneAssetSource::Filename(import.display_name.clone()));
            entry.gltf_material_asset_ids = mat_ids.clone();
            entry.gltf_image_asset_ids = img_ids;
            table.entries.insert(id, entry);
            id
        };
        let template = Arc::new(import.template);
        // Cache the node template under the source-file `AssetId` so any
        // `SkinnedMesh` node from this import can resolve its populate-baked
        // renderer mesh keys (see `node_sync::materialize_skinned_mesh`). Only
        // skinned imports actually consult it; static geometry baked to captured
        // meshes ignores it — but it's cheap + keeps the path uniform.
        crate::engine::bridge::bridge().insert_template(asset_id, template.clone());

        // Cache the import's clean rig glb (built at import for skinned files)
        // under the source-file id, so the player bundle can ship it as
        // `assets/<source>.glb` for this import's `SkinnedMesh` nodes.
        if let Some(glb) = import.skinned_glb {
            crate::engine::bridge::skinned_bake_cache::store_rig_glb(asset_id, glb);
        }

        // glTF primitives with no material use glTF's default material — white,
        // metallic 1.0, roughness 1.0 (NOT the editor's magenta sentinel, which is
        // for deliberately-unassigned meshes). Create one shared "Default"
        // library material iff the model actually has unmaterialed primitives.
        let default_mat_id = if template.roots.iter().any(template_needs_default_material) {
            let id = AssetId::new();
            let def = awsm_editor_protocol::MaterialDef {
                base_color: [1.0, 1.0, 1.0, 1.0],
                metallic: 1.0,
                roughness: 1.0,
                ..Default::default()
            };
            let mat = CM::new_builtin(id, "Default".to_string(), MaterialShading::Pbr);
            mat.builtin.set(Some(def));
            self.custom_materials.lock_mut().push_cloned(mat);
            Some(id)
        } else {
            None
        };

        // Mirror the glTF hierarchy as editor nodes under the scene root. Pass
        // the per-glTF-material library ids so each mesh node is assigned its
        // material (one per node; multi-material nodes are destructured).
        // Built while mirroring the tree: glTF node index → minted editor NodeId.
        // Imported animation channels (keyed by glTF node index) resolve through
        // this to bind onto the real scene nodes.
        let mut node_map: std::collections::HashMap<u32, NodeId> = std::collections::HashMap::new();
        let mut roots: Vec<std::sync::Arc<crate::engine::scene::node::Node>> = Vec::new();
        for root in &template.roots {
            roots.push(build_editor_subtree(
                root,
                asset_id,
                &mat_ids,
                default_mat_id,
                &import.node_meshes,
                Some(&import.display_name),
                &mut node_map,
            ));
        }
        // With `node_map` now complete, build the bone correspondence — each skin
        // joint's bone `NodeId` → its node index in the re-exported clean rig glb
        // (`node_flat_indices`) — and stamp it onto every `SkinnedMesh` node's
        // `skin.joints`. This is what lets the player drive the rig's baked joints
        // from our clips (which target bone NodeIds). Patched on the in-memory
        // nodes BEFORE insertion, so no `node_sync` observer re-materializes on the
        // kind change. Shared across all skinned nodes of this import (one rig).
        let skin_joints =
            assemble_skin_joints(&template.roots, &node_map, &import.node_flat_indices);
        if !skin_joints.is_empty() {
            for root in &roots {
                patch_skin_joints(root, &skin_joints);
            }
        }
        for root in roots {
            mutate::insert_under(&self.scene, None, root);
        }
        self.scene.bump_revision();
        self.dirty.set_neq(true);

        // Skin bridge (#2): for every skinned-model bone, map the editor mirror
        // node → the baked joint TransformKey the renderer skin reads. The
        // per-frame `skin_bridge` copies the mirror's local onto the baked key so
        // animating/posing the bone deforms the skin (otherwise the mesh freezes:
        // the skin reads the baked copy, the animation drives the mirror).
        {
            fn walk_skin_joints(
                nodes: &[crate::engine::bridge::asset_template::AssetTemplateNode],
                node_map: &std::collections::HashMap<u32, NodeId>,
                count: &mut usize,
            ) {
                let bridge = crate::engine::bridge::bridge();
                for n in nodes {
                    if n.is_skin_joint {
                        if let Some(node_id) = node_map.get(&n.gltf_node_index) {
                            bridge.register_skin_joint(*node_id, n.baked_transform_key);
                            *count += 1;
                        }
                    }
                    walk_skin_joints(&n.children, node_map, count);
                }
            }
            let mut skin_joint_count = 0;
            walk_skin_joints(&template.roots, &node_map, &mut skin_joint_count);
            tracing::debug!("skin bridge: registered {skin_joint_count} skin-joint mappings");
        }

        // Convert each extracted glTF animation → a library clip bound to the
        // freshly-instantiated nodes (channels for un-instantiated nodes skip).
        let clip_count =
            self.import_animations(&import.animations, &node_map, &import.display_name);

        if clip_count > 0 {
            Toast::info(format!(
                "Imported {} ({clip_count} clip{})",
                import.display_name,
                if clip_count == 1 { "" } else { "s" }
            ));
        } else {
            Toast::info(format!("Imported {}", import.display_name));
        }
    }

    /// Convert extracted glTF animations into library [`CustomAnimation`] clips
    /// bound (via `node_map`: glTF node index → editor `NodeId`) to the imported
    /// scene nodes. A channel targeting a node we didn't instantiate is skipped
    /// with a warning. Returns the number of clips actually created.
    fn import_animations(
        &self,
        animations: &[awsm_renderer_gltf::extract::ExtractedAnimation],
        node_map: &std::collections::HashMap<u32, NodeId>,
        model_name: &str,
    ) -> usize {
        use animation::{Keyframe, TransformProp};
        use awsm_renderer::animation::{AnimationData, AnimationSampler};
        use awsm_renderer_gltf::extract::ExtractedProperty;

        // Library-clip swatch palette (mirrors the AddClip color scheme).
        const CLIP_COLORS: [&str; 6] = [
            "#7aa2f7", "#9ece6a", "#e0af68", "#f7768e", "#bb9af7", "#7dcfff",
        ];

        let mut created = 0usize;
        for (anim_i, anim) in animations.iter().enumerate() {
            let id = AssetId::new();
            // Index into the swatch palette by the clip's library position (pushes
            // from earlier iterations are already reflected in the live length).
            let base = self.custom_animations.lock_ref().len();
            // Prefer the glTF animation's own name; otherwise name the clip after
            // the model (e.g. `CesiumMan`), suffixing an index only when the file
            // carries several animations — clearer than a generic "Animation N".
            let model = model_name
                .strip_suffix(".glb")
                .or_else(|| model_name.strip_suffix(".gltf"))
                .unwrap_or(model_name);
            let name = anim
                .name
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| {
                    if animations.len() > 1 {
                        format!("{model} {}", anim_i + 1)
                    } else {
                        model.to_string()
                    }
                });
            let clip = CA::new(id, name);
            clip.color
                .set(CLIP_COLORS[base % CLIP_COLORS.len()].to_string());

            let mut tracks: Vec<Arc<Track>> = Vec::new();
            let mut max_duration = 0.0_f64;

            for channel in &anim.channels {
                let node = match node_map.get(&(channel.node_index as u32)) {
                    Some(n) => *n,
                    None => {
                        tracing::warn!(
                            "imported animation channel targets un-instantiated glTF node {} — skipping",
                            channel.node_index
                        );
                        continue;
                    }
                };

                let sampler = &channel.clip.sampler;
                let sampler_kind = match sampler {
                    AnimationSampler::Linear { .. } => SamplerKind::Linear,
                    AnimationSampler::Step { .. } => SamplerKind::Step,
                    AnimationSampler::CubicSpline { .. } => SamplerKind::Cubic,
                };
                let interp = animation::sampler_to_interp(sampler_kind);

                // The track's target + a value-extractor that pulls the right
                // component out of an `AnimationData` for this property.
                let (target, extract): (TrackTarget, fn(&AnimationData) -> TrackValue) =
                    match channel.property {
                        ExtractedProperty::Translation => (
                            TrackTarget::Transform {
                                node,
                                prop: TransformProp::Translation,
                            },
                            extract_translation,
                        ),
                        ExtractedProperty::Rotation => (
                            TrackTarget::Transform {
                                node,
                                prop: TransformProp::Rotation,
                            },
                            extract_rotation,
                        ),
                        ExtractedProperty::Scale => (
                            TrackTarget::Transform {
                                node,
                                prop: TransformProp::Scale,
                            },
                            extract_scale,
                        ),
                        // Per-target-index morph splitting is out of scope: bind
                        // index 0 only (weight[0] of each key).
                        ExtractedProperty::MorphWeights => {
                            (TrackTarget::Morph { node, index: 0 }, extract_morph0)
                        }
                    };

                let times: Vec<f64> = sampler.times().to_vec();
                let values: &[AnimationData] = sampler_values(sampler);
                let (in_tangents, out_tangents): (&[AnimationData], &[AnimationData]) =
                    match sampler {
                        AnimationSampler::CubicSpline {
                            in_tangents,
                            out_tangents,
                            ..
                        } => (in_tangents, out_tangents),
                        _ => (&[], &[]),
                    };

                let keys: Vec<Keyframe> = times
                    .iter()
                    .enumerate()
                    .map(|(i, _)| {
                        let value = values
                            .get(i)
                            .map(extract)
                            .unwrap_or_else(|| TrackValue::Scalar(0.0));
                        let (in_tangent, out_tangent) =
                            if matches!(sampler, AnimationSampler::CubicSpline { .. }) {
                                let it = in_tangents
                                    .get(i)
                                    .map(extract)
                                    .unwrap_or_else(|| animation::zeroed_like(&value));
                                let ot = out_tangents
                                    .get(i)
                                    .map(extract)
                                    .unwrap_or_else(|| animation::zeroed_like(&value));
                                (it, ot)
                            } else {
                                let z = animation::zeroed_like(&value);
                                (z, z)
                            };
                        Keyframe {
                            value,
                            interp,
                            in_tangent,
                            out_tangent,
                        }
                    })
                    .collect();

                max_duration = max_duration.max(channel.clip.duration);

                let track = Track::new(target);
                track.sampler.set(sampler_kind);
                track.times.set(times);
                track.keys.set(keys);
                tracks.push(track);
            }

            if max_duration > 0.0 {
                clip.duration.set(max_duration);
            }
            clip.tracks.lock_mut().replace_cloned(tracks);

            self.custom_animations.lock_mut().push_cloned(clip);
            if self.current_clip.get().is_none() {
                self.current_clip.set(Some(id));
            }
            created += 1;
        }
        created
    }

    /// Pop the newest inverse and apply it; its own inverse becomes a redo entry.
    pub async fn undo(&self) {
        let cmd = self.undo.borrow_mut().pop();
        if let Some(cmd) = cmd {
            if let Ok(Some(inv)) = self.apply(cmd).await {
                self.redo.borrow_mut().push(inv);
            }
            self.refresh_history_signals();
        }
    }

    /// Re-apply the newest redo entry.
    pub async fn redo(&self) {
        let cmd = self.redo.borrow_mut().pop();
        if let Some(cmd) = cmd {
            if let Ok(Some(inv)) = self.apply(cmd).await {
                self.undo.borrow_mut().push(inv);
            }
            self.refresh_history_signals();
        }
    }

    fn refresh_history_signals(&self) {
        self.can_undo.set_neq(!self.undo.borrow().is_empty());
        self.can_redo.set_neq(!self.redo.borrow().is_empty());
    }

    /// Clear the undo/redo log (after a project load — the prior history doesn't
    /// apply to the freshly-loaded scene).
    pub fn reset_history(&self) {
        self.undo.borrow_mut().clear();
        self.redo.borrow_mut().clear();
        self.refresh_history_signals();
    }

    /// A fresh, unique-ish display label for a new asset (`"{kind} N"`), counting
    /// existing material assets so the Content Browser doesn't show duplicates.
    fn next_asset_label(&self, kind: &str) -> String {
        let n = self
            .scene
            .assets
            .lock()
            .unwrap()
            .entries
            .values()
            .filter(|e| matches!(e.source, SceneAssetSource::Material(_)))
            .count()
            + 1;
        format!("{kind} {n}")
    }

    /// A serializable read of editor state for external inspection.
    pub fn snapshot(&self) -> EditorSnapshot {
        let scene_tree = self
            .scene
            .nodes
            .lock_ref()
            .iter()
            .map(|n| spec_from_node(n).to_query())
            .collect();
        EditorSnapshot {
            mode: self.mode.get(),
            project: ProjectSnapshot {
                name: self.project_name.get_cloned(),
                dirty: self.dirty.get(),
                missing_assets: self.missing_assets.get_cloned(),
                coordinate_system: "right-handed, Y-up, -Z forward".to_string(),
                units: self.settings.units.get_cloned(),
            },
            scene_tree,
            selection: self
                .selected
                .get_cloned()
                .iter()
                .map(|id| id.to_string())
                .collect(),
            undo_depth: self.undo.borrow().len(),
            redo_depth: self.redo.borrow().len(),
            animation: self.animation_snapshot(),
            materials: self
                .custom_materials
                .lock_ref()
                .iter()
                .map(|m| {
                    let errors = m.last_diagnostics.get_cloned();
                    query::MaterialSnapshot {
                        id: m.id.to_string(),
                        name: m.name.get_cloned(),
                        registered: m.registered.get(),
                        builtin: m.builtin.lock_ref().is_some(),
                        uniforms: m
                            .uniforms
                            .lock_ref()
                            .iter()
                            .map(|s| s.name.clone())
                            .collect(),
                        compile_ok: errors.is_empty(),
                        errors,
                    }
                })
                .collect(),
            textures: self.texture_snapshots(),
        }
    }

    /// Project texture assets into the snapshot (id / name / procedural-vs-raster).
    fn texture_snapshots(&self) -> Vec<query::TextureSnapshot> {
        use awsm_editor_protocol::{AssetSource as S, TextureDef};
        let assets = self.scene.assets.lock().unwrap();
        assets
            .entries
            .iter()
            .filter_map(|(id, entry)| match &entry.source {
                S::Texture(def) => {
                    let kind = match def {
                        TextureDef::Procedural(_) => "procedural",
                        TextureDef::Raster { .. } => "raster",
                    };
                    let name = entry
                        .source
                        .display_name()
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| format!("texture {id}"));
                    Some(query::TextureSnapshot {
                        id: id.to_string(),
                        name,
                        kind: kind.to_string(),
                    })
                }
                _ => None,
            })
            .collect()
    }

    /// The Animation-mode projection of `snapshot()`.
    fn animation_snapshot(&self) -> query::AnimationSnapshot {
        use crate::controller::animation::TrackTarget;
        let clips = self
            .custom_animations
            .lock_ref()
            .iter()
            .map(|c| {
                let tracks = c
                    .tracks
                    .lock_ref()
                    .iter()
                    .map(|t| {
                        let target = match &t.target {
                            TrackTarget::Transform { prop, .. } => format!("transform:{prop:?}"),
                            TrackTarget::Morph { index, .. } => format!("morph:{index}"),
                            TrackTarget::Uniform { name, .. } => format!("uniform:{name}"),
                            TrackTarget::BuiltinParam { param, .. } => format!("builtin:{param:?}"),
                            TrackTarget::Light { param, .. } => format!("light:{param:?}"),
                            TrackTarget::Camera { param, .. } => format!("camera:{param:?}"),
                        };
                        query::TrackSnapshot {
                            target: target.to_lowercase(),
                            keys: t.keys.lock_ref().len(),
                        }
                    })
                    .collect();
                query::ClipSnapshot {
                    id: c.id.to_string(),
                    name: c.name.get_cloned(),
                    duration: c.duration.get(),
                    tracks,
                }
            })
            .collect();
        query::AnimationSnapshot {
            clips,
            current_clip: self.current_clip.get().map(|id| id.to_string()),
            playhead: self.playhead.get(),
            playing: self.playing.get(),
            fps: self.anim_fps.get(),
            solo_root: self.anim_solo_root.get().map(|id| id.to_string()),
            mixer_layers: self.anim_mixer.lock_ref().layers.len(),
        }
    }

    /// `snapshot()` as a JSON string (the shape an MCP/websocket transport would
    /// return). Used by headless tests + the future external transport.
    pub fn snapshot_json(&self) -> String {
        serde_json::to_string(&self.snapshot()).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
    }

    /// Run a read-only [`EditorQuery`] and return a serializable result.
    /// Read-only: never mutates persisted state, never records undo, never
    /// broadcasts; the pinning handler saves + restores the transport.
    pub async fn query(&self, q: query::EditorQuery) -> query::QueryResult {
        use query::*;
        match q {
            EditorQuery::Snapshot => QueryResult::Snapshot(Box::new(self.snapshot())),
            EditorQuery::SampleClipTimeseries {
                clip,
                times,
                targets,
            } => self.sample_clip_timeseries(clip, times, targets).await,
            EditorQuery::CanvasPixels { coords } => {
                match crate::engine::query::canvas_pixels(&coords).await {
                    Ok(pixels) => QueryResult::Pixels(PixelsResult { pixels }),
                    Err(e) => QueryResult::Error { error: e },
                }
            }
            EditorQuery::CanvasStats { region } => {
                match crate::engine::query::canvas_stats(region).await {
                    Ok(s) => QueryResult::Stats(s),
                    Err(e) => QueryResult::Error { error: e },
                }
            }
            EditorQuery::CustomMaterialWgsl { material } => {
                match find_material(&self.custom_materials, material) {
                    Some(mat) => QueryResult::Text(mat.wgsl.get_cloned()),
                    None => QueryResult::Error {
                        error: format!("no custom material {material}"),
                    },
                }
            }
            EditorQuery::MaterialDiagnostics { material } => {
                match find_material(&self.custom_materials, material) {
                    Some(mat) => {
                        let errors = mat.last_diagnostics.get_cloned();
                        QueryResult::Diagnostics(CompileDiagnostics {
                            registered: mat.registered.get(),
                            ok: errors.is_empty(),
                            errors,
                        })
                    }
                    None => QueryResult::Error {
                        error: format!("no custom material {material}"),
                    },
                }
            }
            EditorQuery::ExportGlb { node } => {
                // Whole-scene export includes animations; single-node does not.
                let result = match node {
                    Some(id) => crate::controller::export::export_glb(&self.scene, Some(id)).await,
                    None => crate::controller::export::export_scene_glb(self).await,
                };
                match result {
                    Ok(bytes) => {
                        use base64::Engine;
                        QueryResult::Text(base64::engine::general_purpose::STANDARD.encode(bytes))
                    }
                    Err(e) => QueryResult::Error { error: e },
                }
            }
            EditorQuery::ExportPlayerBundle { name } => {
                use base64::Engine;
                use serde_json::json;
                // The runtime bundle directory (scene.toml + assets/, per the
                // glb-mesh design) via the native-tested `project_to_scene` +
                // `assemble_bundle`. Each file's bytes are base64 (STANDARD) so the
                // wire result stays JSON-clean.
                match crate::controller::export::bake_player_bundle(self).await {
                    Ok(bundle) => {
                        let files: Vec<serde_json::Value> = bundle
                            .into_iter()
                            .map(|f| {
                                json!({
                                    "path": f.path,
                                    "bytes": base64::engine::general_purpose::STANDARD
                                        .encode(f.bytes),
                                })
                            })
                            .collect();
                        let mut entries = std::collections::BTreeMap::new();
                        entries.insert("name".to_string(), json!(name));
                        entries.insert("files".to_string(), json!(files));
                        QueryResult::Map(query::MapResult {
                            kind: "player_bundle".to_string(),
                            entries,
                        })
                    }
                    Err(e) => QueryResult::Error { error: e },
                }
            }
            EditorQuery::ResolveNodeMaterial { node } => {
                use serde_json::json;
                let Some(n) = mutate::find_by_id(&self.scene, node) else {
                    return QueryResult::Error {
                        error: format!("no node {node}"),
                    };
                };
                let kind = n.kind.get_cloned();
                let mut entries = std::collections::BTreeMap::new();
                match node_material_ref(&kind) {
                    None => {
                        let is_geo =
                            matches!(kind, NodeKind::Mesh { .. } | NodeKind::SkinnedMesh { .. });
                        entries.insert("assigned".to_string(), json!(false));
                        entries.insert(
                            "kind".to_string(),
                            json!(if is_geo { "unassigned" } else { "none" }),
                        );
                    }
                    Some(inst) => {
                        entries.insert("assigned".to_string(), json!(true));
                        entries.insert("asset".to_string(), json!(inst.asset.to_string()));
                        entries.insert("base_color".to_string(), json!(inst.inline.base_color));
                        match crate::controller::custom_material::find_material(
                            &self.custom_materials,
                            inst.asset,
                        ) {
                            Some(m) => {
                                entries.insert("name".to_string(), json!(m.name.get_cloned()));
                                match m.builtin.get_cloned() {
                                    Some(def) => {
                                        entries.insert("kind".to_string(), json!("builtin"));
                                        entries.insert(
                                            "shading".to_string(),
                                            json!(format!("{:?}", def.shading)),
                                        );
                                    }
                                    None => {
                                        entries.insert("kind".to_string(), json!("custom"));
                                    }
                                }
                            }
                            None => {
                                entries.insert("kind".to_string(), json!("unknown"));
                            }
                        }
                    }
                }
                QueryResult::Map(query::MapResult {
                    kind: "node_material".to_string(),
                    entries,
                })
            }
            EditorQuery::SelectVerticesWhere { node, predicate } => {
                use awsm_editor_protocol::VertexPredicate as P;
                use awsm_meshgen::edit::{
                    select_by_axis, select_by_normal_dir, select_top_percent_axis,
                    select_within_aabb, select_within_radius, Cmp,
                };
                use serde_json::json;
                if node_is_skinned(&self.scene, node) {
                    return QueryResult::Error {
                        error: skinned_edit_error(node),
                    };
                }
                let mesh = mutate::find_by_id(&self.scene, node).and_then(|n| {
                    crate::controller::export::node_mesh(&self.scene, &n.kind.get_cloned())
                });
                match mesh {
                    Some(mesh) => {
                        let idx = match predicate {
                            P::NormalDir { dir, threshold } => {
                                select_by_normal_dir(&mesh, dir, threshold)
                            }
                            P::AxisGreater { axis, value } => {
                                select_by_axis(&mesh, axis as usize, Cmp::Greater, value)
                            }
                            P::AxisLess { axis, value } => {
                                select_by_axis(&mesh, axis as usize, Cmp::Less, value)
                            }
                            P::TopPercent { axis, percent } => {
                                select_top_percent_axis(&mesh, axis as usize, percent)
                            }
                            P::WithinRadius { center, radius } => {
                                select_within_radius(&mesh, center, radius)
                            }
                            P::WithinAabb { min, max } => select_within_aabb(&mesh, min, max),
                        };
                        let mut entries = std::collections::BTreeMap::new();
                        entries.insert("count".to_string(), json!(idx.len()));
                        entries.insert("indices".to_string(), json!(idx));
                        QueryResult::Map(query::MapResult {
                            kind: "vertex_selection".to_string(),
                            entries,
                        })
                    }
                    None => QueryResult::Error {
                        error: format!("node {node} has no resolvable mesh"),
                    },
                }
            }
            EditorQuery::MeshStats { node } => {
                use serde_json::json;
                let mesh = mutate::find_by_id(&self.scene, node).and_then(|n| {
                    crate::controller::export::node_mesh(&self.scene, &n.kind.get_cloned())
                });
                match mesh {
                    Some(mesh) => {
                        let s = awsm_meshgen::mesh_stats(&mesh);
                        let mut entries = std::collections::BTreeMap::new();
                        entries.insert("vertices".to_string(), json!(s.vertices));
                        entries.insert("triangles".to_string(), json!(s.triangles));
                        entries.insert("bbox_min".to_string(), json!(s.bbox_min));
                        entries.insert("bbox_max".to_string(), json!(s.bbox_max));
                        entries.insert("centroid".to_string(), json!(s.centroid));
                        entries.insert("surface_area".to_string(), json!(s.surface_area));
                        entries.insert("volume".to_string(), json!(s.volume));
                        entries.insert("watertight".to_string(), json!(s.watertight));
                        QueryResult::Map(query::MapResult {
                            kind: "mesh_stats".to_string(),
                            entries,
                        })
                    }
                    None => QueryResult::Error {
                        error: format!("node {node} has no resolvable mesh"),
                    },
                }
            }
            EditorQuery::MeshCrossSection {
                node,
                axis,
                samples,
            } => {
                use serde_json::json;
                let mesh = mutate::find_by_id(&self.scene, node).and_then(|n| {
                    crate::controller::export::node_mesh(&self.scene, &n.kind.get_cloned())
                });
                match mesh {
                    Some(mesh) => {
                        let profile =
                            awsm_meshgen::cross_section_profile(&mesh, axis as usize, samples);
                        let mut entries = std::collections::BTreeMap::new();
                        entries.insert("axis".to_string(), json!(axis));
                        entries.insert("profile".to_string(), json!(profile));
                        QueryResult::Map(query::MapResult {
                            kind: "mesh_cross_section".to_string(),
                            entries,
                        })
                    }
                    None => QueryResult::Error {
                        error: format!("node {node} has no resolvable mesh"),
                    },
                }
            }
            EditorQuery::MeshModifiers { mesh } => {
                // Resolve the asset's recipe; `null` JSON when it has none.
                let stack = match self
                    .scene
                    .assets
                    .lock()
                    .unwrap()
                    .get(mesh)
                    .map(|e| &e.source)
                {
                    Some(SceneAssetSource::Mesh(def)) => Ok(def.stack.clone()),
                    Some(_) => Err(format!("asset {mesh} is not a mesh")),
                    None => Err(format!("no asset {mesh}")),
                };
                match stack {
                    Ok(stack) => {
                        // Every mesh carries a recipe now — serialize the stack.
                        QueryResult::Text(
                            serde_json::to_string(&stack)
                                .unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}")),
                        )
                    }
                    Err(error) => QueryResult::Error { error },
                }
            }
            EditorQuery::GetVertexData { node, indices } => {
                use serde_json::json;
                if node_is_skinned(&self.scene, node) {
                    return QueryResult::Error {
                        error: skinned_edit_error(node),
                    };
                }
                let mesh = mutate::find_by_id(&self.scene, node).and_then(|n| {
                    crate::controller::export::node_mesh(&self.scene, &n.kind.get_cloned())
                });
                match mesh {
                    Some(md) => {
                        let verts: Vec<serde_json::Value> = indices
                            .iter()
                            .map(|&i| {
                                let idx = i as usize;
                                json!({
                                    "index": i,
                                    "position": md.positions.get(idx),
                                    "normal": md.normals.as_ref().and_then(|n| n.get(idx)),
                                    "color": md.colors.as_ref().and_then(|c| c.get(idx)),
                                    "uv": md.uvs.as_ref().and_then(|u| u.get(idx)),
                                })
                            })
                            .collect();
                        let mut entries = std::collections::BTreeMap::new();
                        entries.insert("vertex_count".to_string(), json!(md.positions.len()));
                        entries.insert("vertices".to_string(), json!(verts));
                        QueryResult::Map(query::MapResult {
                            kind: "vertex_data".to_string(),
                            entries,
                        })
                    }
                    None => QueryResult::Error {
                        error: format!("node {node} has no resolvable mesh"),
                    },
                }
            }
            EditorQuery::GetMeshLayers { node } => {
                use awsm_editor_protocol::MeshBase;
                use serde_json::json;
                if node_is_skinned(&self.scene, node) {
                    return QueryResult::Error {
                        error: skinned_edit_error(node),
                    };
                }
                // Resolve node → mesh asset id, then read its def (stack + overrides).
                let mesh_id =
                    mutate::find_by_id(&self.scene, node).and_then(|n| match n.kind.get_cloned() {
                        NodeKind::Mesh { mesh, .. } => Some(mesh.0),
                        _ => None,
                    });
                let def = mesh_id.and_then(|id| {
                    match self.scene.assets.lock().unwrap().get(id).map(|e| &e.source) {
                        Some(SceneAssetSource::Mesh(def)) => Some(def.clone()),
                        _ => None,
                    }
                });
                match def {
                    Some(def) => {
                        let base_kind = match &def.stack.base {
                            MeshBase::Primitive(_) => "primitive",
                            MeshBase::Lathe { .. } => "lathe",
                            MeshBase::Superquadric { .. } => "superquadric",
                            MeshBase::Sweep(_) => "sweep",
                            MeshBase::Captured(_) => "captured",
                            MeshBase::Sdf { .. } => "sdf",
                        };
                        // Each modifier serialized as its tagged JSON (the variant
                        // name + params) — full fidelity for the layer list.
                        let modifiers: Vec<serde_json::Value> = def
                            .stack
                            .modifiers
                            .iter()
                            .map(|m| serde_json::to_value(m).unwrap_or(serde_json::Value::Null))
                            .collect();
                        let ov = &def.overrides;
                        // "Baked/terminal" = a frozen-topology authorable mesh: a
                        // bare Captured-self base with no modifiers.
                        let frozen = def.stack.modifiers.is_empty()
                            && matches!(def.stack.base, MeshBase::Captured(_));
                        let mut entries = std::collections::BTreeMap::new();
                        entries.insert("mesh".to_string(), json!(mesh_id.map(|i| i.to_string())));
                        entries.insert("base".to_string(), json!(base_kind));
                        entries.insert("modifiers".to_string(), json!(modifiers));
                        entries.insert("modifier_count".to_string(), json!(modifiers.len()));
                        entries.insert("frozen_topology".to_string(), json!(frozen));
                        entries.insert("has_overrides".to_string(), json!(!ov.is_empty()));
                        entries.insert(
                            "override_counts".to_string(),
                            json!({
                                "positions": ov.positions.len(),
                                "colors": ov.colors.len(),
                                "normals": ov.normals.len(),
                                "uvs": ov.uvs.len(),
                            }),
                        );
                        QueryResult::Map(query::MapResult {
                            kind: "mesh_layers".to_string(),
                            entries,
                        })
                    }
                    None => QueryResult::Error {
                        error: format!("node {node} is not a Mesh / has no resolvable mesh asset"),
                    },
                }
            }
            EditorQuery::WaitRenderSettled { max_ms } => self.wait_render_settled(max_ms).await,
            EditorQuery::NodeTransforms { nodes } => self.node_transforms(&nodes).await,
            EditorQuery::NodeKindDetails { nodes } => {
                use serde_json::Value;
                let ids = self.resolve_node_ids(&nodes);
                let mut entries = std::collections::BTreeMap::new();
                for id in ids {
                    if let Some(n) = mutate::find_by_id(&self.scene, id) {
                        let kind = n.kind.get_cloned();
                        entries.insert(
                            id.to_string(),
                            serde_json::to_value(&kind).unwrap_or(Value::Null),
                        );
                    }
                }
                QueryResult::Map(query::MapResult {
                    kind: "kind_details".to_string(),
                    entries,
                })
            }
            EditorQuery::NodeBounds { nodes } => self.node_bounds(&nodes).await,
            EditorQuery::FrameGlobals => {
                use serde_json::json;
                let fg = crate::engine::context::with_renderer_mut(|r| r.frame_globals()).await;
                let mut entries = std::collections::BTreeMap::new();
                entries.insert("time".to_string(), json!(fg.time));
                entries.insert("delta_time".to_string(), json!(fg.delta_time));
                entries.insert("frame_count".to_string(), json!(fg.frame_count));
                entries.insert("resolution".to_string(), json!(fg.resolution));
                QueryResult::Map(query::MapResult {
                    kind: "frame_globals".to_string(),
                    entries,
                })
            }
            EditorQuery::MorphData { nodes } => {
                use serde_json::json;
                // node → first materialized mesh → live geometry morph weights
                // (the same store `SetMorphWeight` writes and morph animation
                // tracks drive). Nodes without materialized morphs are omitted —
                // an empty map on a morph-bearing scene means "not materialized
                // yet", not "no morphs".
                let ids = self.resolve_node_ids(&nodes);
                // (id, meshes, target names) — names ride the import template
                // (glTF `mesh.extras.targetNames`); empty when the source had
                // none or the node isn't a template-backed import.
                let pairs: Vec<(NodeId, Vec<awsm_renderer::meshes::MeshKey>, Vec<String>)> = ids
                    .iter()
                    .map(|id| {
                        (
                            *id,
                            renderer_meshes_for_node(*id),
                            morph_names_for_node(*id),
                        )
                    })
                    .filter(|(_, meshes, _)| !meshes.is_empty())
                    .collect();
                let entries = crate::engine::context::with_renderer_mut(move |r| {
                    let mut entries = std::collections::BTreeMap::new();
                    for (id, meshes, names) in pairs {
                        // First morph-bearing primitive wins (multi-primitive
                        // nodes share one weight set per glTF mesh anyway).
                        let weights = meshes.iter().find_map(|mesh| {
                            let key = r.meshes.geometry_morph_key_for_mesh(*mesh)?;
                            r.meshes.morphs.geometry.read_morph_weights(key).ok()
                        });
                        if let Some(weights) = weights {
                            entries.insert(
                                id.to_string(),
                                json!({
                                    "target_count": weights.len(),
                                    "weights": weights,
                                    "names": names,
                                }),
                            );
                        }
                    }
                    entries
                })
                .await;
                QueryResult::Map(query::MapResult {
                    kind: "morph_data".to_string(),
                    entries,
                })
            }
            EditorQuery::SkinData { nodes } => {
                use serde_json::json;
                // Rig discovery: every SkinnedMesh node's joint table, each joint
                // resolved to its live editor bone node (name + current local TRS).
                // Joints are ordinary scene nodes — SetTransform poses them and a
                // Transform animation track animates them; this query is just the
                // map that makes the rig reachable without walking the outliner.
                let ids = self.resolve_node_ids(&nodes);
                let mut entries = std::collections::BTreeMap::new();
                for id in ids {
                    let Some(n) = mutate::find_by_id(&self.scene, id) else {
                        continue;
                    };
                    let NodeKind::SkinnedMesh { skin, .. } = n.kind.get_cloned() else {
                        continue;
                    };
                    // `live`: the skin bridge holds a mirror→baked mapping for
                    // this bone, i.e. posing it actually deforms the skin. False
                    // means the rig is display-only (registration failed/skipped)
                    // — surfaced so an agent (and we) can SEE a broken chain.
                    let baked_map = crate::engine::bridge::bridge()
                        .skin_joint_baked
                        .lock()
                        .unwrap()
                        .clone();
                    let joints: Vec<serde_json::Value> = skin
                        .joints
                        .iter()
                        .map(|j| {
                            let bone = mutate::find_by_id(&self.scene, j.node);
                            let (name, trs) = bone
                                .map(|b| (b.name.get_cloned(), b.transform.get_cloned()))
                                .unwrap_or_else(|| {
                                    (
                                        "<missing>".to_string(),
                                        crate::engine::scene::Trs::default(),
                                    )
                                });
                            json!({
                                "node": j.node.to_string(),
                                "index": j.index,
                                "name": name,
                                "live": baked_map.contains_key(&j.node),
                                "translation": trs.translation,
                                "rotation": trs.rotation,
                                "scale": trs.scale,
                            })
                        })
                        .collect();
                    entries.insert(
                        id.to_string(),
                        json!({
                            "source": skin.source.to_string(),
                            "primitive_index": skin.primitive_index,
                            "joints": joints,
                        }),
                    );
                }
                QueryResult::Map(query::MapResult {
                    kind: "skin_data".to_string(),
                    entries,
                })
            }
            EditorQuery::ConsoleLogs { limit } => {
                use serde_json::json;
                // Editor toasts (info/warning/error notices).
                let logs: Vec<serde_json::Value> = CONSOLE_LOG.with(|b| {
                    let b = b.borrow();
                    let start = b.len().saturating_sub(limit as usize);
                    b.iter()
                        .skip(start)
                        .map(|(level, message)| json!({ "level": level, "message": message }))
                        .collect()
                });
                // Raw `tracing` events (WARN/ERROR/etc. from anywhere — render
                // loop, bridges, loader) mirrored from the browser console via the
                // web-shared CaptureLayer, so a headless MCP driver can read them.
                let tracing_logs: Vec<serde_json::Value> =
                    awsm_web_shared::logger::captured_logs(limit as usize)
                        .into_iter()
                        .map(|(level, message)| json!({ "level": level, "message": message }))
                        .collect();
                let mut entries = std::collections::BTreeMap::new();
                entries.insert("logs".to_string(), json!(logs));
                entries.insert("tracing".to_string(), json!(tracing_logs));
                QueryResult::Map(query::MapResult {
                    kind: "console_logs".to_string(),
                    entries,
                })
            }
            EditorQuery::GetTrackData { clip, track } => {
                match find_track(&self.custom_animations, clip, track) {
                    Some(t) => {
                        let stored = awsm_editor_protocol::animation::StoredTrack {
                            target: t.target.clone(),
                            sampler: t.sampler.get(),
                            mute: t.mute.get(),
                            solo: t.solo.get(),
                            expanded: t.expanded.get(),
                            times: t.times.get_cloned(),
                            keys: t.keys.get_cloned(),
                        };
                        let mut entries = std::collections::BTreeMap::new();
                        entries.insert(
                            "data".to_string(),
                            serde_json::to_value(&stored).unwrap_or(serde_json::Value::Null),
                        );
                        QueryResult::Map(query::MapResult {
                            kind: "track".to_string(),
                            entries,
                        })
                    }
                    None => QueryResult::Error {
                        error: format!("no track {track} in clip {clip}"),
                    },
                }
            }
        }
    }

    /// Resolve a requested node-id list to concrete ids; an empty request means
    /// every node in the scene (depth-first).
    fn resolve_node_ids(&self, requested: &[NodeId]) -> Vec<NodeId> {
        if !requested.is_empty() {
            return requested.to_vec();
        }
        fn walk(nodes: &[Arc<crate::engine::scene::node::Node>], out: &mut Vec<NodeId>) {
            for n in nodes {
                out.push(n.id);
                walk(&n.children.lock_ref(), out);
            }
        }
        let mut out = Vec::new();
        walk(&self.scene.nodes.lock_ref(), &mut out);
        out
    }

    /// `NodeTransforms` handler — local TRS from the live scene + world matrix
    /// from the renderer transform graph (no animation-clip pin hack).
    async fn node_transforms(&self, nodes: &[NodeId]) -> query::QueryResult {
        use serde_json::json;
        let ids = self.resolve_node_ids(nodes);
        let mut entries = std::collections::BTreeMap::new();
        for id in &ids {
            if let Some(n) = mutate::find_by_id(&self.scene, *id) {
                let t = n.transform.get();
                entries.insert(
                    id.to_string(),
                    json!({
                        "translation": t.translation,
                        "rotation": t.rotation,
                        "scale": t.scale,
                    }),
                );
            }
        }
        // Augment with world matrices read from the renderer transform graph.
        let ids2 = ids.clone();
        let worlds = crate::engine::context::with_renderer_mut(move |r| {
            let mut m = std::collections::BTreeMap::new();
            let bridge = crate::engine::bridge::bridge();
            let nodes = bridge.nodes.lock().unwrap();
            for id in &ids2 {
                if let Some(tk) = nodes.get(id).map(|n| n.transform_key) {
                    if let Ok(w) = r.transforms.get_world(tk) {
                        m.insert(id.to_string(), w.to_cols_array().to_vec());
                    }
                }
            }
            m
        })
        .await;
        for (k, w) in worlds {
            if let Some(serde_json::Value::Object(obj)) = entries.get_mut(&k) {
                obj.insert("world".to_string(), json!(w));
            }
        }
        QueryResult::Map(query::MapResult {
            kind: "transforms".to_string(),
            entries,
        })
    }

    /// `NodeBounds` handler — world-space AABB per node, CPU-estimated from the
    /// node's local extent (primitive dims; unit box otherwise) transformed by
    /// its renderer world matrix.
    async fn node_bounds(&self, nodes: &[NodeId]) -> query::QueryResult {
        use serde_json::json;
        let ids = self.resolve_node_ids(nodes);
        // (id, local-aabb) pairs from the scene; world matrices from the renderer.
        let locals: Vec<(NodeId, Aabb3)> = ids
            .iter()
            .filter_map(|id| {
                mutate::find_by_id(&self.scene, *id)
                    .map(|n| (*id, local_aabb(&n.kind.get_cloned())))
            })
            .collect();
        // Resolve per-node renderer meshes + transform keys BEFORE taking the
        // renderer lock: renderer_meshes_for_node locks the bridge nodes map,
        // which must never nest inside a scope already holding that lock.
        let resolved: Vec<(
            NodeId,
            Aabb3,
            Vec<awsm_renderer::meshes::MeshKey>,
            Option<awsm_renderer::transforms::TransformKey>,
        )> = {
            let bridge = crate::engine::bridge::bridge();
            locals
                .iter()
                .map(|(id, aabb)| {
                    let meshes = renderer_meshes_for_node(*id);
                    let tk = bridge
                        .nodes
                        .lock()
                        .unwrap()
                        .get(id)
                        .map(|n| n.transform_key);
                    (*id, *aabb, meshes, tk)
                })
                .collect()
        };
        let entries = crate::engine::context::with_renderer_mut(move |r| {
            let mut m = std::collections::BTreeMap::new();
            for (id, (lmin, lmax), meshes, tk) in &resolved {
                // Prefer the renderer's LIVE world AABB (union over the node's
                // materialized meshes) — exact for whatever actually renders,
                // including populate-baked SkinnedMesh nodes whose scene-side
                // local_aabb is just a unit-cube fallback (the bug that made
                // frame_node aim at nothing on imported rigs).
                let live = meshes
                    .iter()
                    .filter_map(|mk| {
                        r.meshes
                            .get(*mk)
                            .ok()
                            .and_then(|mesh| mesh.world_aabb.clone())
                    })
                    .reduce(|mut acc, b| {
                        acc.extend(&b);
                        acc
                    });
                if let Some(aabb) = live {
                    m.insert(
                        id.to_string(),
                        json!({ "min": aabb.min.to_array(), "max": aabb.max.to_array() }),
                    );
                    continue;
                }
                let world = tk
                    .and_then(|tk| r.transforms.get_world(tk).ok().copied())
                    .unwrap_or(glam::Mat4::IDENTITY);
                let (wmin, wmax) = transform_aabb(world, *lmin, *lmax);
                m.insert(id.to_string(), json!({ "min": wmin, "max": wmax }));
            }
            m
        })
        .await;
        QueryResult::Map(query::MapResult {
            kind: "bounds".to_string(),
            entries,
        })
    }

    /// Poll until no material recompile is pending (`compile_pending == 0`) and
    /// the renderer's pipeline scheduler has drained, held stable across two
    /// consecutive frames so the settled frame has actually presented. Returns on
    /// timeout otherwise. Polls on a ~frame cadence.
    async fn wait_render_settled(&self, max_ms: u32) -> query::QueryResult {
        const INTERVAL_MS: u32 = 16;
        let max_polls = (max_ms / INTERVAL_MS).max(1);
        let mut stable = 0u32;
        let mut waited = 0u32;
        let mut settled = false;
        for _ in 0..max_polls {
            gloo_timers::future::TimeoutFuture::new(INTERVAL_MS).await;
            waited = waited.saturating_add(INTERVAL_MS);
            let editor_pending = self.compile_pending.get() > 0;
            let renderer_pending = crate::engine::context::with_renderer_mut(|r| {
                let p = r.compile_progress();
                p.materials_pending > 0 || p.in_flight_subcompiles > 0
            })
            .await;
            if !editor_pending && !renderer_pending {
                stable += 1;
                if stable >= 2 {
                    settled = true;
                    break;
                }
            } else {
                stable = 0;
            }
        }
        query::QueryResult::Settled(query::SettledResult {
            settled,
            waited_ms: waited,
        })
    }

    /// `SampleClipTimeseries` handler — the workhorse verification query. Snapshot
    /// the transport, force `playing = false`, then for each `t` pin the renderer
    /// pose (`set_local_time(t)` + `update_animations(0.0)`) and read every target
    /// from CPU-side renderer state. Restores the transport. GPU-independent.
    async fn sample_clip_timeseries(
        &self,
        _clip: AssetId,
        times: Vec<f64>,
        targets: Vec<query::ReadbackTarget>,
    ) -> query::QueryResult {
        use query::*;
        // Save transport, pause for deterministic pinning.
        let saved_playing = self.playing.get();
        let saved_playhead = self.playhead.get();
        self.playing.set_neq(false);

        // Resolve each readback target → a renderer key descriptor once (so the
        // per-frame read loop is cheap). Returns the stable key string + a closure
        // input (the resolved renderer ref) — here we just keep the target and
        // resolve per-read for simplicity (read counts are small).
        let target_keys: Vec<String> = targets.iter().map(readback_key).collect();

        let mut frames: Vec<TimeseriesFrame> = Vec::with_capacity(times.len());
        for &t in &times {
            let targets_ref = targets.clone();
            let keys_ref = target_keys.clone();
            let values = crate::engine::context::with_renderer_mut(move |r| {
                // Pin the pose at t.
                crate::engine::bridge::animation_sync::pin_pose(r, t);
                let mut map = std::collections::BTreeMap::new();
                for (target, key) in targets_ref.iter().zip(keys_ref.iter()) {
                    map.insert(key.clone(), read_readback_target(r, target));
                }
                map
            })
            .await;
            frames.push(TimeseriesFrame { t, values });
        }

        // Restore the transport + re-pin the original playhead.
        self.playing.set_neq(saved_playing);
        self.playhead.set_neq(saved_playhead);
        let restore = saved_playhead;
        crate::engine::context::with_renderer_mut(move |r| {
            crate::engine::bridge::animation_sync::pin_pose(r, restore);
        })
        .await;

        QueryResult::Timeseries(TimeseriesResult {
            targets: target_keys,
            frames,
        })
    }

    /// `query()` as a JSON string (decode-run-encode for the wasm seam).
    pub async fn query_json(&self, query_json: &str) -> String {
        match serde_json::from_str::<query::EditorQuery>(query_json) {
            Ok(q) => {
                let result = self.query(q).await;
                serde_json::to_string(&result).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
            }
            Err(e) => format!("{{\"error\":\"decode: {e}\"}}"),
        }
    }
}

/// A min/max bound as a pair of `[x,y,z]` corners.
type Aabb3 = ([f32; 3], [f32; 3]);

thread_local! {
    /// Renderer meshes + lights a prior `LoadPlayerBundle` populated DIRECTLY
    /// (via `populate_awsm_scene`) — these live OUTSIDE the bridge's per-node
    /// tracking, so the next reset must remove them explicitly or a repeated
    /// round-trip / New-Project-after-round-trip leaves them ghosting.
    static LAST_BUNDLE_RESOURCES: RefCell<(
        Vec<awsm_renderer::meshes::MeshKey>,
        Vec<awsm_renderer::lights::LightKey>,
        Vec<awsm_renderer::animation::AnimationClipKey>,
    )> = const { RefCell::new((Vec::new(), Vec::new(), Vec::new())) };
}

/// Record the resources a `LoadPlayerBundle` populate just created, so the next
/// project reset removes them.
fn set_bundle_resources(
    meshes: Vec<awsm_renderer::meshes::MeshKey>,
    lights: Vec<awsm_renderer::lights::LightKey>,
    clips: Vec<awsm_renderer::animation::AnimationClipKey>,
) {
    LAST_BUNDLE_RESOURCES.with(|c| *c.borrow_mut() = (meshes, lights, clips));
}

/// On a project reset, remove renderer resources that live OUTSIDE the bridge's
/// per-node tracking — otherwise they ghost. Two sources: (a) imported-glTF
/// template meshes (skinned populate copies node teardown skips; `clear_templates`,
/// called right after, only drops the metadata map); (b) a prior
/// `LoadPlayerBundle` populate's direct mesh/light inserts.
async fn clear_untracked_renderer_resources() {
    let templates: Vec<_> = crate::engine::bridge::bridge()
        .templates
        .lock()
        .unwrap()
        .values()
        .cloned()
        .collect();
    let (meshes, lights, clips) =
        LAST_BUNDLE_RESOURCES.with(|c| std::mem::take(&mut *c.borrow_mut()));
    if templates.is_empty() && meshes.is_empty() && lights.is_empty() && clips.is_empty() {
        return;
    }
    crate::engine::context::with_renderer_mut(move |r| {
        for t in &templates {
            crate::engine::bridge::asset_template::remove_template_meshes(r, t);
        }
        for mk in meshes {
            r.remove_mesh(mk);
        }
        for lk in lights {
            r.remove_light(lk);
        }
        // Drop the bundle's animation clips; the mixer referenced them by key, so
        // reset it too (the editor's own clips/mixer re-lower on the next edit).
        if !clips.is_empty() {
            for ck in clips {
                r.animations.remove_clip(ck);
            }
            r.animations.mixer.clear();
        }
    })
    .await;
}

/// A coarse local-space AABB for a node kind (half-extents from primitive dims;
/// a unit box for anything without obvious bounds). Used only to frame the
/// camera + report approximate size — not a tight collision bound.
fn local_aabb(kind: &NodeKind) -> Aabb3 {
    // A Mesh's true bounds come from its baked geometry in the mesh cache; every
    // procedural node (box / sphere / sweep / …) is a Mesh now.
    if let NodeKind::Mesh { mesh, .. } = kind {
        if let Some(raw) = crate::engine::bridge::mesh_cache::get_raw(mesh.0) {
            if !raw.positions.is_empty() {
                let mut min = [f32::INFINITY; 3];
                let mut max = [f32::NEG_INFINITY; 3];
                for p in &raw.positions {
                    for i in 0..3 {
                        min[i] = min[i].min(p[i]);
                        max[i] = max[i].max(p[i]);
                    }
                }
                return (min, max);
            }
        }
    }
    // Lights / cameras / empties / models / un-baked meshes: a small unit box
    // centered on the node (a glTF model's true bounds aren't cheaply available
    // CPU-side).
    ([-0.5, -0.5, -0.5], [0.5, 0.5, 0.5])
}

/// Transform a local AABB by a world matrix and return the enclosing world AABB.
fn transform_aabb(world: glam::Mat4, min: [f32; 3], max: [f32; 3]) -> Aabb3 {
    let corners = [
        [min[0], min[1], min[2]],
        [max[0], min[1], min[2]],
        [min[0], max[1], min[2]],
        [max[0], max[1], min[2]],
        [min[0], min[1], max[2]],
        [max[0], min[1], max[2]],
        [min[0], max[1], max[2]],
        [max[0], max[1], max[2]],
    ];
    let mut wmin = glam::Vec3::splat(f32::INFINITY);
    let mut wmax = glam::Vec3::splat(f32::NEG_INFINITY);
    for c in corners {
        let p = world.transform_point3(glam::Vec3::from_array(c));
        wmin = wmin.min(p);
        wmax = wmax.max(p);
    }
    (wmin.to_array(), wmax.to_array())
}

/// A stable string key for a readback target (the `values` map key).
fn readback_key(t: &query::ReadbackTarget) -> String {
    use query::ReadbackTarget as R;
    match t {
        R::NodeLocalTrs { node } => format!("local_trs/{node}"),
        R::NodeWorldMatrix { node } => format!("world/{node}"),
        R::MorphWeight { node, index } => format!("morph/{node}/{index}"),
        R::Uniform { material, name } => format!("uniform/{material}/{name}"),
        R::BuiltinParam { node, param } => format!("builtin/{node}/{param:?}"),
        R::LightParam { node, param } => format!("light/{node}/{param:?}"),
        R::CameraParam { node, param } => format!("camera/{node}/{param:?}"),
    }
}

/// Renderer mesh keys for a node, covering BOTH materialization paths: a
/// captured/editable node's own `model_meshes`, or — when that's empty — a
/// `SkinnedMesh` node's populate-baked keys resolved through the import
/// template (those keys are template-owned and deliberately never pushed to
/// `model_meshes`; see `materialize_skinned_mesh`). Morph-bearing imports ride
/// the SkinnedMesh path, so any morph resolution MUST use this, not
/// `model_meshes` alone. Empty when the node isn't materialized.
fn renderer_meshes_for_node(node: NodeId) -> Vec<awsm_renderer::meshes::MeshKey> {
    let b = crate::engine::bridge::bridge();
    let entry = { b.nodes.lock().unwrap().get(&node).cloned() };
    let Some(entry) = entry else {
        return Vec::new();
    };
    let own = entry.model_meshes.lock().unwrap().clone();
    if !own.is_empty() {
        return own;
    }
    let NodeKind::SkinnedMesh { skin, .. } = entry.node.kind.get_cloned() else {
        return Vec::new();
    };
    let Some(template) = b.get_template(skin.source) else {
        return Vec::new();
    };
    let Some(tnode) = template.find_by_node_index(skin.node_index) else {
        return Vec::new();
    };
    match skin.primitive_index {
        None => tnode.mesh_keys.clone(),
        Some(i) => tnode
            .mesh_keys
            .get(i as usize)
            .copied()
            .into_iter()
            .collect(),
    }
}

/// Morph target names for a node, via its import template (`SkinnedMesh` ref →
/// template node → glTF `mesh.extras.targetNames`). Empty when the node isn't a
/// template-backed import or the source carried no names.
fn morph_names_for_node(node: NodeId) -> Vec<String> {
    let b = crate::engine::bridge::bridge();
    let entry = { b.nodes.lock().unwrap().get(&node).cloned() };
    let Some(entry) = entry else {
        return Vec::new();
    };
    let NodeKind::SkinnedMesh { skin, .. } = entry.node.kind.get_cloned() else {
        return Vec::new();
    };
    b.get_template(skin.source)
        .and_then(|t| {
            t.find_by_node_index(skin.node_index)
                .map(|tn| tn.morph_target_names.clone())
        })
        .unwrap_or_default()
}

/// Read one readback target from CPU-side renderer state → a JSON number / array
/// (null when unreadable / pending).
fn read_readback_target(
    r: &awsm_renderer::AwsmRenderer,
    t: &query::ReadbackTarget,
) -> serde_json::Value {
    use query::ReadbackTarget as R;
    use serde_json::json;

    let node_tk = |node: NodeId| -> Option<awsm_renderer::transforms::TransformKey> {
        crate::engine::bridge::bridge()
            .nodes
            .lock()
            .unwrap()
            .get(&node)
            .map(|n| n.transform_key)
    };
    let node_mat = |node: NodeId| -> Option<awsm_renderer::materials::MaterialKey> {
        crate::engine::bridge::bridge()
            .nodes
            .lock()
            .unwrap()
            .get(&node)
            .and_then(|n| n.material_keys.lock().unwrap().first().copied())
    };
    let node_light = |node: NodeId| -> Option<awsm_renderer::lights::LightKey> {
        crate::engine::bridge::bridge()
            .nodes
            .lock()
            .unwrap()
            .get(&node)
            .and_then(|n| *n.light_key.lock().unwrap())
    };

    match t {
        R::NodeLocalTrs { node } => {
            match node_tk(*node).and_then(|tk| r.transforms.get_local(tk).ok()) {
                Some(tr) => json!({
                    "translation": [tr.translation.x, tr.translation.y, tr.translation.z],
                    "rotation": [tr.rotation.x, tr.rotation.y, tr.rotation.z, tr.rotation.w],
                    "scale": [tr.scale.x, tr.scale.y, tr.scale.z],
                }),
                None => serde_json::Value::Null,
            }
        }
        R::NodeWorldMatrix { node } => {
            match node_tk(*node).and_then(|tk| r.transforms.get_world(tk).ok().copied()) {
                Some(m) => json!(m.to_cols_array().to_vec()),
                None => serde_json::Value::Null,
            }
        }
        R::Uniform { material, name } => {
            // Custom-material asset → shader id → uniform slot index by name →
            // live MaterialKey → read its current `DynamicMaterial::values[slot]`.
            use awsm_materials::dynamic_layout::UniformValue;
            use awsm_renderer::materials::Material;
            fn uniform_value_to_json(v: &UniformValue) -> serde_json::Value {
                match v {
                    UniformValue::F32(x) => json!(x),
                    UniformValue::U32(x) => json!(x),
                    UniformValue::Bool(x) => json!(x),
                    UniformValue::Vec2(a) => json!(a.to_vec()),
                    UniformValue::Vec3(a) | UniformValue::Color3(a) => json!(a.to_vec()),
                    UniformValue::Vec4(a) | UniformValue::Color4(a) => json!(a.to_vec()),
                    UniformValue::IVec2(a) => json!(a.to_vec()),
                    UniformValue::IVec3(a) => json!(a.to_vec()),
                    UniformValue::IVec4(a) => json!(a.to_vec()),
                    UniformValue::Mat3(a) => json!(a.to_vec()),
                    UniformValue::Mat4(a) => json!(a.to_vec()),
                }
            }
            let Some(shader_id) = crate::engine::bridge::dynamic::shader_id_for_asset(*material)
            else {
                return serde_json::Value::Null;
            };
            let Some(slot) = r
                .dynamic_material_registration(shader_id)
                .and_then(|reg| reg.layout.uniforms.iter().position(|u| u.name == *name))
            else {
                return serde_json::Value::Null;
            };
            // Find the live custom material built from this shader id and read its
            // current uniform value at `slot`.
            let value = r.materials.iter().find_map(|(_, mat)| match mat {
                Material::Custom(dm) if dm.shader_id == shader_id => dm.values.get(slot).cloned(),
                _ => None,
            });
            match value {
                Some(v) => uniform_value_to_json(&v),
                None => serde_json::Value::Null,
            }
        }
        R::BuiltinParam { node, param } => {
            use animation::BuiltinParamKind as P;
            use awsm_renderer::materials::Material;
            let Some(mk) = node_mat(*node) else {
                return serde_json::Value::Null;
            };
            let Ok(m) = r.materials.get(mk) else {
                return serde_json::Value::Null;
            };
            match param {
                P::BaseColor => match m {
                    Material::Pbr(p) => json!(&p.base_color_factor[0..3]),
                    Material::Unlit(u) => json!(&u.base_color_factor[0..3]),
                    Material::Toon(t) => json!(&t.base_color_factor[0..3]),
                    _ => serde_json::Value::Null,
                },
                P::Emissive => match m {
                    Material::Pbr(p) => json!(p.emissive_factor.to_vec()),
                    Material::Unlit(u) => json!(u.emissive_factor.to_vec()),
                    Material::Toon(t) => json!(t.emissive_factor.to_vec()),
                    _ => serde_json::Value::Null,
                },
                P::Metallic => match m {
                    Material::Pbr(p) => json!(p.metallic_factor),
                    _ => serde_json::Value::Null,
                },
                P::Roughness => match m {
                    Material::Pbr(p) => json!(p.roughness_factor),
                    _ => serde_json::Value::Null,
                },
            }
        }
        R::LightParam { node, param } => {
            use animation::LightParamKind as P;
            use awsm_renderer::lights::Light;
            let Some(lk) = node_light(*node) else {
                return serde_json::Value::Null;
            };
            let Some(l) = r.lights.get(lk) else {
                return serde_json::Value::Null;
            };
            match param {
                P::Color => {
                    let c = match l {
                        Light::Directional { color, .. }
                        | Light::Point { color, .. }
                        | Light::Spot { color, .. } => *color,
                    };
                    json!(c.to_vec())
                }
                P::Intensity => {
                    let i = match l {
                        Light::Directional { intensity, .. }
                        | Light::Point { intensity, .. }
                        | Light::Spot { intensity, .. } => *intensity,
                    };
                    json!(i)
                }
                P::Range => match l {
                    Light::Point { range, .. } | Light::Spot { range, .. } => json!(range),
                    Light::Directional { .. } => serde_json::Value::Null,
                },
                P::InnerAngle => match l {
                    Light::Spot { inner_angle, .. } => json!(inner_angle),
                    _ => serde_json::Value::Null,
                },
                P::OuterAngle => match l {
                    Light::Spot { outer_angle, .. } => json!(outer_angle),
                    _ => serde_json::Value::Null,
                },
            }
        }
        R::MorphWeight { node, index } => {
            // node → first materialized mesh → geometry morph key → current
            // weights; return weights[index] as a number. Null if unresolvable
            // (mesh/morph not materialized, or index out of range).
            let weight = renderer_meshes_for_node(*node)
                .into_iter()
                .find_map(|mesh| r.meshes.geometry_morph_key_for_mesh(mesh))
                .and_then(|key| r.meshes.morphs.geometry.read_morph_weights(key).ok())
                .and_then(|weights| weights.get(*index).copied());
            match weight {
                Some(w) => json!(w),
                None => serde_json::Value::Null,
            }
        }
        R::CameraParam { node, param } => {
            // node → camera_key (renderer cameras store, mirrors the node config
            // and mutated by camera animation) → the requested param as a number.
            // Null if the camera slot isn't materialized yet, or FovY on an
            // orthographic camera.
            use animation::CameraParamKind as P;
            use awsm_renderer::cameras::CameraProjectionParams;
            let camera_key = crate::engine::bridge::bridge()
                .nodes
                .lock()
                .unwrap()
                .get(node)
                .and_then(|n| *n.camera_key.lock().unwrap());
            let Some(p) = camera_key.and_then(|key| r.cameras.get(key)) else {
                return serde_json::Value::Null;
            };
            match param {
                P::FovY => match p.projection {
                    CameraProjectionParams::Perspective { fov_y_rad } => json!(fov_y_rad),
                    CameraProjectionParams::Orthographic { .. } => serde_json::Value::Null,
                },
                P::Near => json!(p.near),
                P::Far => json!(p.far),
                P::Aperture => json!(p.aperture),
                P::FocusDistance => json!(p.focus_distance),
            }
        }
    }
}

/// Flip a custom material back to draft and request a (debounced) recompile.
/// The single place the authoring command handlers funnel through — bumping
/// `recompile_rev` is what wakes the auto-register observer (so an alpha/layout/
/// includes edit recompiles, not just a WGSL edit). Mirrors the Studio's old
/// `draft()` helper, now owned by the controller.
fn mark_material_draft(mat: &Arc<CM>) {
    mat.registered.set_neq(false);
    mat.recompile_rev.replace_with(|v| v.wrapping_add(1));
}

/// The current alpha/surface mode of a custom material as the serializable
/// [`awsm_editor_protocol::CustomAlphaMode`] (folds in the mask cutoff).
fn custom_alpha_of(mat: &Arc<CM>) -> awsm_editor_protocol::CustomAlphaMode {
    use awsm_editor_protocol::CustomAlphaMode as M;
    match mat.alpha.get() {
        AlphaMode::Opaque => M::Opaque,
        AlphaMode::Mask => M::Mask {
            cutoff: mat.cutoff.get(),
        },
        AlphaMode::Blend => M::Blend,
    }
}

/// Project the editor's live `Slot`s into serializable `SlotSpec`s (and back).
fn slots_to_specs(slots: &[Slot]) -> Vec<awsm_editor_protocol::SlotSpec> {
    slots
        .iter()
        .map(|s| awsm_editor_protocol::SlotSpec {
            name: s.name.clone(),
            ty: s.ty.clone(),
            val: s.val.clone(),
            debug: s.debug.clone(),
        })
        .collect()
}

fn specs_to_slots(specs: &[awsm_editor_protocol::SlotSpec]) -> Vec<Slot> {
    specs
        .iter()
        .map(|s| Slot {
            name: s.name.clone(),
            ty: s.ty.clone(),
            val: s.val.clone(),
            debug: s.debug.clone(),
        })
        .collect()
}

/// Keep only keys present in `valid` (drops unknowns rather than failing).
fn validate_keys(keys: &[String], valid: &[&str]) -> Vec<String> {
    keys.iter()
        .filter(|k| valid.contains(&k.as_str()))
        .cloned()
        .collect()
}

/// The standard error message for a geometry op aimed at a **skinned** mesh
/// node. Skinned meshes are not editable (their per-vertex skin weights can't
/// survive topology edits); `drop_skinning` bakes the bind pose to a static
/// editable mesh first.
fn skinned_edit_error(node: NodeId) -> String {
    format!("node {node} is skinned; call drop_skinning first")
}

/// `true` if `node` is a `SkinnedMesh` — the edit-guard predicate for
/// geometry-editing commands/queries (which can't target a skinned mesh).
fn node_is_skinned(scene: &Scene, node: NodeId) -> bool {
    mutate::find_by_id(scene, node)
        .map(|n| matches!(n.kind.get_cloned(), NodeKind::SkinnedMesh { .. }))
        .unwrap_or(false)
}

/// The node's single material assignment, if it carries one and is assigned
/// (`Some`). Returns `None` for non-geometry nodes and for unassigned geometry.
fn node_material_ref(
    kind: &NodeKind,
) -> Option<&awsm_editor_protocol::dynamic_material::MaterialInstance> {
    match kind {
        NodeKind::Mesh { material, .. } => material.as_ref(),
        NodeKind::SkinnedMesh { material, .. } => material.as_ref(),
        _ => None,
    }
}

/// Mutable variant of [`node_material_ref`].
fn node_material_mut(
    kind: &mut NodeKind,
) -> Option<&mut awsm_editor_protocol::dynamic_material::MaterialInstance> {
    match kind {
        NodeKind::Mesh { material, .. } => material.as_mut(),
        NodeKind::SkinnedMesh { material, .. } => material.as_mut(),
        _ => None,
    }
}

/// Patch a built-in material factor on a node's per-mesh inline store. Returns
/// false if the node is unassigned (nothing to tweak on a magenta node) or
/// `value` is too short.
fn patch_builtin_param(
    kind: &mut NodeKind,
    param: awsm_editor_protocol::animation::BuiltinParamKind,
    value: &[f32],
) -> bool {
    use awsm_editor_protocol::animation::BuiltinParamKind as P;
    let Some(inst) = node_material_mut(kind) else {
        return false;
    };
    let inline = &mut inst.inline;
    match param {
        P::BaseColor => {
            if value.len() < 3 {
                return false;
            }
            inline.base_color[0] = value[0];
            inline.base_color[1] = value[1];
            inline.base_color[2] = value[2];
        }
        P::Emissive => {
            if value.len() < 3 {
                return false;
            }
            inline.emissive = [value[0], value[1], value[2]];
        }
        P::Metallic => match value.first() {
            Some(&v) => inline.metallic = v,
            None => return false,
        },
        P::Roughness => match value.first() {
            Some(&v) => inline.roughness = v,
            None => return false,
        },
    }
    true
}

/// Bind (or clear) a texture on a node's **built-in/inline** `MaterialDef` slot.
/// Returns false if the node is unassigned (no inline store to tweak).
fn patch_builtin_texture(
    kind: &mut NodeKind,
    slot: awsm_editor_protocol::BuiltinTextureSlot,
    texture: Option<AssetId>,
) -> bool {
    use awsm_editor_protocol::BuiltinTextureSlot as S;
    let Some(inst) = node_material_mut(kind) else {
        return false;
    };
    let inline = &mut inst.inline;
    let tref = texture.map(|asset| awsm_editor_protocol::TextureRef {
        asset,
        uv_index: 0,
        transform: None,
        sampler: None,
    });
    match slot {
        S::BaseColor => inline.base_color_texture = tref,
        S::MetallicRoughness => inline.metallic_roughness_texture = tref,
        S::Normal => inline.normal_texture = tref,
        S::Occlusion => inline.occlusion_texture = tref,
        S::Emissive => inline.emissive_texture = tref,
    }
    true
}

/// Bind (or clear) a texture override on a node's assigned custom material.
/// Returns false if the node has no custom-material instance.
fn patch_material_texture(kind: &mut NodeKind, slot: &str, texture: Option<AssetId>) -> bool {
    let Some(inst) = node_material_mut(kind) else {
        return false;
    };
    match texture {
        Some(asset) => {
            inst.texture_overrides.insert(
                slot.to_string(),
                awsm_editor_protocol::TextureRef::new(asset),
            );
        }
        None => {
            inst.texture_overrides.remove(slot);
        }
    }
    true
}

/// Bind (or clear) a buffer-data override on a node's assigned custom material.
/// The `data` words are stashed in the session buffer store and referenced by a
/// synthetic `session://buffer/<id>` path (the bundle bake later emits the bytes
/// then rewrites the path to `assets/buffer-<id>.bin`). Returns false if the node
/// has no custom-material instance.
fn patch_material_buffer(kind: &mut NodeKind, slot: &str, data: Option<Vec<u32>>) -> bool {
    let Some(inst) = node_material_mut(kind) else {
        return false;
    };
    match data {
        Some(words) => {
            let path = crate::engine::bridge::dynamic::store_buffer_words(words);
            inst.buffer_overrides.insert(
                slot.to_string(),
                awsm_editor_protocol::dynamic_material::BufferRef {
                    path: std::path::PathBuf::from(path),
                },
            );
        }
        None => {
            inst.buffer_overrides.remove(slot);
        }
    }
    true
}

/// Patch a light parameter on a `LightConfig`. Returns false if the param
/// doesn't apply to the light kind or `value` is too short.
fn patch_light_param(
    cfg: &mut awsm_editor_protocol::LightConfig,
    param: awsm_editor_protocol::animation::LightParamKind,
    value: &[f32],
) -> bool {
    use awsm_editor_protocol::animation::LightParamKind as P;
    use awsm_editor_protocol::LightConfig as L;
    match param {
        P::Color => {
            if value.len() < 3 {
                return false;
            }
            let c = [value[0], value[1], value[2]];
            match cfg {
                L::Directional { color, .. } | L::Point { color, .. } | L::Spot { color, .. } => {
                    *color = c
                }
            }
        }
        P::Intensity => {
            let Some(&v) = value.first() else {
                return false;
            };
            match cfg {
                L::Directional { intensity, .. }
                | L::Point { intensity, .. }
                | L::Spot { intensity, .. } => *intensity = v,
            }
        }
        P::Range => {
            let Some(&v) = value.first() else {
                return false;
            };
            match cfg {
                L::Point { range, .. } | L::Spot { range, .. } => *range = v,
                L::Directional { .. } => return false,
            }
        }
        P::InnerAngle => {
            let Some(&v) = value.first() else {
                return false;
            };
            match cfg {
                L::Spot { inner_angle, .. } => *inner_angle = v,
                _ => return false,
            }
        }
        P::OuterAngle => {
            let Some(&v) = value.first() else {
                return false;
            };
            match cfg {
                L::Spot { outer_angle, .. } => *outer_angle = v,
                _ => return false,
            }
        }
    }
    true
}

/// Mark a material compile as in-flight (or debounce-scheduled). Paired with
/// [`compile_end`]; the `WaitRenderSettled` query waits for the count to hit 0.
pub(crate) fn compile_begin() {
    let c = controller().compile_pending;
    c.set(c.get().saturating_add(1));
}

/// Mark an in-flight material compile as finished (see [`compile_begin`]).
pub(crate) fn compile_end() {
    let c = controller().compile_pending;
    c.set(c.get().saturating_sub(1));
}

/// Compile + register a dynamic material into a renderer bucket, then
/// re-materialize meshes using it. Returns true on success; leaves
/// `registered = false` on a compile error (the code pane surfaces the problems).
/// Poll the renderer for a dynamic material's real pipeline-compile result after
/// register: `Some(Ok)` ready, `Some(Err(msg))` failed (the browser WGSL/driver
/// diagnostic), or `None` if it never resolved within the window (e.g. a paused
/// RAF/compile loop on a backgrounded tab — the caller then stays optimistic).
/// Polls on a frame cadence so `poll_pipeline_scheduler` (driven by the RAF
/// render loop) can resolve the async compile between checks.
async fn await_dynamic_compile(
    shader_id: awsm_renderer::materials::MaterialShaderId,
) -> Option<Result<(), String>> {
    // ~2s ceiling (shader compiles are typically a handful of frames; the cap
    // just bounds a stuck/backgrounded loop).
    for _ in 0..120 {
        let status = crate::engine::context::with_renderer_mut(move |r| {
            r.dynamic_material_compile_status(shader_id)
        })
        .await;
        if status.is_some() {
            return status; // Ready (Ok) or Failed (Err) — resolved.
        }
        gloo_timers::future::TimeoutFuture::new(16).await;
    }
    None
}

async fn register_material(mat: &Arc<CM>) -> bool {
    // Lightweight, author-relative syntax pre-check — its line numbers index the
    // author's WGSL body (the GPU/naga pass can't, since it sees the assembled
    // module). Record these as diagnostics so MCP callers see them.
    let syntax = compile_wgsl(&mat.wgsl.get_cloned());
    if !syntax.is_empty() {
        mat.last_diagnostics.set(
            syntax
                .into_iter()
                .map(|(line, message)| query::CompileError {
                    line: Some(line as u32),
                    message,
                })
                .collect(),
        );
        mat.registered.set_neq(false);
        return false;
    }
    // Show "Compiling …" in the activity indicator for the duration of the
    // (async, pipeline-building) registration — issue #7.
    let _activity = crate::engine::activity::begin_activity(format!(
        "Compiling material “{}” — render pipelines…",
        mat.name.get_cloned()
    ));
    match crate::engine::bridge::dynamic::register(mat).await {
        Ok(shader_id) => {
            // `register` only QUEUES an async pipeline compile (the launch path
            // skips synchronous shader validation), so a WGSL error in the
            // author body — undefined symbol, type mismatch, garbage — isn't
            // known yet. Poll the scheduler for this material's real compile
            // result and report the truth (the trailing-`;` heuristic above
            // never catches these).
            match await_dynamic_compile(shader_id).await {
                Some(Err(msg)) => {
                    Toast::error(format!("Material compile failed: {msg}"));
                    mat.last_diagnostics.set(vec![query::CompileError {
                        line: None,
                        message: msg,
                    }]);
                    mat.registered.set_neq(false);
                    false
                }
                // Ready, or undetermined (timeout — e.g. a backgrounded tab whose
                // RAF/compile loop is paused). Optimistic: don't invent an error.
                _ => {
                    mat.last_diagnostics.set(Vec::new());
                    mat.registered.set_neq(true);
                    crate::engine::bridge::rematerialize_for_material(mat.id);
                    true
                }
            }
        }
        Err(e) => {
            // Registration-level rejection (name collision / bucket-cap overflow).
            Toast::error(format!("Material compile failed: {e}"));
            mat.last_diagnostics.set(vec![query::CompileError {
                line: None,
                message: e,
            }]);
            mat.registered.set_neq(false);
            false
        }
    }
}

/// Auto-register a dynamic material: compile it now, then re-compile (debounced
/// ~400 ms) on any WGSL edit — so it's always live without a manual Register step.
pub(crate) fn spawn_auto_register(mat: Arc<CM>) {
    use futures_signals::signal::SignalExt;
    let first_mat = mat.clone();
    spawn_local(async move {
        // A fresh material must come up READY (not "draft"). Compile now; if the
        // very first attempt fails (e.g. the renderer's pipeline scheduler is still
        // warming up on a cold load), retry a few times so it doesn't get stuck as
        // a draft requiring a manual edit to recompile.
        compile_begin();
        for attempt in 0..4 {
            if register_material(&first_mat).await {
                break;
            }
            if attempt < 3 {
                gloo_timers::future::TimeoutFuture::new(300).await;
            }
        }
        compile_end();
    });
    spawn_local(async move {
        let gen = std::rc::Rc::new(std::cell::Cell::new(0u64));
        // Observe the recompile counter (bumped by every compile-affecting edit:
        // WGSL, alpha, layout, includes/inputs) — not just the WGSL field.
        let sig = mat.recompile_rev.signal();
        let mut first = true;
        sig.for_each(move |_| {
            let fire = !first;
            first = false;
            let g = gen.get().wrapping_add(1);
            gen.set(g);
            // A recompile is now pending — count it so `WaitRenderSettled` blocks
            // a screenshot until the debounce fires and the bucket refreshes.
            if fire {
                compile_begin();
            }
            let mat = mat.clone();
            let gen = gen.clone();
            async move {
                if !fire {
                    return; // the initial value was already registered above
                }
                gloo_timers::future::TimeoutFuture::new(400).await;
                if gen.get() == g {
                    let _ = register_material(&mat).await;
                }
                compile_end();
            }
        })
        .await;
    });
}

/// Re-materialize meshes using a **built-in** material whenever its shared
/// variant settings change (node_sync re-merges the variant with each mesh's
/// per-mesh uniforms).
pub(crate) fn spawn_builtin_resync(mat: Arc<CM>) {
    use futures_signals::signal::SignalExt;
    let id = mat.id;
    spawn_local(async move {
        let sig = mat.builtin.signal_cloned();
        let mut first = true;
        sig.for_each(move |_| {
            let fire = !first;
            first = false;
            async move {
                if fire {
                    crate::engine::bridge::rematerialize_for_material(id);
                }
            }
        })
        .await;
    });
}

/// Default parameters for a freshly-created procedural texture asset, one per
/// generator family the Content Browser offers.
fn default_procedural(proc: ProceduralKind) -> ProceduralTextureDef {
    match proc {
        ProceduralKind::Checker => ProceduralTextureDef::Checker {
            width: 512,
            height: 512,
            cells_x: 8,
            cells_y: 8,
            color_a: [0.81, 0.83, 0.85, 1.0],
            color_b: [0.16, 0.18, 0.20, 1.0],
        },
        ProceduralKind::Gradient => ProceduralTextureDef::Gradient {
            width: 512,
            height: 512,
            color_a: [0.10, 0.45, 0.95, 1.0],
            color_b: [0.02, 0.02, 0.04, 1.0],
            horizontal: false,
        },
        ProceduralKind::Noise => ProceduralTextureDef::Noise {
            width: 512,
            height: 512,
            seed: 1337,
            scale: 4.0,
        },
    }
}

/// Read the `TextureRef` at an extension texture slot, keyed `"<ext>.<field>"`.
pub(crate) fn get_ext_texture(
    ext: &awsm_editor_protocol::PbrExtensions,
    slot: &str,
) -> Option<awsm_editor_protocol::TextureRef> {
    match slot {
        "specular.tex" => ext.specular.and_then(|e| e.tex),
        "specular.color_tex" => ext.specular.and_then(|e| e.color_tex),
        "transmission.tex" => ext.transmission.and_then(|e| e.tex),
        "diffuse_transmission.tex" => ext.diffuse_transmission.and_then(|e| e.tex),
        "diffuse_transmission.color_tex" => ext.diffuse_transmission.and_then(|e| e.color_tex),
        "volume.thickness_tex" => ext.volume.and_then(|e| e.thickness_tex),
        "clearcoat.tex" => ext.clearcoat.and_then(|e| e.tex),
        "clearcoat.roughness_tex" => ext.clearcoat.and_then(|e| e.roughness_tex),
        "clearcoat.normal_tex" => ext.clearcoat.and_then(|e| e.normal_tex),
        "sheen.color_tex" => ext.sheen.and_then(|e| e.color_tex),
        "sheen.roughness_tex" => ext.sheen.and_then(|e| e.roughness_tex),
        "anisotropy.tex" => ext.anisotropy.and_then(|e| e.tex),
        "iridescence.tex" => ext.iridescence.and_then(|e| e.tex),
        "iridescence.thickness_tex" => ext.iridescence.and_then(|e| e.thickness_tex),
        _ => None,
    }
}

/// Write a resolved extension-texture `TextureRef` onto the matching field of an
/// enabled extension, keyed by `"<ext>.<field>"`. No-op if the extension isn't
/// present (it was the variant enable that decided whether the slot exists).
pub(crate) fn set_ext_texture(
    ext: &mut awsm_editor_protocol::PbrExtensions,
    slot: &str,
    tref: Option<awsm_editor_protocol::TextureRef>,
) {
    match slot {
        "specular.tex" => {
            if let Some(e) = &mut ext.specular {
                e.tex = tref;
            }
        }
        "specular.color_tex" => {
            if let Some(e) = &mut ext.specular {
                e.color_tex = tref;
            }
        }
        "transmission.tex" => {
            if let Some(e) = &mut ext.transmission {
                e.tex = tref;
            }
        }
        "diffuse_transmission.tex" => {
            if let Some(e) = &mut ext.diffuse_transmission {
                e.tex = tref;
            }
        }
        "diffuse_transmission.color_tex" => {
            if let Some(e) = &mut ext.diffuse_transmission {
                e.color_tex = tref;
            }
        }
        "volume.thickness_tex" => {
            if let Some(e) = &mut ext.volume {
                e.thickness_tex = tref;
            }
        }
        "clearcoat.tex" => {
            if let Some(e) = &mut ext.clearcoat {
                e.tex = tref;
            }
        }
        "clearcoat.roughness_tex" => {
            if let Some(e) = &mut ext.clearcoat {
                e.roughness_tex = tref;
            }
        }
        "clearcoat.normal_tex" => {
            if let Some(e) = &mut ext.clearcoat {
                e.normal_tex = tref;
            }
        }
        "sheen.color_tex" => {
            if let Some(e) = &mut ext.sheen {
                e.color_tex = tref;
            }
        }
        "sheen.roughness_tex" => {
            if let Some(e) = &mut ext.sheen {
                e.roughness_tex = tref;
            }
        }
        "anisotropy.tex" => {
            if let Some(e) = &mut ext.anisotropy {
                e.tex = tref;
            }
        }
        "iridescence.tex" => {
            if let Some(e) = &mut ext.iridescence {
                e.tex = tref;
            }
        }
        "iridescence.thickness_tex" => {
            if let Some(e) = &mut ext.iridescence {
                e.thickness_tex = tref;
            }
        }
        _ => {}
    }
}

/// Create (or dedupe) a texture asset for a baked glTF texture key and return a
/// `TextureRef` to it. The asset id is pre-registered against the already-baked
/// renderer `TextureKey`, so when the material resolves this slot it reuses the
/// GPU texture rather than re-decoding (preserving the model's real textures).
fn ensure_import_texture(
    tex_for_key: &mut std::collections::HashMap<awsm_renderer::textures::TextureKey, AssetId>,
    texture_entries: &mut Vec<(AssetId, String)>,
    baked: Option<(
        awsm_renderer::textures::TextureKey,
        crate::engine::bridge::gltf::TexBinding,
    )>,
    name: &str,
) -> Option<awsm_editor_protocol::TextureRef> {
    let (key, binding) = baked?;
    // The texture-asset id is deduped by baked key, but the binding (UV set +
    // transform) is per-slot, so it goes on the TextureRef, not the asset.
    let mk = |asset: AssetId| awsm_editor_protocol::TextureRef {
        asset,
        uv_index: binding.uv_index,
        transform: binding.transform,
        sampler: binding.sampler,
    };
    if let Some(id) = tex_for_key.get(&key) {
        return Some(mk(*id));
    }
    let id = AssetId::new();
    crate::engine::bridge::material::register_texture_key(id, key);
    tex_for_key.insert(key, id);
    texture_entries.push((id, name.to_string()));
    Some(mk(id))
}

/// Mint a captured-mesh `MeshDef` asset from CPU-extracted glTF geometry: store
/// the baked bytes in the [`mesh_cache`](crate::engine::bridge::mesh_cache) under
/// a deterministic id (`AssetId(node_id.0)`, matching the primitive-insert
/// convention) and register an `AssetSource::Mesh` whose stack `base` is
/// [`MeshBase::Captured`] (no modifiers). `source_asset` is the imported model's
/// Filename asset id, recorded as the mesh's [`CapturedSource::Imported`] origin.
/// Returns the `MeshRef` for the new node's `NodeKind::Mesh`.
fn mint_imported_mesh(
    node_id: NodeId,
    label: &str,
    mesh: &awsm_glb_export::MeshData,
    source_asset: AssetId,
) -> awsm_editor_protocol::MeshRef {
    use crate::engine::bridge::mesh_cache;
    use awsm_editor_protocol::{CapturedSource, MeshDef, MeshRef};
    use awsm_editor_protocol::{MeshBase, ModifierStack};

    let mesh_id = AssetId(node_id.0);
    mesh_cache::store_with_id(mesh_id, mesh_cache::from_mesh_data(mesh.clone()));
    let stack = ModifierStack {
        base: MeshBase::Captured(MeshRef(mesh_id)),
        modifiers: vec![],
    };
    controller().scene.assets.lock().unwrap().entries.insert(
        mesh_id,
        AssetEntry::new(SceneAssetSource::Mesh(MeshDef {
            label: label.to_string(),
            source: Some(CapturedSource::Imported {
                source: source_asset,
            }),
            editable: true,
            stack,
            overrides: Default::default(),
        })),
    );
    MeshRef(mesh_id)
}

/// Recursively mirror one glTF template node as an editor `Node`. Mesh-bearing
/// nodes become unified `NodeKind::Mesh` nodes backed by a captured `MeshDef`
/// asset (CPU-extracted glTF geometry, via [`mint_imported_mesh`]); pure
/// transform/bone nodes become `Group`s. The local transform is carried over so
/// the reconstructed hierarchy matches the glTF — the captured geometry is the
/// node's *raw* local accessor positions, so this transform places it with no
/// extra matrix. `fallback_name` only labels an unnamed *top-level* node (so a
/// single-root import shows the file name); children fall back to `Node {index}`.
#[allow(clippy::too_many_arguments)]
/// Build the skin-joint correspondence for an import: every template node flagged
/// `is_skin_joint`, paired with its bone `NodeId` (via `node_map`) and its node
/// index in the re-exported clean rig glb (via `node_flat_indices`). Returns the
/// `SkinJoint` table stored on each `SkinnedMesh` node so the player can bind our
/// clips' bone targets to the rig's baked joints. Joints whose bone or clean
/// index is missing are skipped.
fn assemble_skin_joints(
    nodes: &[crate::engine::bridge::asset_template::AssetTemplateNode],
    node_map: &std::collections::HashMap<u32, NodeId>,
    node_flat_indices: &std::collections::HashMap<u32, u32>,
) -> Vec<awsm_editor_protocol::SkinJoint> {
    let mut out = Vec::new();
    fn walk(
        nodes: &[crate::engine::bridge::asset_template::AssetTemplateNode],
        node_map: &std::collections::HashMap<u32, NodeId>,
        node_flat_indices: &std::collections::HashMap<u32, u32>,
        out: &mut Vec<awsm_editor_protocol::SkinJoint>,
    ) {
        for n in nodes {
            if n.is_skin_joint {
                if let (Some(&node), Some(&index)) = (
                    node_map.get(&n.gltf_node_index),
                    node_flat_indices.get(&n.gltf_node_index),
                ) {
                    out.push(awsm_editor_protocol::SkinJoint { node, index });
                }
            }
            walk(&n.children, node_map, node_flat_indices, out);
        }
    }
    walk(nodes, node_map, node_flat_indices, &mut out);
    out
}

/// Stamp `joints` onto every `SkinnedMesh` node in a freshly-built (not-yet-
/// inserted) subtree. Mutating the kind before insertion avoids triggering a
/// `node_sync` re-materialize (the field is metadata; the renderer mesh is
/// unaffected).
fn patch_skin_joints(
    node: &std::sync::Arc<crate::engine::scene::node::Node>,
    joints: &[awsm_editor_protocol::SkinJoint],
) {
    use awsm_editor_protocol::NodeKind;
    let mut kind = node.kind.get_cloned();
    if let NodeKind::SkinnedMesh { skin, .. } = &mut kind {
        skin.joints = joints.to_vec();
        node.kind.set(kind);
    }
    for child in node.children.lock_ref().iter() {
        patch_skin_joints(child, joints);
    }
}

fn build_editor_subtree(
    tn: &crate::engine::bridge::asset_template::AssetTemplateNode,
    asset_id: AssetId,
    mat_ids: &[AssetId],
    default_mat_id: Option<AssetId>,
    node_meshes: &std::collections::HashMap<(u32, Option<u32>), awsm_glb_export::MeshData>,
    fallback_name: Option<&str>,
    node_map: &mut std::collections::HashMap<u32, NodeId>,
) -> Arc<crate::engine::scene::node::Node> {
    use crate::engine::scene::node::Node;
    use awsm_editor_protocol::{dynamic_material::MaterialInstance, NodeKind, SkinnedMeshRef, Trs};

    let name = tn.label.clone().unwrap_or_else(|| {
        fallback_name
            .map(str::to_string)
            .unwrap_or_else(|| format!("Node {}", tn.gltf_node_index))
    });

    let trs = crate::engine::bridge::asset_template::transform_to_trs(&tn.local);

    // A glTF material index → an assigned library-material *instance* (one
    // material per node, derived at import; the instance is shared across every
    // node that uses this glTF material and can be customized per node). `None`
    // (no such material) leaves the node unassigned → magenta.
    //
    // The instance's `inline` per-mesh store is seeded as a *clone of the
    // assigned material's defaults*. `builtin_merged` then layers its
    // uniform-class fields (factors, extension params, Toon knobs, mask cutoff)
    // over the shared variant, so editing it customizes this node without
    // touching the shared material.
    let instance_for = |mi: Option<usize>| -> Option<MaterialInstance> {
        // A primitive's glTF material index → its library material; a primitive
        // with NO material (`None`) uses glTF's default material (white,
        // metallic=1, roughness=1) rather than the editor's magenta sentinel.
        let id = match mi {
            Some(i) => mat_ids.get(i).copied(),
            None => default_mat_id,
        };
        id.map(|id| {
            let inline = crate::controller::custom_material::find_material(
                &controller().custom_materials,
                id,
            )
            .and_then(|m| m.builtin.get_cloned())
            .unwrap_or_default();
            MaterialInstance {
                asset: id,
                inline,
                ..Default::default()
            }
        })
    };

    // Skinned-ness / morphed-ness is per-primitive; a node qualifies if ANY
    // primitive does. Both categories must keep the populate-baked renderer
    // mesh (`NodeKind::SkinnedMesh`) rather than baking to a captured (static)
    // `Mesh`: skins because the capture freezes at bind pose (the step-2
    // regression), morphs because the captured-MeshData path drops the morph
    // buffers entirely — freezing `set_morph_weight` + morph animation tracks.
    let node_is_skinned =
        tn.mesh_is_skinned.iter().any(|&s| s) || tn.mesh_has_morphs.iter().any(|&m| m);

    let node = if let Some(light_cfg) = &tn.light {
        // A KHR_lights_punctual node → an editable Light node. Its renderer light
        // is (re)created by node_sync `apply_light` bound to THIS node's
        // transform_key, so it follows animation + exposes the shadow inspector.
        // The populate-baked copy was removed at import (`remove_template_lights`).
        Node::new_with_transform_and_kind(name, trs, NodeKind::Light(light_cfg.clone()))
    } else if tn.mesh_keys.is_empty() {
        Node::new_with_transform_and_kind(name, trs, NodeKind::Group)
    } else if node_is_skinned {
        // A skinned mesh node. With one material per node, a single-material node
        // maps 1:1 to one `SkinnedMesh` referencing the whole node; a
        // multi-material node is destructured into one `SkinnedMesh` child per
        // primitive (each with its own `primitive_index` + material), mirroring
        // the static (captured-mesh) destructure path.
        let mat_indices = &tn.mesh_gltf_material_indices;
        let distinct: std::collections::BTreeSet<Option<usize>> =
            mat_indices.iter().copied().collect();
        if distinct.len() <= 1 {
            let material = instance_for(mat_indices.first().copied().flatten());
            // Stash the bind-pose geometry (no JOINTS/WEIGHTS) so `drop_skinning`
            // can bake it to a static editable Mesh later.
            if let Some(mesh) = node_meshes.get(&(tn.gltf_node_index, None)) {
                crate::engine::bridge::skinned_bake_cache::store(
                    asset_id,
                    tn.gltf_node_index,
                    None,
                    mesh.clone(),
                );
            }
            Node::new_with_transform_and_kind(
                name,
                trs,
                NodeKind::SkinnedMesh {
                    skin: SkinnedMeshRef {
                        source: asset_id,
                        node_index: tn.gltf_node_index,
                        primitive_index: None,
                        // Filled after the whole subtree is built (node_map
                        // complete) — see `assemble_skin_joints` / patch below.
                        joints: Vec::new(),
                    },
                    material,
                    shadow: Default::default(),
                },
            )
        } else {
            let group = Node::new_with_transform_and_kind(name.clone(), trs, NodeKind::Group);
            for (i, mi) in mat_indices.iter().enumerate() {
                let material = instance_for(*mi);
                let part_label = material
                    .as_ref()
                    .and_then(|inst| {
                        crate::controller::custom_material::find_material(
                            &controller().custom_materials,
                            inst.asset,
                        )
                        .map(|m| m.name.get_cloned())
                    })
                    .unwrap_or_else(|| format!("{name} · part {i}"));
                if let Some(mesh) = node_meshes.get(&(tn.gltf_node_index, Some(i as u32))) {
                    crate::engine::bridge::skinned_bake_cache::store(
                        asset_id,
                        tn.gltf_node_index,
                        Some(i as u32),
                        mesh.clone(),
                    );
                }
                let part = Node::new_with_transform_and_kind(
                    part_label,
                    Trs::IDENTITY,
                    NodeKind::SkinnedMesh {
                        skin: SkinnedMeshRef {
                            source: asset_id,
                            node_index: tn.gltf_node_index,
                            primitive_index: Some(i as u32),
                            joints: Vec::new(),
                        },
                        material,
                        shadow: Default::default(),
                    },
                );
                group.children.lock_mut().push_cloned(part);
            }
            group
        }
    } else {
        // With one material per node, a node whose primitives all share a
        // material (the common case) maps 1:1 to a single Mesh node. A node whose
        // primitives use *different* materials is destructured: a Group keeps the
        // transform + glTF children, and one Mesh child per primitive carries its
        // own captured geometry + assigned material.
        let mat_indices = &tn.mesh_gltf_material_indices;
        let distinct: std::collections::BTreeSet<Option<usize>> =
            mat_indices.iter().copied().collect();
        if distinct.len() <= 1 {
            let material = instance_for(mat_indices.first().copied().flatten());
            // The whole-node merged geometry (every primitive concatenated).
            let mesh_node = Node::new_with_transform_and_kind(name.clone(), trs, NodeKind::Group);
            if let Some(mesh) = node_meshes.get(&(tn.gltf_node_index, None)) {
                let mesh_ref = mint_imported_mesh(mesh_node.id, &name, mesh, asset_id);
                mesh_node.kind.set(NodeKind::Mesh {
                    mesh: mesh_ref,
                    material,
                    shadow: Default::default(),
                });
            } else {
                tracing::warn!(
                    "import: glTF node {} has mesh keys but no extracted geometry; \
                     leaving an empty Group",
                    tn.gltf_node_index
                );
            }
            mesh_node
        } else {
            let group = Node::new_with_transform_and_kind(name.clone(), trs, NodeKind::Group);
            for (i, mi) in mat_indices.iter().enumerate() {
                let material = instance_for(*mi);
                let part_label = material
                    .as_ref()
                    .and_then(|inst| {
                        crate::controller::custom_material::find_material(
                            &controller().custom_materials,
                            inst.asset,
                        )
                        .map(|m| m.name.get_cloned())
                    })
                    .unwrap_or_else(|| format!("{name} · part {i}"));
                let part = Node::new_with_transform_and_kind(
                    part_label.clone(),
                    Trs::IDENTITY,
                    NodeKind::Group,
                );
                if let Some(mesh) = node_meshes.get(&(tn.gltf_node_index, Some(i as u32))) {
                    let mesh_ref = mint_imported_mesh(part.id, &part_label, mesh, asset_id);
                    part.kind.set(NodeKind::Mesh {
                        mesh: mesh_ref,
                        material,
                        shadow: Default::default(),
                    });
                } else {
                    tracing::warn!(
                        "import: glTF node {} primitive {} has no extracted geometry; \
                         leaving an empty Group",
                        tn.gltf_node_index,
                        i
                    );
                }
                group.children.lock_mut().push_cloned(part);
            }
            group
        }
    };

    // Record this glTF node index → its minted editor `NodeId`, so imported
    // animation channels (keyed by glTF node index) can resolve their target.
    // For a destructured multi-material node, the transform-bearing Group keeps
    // the glTF index (its Mesh-child parts are unindexed primitive splits).
    node_map.insert(tn.gltf_node_index, node.id);

    for child in &tn.children {
        node.children.lock_mut().push_cloned(build_editor_subtree(
            child,
            asset_id,
            mat_ids,
            default_mat_id,
            node_meshes,
            None,
            node_map,
        ));
    }
    node
}

/// The keyframe `values` of an animation sampler (variant-agnostic; tangents
/// live separately on the cubic variant).
fn sampler_values(
    s: &awsm_renderer::animation::AnimationSampler,
) -> &[awsm_renderer::animation::AnimationData] {
    use awsm_renderer::animation::AnimationSampler;
    match s {
        AnimationSampler::Linear { values, .. } => values,
        AnimationSampler::Step { values, .. } => values,
        AnimationSampler::CubicSpline { values, .. } => values,
    }
}

/// Pull a translation vec3 out of an imported `AnimationData::Transform`.
fn extract_translation(d: &awsm_renderer::animation::AnimationData) -> TrackValue {
    match d {
        awsm_renderer::animation::AnimationData::Transform(t) => {
            let v = t.translation.unwrap_or(glam::Vec3::ZERO);
            TrackValue::Vec3([v.x, v.y, v.z])
        }
        _ => TrackValue::Vec3([0.0; 3]),
    }
}

/// Pull a scale vec3 out of an imported `AnimationData::Transform`.
fn extract_scale(d: &awsm_renderer::animation::AnimationData) -> TrackValue {
    match d {
        awsm_renderer::animation::AnimationData::Transform(t) => {
            let v = t.scale.unwrap_or(glam::Vec3::ONE);
            TrackValue::Vec3([v.x, v.y, v.z])
        }
        _ => TrackValue::Vec3([1.0; 3]),
    }
}

/// Pull a rotation quat (xyzw) out of an imported `AnimationData::Transform`
/// (quaternion-native — no Euler conversion).
fn extract_rotation(d: &awsm_renderer::animation::AnimationData) -> TrackValue {
    match d {
        awsm_renderer::animation::AnimationData::Transform(t) => {
            let q = t.rotation.unwrap_or(glam::Quat::IDENTITY);
            TrackValue::Quat([q.x, q.y, q.z, q.w])
        }
        _ => TrackValue::Quat([0.0, 0.0, 0.0, 1.0]),
    }
}

/// Pull morph weight index 0 out of an imported `AnimationData::Vertex`. (Cubic
/// tangents carry the same `Vertex` shape, so this also reads tangent weights.)
fn extract_morph0(d: &awsm_renderer::animation::AnimationData) -> TrackValue {
    match d {
        awsm_renderer::animation::AnimationData::Vertex(v) => {
            TrackValue::Scalar(v.weights.first().copied().unwrap_or(0.0))
        }
        _ => TrackValue::Scalar(0.0),
    }
}

/// Whether any primitive anywhere in the template has no glTF material (so the
/// import needs a default material for them). Recurses the template tree.
fn template_needs_default_material(
    tn: &crate::engine::bridge::asset_template::AssetTemplateNode,
) -> bool {
    tn.mesh_gltf_material_indices.iter().any(|m| m.is_none())
        || tn.children.iter().any(template_needs_default_material)
}

/// The **structural** identity of a kind — what determines which inspector rows
/// exist. Changes on shape/shading/projection/light *variant* (and custom-
/// material presence), but is invariant under numeric edits (radius, fov, …).
/// Drives `structure_rev` so the inspector rebuilds on a discrete toggle but not
/// on a continuous scrub.
fn structure_key(kind: &NodeKind) -> String {
    use awsm_editor_protocol::{CameraProjection, LightConfig, MaterialShading};
    match kind {
        // The Mesh inspector rows depend on the assigned material's shading model
        // (its shared variant) — read it from the per-mesh inline store, which is
        // seeded from that variant. Unassigned → no material rows. (Geometry is no
        // longer edited inline — the base/stack display is informational — so the
        // structure key doesn't vary on the stack base.)
        NodeKind::Mesh { material, .. } => {
            let shading = match material.as_ref().map(|m| m.inline.shading) {
                Some(MaterialShading::Pbr) => "pbr",
                Some(MaterialShading::Unlit) => "unlit",
                Some(MaterialShading::Toon { .. }) => "toon",
                None => "none",
            };
            format!("mesh/{shading}/{}", material.is_some())
        }
        NodeKind::Camera(c) => match c.projection {
            CameraProjection::Perspective { .. } => "cam/persp".into(),
            CameraProjection::Orthographic { .. } => "cam/ortho".into(),
        },
        NodeKind::Light(l) => match l {
            LightConfig::Directional { .. } => "light/dir".into(),
            LightConfig::Point { .. } => "light/point".into(),
            LightConfig::Spot { .. } => "light/spot".into(),
        },
        other => other.label().to_string(),
    }
}

/// Find a track by (clip id, track index) in the live animation library.
fn find_track(
    clips: &MutableVec<Arc<CA>>,
    clip: AssetId,
    track: usize,
) -> Option<Arc<animation::Track>> {
    find_clip(clips, clip).and_then(|c| c.tracks.lock_ref().get(track).map(Arc::clone))
}

/// A coalescing key for continuous edits — consecutive commands with the same
/// key collapse into one undo step. `None` = never coalesce. Animation keys use a
/// disjoint tag space (the `NodeId` slot carries a synthetic id derived from the
/// clip/track/index so the existing scene-node mechanism still applies).
fn coalesce_key(cmd: &EditorCommand) -> Option<(u8, NodeId)> {
    use awsm_editor_protocol::AssetId as Aid;
    // Pack a (clip asset id, small index) into a NodeId so animation edits coalesce
    // per (clip, track/layer, keyframe/strip) identity without a second key type.
    let pack = |asset: Aid, a: usize, b: usize| -> NodeId {
        let mut bytes = asset.0.into_bytes();
        bytes[0] ^= a as u8;
        bytes[1] ^= (a >> 8) as u8;
        bytes[2] ^= b as u8;
        bytes[3] ^= (b >> 8) as u8;
        NodeId(uuid::Uuid::from_bytes(bytes))
    };
    match cmd {
        EditorCommand::SetTransform { id, .. } => Some((0, *id)),
        EditorCommand::Rename { id, .. } => Some((1, *id)),
        EditorCommand::SetKind { id, .. } => Some((2, *id)),
        EditorCommand::SetClipDuration { id, .. } => Some((3, pack(*id, 0, 0))),
        EditorCommand::SetClipSpeed { id, .. } => Some((4, pack(*id, 0, 0))),
        EditorCommand::SetKeyframe {
            clip, track, index, ..
        } => Some((5, pack(*clip, *track, *index))),
        EditorCommand::SetLayerWeight { layer, .. } => {
            Some((6, pack(Aid(uuid::Uuid::nil()), *layer, 0)))
        }
        EditorCommand::MoveStrip { layer, strip, .. }
        | EditorCommand::TrimStrip { layer, strip, .. } => {
            Some((7, pack(Aid(uuid::Uuid::nil()), *layer, *strip)))
        }
        // Material authoring — coalesce continuous edits (WGSL typing, cutoff /
        // color scrubs, dep bulk-toggles) per material into one undo step.
        EditorCommand::SetCustomMaterialWgsl { id, .. } => Some((8, pack(*id, 0, 0))),
        EditorCommand::SetCustomMaterialAlphaMode { id, .. } => Some((9, pack(*id, 0, 0))),
        EditorCommand::SetCustomMaterialDoubleSided { id, .. } => Some((10, pack(*id, 0, 0))),
        EditorCommand::SetCustomMaterialDebugColor { id, .. } => Some((11, pack(*id, 0, 0))),
        EditorCommand::SetCustomMaterialLayout { id, .. } => Some((12, pack(*id, 0, 0))),
        EditorCommand::SetCustomMaterialShaderIncludes { id, .. } => Some((13, pack(*id, 0, 0))),
        EditorCommand::SetCustomMaterialFragmentInputs { id, .. } => Some((14, pack(*id, 0, 0))),
        EditorCommand::SetMaterialUniform { material, name, .. } => {
            let h = name
                .bytes()
                .fold(0usize, |a, b| a.wrapping_mul(31).wrapping_add(b as usize));
            Some((15, pack(*material, h, h >> 16)))
        }
        EditorCommand::SetBuiltinParam { node, .. } => Some((16, *node)),
        EditorCommand::SetLightParam { node, .. } => Some((17, *node)),
        // Mesh editing — collapse a continuous edit (modifier-param scrub, a
        // soft-transform drag) per mesh into one undo step. Explicit
        // `SetVertexPositions` is left granular (distinct edits stay distinct).
        EditorCommand::SetMeshModifiers { mesh, .. } => Some((18, pack(*mesh, 0, 0))),
        EditorCommand::SoftTransformVertices { mesh, .. } => Some((19, pack(*mesh, 0, 0))),
        // Per-vertex authoring — coalesce continuous strokes per mesh + channel
        // (consecutive paints / normal tweaks on one mesh = one undo step). The
        // explicit `SetVertexPositions` stays granular (distinct edits stay
        // distinct, matching the prior behaviour); `BakeAll` is a discrete,
        // never-coalesced finalize.
        EditorCommand::PaintVertexColors { mesh, .. } => Some((20, pack(*mesh, 0, 0))),
        EditorCommand::SetVertexNormals { mesh, .. } => Some((21, pack(*mesh, 0, 0))),
        _ => None,
    }
}

/// Index of `id` within its parent's children (or the scene root when `parent`
/// is `None`). Used to capture a node's position before deletion so undo can
/// restore it in place.
fn node_index(scene: &Scene, id: NodeId, parent: Option<NodeId>) -> Option<usize> {
    match parent {
        None => scene.nodes.lock_ref().iter().position(|n| n.id == id),
        Some(pid) => mutate::find_by_id(scene, pid)
            .and_then(|p| p.children.lock_ref().iter().position(|n| n.id == id)),
    }
}
