use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;

use awsm_renderer_web_shared::prelude::{Mutable, MutableVec, Toast};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;

use super::animation::{find_clip, CustomAnimation as CA};
use super::custom_material::{find_material, CustomMaterial as CM};
use super::*;
use crate::engine::scene::{mutate, AssetId, ColliderShape, NodeId, NodeKind, Scene};
use crate::error::EditorResult;
use awsm_renderer_editor_protocol::{
    AssetEntry, AssetSource as SceneAssetSource, BoundedHistory, MaterialDef, ModifierStack,
    ProceduralTextureDef, TextureColorKind, TextureDef,
};
use std::sync::Arc;

thread_local! {
    static CONTROLLER: OnceCell<EditorController> = const { OnceCell::new() };
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
    // Mirror every toast into the console-log ring buffer (MCP `get_console_logs`).
    awsm_renderer_web_shared::prelude::set_toast_log_hook(|kind, msg| {
        use awsm_renderer_web_shared::prelude::ToastKind;
        let level = match kind {
            ToastKind::Info => "info",
            ToastKind::Warning => "warning",
            ToastKind::Error => "error",
        };
        record_console_log(level, msg);
        // Push the noteworthy notices to the agent (editor → agent channel).
        if matches!(kind, ToastKind::Warning | ToastKind::Error) {
            crate::remote::notify_event(awsm_renderer_editor_protocol::EditorEvent {
                kind: "toast".to_string(),
                level: Some(level.to_string()),
                message: Some(msg.to_string()),
                nodes: None,
                hidden: None,
            });
        }
    });
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
    /// The environment config as of the last save / load / new-project — the
    /// baseline `snapshot()` compares against to surface `env_unsaved` (a
    /// live-applied `set_environment` is lost on reload unless saved; agents
    /// read the flag to know to prompt for a Save). Updated wherever `dirty`
    /// resets to false.
    pub env_saved_baseline: Mutable<awsm_renderer_editor_protocol::EnvironmentConfig>,
    pub missing_assets: Mutable<Vec<String>>,
    pub can_undo: Mutable<bool>,
    pub can_redo: Mutable<bool>,
    /// Bumps only when a `SetKind` changes a node's **structural** shape (the
    /// shape/shading/projection/light *variant*, not a numeric value). The
    /// inspector rebuilds on this so a discrete toggle (PBR↔Unlit, Persp↔Ortho)
    /// refreshes which rows exist — while a continuous numeric scrub, which
    /// keeps the structure key constant, never tears out the field being dragged.
    pub structure_rev: Mutable<u64>,
    /// Bumps after every mutation that arrives from OUTSIDE the local UI — i.e.
    /// the MCP / remote dispatch path (see `remote.rs`). The inspector rebuilds
    /// on this so the seed-once property widgets (light/shadow/material/etc.,
    /// which read their node's value once at build time and are otherwise
    /// one-way) re-seed from the freshly-mutated `node.kind`. Local UI edits do
    /// NOT bump this (they own the value they just typed), so an in-progress
    /// numeric scrub is never torn out by its own dispatch — only an external
    /// (agent) edit forces the refresh. Kept separate from `structure_rev` so
    /// the "structure changed" meaning stays precise.
    pub external_rev: Mutable<u64>,
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
    /// Report of the most recent model import (roots, counts, source asset) —
    /// served by [`EditorQuery::LastImportReport`] and returned inline by the
    /// MCP import tools. `None` until the first import of the session.
    pub last_import_report: Mutable<Option<serde_json::Value>>,
    /// Report of the most recent `VerifyRoundtrip` self-test (before/after
    /// save-census + equality flags) — served by
    /// [`EditorQuery::VerifyRoundtripReport`] (mirrors
    /// [`Self::last_import_report`]). `None` until the self-test first runs.
    pub verify_roundtrip_report: Mutable<Option<serde_json::Value>>,
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
    /// Editor (view-only) settings — viewport toggles, units, etc. Not saved
    /// into the project file.
    pub settings: Settings,
    /// Whether the Settings drawer is open.
    pub settings_open: Mutable<bool>,
    /// Inverses of applied commands, newest last (the undo log). Bounded by a
    /// total-byte budget (drop-oldest) so a high-volume agent session can't grow
    /// it toward the ~2 GB WASM-realloc OOM cliff — see [`BoundedHistory`].
    undo: Rc<RefCell<BoundedHistory>>,
    /// Inverses popped by undo, re-appliable by redo. Same byte budget.
    redo: Rc<RefCell<BoundedHistory>>,
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
    /// Auto-key: in ANIMATION mode, a gizmo edit on a node that the current
    /// clip tracks writes keyframe(s) at the playhead automatically. **On by
    /// default** — DCC users expect dragging the gizmo to record a key without
    /// hunting for a toggle (the toggle still lives in the transport so it can be
    /// turned off). Only ever fires in Animation mode, so a default-on value is
    /// inert in Scene/Material modes.
    pub auto_key: Mutable<bool>,
    pub msaa: Mutable<bool>,
    /// SMAA (post-process morphological AA). Transient, like [`Self::msaa`] —
    /// NOT persisted (AA is a player/runtime decision, not scene data); it's here
    /// only so aliasing can be eyeballed/debugged in the editor viewport. Drives
    /// the renderer's `AntiAliasing::smaa` via `settings_sync`. Off by default
    /// (MSAA already on), independent of MSAA so both can be compared.
    pub smaa: Mutable<bool>,
    pub heatmap: Mutable<bool>,
    /// Edge-aware shadow denoise blur (global). Drives the renderer's
    /// `ShadowsConfig::denoise` via `settings_sync`.
    pub shadow_denoise: Mutable<bool>,
    pub snap: Mutable<bool>,
    pub units: Mutable<String>,
    /// Built-in editor view camera projection: `true` = orthographic, `false` =
    /// perspective. Kept authoritative by the `SetCameraProjection` handler, so the
    /// viewport toggle/keyboard shortcut and any MCP-driven change stay in sync.
    pub editor_ortho: Mutable<bool>,
    /// Editor-camera clip planes: `false` (default) = AUTO — near/far are
    /// re-derived from the orbit distance every move by a depth-precision-aware
    /// formula (bounded ~5000:1 far:near ratio, and `far` can't clip the scene);
    /// `true` = MANUAL, near/far pinned to
    /// [`Self::cam_clip_near`]/[`Self::cam_clip_far`]. Session-only (the editor
    /// camera isn't persisted).
    pub cam_clip_manual: Mutable<bool>,
    /// Manual near plane (metres). Applied only when [`Self::cam_clip_manual`].
    pub cam_clip_near: Mutable<f64>,
    /// Manual far plane (metres). Applied only when [`Self::cam_clip_manual`].
    pub cam_clip_far: Mutable<f64>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            // Grid + light gizmos default OFF (David, 2026-07-12): they
            // pollute screenshots/agent verification and lit scenes read
            // better without them; toggle on per session when needed.
            grid: Mutable::new(false),
            gizmo: Mutable::new(true),
            light_gizmos: Mutable::new(false),
            skeleton_viz: Mutable::new(true),
            auto_key: Mutable::new(true),
            msaa: Mutable::new(true),
            smaa: Mutable::new(false),
            heatmap: Mutable::new(false),
            // On by default — matches the renderer's `ShadowsConfig::denoise`
            // default; keeps point-light soft/PCSS penumbras clean out of box.
            shadow_denoise: Mutable::new(true),
            snap: Mutable::new(false),
            units: Mutable::new("meters".to_string()),
            editor_ortho: Mutable::new(false),
            // AUTO by default — the robust orbit-distance formula (see
            // `free_camera::auto_clip_planes`) eliminates the z-fighting the old
            // manual 0.1/10000 (100,000:1) default caused. Manual stays an
            // escape hatch with a saner starting pair.
            cam_clip_manual: Mutable::new(false),
            cam_clip_near: Mutable::new(1.0),
            cam_clip_far: Mutable::new(5000.0),
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
            env_saved_baseline: Mutable::new(Default::default()),
            missing_assets: Mutable::new(Vec::new()),
            can_undo: Mutable::new(false),
            can_redo: Mutable::new(false),
            structure_rev: Mutable::new(0),
            external_rev: Mutable::new(0),
            content_browser_open: Mutable::new(false),
            active_camera: Mutable::new(None),
            asset_selection: Mutable::new(None),
            custom_materials: MutableVec::new(),
            current_material: Mutable::new(None),
            last_import_report: Mutable::new(None),
            verify_roundtrip_report: Mutable::new(None),
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
            settings: Settings::default(),
            settings_open: Mutable::new(false),
            undo: Rc::new(RefCell::new(BoundedHistory::with_default_budget())),
            redo: Rc::new(RefCell::new(BoundedHistory::with_default_budget())),
            compile_pending: Rc::new(Cell::new(0)),
        }
    }

    /// The single entry point. UI handlers build a command and dispatch it here;
    /// Force the inspector to re-seed its property widgets. Call this after a
    /// mutation that did NOT originate from a local UI widget (the MCP / remote
    /// path) so the seed-once light/shadow/material inspectors pick up the new
    /// `node.kind` values. Local edits skip this — their widgets already hold the
    /// value the user typed, so bumping here would tear an in-progress scrub.
    pub fn note_external_mutation(&self) {
        self.external_rev
            .set(self.external_rev.get().wrapping_add(1));
    }

    /// async because some commands await the renderer / FS / network.
    pub async fn dispatch(&self, cmd: EditorCommand) -> EditorResult<()> {
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

    /// Apply a command and, if it changes anything the renderer must re-lower
    /// for animation, bump [`Self::anim_revision`] — the single signal the bridge
    /// debounced-observes. This is the ONE chokepoint every path (`dispatch`,
    /// undo, redo) funnels through, so no edit can skip the
    /// re-lower (the stale-channel bug). The actual effect lives in `apply_inner`.
    async fn apply(&self, cmd: EditorCommand) -> EditorResult<Option<EditorCommand>> {
        // A `Batch` applies its sub-commands in order (each a leaf — batches don't
        // nest) and returns a `Batch` of their inverses, reversed, so undo replays
        // them back-to-front as one step. Handled here (not `apply_inner`) so the
        // async fn doesn't recurse into itself.
        if let EditorCommand::Batch(cmds) = cmd {
            let mut inverses = Vec::new();
            for (i, c) in cmds.into_iter().enumerate() {
                let touches_anim = c.affects_animation();
                let touches_mesh = c.affects_mesh();
                // Name the failing sub-command (index + human label) — a bare
                // index is useless in a 150-command batch.
                let label = c.label();
                match Box::pin(self.apply_inner(c)).await {
                    Ok(Some(inv)) => inverses.push(inv),
                    Ok(None) => {}
                    Err(e) => {
                        return Err(crate::error::EditorError::msg(format!(
                            "batch[{i}] ({label}): {e}"
                        )));
                    }
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
    /// undo entry. The MCP `dispatch_batch` round-trips here. The combined inverse
    /// is pushed as one `Batch` so undo reverses the whole thing.
    pub async fn dispatch_batch(&self, cmds: Vec<EditorCommand>) -> EditorResult<()> {
        let mut inverses = Vec::new();
        let mut any_recorded = false;
        for (i, cmd) in cmds.into_iter().enumerate() {
            let transient = cmd.is_transient();
            // Name the failing sub-command (index + human label) — a bare
            // index is useless in a 150-command batch.
            let label = cmd.label();
            let inv = self.apply(cmd).await.map_err(|e| {
                crate::error::EditorError::msg(format!("batch[{i}] ({label}): {e}"))
            })?;
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
    fn apply_mesh_stack(&self, mesh: AssetId, mut stack: ModifierStack) -> Option<EditorCommand> {
        use crate::engine::bridge::mesh_cache;
        use awsm_renderer_editor_protocol::{MeshBase, MeshRef};
        // Capture the prior recipe + overrides + baked bytes for the inverse, and
        // bail if this asset isn't a mesh.
        let (prior_stack, prior_overrides) = match self
            .scene
            .assets
            .lock()
            .unwrap()
            .get(mesh)
            .map(|e| &e.source)
        {
            Some(SceneAssetSource::Mesh(def)) => (def.stack.clone(), def.overrides.clone()),
            _ => return None,
        };
        let prior_bytes = mesh_cache::get_captured(mesh);
        let requested_base = stack.base.clone();

        // Relocate a self-aliasing `Captured(mesh)` base the moment it gains
        // modifiers. The render bake writes `mesh_cache[mesh]`, so a `Captured(mesh)`
        // base would read its OWN previous output (modifiers already baked in) and
        // re-apply the stack — compounding geometry (the 4096 → ×256 field report).
        // Copy the frozen bytes to a distinct snapshot id ONCE and point the base
        // there; from then on the bake reads the immutable snapshot.
        if let MeshBase::Captured(MeshRef(r)) = requested_base {
            if r == mesh && !stack.modifiers.is_empty() {
                let snap = captured_snapshot_id(mesh);
                if mesh_cache::get_captured(snap).is_none() {
                    if let Some(frozen) = mesh_cache::get_captured(mesh) {
                        mesh_cache::store_with_id(snap, frozen);
                    }
                }
                stack.base = MeshBase::Captured(MeshRef(snap));
            }
        }

        // A genuine base change (a different generator) invalidates the
        // index-keyed overrides — they reference the prior topology. Drop them so
        // the new recipe regenerates clean (the stale-ghost-tip field report).
        // AddModifier/SetModifier/RemoveModifier keep the base, so this only fires
        // on a `SetMeshModifiers` that swaps the base.
        let base_changed = requested_base != prior_stack.base;
        let clear_overrides = base_changed && !prior_overrides.is_empty();

        let def = {
            let mut assets = self.scene.assets.lock().unwrap();
            match assets.entries.get_mut(&mesh).map(|e| &mut e.source) {
                Some(SceneAssetSource::Mesh(def)) => {
                    def.stack = stack;
                    def.editable = true;
                    if clear_overrides {
                        def.overrides = Default::default();
                    }
                    def.clone()
                }
                _ => return None,
            }
        };
        // Re-evaluate → re-bake the cache (the bridge re-materializes via the
        // mesh-revision bump in `apply`).
        let baked = crate::controller::mesh_eval::evaluate_def(&self.scene, &def);
        mesh_cache::store_with_id(mesh, mesh_cache::from_mesh_data(baked));
        self.scene.bump_revision();

        // Inverse. The frozen snapshot id is never clobbered, so restoring a
        // `Captured(snapshot)` recipe re-bakes correctly on its own — but a
        // `Captured(mesh)`-self prior recipe read the now-overwritten cache, and a
        // base swap dropped overrides, so those restore the exact prior bytes +
        // overrides explicitly (SetMeshData → SetVertexOverrides re-bake last).
        let prior_self_captured =
            matches!(prior_stack.base, MeshBase::Captured(MeshRef(r)) if r == mesh);
        let restore_stack = EditorCommand::SetMeshModifiers {
            mesh,
            stack: prior_stack,
        };
        if !clear_overrides && !prior_self_captured {
            return Some(restore_stack);
        }
        let mut inv = vec![restore_stack];
        if let Some(bytes) = prior_bytes {
            inv.push(EditorCommand::SetMeshData {
                mesh,
                data: bytes,
                allow_empty: true,
            });
        }
        inv.push(EditorCommand::SetVertexOverrides {
            mesh,
            overrides: prior_overrides,
        });
        Some(EditorCommand::Batch(inv))
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
    /// Resolve a node to its editable mesh asset id + resolved (post-eval)
    /// geometry — the pair the fused `*_where` vertex ops (§10) need: the asset
    /// id to write overrides on, the geometry to run the predicate against.
    /// Errors (as a string) on a skinned node, a missing node, or a non-mesh kind.
    fn node_editable_mesh(
        &self,
        node: NodeId,
    ) -> Result<(AssetId, awsm_renderer_meshgen::MeshData), String> {
        use awsm_renderer_editor_protocol::{MeshRef, NodeKind};
        if node_is_skinned(&self.scene, node) {
            return Err(skinned_edit_error(node));
        }
        let n = mutate::find_by_id(&self.scene, node)
            .ok_or_else(|| format!("node {node} not found"))?;
        let kind = n.kind.get_cloned();
        let mesh_id = match &kind {
            NodeKind::Mesh {
                mesh: MeshRef(id), ..
            } => *id,
            _ => {
                return Err(format!(
                    "node {node} has no editable mesh (not a Mesh node)"
                ))
            }
        };
        let md = crate::controller::export::node_mesh(&self.scene, &kind)
            .ok_or_else(|| format!("node {node} has no resolvable mesh geometry"))?;
        Ok((mesh_id, md))
    }

    /// Soft-transform a vertex selection on `mesh` (shared by
    /// `SoftTransformVertices` and the fused `TransformVerticesWhere`, §10).
    fn soft_transform_mesh(
        &self,
        mesh: AssetId,
        indices: &[u32],
        translation: [f32; 3],
        falloff: f32,
    ) -> EditorResult<Option<EditorCommand>> {
        use crate::engine::bridge::mesh_cache;
        // Resolve the current (post-eval+override) geometry to weight the falloff
        // against, then bake the move into `overrides.positions`.
        let collapse = self.ensure_authorable(mesh)?;
        let Some(cap) = mesh_cache::get_captured(mesh) else {
            return Ok(None);
        };
        let md = awsm_renderer_meshgen::MeshData {
            positions: cap.positions.clone(),
            normals: cap.normals.clone(),
            uvs: cap
                .uvs
                .clone()
                .into_iter()
                .chain(cap.uvs1.clone())
                .collect(),
            colors: cap.colors.clone(),
            indices: cap.indices.clone(),
        };
        let new_positions = awsm_renderer_meshgen::edit::soft_transform_positions(
            &md,
            indices,
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

    fn ensure_authorable(&self, mesh: AssetId) -> EditorResult<Option<ModifierStack>> {
        use crate::engine::bridge::mesh_cache;
        use awsm_renderer_editor_protocol::MeshRef;
        use awsm_renderer_editor_protocol::{MeshBase, ModifierStack};
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
        mutate: impl FnOnce(&mut awsm_renderer_editor_protocol::VertexOverrides),
    ) -> EditorResult<awsm_renderer_editor_protocol::VertexOverrides> {
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
        prior: awsm_renderer_editor_protocol::VertexOverrides,
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
        use awsm_renderer_editor_protocol::{
            CapturedSource, MeshDef, MeshRef, PrimitiveShape, Trs,
        };
        use awsm_renderer_editor_protocol::{MeshBase, ModifierStack};

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
                let def = awsm_renderer_editor_protocol::SweepAlongCurveDef::default();
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
                material_variants: Vec::new(),
                selected_variant: None,
                shadow: Default::default(),
                lod: Default::default(),
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
                crate::remote::notify_event(awsm_renderer_editor_protocol::EditorEvent {
                    kind: "selection".to_string(),
                    level: None,
                    message: None,
                    nodes: Some(ids.iter().map(|id| id.to_string()).collect()),
                    hidden: None,
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
                None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
            },
            EditorCommand::PatchKind { id, patch } => {
                // RFC 7386 merge-patch over the node's serialized kind (§3). Reject
                // loudly if the result isn't a valid NodeKind — never store-and-
                // ignore. Delegate the actual swap to SetKind so the structure-rev
                // / light-relower / re-materialize bookkeeping + inverse match a
                // full kind replacement exactly.
                let prev = match mutate::find_by_id(&self.scene, id) {
                    Some(node) => node.kind.get_cloned(),
                    None => return Ok(None),
                };
                let mut json = serde_json::to_value(&prev)
                    .map_err(|e| crate::error::EditorError::msg(format!("serialize kind: {e}")))?;
                // Some MCP clients stringify the patch (a bare `Value` param derives
                // an unconstrained schema). Coerce a JSON-string patch back to
                // structured JSON before merging, so it doesn't replace the whole
                // kind wholesale. Covers both `patch_kind` and `dispatch_command`.
                let patch = awsm_renderer_editor_protocol::coerce_patch(patch)
                    .map_err(crate::error::EditorError::msg)?;
                awsm_renderer_editor_protocol::json_merge_patch(&mut json, &patch);
                let next: NodeKind = serde_json::from_value(json).map_err(|e| {
                    crate::error::EditorError::msg(format!(
                        "patched kind is not a valid NodeKind: {e}"
                    ))
                })?;
                Box::pin(self.apply_inner(EditorCommand::SetKind {
                    id,
                    kind: Box::new(next),
                }))
                .await
            }
            EditorCommand::SetParticleEmitter {
                node,
                spawn_rate,
                burst_count,
                max_alive,
                one_shot,
                space,
                shape,
                initial_speed,
                lifetime,
                size,
                forces,
                color_over_life,
                size_over_life,
                blend,
                texture,
            } => {
                let prev = match mutate::find_by_id(&self.scene, node) {
                    Some(n) => n.kind.get_cloned(),
                    None => return Ok(None),
                };
                // Reject loudly if the node isn't an emitter (no silent no-op).
                let NodeKind::ParticleEmitter(mut def) = prev.clone() else {
                    return Err(crate::error::EditorError::msg(
                        "node is not a particle emitter",
                    ));
                };
                // Patch only the provided fields; the rest keep their values.
                if let Some(v) = spawn_rate {
                    def.spawn_rate = v;
                }
                if let Some(v) = burst_count {
                    def.burst_count = v;
                }
                if let Some(v) = max_alive {
                    def.max_alive = v;
                }
                if let Some(v) = one_shot {
                    def.one_shot = v;
                }
                if let Some(v) = space {
                    def.space = v;
                }
                if let Some(v) = shape {
                    def.shape = v;
                }
                if let Some(v) = initial_speed {
                    def.initial_speed = v;
                }
                if let Some(v) = lifetime {
                    def.lifetime = v;
                }
                if let Some(v) = size {
                    def.size = v;
                }
                if let Some(v) = forces {
                    def.forces = v;
                }
                if let Some(v) = color_over_life {
                    def.color_over_life = v;
                }
                if let Some(v) = size_over_life {
                    def.size_over_life = v;
                }
                if let Some(v) = blend {
                    def.blend = v;
                }
                // §14: bind/clear the billboard sprite texture. Some(Some) binds,
                // Some(None) clears, None leaves it untouched.
                if let Some(tex) = texture {
                    def.texture = tex.map(awsm_renderer_editor_protocol::TextureRef::new);
                }
                // Delegate to SetKind for identical re-materialize + inverse.
                Box::pin(self.apply_inner(EditorCommand::SetKind {
                    id: node,
                    kind: Box::new(NodeKind::ParticleEmitter(def)),
                }))
                .await
            }
            EditorCommand::SetInstancerTransforms {
                node,
                transforms,
                per_instance_colors,
            } => {
                let prev = match mutate::find_by_id(&self.scene, node) {
                    Some(n) => n.kind.get_cloned(),
                    None => {
                        return Err(crate::error::EditorError::msg(
                            "target id not found — command not applied (check ids against get_snapshot)",
                        ))
                    }
                };
                // Reject loudly if the node isn't an instancer (no silent no-op).
                let NodeKind::Instancer(mut def) = prev else {
                    return Err(crate::error::EditorError::msg("node is not an instancer"));
                };
                // REPLACE the transform list wholesale (the bulk authoring
                // contract — mirrors SetTrackKeys). Colors only when provided.
                def.transforms = transforms;
                if let Some(colors) = per_instance_colors {
                    def.per_instance_colors = colors;
                }
                // Delegate to SetKind for identical re-materialize + inverse
                // (undo restores the ENTIRE prior kind = the prior list).
                Box::pin(self.apply_inner(EditorCommand::SetKind {
                    id: node,
                    kind: Box::new(NodeKind::Instancer(def)),
                }))
                .await
            }
            EditorCommand::SetTransform { id, mut transform } => {
                match mutate::find_by_id(&self.scene, id) {
                    Some(node) => {
                        // A Rapier collider has no scale: its size is the
                        // ColliderShape extents and its placement is an isometry
                        // (translation + rotation). Scale on a collider node is
                        // silently dropped at export (`ColliderSpec::from_node`), so
                        // lock it to unit here — the single chokepoint every write
                        // (gizmo, inspector, MCP, import) flows through. (FIXES.md #2.)
                        if matches!(node.kind.get_cloned(), NodeKind::Collider(_)) {
                            transform.scale = [1.0, 1.0, 1.0];
                        }
                        let prev = node.transform.get();
                        node.transform.set(transform);
                        self.scene.bump_revision();
                        Ok(Some(EditorCommand::SetTransform {
                            id,
                            transform: prev,
                        }))
                    }
                    None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
                }
            }
            EditorCommand::ResetToBindPose { id } => {
                let Some(root) = mutate::find_by_id(&self.scene, id) else {
                    return Err(crate::error::EditorError::msg(format!("no node {id}")));
                };
                let rest = crate::engine::bridge::bridge()
                    .joint_rest
                    .lock()
                    .unwrap()
                    .clone();
                // Restore every joint in the subtree that has a recorded rest;
                // collect per-node inverses so undo replays the prior pose.
                fn walk(
                    node: &std::sync::Arc<crate::engine::scene::node::Node>,
                    rest: &std::collections::HashMap<NodeId, crate::engine::scene::Trs>,
                    inverses: &mut Vec<EditorCommand>,
                ) {
                    if let Some(r) = rest.get(&node.id) {
                        let prev = node.transform.get();
                        if prev != *r {
                            node.transform.set(*r);
                            inverses.push(EditorCommand::SetTransform {
                                id: node.id,
                                transform: prev,
                            });
                        }
                    }
                    for child in node.children.lock_ref().iter() {
                        walk(child, rest, inverses);
                    }
                }
                let mut inverses = Vec::new();
                walk(&root, &rest, &mut inverses);
                if inverses.is_empty() {
                    // Nothing was posed away from rest (or no joints under here).
                    return Ok(None);
                }
                self.scene.bump_revision();
                inverses.reverse();
                Ok(Some(EditorCommand::Batch(inverses)))
            }
            EditorCommand::Rename { id, name } => match mutate::find_by_id(&self.scene, id) {
                Some(node) => {
                    let prev = node.name.get_cloned();
                    node.name.set(name);
                    self.scene.bump_revision();
                    Ok(Some(EditorCommand::Rename { id, name: prev }))
                }
                None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
            },
            EditorCommand::SetVisible { id, visible } => match mutate::find_by_id(&self.scene, id) {
                Some(node) => {
                    let prev = node.visible.get();
                    node.visible.set_neq(visible);
                    self.scene.bump_revision();
                    Ok(Some(EditorCommand::SetVisible { id, visible: prev }))
                }
                None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
            },
            EditorCommand::SetLocked { id, locked } => match mutate::find_by_id(&self.scene, id) {
                Some(node) => {
                    let prev = node.locked.get();
                    node.locked.set_neq(locked);
                    self.scene.bump_revision();
                    Ok(Some(EditorCommand::SetLocked { id, locked: prev }))
                }
                None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
            },
            EditorCommand::SetPrefab { id, prefab } => match mutate::find_by_id(&self.scene, id) {
                Some(node) => {
                    let prev = node.prefab.get();
                    node.prefab.set_neq(prefab);
                    self.scene.bump_revision();
                    Ok(Some(EditorCommand::SetPrefab { id, prefab: prev }))
                }
                None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
            },
            EditorCommand::Duplicate { id, new_id } => {
                match mutate::duplicate_by_id(&self.scene, id, new_id) {
                    Some((new_id, id_map)) => {
                        self.scene.bump_revision();
                        self.selected.set(vec![new_id]);
                        // RETARGET animation onto the clone: any clip track that
                        // targets a node inside the duplicated subtree gets a
                        // duplicated track driving the cloned node — so playing
                        // the clip animates the original AND the duplicate (a
                        // duplicated walking character keeps walking). Model
                        // choice (documented on `retarget_track_for_duplicate`):
                        // EXTEND the same clip rather than mint a clip per clone,
                        // so the one authored clip drives every instance. The
                        // inverse batches `DeleteTrack`s (descending indices)
                        // before the node `Delete`, keeping undo coherent.
                        let mut inverse: Vec<EditorCommand> = Vec::new();
                        for clip in self.custom_animations.lock_ref().iter() {
                            let clones: Vec<_> = clip
                                .tracks
                                .lock_ref()
                                .iter()
                                .filter_map(|t| {
                                    crate::controller::animation::retarget_track_for_duplicate(
                                        t, &id_map,
                                    )
                                })
                                .collect();
                            if clones.is_empty() {
                                continue;
                            }
                            let mut tracks = clip.tracks.lock_mut();
                            for track in clones {
                                inverse.push(EditorCommand::DeleteTrack {
                                    clip: clip.id,
                                    track: tracks.len(),
                                });
                                tracks.push_cloned(track);
                            }
                        }
                        if inverse.is_empty() {
                            return Ok(Some(EditorCommand::Delete { id: new_id }));
                        }
                        // `Duplicate` isn't in `affects_animation` (the common
                        // case adds no tracks) — bump the relower signal here,
                        // where tracks WERE added, so the clip lowers onto the
                        // clone as soon as it materializes.
                        self.anim_revision.replace_with(|v| v.wrapping_add(1));
                        // Descending track indices so each `DeleteTrack` in the
                        // batch still points at the right slot after the ones
                        // behind it are removed.
                        inverse.reverse();
                        inverse.push(EditorCommand::Delete { id: new_id });
                        Ok(Some(EditorCommand::Batch(inverse)))
                    }
                    None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
                }
            }
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
                // Transient vertex-selection view state — its viewport markers
                // otherwise survive the reset (stress-test finding: a ghost
                // dome of highlight crosses floating in the fresh project).
                self.vertex_selection.set(None);
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
                crate::engine::bridge::texture_cache::clear();
                crate::engine::bridge::material::clear_texture_keys();
                crate::engine::bridge::buffer_cache::clear();
                self.project_name.set("untitled.awsm".to_string());
                self.missing_assets.set(Vec::new());
                // Seed a sane default scene: a key directional light (tilted ~50°
                // by `new_light`) + the built-in skybox/IBL environment, so the
                // first PBR/lit material isn't black out of the box (the §E3 fix —
                // applies to the human editor and MCP alike).
                let light = build_insert(&InsertSpec::Light(
                    awsm_renderer_editor_protocol::LightKind::Directional,
                ));
                mutate::insert_under(&self.scene, None, light);
                self.scene
                    .environment
                    .set(awsm_renderer_editor_protocol::EnvironmentConfig::default());
                self.scene
                    .shadows
                    .set(awsm_renderer_editor_protocol::ShadowsConfig::default());
                self.scene
                    .post_process
                    .set(awsm_renderer_editor_protocol::PostProcessConfig::default());
                self.scene.bump_revision();
                self.dirty.set_neq(false);
                self.env_saved_baseline
                    .set(self.scene.environment.get_cloned());
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

                // Hold the `WaitRenderSettled` barrier for the WHOLE handler
                // (created synchronously, before the first await): a driver
                // dispatching through the fire-and-forget seam
                // (`editor_dispatch_json` spawns the dispatch and returns
                // immediately) must not observe "settled" while the bake +
                // populate are still running. RAII — drops on every exit path.
                let _load_guard = CompileGuard::new();
                // 1. Bake the CURRENT project — must read it before we clear.
                let files = crate::controller::export::bake_player_bundle(self, None)
                    .await
                    .map_err(|e| crate::error::EditorError::msg(format!("bake: {e}")))?;
                // 2. Split scene.toml out; the rest is the asset map
                //    (bundle-relative path → bytes) `populate_awsm_scene` reads.
                let mut scene_toml: Option<String> = None;
                let mut assets: std::collections::HashMap<String, Vec<u8>> =
                    std::collections::HashMap::new();
                for f in files {
                    if f.path == awsm_renderer_editor_protocol::SCENE_FILE {
                        scene_toml = Some(String::from_utf8_lossy(&f.bytes).into_owned());
                    } else {
                        assets.insert(f.path, f.bytes);
                    }
                }
                let scene_toml = scene_toml
                    .ok_or_else(|| crate::error::EditorError::msg("bundle missing scene.toml"))?;
                let scene = awsm_renderer_editor_protocol::scene_from_toml(&scene_toml)
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
                crate::engine::bridge::texture_cache::clear();
                crate::engine::bridge::material::clear_texture_keys();
                crate::engine::bridge::buffer_cache::clear();
                // Unregister the editor session's dynamic materials BEFORE the
                // player populate: the round-trip shares this renderer, and the
                // bundle re-registers the same material ids — with the session
                // registrations still live, the loader's register hits the
                // duplicate-name guard and every custom-material mesh falls back
                // to the default (white) material. A real player boots a fresh
                // renderer and can't collide; this is round-trip-seam-only.
                crate::engine::bridge::dynamic::unregister_all().await;
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
                    let res = awsm_renderer_scene_loader::populate_awsm_scene(
                        &mut r,
                        &scene,
                        &assets,
                        |p| {
                            crate::engine::activity::set_load_phase(Some(p.label()));
                        },
                    )
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
                self.env_saved_baseline
                    .set(self.scene.environment.get_cloned());
                self.undo.borrow_mut().clear();
                self.redo.borrow_mut().clear();
                self.refresh_history_signals();
                Toast::info("Round-trip: reloaded via populate_awsm_scene");
                Ok(None)
            }
            EditorCommand::ReloadProjectInMemory => {
                use crate::controller::persistence;
                // Settle-visibility front guard (see LoadPlayerBundle): held
                // synchronously from handler entry until the arm returns; the
                // async materialization tail past `apply_inmem` is covered by
                // the armed load barrier (`apply_project` → node_sync release).
                let _load_guard = CompileGuard::new();
                // Editor-path round-trip self-test (no dir picker). Serialize the
                // open project to its persisted form BEFORE clearing anything.
                let (toml, mesh_map) = persistence::serialize_inmem(self)?;
                // Faithfully model a COLD load: drop the session-local caches a
                // fresh page wouldn't have — imported-glTF templates + their
                // renderer meshes, the skinned bind-pose/rig cache, and skin-joint
                // mappings. Without this a skinned model's stale template would
                // survive and mask the real save→reload gap (skinned data is held
                // only in these session-local caches, not in project.toml). The
                // captured-mesh `mesh_cache` is intentionally NOT cleared — its
                // bytes ARE persisted (`.mesh.bin`) and `apply_inmem` restores them.
                crate::engine::bridge::bridge().clear_skin_joints();
                clear_untracked_renderer_resources().await;
                crate::engine::bridge::bridge().clear_templates();
                crate::engine::bridge::skinned_bake_cache::clear();
                crate::engine::bridge::texture_cache::clear();
                crate::engine::bridge::material::clear_texture_keys();
                crate::engine::bridge::buffer_cache::clear();
                // View-only cluster ("nanite") DAGs live only in `cluster_cache`;
                // drop it too so the round-trip exercises the real save→reload
                // restore (`restore_cluster_meshes` re-reads the persisted DAG).
                crate::engine::bridge::cluster_cache::clear();
                persistence::apply_inmem(self, toml, mesh_map).await?;
                Toast::info("Round-trip: project reloaded in-memory (cold caches)");
                Ok(None)
            }
            EditorCommand::VerifyRoundtrip => {
                use crate::controller::persistence;
                // Settle-visibility front guard (see LoadPlayerBundle).
                let _load_guard = CompileGuard::new();
                // End-to-end losslessness proof (destructive self-test, not
                // undoable). Census FIRST — the ground truth the cold reload
                // must reproduce — then serialize while every cache is warm.
                let before = persistence::save_census(self);
                let (toml, byte_map) = persistence::serialize_inmem(self)?;
                // Clear EVERY byte cache the reload path could otherwise
                // reuse — unlike `ReloadProjectInMemory`, which deliberately
                // keeps `mesh_cache` warm (exactly where the historical
                // byte-loss bug hid). If `apply_inmem` can rebuild the census
                // from the serialized bytes ALONE, save→load drops nothing.
                crate::engine::bridge::bridge().clear_skin_joints();
                clear_untracked_renderer_resources().await;
                crate::engine::bridge::bridge().clear_templates();
                crate::engine::bridge::skinned_bake_cache::clear();
                crate::engine::bridge::texture_cache::clear();
                crate::engine::bridge::material::clear_texture_keys();
                crate::engine::bridge::buffer_cache::clear();
                crate::engine::bridge::cluster_cache::clear();
                crate::engine::bridge::mesh_cache::clear();
                crate::engine::bridge::env_sync::clear_ktx_stash();
                persistence::apply_inmem(self, toml, byte_map).await?;
                let after = persistence::save_census(self);
                let equal = before == after;
                // Lossless = the reload reproduced the census exactly AND the
                // reloaded project is itself fully persistable (no cache went
                // missing on the way back in).
                let after_complete = after.is_complete();
                let lossless = equal && after_complete;
                let report = serde_json::json!({
                    "before": before,
                    "after": after,
                    "equal": equal,
                    "after_complete": after_complete,
                    "lossless": lossless,
                });
                tracing::info!("verify_roundtrip: {report}");
                self.verify_roundtrip_report.set(Some(report));
                if lossless {
                    Toast::info("Verify round-trip: LOSSLESS (census identical)");
                } else {
                    Toast::error(
                        "Verify round-trip: census MISMATCH — see verify_roundtrip_report",
                    );
                }
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
                        // Free any clips left fully orphaned by this delete (e.g.
                        // every animation of a just-deleted imported model).
                        self.prune_orphaned_clips();
                        self.scene.bump_revision();
                        Ok(Some(EditorCommand::InsertTree {
                            node: Box::new(spec),
                            parent,
                            index,
                        }))
                    }
                    None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
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
                    None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
                }
            }
            EditorCommand::RestoreAsset { id, entry } => {
                self.scene.assets.lock().unwrap().entries.insert(id, *entry);
                self.scene.bump_revision();
                Ok(Some(EditorCommand::DeleteAsset { id }))
            }
            EditorCommand::SetTextureExport { id, export } => {
                let prev = {
                    let mut assets = self.scene.assets.lock().unwrap();
                    match assets.entries.get_mut(&id) {
                        Some(entry) => {
                            let prev = entry.texture_export;
                            if prev == export {
                                return Ok(None); // no-op — don't churn undo history
                            }
                            entry.texture_export = export;
                            prev
                        }
                        None => {
                            return Err(crate::error::EditorError::msg(
                                "target id not found — command not applied (check ids against get_snapshot)",
                            ))
                        }
                    }
                };
                self.scene.bump_revision();
                Ok(Some(EditorCommand::SetTextureExport { id, export: prev }))
            }
            EditorCommand::SetBundleOptions { patch } => {
                // Patch semantics live in `BundleOptionsPatch::apply` (host-
                // tested in editor-protocol): `None` preserves, `Some` sets.
                let prev = self.scene.bundle_options.get();
                let next = patch.apply(prev);
                if prev == next {
                    return Ok(None); // no-op — don't churn undo history
                }
                self.scene.bundle_options.set(next);
                self.scene.bump_revision();
                Ok(Some(EditorCommand::SetBundleOptions {
                    patch: awsm_renderer_editor_protocol::BundleOptionsPatch::replace(&prev),
                }))
            }
            EditorCommand::PurgeUnusedAssets => {
                // Delete every asset the live scene no longer references. The
                // reachable set is computed conservatively (over-marking keeps an
                // asset; it can never drop a used one) — see `reachable_assets`.
                let reachable = reachable_assets(self);
                let unused: Vec<AssetId> = {
                    let assets = self.scene.assets.lock().unwrap();
                    assets
                        .entries
                        .keys()
                        .copied()
                        .filter(|id| !reachable.contains(id))
                        .collect()
                };
                if unused.is_empty() {
                    Toast::info("No unused assets to purge");
                    return Ok(None);
                }
                // Remove each, capturing its entry so the inverse `Batch` of
                // `RestoreAsset` makes the whole purge one undo step.
                let mut restores = Vec::with_capacity(unused.len());
                {
                    let mut assets = self.scene.assets.lock().unwrap();
                    for id in &unused {
                        if let Some(entry) = assets.entries.remove(id) {
                            restores.push(EditorCommand::RestoreAsset {
                                id: *id,
                                entry: Box::new(entry),
                            });
                        }
                    }
                }
                if let Some(sel) = self.asset_selection.get() {
                    if unused.contains(&sel) {
                        self.asset_selection.set(None);
                    }
                }
                self.scene.bump_revision();
                self.dirty.set_neq(true);
                Toast::info(format!("Purged {} unused asset(s)", restores.len()));
                Ok(Some(EditorCommand::Batch(restores)))
            }
            EditorCommand::DropSkinning { node } => {
                use awsm_renderer_editor_protocol::SkinnedMeshRef;
                let Some(n) = mutate::find_by_id(&self.scene, node) else {
                    return Ok(None);
                };
                let prev = n.kind.get_cloned();
                // Only a SkinnedMesh can be dropped to editable — anything else is
                // a no-op (the UI/MCP layer surfaces a clearer message).
                let (skin, material_variants, selected_variant, shadow, lod): (
                    SkinnedMeshRef,
                    _,
                    _,
                    _,
                    _,
                ) = match prev.clone() {
                    NodeKind::SkinnedMesh {
                        skin,
                        material_variants,
                        selected_variant,
                        shadow,
                        lod,
                    } => (skin, material_variants, selected_variant, shadow, lod),
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
                // drop_skinning bakes the bind pose; UV sets ride mesh.uvs. No authored
                // tangents on a baked bind pose → None (regenerated at commit).
                let mesh_ref = mint_imported_mesh(node, &label, &mesh, None, skin.source);
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
                    material_variants,
                    selected_variant,
                    shadow,
                    lod,
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
            EditorCommand::SetMeshData {
                mesh,
                data,
                allow_empty,
            } => {
                use crate::engine::bridge::mesh_cache;
                // Reject the silent mesh-wipe footgun + structurally-broken input
                // BEFORE storing (undo can't help if we never warned).
                data.validate(allow_empty)
                    .map_err(crate::error::EditorError::msg)?;
                let prior = mesh_cache::get_captured(mesh);
                mesh_cache::store_with_id(mesh, data);
                self.scene.bump_revision();
                // Inverse restores the prior geometry; if there was none (the mesh
                // didn't exist), the edit isn't undoable. allow_empty:true so a
                // legitimately-empty prior round-trips through the guard.
                Ok(prior.map(|data| EditorCommand::SetMeshData {
                    mesh,
                    data,
                    allow_empty: true,
                }))
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
                selection,
            } => {
                let indices = resolve_vertex_selection_or(selection, indices)?;
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
                selection,
            } => {
                let indices = resolve_vertex_selection_or(selection, indices)?;
                self.soft_transform_mesh(mesh, &indices, translation, falloff)
            }
            EditorCommand::PaintVertexColors {
                mesh,
                indices,
                color,
                selection,
            } => {
                let indices = resolve_vertex_selection_or(selection, indices)?;
                let collapse = self.ensure_authorable(mesh)?;
                let prior = self.apply_vertex_overrides(mesh, |ov| {
                    for &idx in &indices {
                        ov.colors.insert(idx, color);
                    }
                })?;
                Ok(Some(self.overrides_inverse(mesh, prior, collapse)))
            }
            EditorCommand::PaintVerticesWhere {
                node,
                predicate,
                color,
            } => {
                // §10: select + paint in one call so the (potentially huge) index
                // array never crosses the MCP boundary.
                let (mesh, md) = match self.node_editable_mesh(node) {
                    Ok(v) => v,
                    Err(e) => {
                        Toast::error(e);
                        return Ok(None);
                    }
                };
                let indices = select_vertices_by_predicate(&md, &predicate);
                if indices.is_empty() {
                    return Ok(None);
                }
                let collapse = self.ensure_authorable(mesh)?;
                let prior = self.apply_vertex_overrides(mesh, |ov| {
                    for &idx in &indices {
                        ov.colors.insert(idx, color);
                    }
                })?;
                Ok(Some(self.overrides_inverse(mesh, prior, collapse)))
            }
            EditorCommand::TransformVerticesWhere {
                node,
                predicate,
                translation,
                falloff,
            } => {
                // §10: select + soft-transform in one call (indices stay server-side).
                let (mesh, md) = match self.node_editable_mesh(node) {
                    Ok(v) => v,
                    Err(e) => {
                        Toast::error(e);
                        return Ok(None);
                    }
                };
                let indices = select_vertices_by_predicate(&md, &predicate);
                if indices.is_empty() {
                    return Ok(None);
                }
                self.soft_transform_mesh(mesh, &indices, translation, falloff)
            }
            EditorCommand::SetVertexNormals {
                mesh,
                indices,
                normal,
                selection,
            } => {
                let indices = resolve_vertex_selection_or(selection, indices)?;
                let collapse = self.ensure_authorable(mesh)?;
                let prior = self.apply_vertex_overrides(mesh, |ov| {
                    for &idx in &indices {
                        ov.normals.insert(idx, normal);
                    }
                })?;
                Ok(Some(self.overrides_inverse(mesh, prior, collapse)))
            }
            EditorCommand::SetVertexUvs {
                mesh,
                indices,
                uvs,
                selection,
            } => {
                let indices = resolve_vertex_selection_or(selection, indices)?;
                // Per-index parallel-array write (mirrors SetVertexPositions): the
                // bake applies `overrides.uvs`, creating the UV channel if absent.
                let collapse = self.ensure_authorable(mesh)?;
                let prior = self.apply_vertex_overrides(mesh, |ov| {
                    for (k, &idx) in indices.iter().enumerate() {
                        if let Some(uv) = uvs.get(k) {
                            ov.uvs.insert(idx, *uv);
                        }
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
            EditorCommand::DisplaceFromTexture {
                node,
                url,
                strength,
            } => {
                // §16: displace every vertex along its normal by the heightmap's
                // luminance at the vertex UV — the generic "supply your own
                // heightfield" hook. Fetch + decode (loudly) BEFORE collapsing.
                let (mesh, md) = self
                    .node_editable_mesh(node)
                    .map_err(crate::error::EditorError::msg)?;
                let (rgba, w, h) = crate::engine::bridge::material::decode_rgba_from_url(&url)
                    .await
                    .map_err(crate::error::EditorError::msg)?;
                let normals = md.normals.clone();
                let uvs0 = md.uvs.first().cloned();
                let positions = md.positions.clone();
                let collapse = self.ensure_authorable(mesh)?;
                let prior = self.apply_vertex_overrides(mesh, |ov| {
                    for (i, p) in positions.iter().enumerate() {
                        let uv = uvs0
                            .as_ref()
                            .and_then(|u| u.get(i))
                            .copied()
                            .unwrap_or([0.0, 0.0]);
                        let height = sample_heightmap_luminance(&rgba, w, h, uv[0], uv[1]);
                        let n = normals
                            .as_ref()
                            .and_then(|nn| nn.get(i))
                            .copied()
                            .unwrap_or([0.0, 1.0, 0.0]);
                        let d = height * strength;
                        ov.positions.insert(
                            i as u32,
                            [p[0] + n[0] * d, p[1] + n[1] * d, p[2] + n[2] * d],
                        );
                    }
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
                use awsm_renderer_editor_protocol::MeshRef;
                use awsm_renderer_editor_protocol::{MeshBase, ModifierStack};
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
                        allow_empty: true,
                    },
                ])))
            }
            EditorCommand::SeparateMesh {
                node,
                indices,
                selection,
                new_node,
                keep_remainder,
            } => {
                use crate::engine::bridge::mesh_cache;
                use crate::engine::scene::node::Node;
                use awsm_renderer_editor_protocol::{
                    CapturedSource, MeshBase, MeshDef, MeshRef, ModifierStack,
                };

                let (src_mesh, md) = match self.node_editable_mesh(node) {
                    Ok(v) => v,
                    Err(e) => {
                        Toast::error(e);
                        return Ok(None);
                    }
                };
                let sel = resolve_vertex_selection_or(selection, indices)?;
                if sel.is_empty() {
                    Toast::error("separate_mesh: empty selection");
                    return Ok(None);
                }
                let (extracted, remainder) = awsm_renderer_meshgen::edit::extract_faces(&md, &sel);
                if extracted.positions.is_empty() {
                    Toast::error("separate_mesh: selection contains no complete face");
                    return Ok(None);
                }
                // Source transform + material to inherit onto the new node.
                let (src_trs, src_variants, src_selected) =
                    match mutate::find_by_id(&self.scene, node) {
                        Some(n) => {
                            let k = n.kind.get_cloned();
                            if !matches!(k, NodeKind::Mesh { .. }) {
                                return Ok(None);
                            }
                            (
                                n.transform.get(),
                                k.material_variants().cloned().unwrap_or_default(),
                                k.selected_variant_id(),
                            )
                        }
                        None => return Ok(None),
                    };
                // Mint the new node + asset (deterministic id ⇒ idempotent replay).
                let new_node_id = new_node.unwrap_or_else(NodeId::new);
                if mutate::find_by_id(&self.scene, new_node_id).is_some() {
                    return Ok(None);
                }
                let new_mesh_id = AssetId(new_node_id.0);
                mesh_cache::store_with_id(new_mesh_id, mesh_cache::from_mesh_data(extracted));
                self.scene.assets.lock().unwrap().entries.insert(
                    new_mesh_id,
                    AssetEntry::new(SceneAssetSource::Mesh(MeshDef {
                        label: "Separated".to_string(),
                        source: Some(CapturedSource::Editable),
                        editable: true,
                        stack: ModifierStack {
                            base: MeshBase::Captured(MeshRef(new_mesh_id)),
                            modifiers: vec![],
                        },
                        overrides: Default::default(),
                    })),
                );
                let mut newn = Node::new_with_transform_and_kind(
                    "Separated",
                    src_trs,
                    NodeKind::Mesh {
                        mesh: MeshRef(new_mesh_id),
                        material_variants: src_variants,
                        selected_variant: src_selected,
                        shadow: Default::default(),
                        lod: Default::default(),
                    },
                );
                std::sync::Arc::get_mut(&mut newn)
                    .expect("freshly built node is sole-owned")
                    .id = new_node_id;
                let parent = mutate::find_parent(&self.scene, node).map(|p| p.id);
                if !mutate::insert_under(&self.scene, parent, newn) {
                    return Ok(None);
                }

                // Inverse: drop the new node + asset (and, below, restore the source).
                let mut inverse = vec![EditorCommand::Batch(vec![
                    EditorCommand::Delete { id: new_node_id },
                    EditorCommand::DeleteAsset { id: new_mesh_id },
                ])];

                if keep_remainder {
                    // Capture source prior state for a wholesale undo restore.
                    let (prior_stack, prior_overrides) = {
                        let assets = self.scene.assets.lock().unwrap();
                        match assets.get(src_mesh).map(|e| &e.source) {
                            Some(SceneAssetSource::Mesh(def)) => {
                                (def.stack.clone(), def.overrides.clone())
                            }
                            _ => (
                                ModifierStack {
                                    base: MeshBase::Captured(MeshRef(src_mesh)),
                                    modifiers: vec![],
                                },
                                Default::default(),
                            ),
                        }
                    };
                    let prior_bytes = mesh_cache::get_captured(src_mesh);
                    // Source ← remainder: flatten to a bare capture, clear overrides
                    // (they index the now-stale topology).
                    {
                        let mut assets = self.scene.assets.lock().unwrap();
                        if let Some(entry) = assets.entries.get_mut(&src_mesh) {
                            if let SceneAssetSource::Mesh(def) = &mut entry.source {
                                def.stack = ModifierStack {
                                    base: MeshBase::Captured(MeshRef(src_mesh)),
                                    modifiers: vec![],
                                };
                                def.overrides = Default::default();
                                def.editable = true;
                            }
                        }
                    }
                    mesh_cache::store_with_id(src_mesh, mesh_cache::from_mesh_data(remainder));
                    // Restore source on undo: recipe → exact bytes → overrides.
                    let mut restore = vec![EditorCommand::SetMeshModifiers {
                        mesh: src_mesh,
                        stack: prior_stack,
                    }];
                    if let Some(bytes) = prior_bytes {
                        restore.push(EditorCommand::SetMeshData {
                            mesh: src_mesh,
                            data: bytes,
                            allow_empty: true,
                        });
                    }
                    restore.push(EditorCommand::SetVertexOverrides {
                        mesh: src_mesh,
                        overrides: prior_overrides,
                    });
                    inverse.push(EditorCommand::Batch(restore));
                }
                self.scene.bump_revision();
                Ok(Some(EditorCommand::Batch(inverse)))
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
                    awsm_renderer_editor_protocol::MaterialShading::Pbr => "PBR",
                    awsm_renderer_editor_protocol::MaterialShading::Unlit => "Unlit",
                    awsm_renderer_editor_protocol::MaterialShading::Toon { .. } => "Toon",
                    awsm_renderer_editor_protocol::MaterialShading::FlipBook { .. } => "FlipBook",
                };
                let mat = CM::new_builtin(id, format!("{label} Material {n}"), shading);
                self.custom_materials.lock_mut().push_cloned(mat.clone());
                self.current_material.set(Some(id));
                self.scene.bump_revision();
                self.dirty.set_neq(true);
                Ok(None)
            }
            EditorCommand::UpdateBuiltinMaterial { id, def } => {
                let Some(mat) = find_material(&self.custom_materials, id) else {
                    return Err(crate::error::EditorError::msg(format!("no material {id}")));
                };
                let Some(prior) = mat.builtin.get_cloned() else {
                    return Err(crate::error::EditorError::msg(format!(
                        "material {id} is not a built-in (custom WGSL materials \
                         use the SetCustomMaterial* commands)"
                    )));
                };
                mat.builtin.set(Some((*def).clone()));
                // Variant changed → refresh its card thumbnail + re-seed every
                // assigned mesh's per-variant inline store DIRECTLY (which also
                // re-materializes it). This used to ride a per-material
                // `spawn_builtin_resync` observer, which only existed for
                // materials created through `AddBuiltinMaterial` / project load —
                // glTF-IMPORTED library materials never spawned one, so updating
                // them (e.g. enabling a PBR extension over MCP) silently changed
                // nothing on screen. And a bare re-materialize is not enough
                // either: each variant renders from its own inline VALUE copy
                // (seeded at import/assign), so without the field-wise re-seed a
                // def edit re-applied the stale values and nothing visibly
                // changed. Slots the mesh customized (≠ prior def) are kept.
                crate::engine::thumbnail::invalidate(mat.id);
                crate::engine::thumbnail::request(mat.clone());
                crate::engine::bridge::reseed_inline_for_material(id, &prior, &def);
                // A variant edit changes which per-mesh controls exist on every
                // node assigned this material (extension rows appear/disappear,
                // texture slots gate on it) — rebuild the node inspector like
                // AssignMaterial does. Without this a LOCAL material-panel edit
                // (which by design bumps no external_rev) left a still-selected
                // node's panel stale until reselect.
                self.structure_rev
                    .set(self.structure_rev.get().wrapping_add(1));
                self.scene.bump_revision();
                self.dirty.set_neq(true);
                Ok(Some(EditorCommand::UpdateBuiltinMaterial {
                    id,
                    def: Box::new(prior),
                }))
            }
            EditorCommand::DeleteCustomMaterial { id } => {
                self.custom_materials.lock_mut().retain(|m| m.id != id);
                if self.current_material.get() == Some(id) {
                    let next = self.custom_materials.lock_ref().first().map(|m| m.id);
                    self.current_material.set(next);
                }
                // Drop the renderer-side registration too, else its compiled GPU
                // compute pipelines + shader modules leak forever (the pipeline-
                // leak / "aw snap" fix). No-op if it was never registered.
                crate::engine::bridge::dynamic::unregister(id).await;
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
                    None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
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
                    None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
                }
            }
            EditorCommand::SetCustomMaterialVertexWgsl { id, wgsl } => {
                // Replace a material's 3rd vertex-displacement WGSL window.
                // Setting the live `vertex_wgsl` field marks the material a draft
                // + bumps the recompile rev (via mark_material_draft), so the
                // auto-register observer recompiles the custom-vertex pipeline.
                match find_material(&self.custom_materials, id) {
                    Some(mat) => {
                        let prev = mat.vertex_wgsl.get_cloned();
                        mat.vertex_wgsl.set(wgsl);
                        mark_material_draft(&mat);
                        self.dirty.set_neq(true);
                        Ok(Some(EditorCommand::SetCustomMaterialVertexWgsl {
                            id,
                            wgsl: prev,
                        }))
                    }
                    None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
                }
            }
            EditorCommand::SelectMaterialVariant { node, variant } => {
                let Some(n) = mutate::find_by_id(&self.scene, node) else {
                    return Ok(None);
                };
                let mut next = n.kind.get_cloned();
                let Some(variants) = next.material_variants() else {
                    return Err(crate::error::EditorError::msg(
                        "select_material_variant: node has no material palette",
                    ));
                };
                if let Some(id) = variant {
                    if !variants.iter().any(|v| v.id == id) {
                        return Err(crate::error::EditorError::msg(format!(
                            "select_material_variant: no variant {id} on this mesh"
                        )));
                    }
                }
                next.set_selected_variant(variant);
                // Delegate to SetKind: re-materialize + undo inverse for free.
                Box::pin(self.apply_inner(EditorCommand::SetKind {
                    id: node,
                    kind: Box::new(next),
                }))
                .await
            }
            EditorCommand::AddMaterialVariant {
                node,
                material,
                id,
                name,
            } => {
                let Some(n) = mutate::find_by_id(&self.scene, node) else {
                    return Ok(None);
                };
                let Some(m) = find_material(&self.custom_materials, material) else {
                    return Err(crate::error::EditorError::msg(format!(
                        "add_material_variant: no library material with id {material}"
                    )));
                };
                let mut next = n.kind.get_cloned();
                let Some(variants) = next.material_variants_mut() else {
                    return Err(crate::error::EditorError::msg(
                        "add_material_variant: node has no material palette",
                    ));
                };
                // Display name: explicit, else the library material's name,
                // counter-suffixed until free on THIS mesh ("Felt", "Felt 2", …).
                let name = name.unwrap_or_else(|| {
                    let base = m.name.get_cloned();
                    let mut candidate = base.clone();
                    let mut i = 2;
                    while variants.iter().any(|v| v.name == candidate) {
                        candidate = format!("{base} {i}");
                        i += 1;
                    }
                    candidate
                });
                variants.push(awsm_renderer_editor_protocol::MaterialVariant {
                    id: id.unwrap_or_default(),
                    name,
                    // A fresh instance seeded from the library material's
                    // defaults (a dynamic material has no built-in def → the
                    // inline is ignored anyway).
                    instance: awsm_renderer_editor_protocol::dynamic_material::MaterialInstance {
                        asset: material,
                        inline: m.builtin.get_cloned().unwrap_or_default(),
                        uniform_overrides: Default::default(),
                        texture_overrides: Default::default(),
                        buffer_overrides: Default::default(),
                    },
                });
                // NEVER changes the selection — rendering it is an explicit
                // SelectMaterialVariant.
                Box::pin(self.apply_inner(EditorCommand::SetKind {
                    id: node,
                    kind: Box::new(next),
                }))
                .await
            }
            EditorCommand::RemoveMaterialVariant { node, variant } => {
                let Some(n) = mutate::find_by_id(&self.scene, node) else {
                    return Ok(None);
                };
                let mut next = n.kind.get_cloned();
                let Some(variants) = next.material_variants_mut() else {
                    return Err(crate::error::EditorError::msg(
                        "remove_material_variant: node has no material palette",
                    ));
                };
                let Some(pos) = variants.iter().position(|v| v.id == variant) else {
                    return Err(crate::error::EditorError::msg(format!(
                        "remove_material_variant: no variant {variant} on this mesh"
                    )));
                };
                variants.remove(pos);
                // Removing the rendered variant leaves the mesh unassigned.
                if next.selected_variant_id() == Some(variant) {
                    next.set_selected_variant(None);
                }
                Box::pin(self.apply_inner(EditorCommand::SetKind {
                    id: node,
                    kind: Box::new(next),
                }))
                .await
            }
            EditorCommand::RenameMaterialVariant {
                node,
                variant,
                name,
            } => {
                let Some(n) = mutate::find_by_id(&self.scene, node) else {
                    return Ok(None);
                };
                let mut next = n.kind.get_cloned();
                let Some(variants) = next.material_variants_mut() else {
                    return Err(crate::error::EditorError::msg(
                        "rename_material_variant: node has no material palette",
                    ));
                };
                let Some(v) = variants.iter_mut().find(|v| v.id == variant) else {
                    return Err(crate::error::EditorError::msg(format!(
                        "rename_material_variant: no variant {variant} on this mesh"
                    )));
                };
                v.name = name;
                Box::pin(self.apply_inner(EditorCommand::SetKind {
                    id: node,
                    kind: Box::new(next),
                }))
                .await
            }
            EditorCommand::CopyMaterialInstance { from, to } => {
                let (Some(src), Some(dst)) = (
                    mutate::find_by_id(&self.scene, from),
                    mutate::find_by_id(&self.scene, to),
                ) else {
                    return Ok(None);
                };
                // The source's SELECTED variant's instance.
                let src_kind = src.kind.get_cloned();
                let Some(src_inst) = src_kind.selected_material().cloned() else {
                    return Ok(None);
                };
                let prev = dst.kind.get_cloned();
                let mut next = prev.clone();
                // Paste onto the destination's SELECTED variant — and only
                // between instances of the SAME library material.
                let Some(dst_inst) = next.selected_material_mut() else {
                    return Ok(None);
                };
                if dst_inst.asset != src_inst.asset {
                    return Ok(None);
                }
                // Copy the whole instance (inline uniforms + override maps).
                *dst_inst = src_inst;
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
                            awsm_renderer_editor_protocol::CustomAlphaMode::Opaque => {
                                mat.alpha.set_neq(AlphaMode::Opaque);
                            }
                            awsm_renderer_editor_protocol::CustomAlphaMode::Mask { cutoff } => {
                                mat.alpha.set_neq(AlphaMode::Mask);
                                mat.cutoff.set_neq(cutoff);
                            }
                            awsm_renderer_editor_protocol::CustomAlphaMode::Blend => {
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
                    None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
                }
            }
            EditorCommand::SetCustomMaterialDoubleSided { id, double_sided } => match find_material(
                &self.custom_materials,
                id,
            ) {
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
                None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
            },
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
                    None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
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
                None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
            },
            EditorCommand::SetCustomMaterialShaderIncludes { id, includes } => {
                match find_material(&self.custom_materials, id) {
                    Some(mat) => {
                        let prev = mat.shader_includes.get_cloned();
                        mat.shader_includes.set(validate_keys(
                            &includes,
                            custom_material::SHADER_INCLUDE_KEYS.as_slice(),
                        ));
                        mark_material_draft(&mat);
                        self.dirty.set_neq(true);
                        Ok(Some(EditorCommand::SetCustomMaterialShaderIncludes {
                            id,
                            includes: prev,
                        }))
                    }
                    None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
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
                    None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
                }
            }
            EditorCommand::SetMaterialUniform {
                material,
                name,
                value,
            } => match find_material(&self.custom_materials, material) {
                Some(mat) => {
                    let value = value.into_text();
                    let mut slots = mat.uniforms.get_cloned();
                    let Some(slot) = slots.iter_mut().find(|s| s.name == name) else {
                        return Err(crate::error::EditorError::msg(format!(
                            "material has no uniform named `{name}` — check the layout"
                        )));
                    };
                    let prev = slot.val.clone();
                    slot.val = value.clone();
                    mat.uniforms.set(slots);
                    self.dirty.set_neq(true);
                    // D3: previously this only updated the authored default + flipped
                    // the material to draft (mark_material_draft) for a debounced
                    // re-register — which did NOT update the live render (the report's
                    // complaint). Instead, push the value straight into the running
                    // material(s) — the same write a uniform animation track does — so
                    // the change shows IMMEDIATELY, no re-register / recompile. The
                    // authored default (set above) persists + seeds the next register.
                    let (asset, slot_name, val) = (material, name.clone(), value);
                    crate::engine::context::with_renderer_mut(move |r| {
                        crate::engine::bridge::dynamic::set_uniform_live(
                            r, asset, &slot_name, &val,
                        );
                    })
                    .await;
                    Ok(Some(EditorCommand::SetMaterialUniform {
                        material,
                        name,
                        value: prev.into(),
                    }))
                }
                None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
            },
            EditorCommand::SetNodeMaterialUniform { node, name, value } => {
                match mutate::find_by_id(&self.scene, node) {
                    Some(n) => {
                        let prev = n.kind.get_cloned();
                        let mut next = prev.clone();
                        let Some(inst) = node_material_mut(&mut next) else {
                            return Err(crate::error::EditorError::msg(
                                "node has no material instance — assign a material first",
                            ));
                        };
                        inst.uniform_overrides.insert(name, value);
                        // `kind.set` re-materializes the node so the override renders.
                        n.kind.set(next);
                        self.scene.bump_revision();
                        Ok(Some(EditorCommand::SetKind {
                            id: node,
                            kind: Box::new(prev),
                        }))
                    }
                    None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
                }
            }
            EditorCommand::SetBuiltinParam { node, param, value } => {
                match mutate::find_by_id(&self.scene, node) {
                    Some(n) => {
                        let prev = n.kind.get_cloned();
                        // A selected CUSTOM (dynamic-WGSL) variant has no builtin
                        // def to patch — this used to return Ok and silently do
                        // nothing. Point the caller at the right tool instead.
                        if let Some(inst) = prev.selected_material() {
                            let is_custom = crate::controller::custom_material::find_material(
                                &self.custom_materials,
                                inst.asset,
                            )
                            .map(|m| m.builtin.get_cloned().is_none())
                            .unwrap_or(false);
                            if is_custom {
                                return Err(crate::error::EditorError::msg(
                                    "this node's selected variant is a CUSTOM material — \
                                     builtin params don't apply; use set_material_uniform \
                                     (shared default) or set_node_material_uniform \
                                     (per-instance) instead",
                                ));
                            }
                        }
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
                    None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
                }
            }
            EditorCommand::SetBuiltinAlphaMode { material, mode } => {
                // Alpha mode is pipeline routing → a MATERIAL edit. Delegate to
                // the full def-update path (thumbnail refresh, per-mesh inline
                // reseed, inspector rebuild, inverse = prior def) so the typed
                // command and `update_builtin_material` behave identically.
                let Some(mat) = find_material(&self.custom_materials, material) else {
                    return Err(crate::error::EditorError::msg(format!(
                        "no material {material}"
                    )));
                };
                let Some(mut def) = mat.builtin.get_cloned() else {
                    return Err(crate::error::EditorError::msg(format!(
                        "material {material} is not a built-in (custom WGSL materials \
                         author their alpha via set_material_alpha_wgsl)"
                    )));
                };
                def.alpha_mode = mode;
                Box::pin(self.apply_inner(EditorCommand::UpdateBuiltinMaterial {
                    id: material,
                    def: Box::new(def),
                }))
                .await
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
                None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
            },
            EditorCommand::SetTextureUseProfile {
                node,
                slot,
                profile,
            } => match mutate::find_by_id(&self.scene, node) {
                Some(n) => {
                    let prev = n.kind.get_cloned();
                    let mut next = prev.clone();
                    // Reject loudly: no silent no-op when nothing is bound at
                    // the named slot. A bake-only knob — but `kind.set` keeps
                    // the stored kind + undo path consistent with its peers.
                    patch_texture_use_profile(&mut next, &slot, profile)
                        .map_err(crate::error::EditorError::msg)?;
                    n.kind.set(next);
                    self.scene.bump_revision();
                    Ok(Some(EditorCommand::SetKind {
                        id: node,
                        kind: Box::new(prev),
                    }))
                }
                None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
            },
            EditorCommand::SetNodeTextureTransform {
                node,
                slot,
                offset,
                scale,
                rotation,
                flow,
                wrap_u,
                wrap_v,
                uv_set,
                mag_filter,
                min_filter,
                mipmap_filter,
            } => match mutate::find_by_id(&self.scene, node) {
                Some(n) => {
                    let prev = n.kind.get_cloned();
                    let mut next = prev.clone();
                    // Reject loudly (§1): no silent no-op when the slot has no
                    // texture to transform. `kind.set` re-materializes the node so
                    // the new transform/flow actually renders.
                    patch_builtin_texture_transform(
                        &mut next,
                        slot,
                        offset,
                        scale,
                        rotation,
                        flow,
                        wrap_u,
                        wrap_v,
                        uv_set,
                        mag_filter,
                        min_filter,
                        mipmap_filter,
                    )
                    .map_err(crate::error::EditorError::msg)?;
                    n.kind.set(next);
                    self.scene.bump_revision();
                    Ok(Some(EditorCommand::SetKind {
                        id: node,
                        kind: Box::new(prev),
                    }))
                }
                None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
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
                    None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
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
            EditorCommand::SetSkinWeights {
                node,
                entries,
                normalize,
            } => {
                // Live skin-stream surgery: rewrite set-0 joint/weight pairs for
                // the given ORIGINAL vertices (layout: per vertex, per set,
                // 4 × (u32 joint LE, f32 weight LE) — 32 B/set/vertex). The
                // inverse restores the prior pairs for the touched vertices.
                let meshes = renderer_meshes_for_node(node);
                let prior = crate::engine::context::with_renderer_mut(move |r| {
                    let skin_key = meshes
                        .iter()
                        .find_map(|mk| r.meshes.mesh_skin_key(*mk).flatten())?;
                    let sets = r.meshes.skins.sets_len(skin_key).ok()?;
                    let stride = sets * 32;
                    let bytes = r.meshes.skins.read_joint_index_weights(skin_key).ok()?;
                    let vertex_count = bytes.len().checked_div(stride).unwrap_or(0);
                    // Capture prior values for the inverse.
                    let mut prior: Vec<awsm_renderer_editor_protocol::SkinWeightEntry> = Vec::new();
                    for e in &entries {
                        let v = e.vertex as usize;
                        if v >= vertex_count {
                            continue;
                        }
                        let off = v * stride;
                        let mut joints = [0u32; 4];
                        let mut weights = [0f32; 4];
                        for i in 0..4 {
                            let p = off + i * 8;
                            joints[i] = u32::from_le_bytes(bytes[p..p + 4].try_into().unwrap());
                            weights[i] =
                                f32::from_le_bytes(bytes[p + 4..p + 8].try_into().unwrap());
                        }
                        prior.push(awsm_renderer_editor_protocol::SkinWeightEntry {
                            vertex: e.vertex,
                            joints,
                            weights,
                        });
                    }
                    // Write the new values. `update_skin_weights` copies-on-
                    // write when this node is a weight-sharing duplicate, so
                    // the edit diverges only THIS instance (and re-patches its
                    // meshes' geometry meta if the stream moved).
                    let write_result = r.update_skin_weights(skin_key, |buf| {
                        for e in &entries {
                            let v = e.vertex as usize;
                            if v >= vertex_count {
                                continue;
                            }
                            let mut w = e.weights;
                            if normalize {
                                let sum: f32 = w.iter().sum();
                                if sum > 1e-6 {
                                    for x in &mut w {
                                        *x /= sum;
                                    }
                                }
                            }
                            let off = v * stride;
                            for (i, (joint, weight)) in e.joints.iter().zip(w.iter()).enumerate() {
                                let p = off + i * 8;
                                buf[p..p + 4].copy_from_slice(&joint.to_le_bytes());
                                buf[p + 4..p + 8].copy_from_slice(&weight.to_le_bytes());
                            }
                        }
                    });
                    if let Err(e) = write_result {
                        tracing::warn!("set_skin_weights: copy-on-write failed: {e}");
                        return None;
                    }
                    Some(prior)
                })
                .await;
                match prior {
                    Some(prior) if !prior.is_empty() => {
                        self.dirty.set_neq(true);
                        Ok(Some(EditorCommand::SetSkinWeights {
                            node,
                            entries: prior,
                            normalize: false,
                        }))
                    }
                    _ => Ok(None),
                }
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
                None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
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
                    None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
                }
            }
            EditorCommand::SetEnvironment { env } => {
                let prev = self.scene.environment.get_cloned();
                self.scene.environment.set(env);
                self.scene.bump_revision();
                Ok(Some(EditorCommand::SetEnvironment { env: prev }))
            }
            EditorCommand::PatchEnvironment {
                skybox,
                specular,
                irradiance,
                probe,
            } => {
                // Partial update: `None` slots PRESERVE the current config, so
                // setting just one slot (skybox / specular / irradiance) doesn't
                // silently reset the others — mixed workflows (default-sky
                // irradiance + custom specular, neutral skybox + keyed IBL, …)
                // survive sequential MCP calls. Inverse: the prior FULL env.
                let prev = self.scene.environment.get_cloned();
                let next = crate::engine::scene::EnvironmentConfig {
                    skybox: skybox.unwrap_or(prev.skybox),
                    specular: specular.unwrap_or(prev.specular),
                    irradiance: irradiance.unwrap_or(prev.irradiance),
                    probe: probe.unwrap_or(prev.probe),
                };
                self.scene.environment.set(next);
                self.scene.bump_revision();
                Ok(Some(EditorCommand::SetEnvironment { env: prev }))
            }
            EditorCommand::SetShadows { patch } => {
                // Patch semantics live in `ShadowsPatch::apply` (host-tested in
                // editor-protocol): `None` preserves, `Some` sets clamped. The
                // `settings_sync` observer pushes the new block into the
                // renderer (SSCS enabled/step_count recompile; the resource-
                // shape fields recreate their GPU textures next frame).
                let prev = self.scene.shadows.get_cloned();
                self.scene.shadows.set(patch.apply(prev.clone()));
                self.scene.bump_revision();
                Ok(Some(EditorCommand::SetShadows {
                    patch: awsm_renderer_editor_protocol::ShadowsPatch::replace(&prev),
                }))
            }
            EditorCommand::SetShadowsSscs {
                enabled,
                step_count,
                step_world,
                thickness,
                directional_darkening,
                punctual_darkening,
            } => {
                let prev = self.scene.shadows.get_cloned();
                let mut next = prev.clone();
                if let Some(v) = enabled {
                    next.sscs_enabled = v;
                }
                if let Some(v) = step_count {
                    next.sscs_step_count = v.max(1);
                }
                if let Some(v) = step_world {
                    next.sscs_step_world = v;
                }
                if let Some(v) = thickness {
                    next.sscs_thickness = v;
                }
                if let Some(v) = directional_darkening {
                    next.sscs_directional_darkening = v;
                }
                if let Some(v) = punctual_darkening {
                    next.sscs_punctual_darkening = v;
                }
                self.scene.shadows.set(next);
                self.scene.bump_revision();
                // Inverse restores every SSCS field to its prior value.
                Ok(Some(EditorCommand::SetShadowsSscs {
                    enabled: Some(prev.sscs_enabled),
                    step_count: Some(prev.sscs_step_count),
                    step_world: Some(prev.sscs_step_world),
                    thickness: Some(prev.sscs_thickness),
                    directional_darkening: Some(prev.sscs_directional_darkening),
                    punctual_darkening: Some(prev.sscs_punctual_darkening),
                }))
            }
            EditorCommand::SetPostProcess {
                tonemapping,
                bloom,
                dof,
                exposure,
                bloom_threshold,
                bloom_knee,
                bloom_intensity,
                bloom_scatter,
                ssr_enabled,
                ssr_intensity,
                ssr_max_distance,
                ssr_thickness,
                ssr_max_steps,
                ssr_spread_cutoff,
                ssr_edge_fade,
                ssr_temporal,
                ssr_resolution_scale,
                ssr_temporal_weight,
                ssr_debug,
                ssr_bvh_reflections,
            } => {
                let prev = self.scene.post_process.get_cloned();
                let mut next = prev.clone();
                if let Some(v) = tonemapping {
                    next.tonemapping = v;
                }
                if let Some(v) = bloom {
                    next.bloom = v;
                }
                if let Some(v) = dof {
                    next.dof = v;
                }
                if let Some(v) = exposure {
                    next.exposure = v;
                }
                if let Some(v) = bloom_threshold {
                    next.bloom_threshold = v;
                }
                if let Some(v) = bloom_knee {
                    next.bloom_knee = v;
                }
                if let Some(v) = bloom_intensity {
                    next.bloom_intensity = v;
                }
                if let Some(v) = bloom_scatter {
                    next.bloom_scatter = v;
                }
                if let Some(v) = ssr_enabled {
                    next.ssr.enabled = v;
                }
                if let Some(v) = ssr_intensity {
                    next.ssr.intensity = v;
                }
                if let Some(v) = ssr_max_distance {
                    next.ssr.max_distance = v;
                }
                if let Some(v) = ssr_thickness {
                    next.ssr.thickness = v;
                }
                if let Some(v) = ssr_max_steps {
                    next.ssr.max_steps = v;
                }
                if let Some(v) = ssr_spread_cutoff {
                    next.ssr.spread_cutoff = v;
                }
                if let Some(v) = ssr_edge_fade {
                    next.ssr.edge_fade = v;
                }
                if let Some(v) = ssr_temporal {
                    next.ssr.temporal = v;
                }
                if let Some(v) = ssr_resolution_scale {
                    next.ssr.resolution_scale = v;
                }
                if let Some(v) = ssr_temporal_weight {
                    next.ssr.temporal_weight = v;
                }
                if let Some(v) = ssr_bvh_reflections {
                    next.ssr.bvh_reflections = v;
                }
                if let Some(v) = ssr_debug {
                    next.ssr.debug = v;
                }
                self.scene.post_process.set(next);
                self.scene.bump_revision();
                self.dirty.set_neq(true);
                // Inverse restores every field to its prior value.
                Ok(Some(EditorCommand::SetPostProcess {
                    tonemapping: Some(prev.tonemapping),
                    bloom: Some(prev.bloom),
                    dof: Some(prev.dof),
                    exposure: Some(prev.exposure),
                    bloom_threshold: Some(prev.bloom_threshold),
                    bloom_knee: Some(prev.bloom_knee),
                    bloom_intensity: Some(prev.bloom_intensity),
                    bloom_scatter: Some(prev.bloom_scatter),
                    ssr_enabled: Some(prev.ssr.enabled),
                    ssr_intensity: Some(prev.ssr.intensity),
                    ssr_max_distance: Some(prev.ssr.max_distance),
                    ssr_thickness: Some(prev.ssr.thickness),
                    ssr_max_steps: Some(prev.ssr.max_steps),
                    ssr_spread_cutoff: Some(prev.ssr.spread_cutoff),
                    ssr_edge_fade: Some(prev.ssr.edge_fade),
                    ssr_temporal: Some(prev.ssr.temporal),
                    ssr_resolution_scale: Some(prev.ssr.resolution_scale),
                    ssr_temporal_weight: Some(prev.ssr.temporal_weight),
                    ssr_debug: Some(prev.ssr.debug),
                    ssr_bvh_reflections: Some(prev.ssr.bvh_reflections),
                }))
            }
            EditorCommand::SetViewOptions {
                grid,
                gizmos,
                light_gizmos,
                skeleton_viz,
                follow_agent,
                activity_overlay,
                mcp_notifications,
                msaa,
                smaa,
            } => {
                let s = &self.settings;
                if let Some(v) = grid {
                    s.grid.set_neq(v);
                }
                if let Some(v) = gizmos {
                    s.gizmo.set_neq(v);
                }
                if let Some(v) = light_gizmos {
                    s.light_gizmos.set_neq(v);
                }
                if let Some(v) = skeleton_viz {
                    s.skeleton_viz.set_neq(v);
                }
                if let Some(v) = follow_agent {
                    crate::engine::activity_feed::follow_enabled().set_neq(v);
                }
                if let Some(v) = activity_overlay {
                    crate::engine::activity_feed::enabled().set_neq(v);
                }
                if let Some(v) = mcp_notifications {
                    crate::remote::show_notifications().set_neq(v);
                }
                if let Some(v) = msaa {
                    s.msaa.set_neq(v);
                }
                if let Some(v) = smaa {
                    s.smaa.set_neq(v);
                }
                // Transient view state — no undo entry (same class as camera).
                Ok(None)
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
                use awsm_renderer_web_shared::util::free_camera::ProjectionMode;
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
            EditorCommand::SetCameraClip { manual, near, far } => {
                // Drive the reactive settings — `settings_sync` observes these and
                // pushes the resulting clip override into the camera, so the Settings
                // drawer toggle/fields and any MCP-driven change stay in sync.
                if let Some(m) = manual {
                    self.settings.cam_clip_manual.set_neq(m);
                }
                if let Some(n) = near {
                    self.settings.cam_clip_near.set_neq(n);
                }
                if let Some(f) = far {
                    self.settings.cam_clip_far.set_neq(f);
                }
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
                    // §9: `frame_aabb` already fits the bounding SPHERE to the FOV
                    // (conservative — encloses the box at any orbit angle), so the
                    // subject reads small. Frame to margin `1.0 + padding` with NO
                    // extra breathe multiplier (the old `* 1.15` left heads/parts
                    // too small in frame); the user's `padding` is the only slack.
                    crate::engine::context::try_with_camera_mut(|c| {
                        c.frame_aabb(aabb, 1.0 + padding.max(0.0))
                    });
                })
                .await;
                Ok(None)
            }
            EditorCommand::ResetPose { node } => {
                // Collect (node + descendants) scene transforms, then re-push them
                // onto the renderer mirror locals — reverting a clip's last
                // previewed pose (pin_pose writes the mirror directly, NOT the
                // scene, so the scene transform is the base). Viewport-only: no
                // scene mutation, no undo entry (like FrameNode/SetFrameTime).
                fn collect(
                    node: &crate::engine::scene::node::Node,
                    out: &mut Vec<(NodeId, awsm_renderer_editor_protocol::Trs)>,
                ) {
                    out.push((node.id, node.transform.get()));
                    for c in node.children.lock_ref().iter() {
                        collect(c, out);
                    }
                }
                let mut scene_trs: Vec<(NodeId, awsm_renderer_editor_protocol::Trs)> = Vec::new();
                if let Some(n) = mutate::find_by_id(&self.scene, node) {
                    collect(&n, &mut scene_trs);
                }
                // Resolve transform keys from the bridge BEFORE the renderer lock.
                let pairs: Vec<(
                    awsm_renderer::transforms::TransformKey,
                    awsm_renderer_editor_protocol::Trs,
                )> = {
                    let b = crate::engine::bridge::bridge();
                    let nodes = b.nodes.lock().unwrap();
                    scene_trs
                        .into_iter()
                        .filter_map(|(id, trs)| nodes.get(&id).map(|e| (e.transform_key, trs)))
                        .collect()
                };
                crate::engine::context::with_renderer_mut(move |r| {
                    for (tk, trs) in &pairs {
                        let _ = r.transforms.set_local(
                            *tk,
                            crate::engine::bridge::node_sync::trs_to_transform(trs),
                        );
                    }
                })
                .await;
                Ok(None)
            }
            EditorCommand::LoadProjectFromUrl { base_url } => {
                // Settle-visibility FRONT guard: `editor_dispatch_json` (the
                // headless/MCP fire-and-forget seam) spawns this dispatch and
                // returns "ok" immediately, so a driver's `wait_render_settled`
                // can land while this handler is still FETCHING project.toml —
                // before `apply_project` arms the load barrier — and settle on
                // an unpopulated scene (verified: settled in 79 ms, roots=1,
                // tree populated ms later). Created synchronously at handler
                // entry (the spawned task's first synchronous slice runs before
                // the settle query's first 16 ms timer poll); RAII drop covers
                // the fetch/parse ERROR paths, which now happen while guarded.
                // The async tail past `apply_project` (Replace materialization
                // → bulk commit) is covered by the armed barrier hand-off.
                let _load_guard = CompileGuard::new();
                match persistence::load_project_from_url(self, &base_url).await {
                    Ok(()) => {
                        self.undo.borrow_mut().clear();
                        self.redo.borrow_mut().clear();
                        self.refresh_history_signals();
                        self.dirty.set_neq(false);
                        self.env_saved_baseline
                            .set(self.scene.environment.get_cloned());
                        Toast::info("Project loaded");
                    }
                    Err(e) => Toast::error(format!("Load failed: {e}")),
                }
                Ok(None)
            }
            EditorCommand::ImportModelFromUrl { url } => {
                // Settle-visibility front guard (see LoadProjectFromUrl): the
                // fetch + glTF populate must hold the barrier for a driver
                // dispatching through the fire-and-forget seam; the inserted
                // subtree's async materialization is covered by the Replace
                // arm's own guard in node_sync.
                let _load_guard = CompileGuard::new();
                let _activity =
                    crate::engine::activity::begin_activity("Inserting model — uploading to GPU…");
                self.finish_model_import(crate::engine::bridge::gltf::import(&url).await)
                    .map_err(|e| {
                        // A cross-origin fetch the host didn't allow surfaces as a bare
                        // "Failed to fetch" — the single most common silent-import
                        // trap for MCP agents (e.g. `python3 -m http.server` sends no
                        // CORS headers). Name the cause in the error the agent sees.
                        let hint = if e.contains("Failed to fetch") {
                            "\n(likely CORS: the editor is a browser app, so the file \
                             server must send `Access-Control-Allow-Origin` — plain \
                             `python3 -m http.server` does not)"
                        } else {
                            ""
                        };
                        crate::error::EditorError::msg(format!("import failed: {e}{hint}"))
                    })?;
                Ok(None)
            }
            EditorCommand::ImportModelFromFile { name, url } => {
                // Settle-visibility front guard (see ImportModelFromUrl).
                let _load_guard = CompileGuard::new();
                let _activity =
                    crate::engine::activity::begin_activity("Inserting model — uploading to GPU…");
                let result = crate::engine::bridge::gltf::import_file(&name, &url).await;
                // The blob: object URL was minted just for this load; release it.
                let _ = web_sys::Url::revoke_object_url(&url);
                self.finish_model_import(result)
                    .map_err(|e| crate::error::EditorError::msg(format!("import failed: {e}")))?;
                Ok(None)
            }
            // Import a PRE-BAKED nanite asset (awsm-renderer-lod-bake CLI output) as a
            // VIEW-ONLY ClusterMesh node, rendered via the bounded cluster pipeline
            // (the player path). No in-editor bake, no dense explode — large meshes
            // come in without crashing. Geometry is non-editable (it IS the LOD).
            EditorCommand::ImportNaniteAsset { clusters_url } => {
                let _activity = crate::engine::activity::begin_activity("Importing nanite asset…");
                match fetch_cluster_mesh(&clusters_url).await {
                    Ok(cm) => {
                        // Register an asset so the node's `ClusterMeshRef` resolves
                        // (and the project round-trips the source reference).
                        let asset_id = AssetId::new();
                        self.scene.assets.lock().unwrap().entries.insert(
                            asset_id,
                            AssetEntry::new(SceneAssetSource::Url(clusters_url)),
                        );
                        // Stash the parsed DAG for the bridge materializer (must be in
                        // the cache BEFORE the node is inserted + observed).
                        crate::engine::bridge::cluster_cache::insert(asset_id, cm);
                        // Build + insert the view-only node at the scene root; the
                        // bridge observer materializes it through the cluster path.
                        let node_id = NodeId::new();
                        let spec = crate::controller::node_spec::NodeSpec {
                            id: node_id,
                            name: "Nanite Mesh".to_string(),
                            transform: awsm_renderer_editor_protocol::Trs::default(),
                            kind: NodeKind::ClusterMesh {
                                cluster: awsm_renderer_editor_protocol::ClusterMeshRef {
                                    source: asset_id,
                                },
                                material_variants: Vec::new(),
                                selected_variant: None,
                                shadow: Default::default(),
                            },
                            locked: false,
                            visible: true,
                            prefab: false,
                            children: vec![],
                        };
                        let node = crate::controller::node_spec::node_from_spec(&spec);
                        mutate::insert_under(&self.scene, None, node);
                        self.scene.bump_revision();
                        self.dirty.set_neq(true);
                        Ok(Some(EditorCommand::Delete { id: node_id }))
                    }
                    Err(e) => {
                        Toast::error(format!("Nanite import failed: {e}"));
                        Ok(None)
                    }
                }
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
                    Ok((bytes, mime)) => {
                        // Name the persisted side file with an extension matching the
                        // mime, so `asset_filename` + `restore_textures` round-trip it.
                        let base = url
                            .split(['?', '#'])
                            .next()
                            .unwrap_or(&url)
                            .rsplit('/')
                            .next()
                            .filter(|s| !s.is_empty())
                            .unwrap_or("texture");
                        let stem = base.rsplit_once('.').map(|(s, _)| s).unwrap_or(base);
                        let name = format!("{stem}.{}", mime.ext());
                        // Capture the encoded bytes + content hash so Save can persist
                        // this texture (without a hash the save gate refuses with
                        // "texture(s) with no captured bytes").
                        let hash = content_hash(&bytes);
                        crate::engine::bridge::texture_cache::store(id, bytes, mime);
                        self.scene.assets.lock().unwrap().entries.insert(
                            id,
                            AssetEntry::new_with_hash(
                                SceneAssetSource::Texture(TextureDef::Raster {
                                    display_name: name,
                                    color_kind: None,
                                }),
                                hash,
                            ),
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
                // Fetch the KTX bytes NOW and stash them so the environment PERSISTS:
                // Save writes `assets/<id>.ktx2` from the stash (`ktx_files`) and a
                // reload re-stashes it (`restore_ktx`) — mirroring the ribbon HDR
                // picker (Filename source + `stash_ktx`). A bare `Url` source would
                // only refetch on reload and break once the URL is gone. `env_sync`'s
                // `Filename` arm then loads the cubemap from the stash at apply time.
                let bytes = gloo_net::http::Request::get(&url)
                    .send()
                    .await
                    .map_err(|e| crate::error::EditorError::msg(format!("fetch {url}: {e}")))?
                    .binary()
                    .await
                    .map_err(|e| {
                        crate::error::EditorError::msg(format!("fetch {url} body: {e}"))
                    })?;
                // Validate the bytes parse as a KTX2 CUBEMAP *now*, so a bad
                // URL (2D texture, wrong supercompression, truncated file)
                // fails THIS command loudly. Without this the parse only
                // happened at env-APPLY time, inside the async env_sync
                // observer — a toast + console error the MCP caller never
                // sees, while its set_environment returned "ok" and the
                // previous IBL silently stayed bound ("the prefiltered slot
                // is dead" class of misdiagnosis).
                awsm_renderer_core::cubemap::CubemapImage::load_ktx_bytes(bytes.clone()).map_err(
                    |e| {
                        crate::error::EditorError::msg(format!(
                            "{url} is not a loadable KTX2 cubemap: {e}"
                        ))
                    },
                )?;
                crate::engine::bridge::env_sync::stash_ktx(id, bytes);
                let name = url
                    .split(['?', '#'])
                    .next()
                    .unwrap_or(&url)
                    .rsplit('/')
                    .next()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("env.ktx2")
                    .to_string();
                self.scene
                    .assets
                    .lock()
                    .unwrap()
                    .entries
                    .insert(id, AssetEntry::new(SceneAssetSource::Filename(name)));
                self.scene.bump_revision();
                self.dirty.set_neq(true);
                Ok(Some(EditorCommand::DeleteAsset { id }))
            }
            // ───────────────────── Animation: clip lifecycle ─────────────────
            EditorCommand::AddClip { id, name } => {
                // Idempotent: a cross-tab relay replays this; if the clip id
                // already exists (or a self-echo slips through) it's a no-op.
                if find_clip(&self.custom_animations, id).is_none() {
                    let name = name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| {
                        let n = self.custom_animations.lock_ref().len() + 1;
                        format!("Clip {n}")
                    });
                    let clip = CA::new(id, name);
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
                // Bump anim_revision so the bridge RE-LOWERS the now-active clip.
                // The relower clears all renderer clip groups and lowers only the
                // active clip (+ mixer refs), so switching clips MUST re-lower —
                // otherwise the newly-selected clip has no clip group and `pin_pose`
                // can't pose it. This is what made IMPORTED glTF clips (and any
                // clip-switch) not play: selecting them never triggered a re-lower,
                // since only authoring edits (SetKeyframe/SetTrackSampler/…) bumped
                // anim_revision.
                self.anim_revision.replace_with(|v| v.wrapping_add(1));
                Ok(None)
            }
            // ───────────────────── Animation: clip props ─────────────────────
            EditorCommand::RenameClip { id, name } => match find_clip(&self.custom_animations, id) {
                Some(c) => {
                    let prev = c.name.get_cloned();
                    c.name.set(name);
                    self.dirty.set_neq(true);
                    Ok(Some(EditorCommand::RenameClip { id, name: prev }))
                }
                None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
            },
            EditorCommand::SetClipDuration { id, duration } => {
                match find_clip(&self.custom_animations, id) {
                    Some(c) => {
                        let prev = c.duration.get();
                        c.duration.set(duration.max(0.0));
                        self.dirty.set_neq(true);
                        Ok(Some(EditorCommand::SetClipDuration { id, duration: prev }))
                    }
                    None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
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
                    None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
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
                    None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
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
                    None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
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
                    None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
                }
            }
            // ───────────────────── Animation: tracks ─────────────────────────
            EditorCommand::AddTrack { clip, target } => {
                // Validate the target references a live object BEFORE creating the
                // track, so an invalid pick surfaces a user-facing error instead of
                // failing only via a silent `tracing::error` during lowering (the
                // add-track "doesn't exist" report). A node-bound target is valid
                // when the node is still in the scene; a Uniform target when its
                // material still exists. (A node that's in the scene but not yet
                // GPU-materialized is *pending*, not invalid — it resolves once
                // materialized — so we check scene membership, not the bridge.)
                if let Some(node) = animation::target_node(&target) {
                    if mutate::find_by_id(&self.scene, node).is_none() {
                        Toast::error("Can't add that track — its target node no longer exists.");
                        return Ok(None);
                    }
                } else if let animation::TrackTarget::Uniform { material, .. } = &target {
                    if find_material(&self.custom_materials, *material).is_none() {
                        Toast::error(
                            "Can't add that track — its target material no longer exists.",
                        );
                        return Ok(None);
                    }
                }
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
                    None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
                }
            }
            EditorCommand::AddSpinTrack {
                clip,
                node,
                axis,
                turns,
                duration,
                keys_per_turn,
            } => {
                // Validate the node exists (mirrors AddTrack) before building.
                if mutate::find_by_id(&self.scene, node).is_none() {
                    Toast::error("Can't add a spin track — its target node no longer exists.");
                    return Ok(None);
                }
                match find_clip(&self.custom_animations, clip) {
                    Some(c) => {
                        let (times, keys) = animation::spin_keyframes(
                            axis,
                            turns,
                            duration,
                            keys_per_turn.unwrap_or(4),
                        );
                        let st = animation::StoredTrack {
                            target: animation::TrackTarget::Transform {
                                node,
                                prop: animation::TransformProp::Rotation,
                            },
                            sampler: animation::SamplerKind::Linear,
                            mute: false,
                            solo: false,
                            expanded: false,
                            times,
                            keys,
                        };
                        let live = animation::stored_track_to_live(&st);
                        let index = c.tracks.lock_ref().len();
                        c.tracks.lock_mut().push_cloned(live);
                        self.dirty.set_neq(true);
                        Toast::info(format!("Added spin track ({turns} turn(s))"));
                        Ok(Some(EditorCommand::DeleteTrack { clip, track: index }))
                    }
                    None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
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
                            None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
                        }
                    }
                    None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
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
                    None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
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
                None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
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
                    None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
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
                    None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
                }
            }
            // ───────────────────── Animation: keyframes ──────────────────────
            EditorCommand::SetTrackKeys {
                clip,
                track,
                times,
                values,
                interp,
                keys,
            } => match find_track(&self.custom_animations, clip, track) {
                Some(tr) => {
                    // Build the replacement key list: full `keys` win (lossless
                    // undo form), else one keyframe per (time, value) pair.
                    let new_keys: Vec<animation::Keyframe> = if !keys.is_empty() {
                        if keys.len() != times.len() {
                            return Err(crate::error::EditorError::msg(format!(
                                "set_track_keys: {} times but {} keys",
                                times.len(),
                                keys.len()
                            )));
                        }
                        keys
                    } else {
                        if values.len() != times.len() {
                            return Err(crate::error::EditorError::msg(format!(
                                "set_track_keys: {} times but {} values",
                                times.len(),
                                values.len()
                            )));
                        }
                        let interp = interp
                            .unwrap_or_else(|| animation::sampler_to_interp(tr.sampler.get()));
                        values
                            .into_iter()
                            .map(|v| animation::new_keyframe(v, interp))
                            .collect()
                    };
                    // Sort by time (callers need not pre-sort).
                    let mut paired: Vec<(f64, animation::Keyframe)> =
                        times.into_iter().zip(new_keys).collect();
                    paired.sort_by(|a, b| a.0.total_cmp(&b.0));
                    let (sorted_times, sorted_keys): (Vec<f64>, Vec<animation::Keyframe>) =
                        paired.into_iter().unzip();

                    let prev_times = tr.times.lock_ref().to_vec();
                    let prev_keys = tr.keys.lock_ref().to_vec();
                    *tr.times.lock_mut() = sorted_times;
                    *tr.keys.lock_mut() = sorted_keys;
                    self.dirty.set_neq(true);
                    Ok(Some(EditorCommand::SetTrackKeys {
                        clip,
                        track,
                        times: prev_times,
                        values: Vec::new(),
                        interp: None,
                        keys: prev_keys,
                    }))
                }
                None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
            },
            EditorCommand::AddKeyframe {
                clip,
                track,
                t,
                value,
                interp,
            } => match find_track(&self.custom_animations, clip, track) {
                Some(tr) => {
                    // Caller-supplied interp wins; else derive from the track sampler
                    // (prior behavior) so existing callers are unchanged.
                    let interp =
                        interp.unwrap_or_else(|| animation::sampler_to_interp(tr.sampler.get()));
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
                None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
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
                    None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
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
                None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
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
                None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
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
                    None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
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
                    None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
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
                    None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
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
                    None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
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
                    None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
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
                    None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
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
                    None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
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
                    None => Err(crate::error::EditorError::msg(
                    "target id not found — command not applied (check ids against get_snapshot)",
                )),
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
    /// Returns `Err` with the load/parse failure so command dispatch can surface
    /// it to the caller (the MCP agent sees a real error instead of a silent
    /// "ok"); the toast is kept so local UI flows still get visible feedback.
    fn finish_model_import(
        &self,
        result: Result<crate::engine::bridge::gltf::GltfImport, String>,
    ) -> Result<(), String> {
        let import = match result {
            Ok(i) => i,
            Err(e) => {
                Toast::error(format!("Import failed: {e}"));
                return Err(e);
            }
        };

        if import.template.roots.is_empty() {
            Toast::error("This model contains no nodes to insert");
            return Err("this model contains no nodes to insert".to_string());
        }

        // Bring the imported materials into the **assignable library** (so they
        // can be used on any mesh) and wire them onto the model's meshes — with
        // their textures preserved by reusing the renderer textures populate
        // already uploaded (see `gltf::ExtractedMaterial`). Each glTF material
        // becomes a built-in PBR library material; its textures become texture
        // assets (deduped by baked key) pre-registered to the baked GPU texture.
        use awsm_renderer_editor_protocol::MaterialShading;

        let mut tex_for_key: std::collections::HashMap<
            awsm_renderer::textures::TextureKey,
            AssetId,
        > = std::collections::HashMap::new();
        #[allow(clippy::type_complexity)]
        let mut texture_entries: Vec<(
            AssetId,
            String,
            Option<(String, awsm_renderer_glb_export::ImageMime)>,
            TextureColorKind,
        )> = Vec::new();
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
                TextureColorKind::Albedo,
                &import.texture_images,
            );
            def.metallic_roughness_texture = ensure_import_texture(
                &mut tex_for_key,
                &mut texture_entries,
                ex.textures.metallic_roughness,
                &format!("{label} · metal/rough"),
                TextureColorKind::MetallicRoughness,
                &import.texture_images,
            );
            def.normal_texture = ensure_import_texture(
                &mut tex_for_key,
                &mut texture_entries,
                ex.textures.normal,
                &format!("{label} · normal"),
                TextureColorKind::Normal,
                &import.texture_images,
            );
            def.occlusion_texture = ensure_import_texture(
                &mut tex_for_key,
                &mut texture_entries,
                ex.textures.occlusion,
                &format!("{label} · occlusion"),
                TextureColorKind::Occlusion,
                &import.texture_images,
            );
            def.emissive_texture = ensure_import_texture(
                &mut tex_for_key,
                &mut texture_entries,
                ex.textures.emissive,
                &format!("{label} · emissive"),
                TextureColorKind::Emissive,
                &import.texture_images,
            );
            // KHR-extension texture slots (clearcoat normal map, specular colour
            // map, sheen colour map, …): create a texture asset for each + write
            // the TextureRef onto the matching extension field. The slot name maps
            // to the texture's color kind (so its color space + mipmaps persist).
            for (slot, baked) in &ex.ext_textures {
                let tref = ensure_import_texture(
                    &mut tex_for_key,
                    &mut texture_entries,
                    Some(*baked),
                    &format!("{label} · {slot}"),
                    ext_slot_color_kind(slot),
                    &import.texture_images,
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
        let img_ids: Vec<AssetId> = texture_entries.iter().map(|(id, ..)| *id).collect();
        let asset_id = {
            let mut table = self.scene.assets.lock().unwrap();
            for (id, name, hash_mime, color_kind) in &texture_entries {
                // Captured bytes ⇒ a file-backed entry (content_hash addresses
                // `assets/<hash>.<ext>`; the ext rides the display_name so
                // `asset_filename` derives it). Otherwise a plain (session-only)
                // entry — e.g. an external-file-URI texture we couldn't capture.
                // `color_kind` carries the slot's semantic (color space + mipmaps)
                // so a Save→reload re-uploads with the same meaning.
                let entry = match hash_mime {
                    Some((hash, mime)) => AssetEntry::new_with_hash(
                        SceneAssetSource::Texture(TextureDef::Raster {
                            display_name: format!("{name}.{}", mime.ext()),
                            color_kind: Some(*color_kind),
                        }),
                        hash.clone(),
                    ),
                    None => AssetEntry::new(SceneAssetSource::Texture(TextureDef::Raster {
                        display_name: name.clone(),
                        color_kind: Some(*color_kind),
                    })),
                };
                table.entries.insert(*id, entry);
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
            let def = awsm_renderer_editor_protocol::MaterialDef {
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
                &import.node_flat_indices,
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
        // Track every minted node against this import's template so the
        // template's populate-baked renderer resources (meshes / materials →
        // pooled textures / baked transforms) are reclaimed when the LAST
        // instance is deleted mid-session — not only at project reset. Walk the
        // whole subtree (not just `node_map`, which omits per-primitive
        // destructured children) so the refcount counts every deletable node.
        {
            fn collect_ids(node: &crate::engine::scene::node::Node, out: &mut Vec<NodeId>) {
                out.push(node.id);
                for c in node.children.lock_ref().iter() {
                    collect_ids(c, out);
                }
            }
            let mut ids = Vec::new();
            for root in &roots {
                collect_ids(root, &mut ids);
            }
            crate::engine::bridge::bridge().register_template_instances(asset_id, ids);
        }

        // Captured for the import report (LastImportReport query / MCP return).
        let report_roots: Vec<serde_json::Value> = roots
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.id.to_string(),
                    "name": r.name.get_cloned(),
                })
            })
            .collect();

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
        let mut skin_joint_count = 0;
        {
            fn walk_skin_joints(
                scene: &crate::engine::scene::Scene,
                nodes: &[crate::engine::bridge::asset_template::AssetTemplateNode],
                node_map: &std::collections::HashMap<u32, NodeId>,
                count: &mut usize,
            ) {
                let bridge = crate::engine::bridge::bridge();
                for n in nodes {
                    if n.is_skin_joint {
                        if let Some(node_id) = node_map.get(&n.gltf_node_index) {
                            bridge.register_skin_joint(*node_id, n.baked_transform_key);
                            // The freshly-built editor node still carries the
                            // glTF-parsed local — record it as this joint's
                            // bind/rest pose for `ResetToBindPose`.
                            if let Some(bone) = mutate::find_by_id(scene, *node_id) {
                                bridge.register_joint_rest(*node_id, bone.transform.get());
                            }
                            *count += 1;
                        }
                    }
                    walk_skin_joints(scene, &n.children, node_map, count);
                }
            }
            walk_skin_joints(
                &self.scene,
                &template.roots,
                &node_map,
                &mut skin_joint_count,
            );
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

        // Publish the import report (LastImportReport query / inline MCP return).
        self.last_import_report.set(Some(serde_json::json!({
            "display_name": import.display_name,
            "source_asset": asset_id.to_string(),
            "roots": report_roots,
            "node_count": node_map.len(),
            "materials": mat_ids.iter().map(|m| m.to_string()).collect::<Vec<_>>(),
            "skin_joints": skin_joint_count,
            "clips": clip_count,
        })));
        Ok(())
    }

    /// Remove animation clips that are now FULLY orphaned — every track targets
    /// a node no longer in the scene (e.g. all clips of a just-deleted imported
    /// model). Returns the count freed. Kept: clips with any still-present node
    /// target, any material (`Uniform`) target, or no tracks. `animation_sync`
    /// re-lowers off `custom_animations`, so the freed clips also drop from the
    /// renderer; a toast surfaces the cleanup.
    ///
    /// NOT recorded in undo — an orphaned clip can't animate anything (all its
    /// targets are gone), so this is a one-way cleanup of dead data: undoing the
    /// delete restores the nodes, and the (re-importable) clips stay freed. This
    /// matches standard DCC behavior — deleting an imported model frees its
    /// imported animations.
    fn prune_orphaned_clips(&self) -> usize {
        use super::animation::TrackTarget;
        let mut removed = 0usize;
        self.custom_animations.lock_mut().retain(|clip| {
            let tracks = clip.tracks.lock_ref();
            if tracks.is_empty() {
                return true;
            }
            let all_orphaned = tracks.iter().all(|t| match &t.target {
                TrackTarget::Transform { node, .. }
                | TrackTarget::Morph { node, .. }
                | TrackTarget::BuiltinParam { node, .. }
                | TrackTarget::Light { node, .. }
                | TrackTarget::Camera { node, .. }
                | TrackTarget::TextureTransform { node, .. } => {
                    mutate::find_by_id(&self.scene, *node).is_none()
                }
                // A material-targeted track isn't node-orphaned — keep the clip.
                TrackTarget::Uniform { .. } => false,
            });
            if all_orphaned {
                removed += 1;
            }
            !all_orphaned
        });
        if removed > 0 {
            if let Some(cur) = self.current_clip.get() {
                if find_clip(&self.custom_animations, cur).is_none() {
                    let next = self.custom_animations.lock_ref().first().map(|c| c.id);
                    self.current_clip.set(next);
                }
            }
            self.dirty.set_neq(true);
            Toast::info(format!(
                "Freed {removed} orphaned clip{}",
                if removed == 1 { "" } else { "s" }
            ));
        }
        removed
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
                env_unsaved: self.scene.environment.get_cloned()
                    != self.env_saved_baseline.get_cloned(),
                missing_assets: self.missing_assets.get_cloned(),
                environment: self.environment_snapshot(),
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
                        builtin_def: m.builtin.get_cloned().map(Box::new),
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
        use awsm_renderer_editor_protocol::{AssetSource as S, TextureDef};
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

    /// Per-slot read of the current environment for the snapshot — so an MCP
    /// driver can see WHAT is set (built-in default vs. a KTX asset vs. a sky
    /// gradient), mirroring the editor's three per-slot pickers.
    fn environment_snapshot(&self) -> query::EnvironmentSnapshot {
        use crate::engine::scene::{AssetSource, EnvSlot};
        let env = self.scene.environment.get_cloned();
        let assets = self.scene.assets.lock().unwrap();
        let slot = |s: &EnvSlot| -> query::EnvSlotSnapshot {
            match s {
                EnvSlot::BuiltInDefault => query::EnvSlotSnapshot::default(),
                EnvSlot::SkyGradient { zenith, nadir } => query::EnvSlotSnapshot {
                    kind: "sky_gradient".to_string(),
                    asset_id: None,
                    label: None,
                    gradient: Some([*zenith, *nadir]),
                },
                EnvSlot::Ktx { asset_id } => {
                    let label = assets.entries.get(asset_id).and_then(|e| match &e.source {
                        AssetSource::Filename(name) => Some(name.clone()),
                        AssetSource::Url(url) => url.rsplit('/').next().map(str::to_string),
                        _ => None,
                    });
                    query::EnvSlotSnapshot {
                        kind: "ktx".to_string(),
                        asset_id: Some(asset_id.to_string()),
                        label,
                        gradient: None,
                    }
                }
            }
        };
        query::EnvironmentSnapshot {
            skybox: slot(&env.skybox),
            specular: slot(&env.specular),
            irradiance: slot(&env.irradiance),
            probe: env.probe,
        }
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
                            TrackTarget::TextureTransform { slot, prop, .. } => {
                                format!("texuv:{slot:?}:{prop:?}")
                            }
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

    /// Bake a node's subtree (`Some`) or the whole scene (`None`) to a binary
    /// glTF. Whole-scene export includes animations; single-node does not. The
    /// raw `.glb` bytes ride the `/glb/<id>` side-channel (see `Request::ExportGlb`)
    /// rather than the control link, so a large export can't blow the session.
    pub async fn export_glb_bytes(&self, node: Option<NodeId>) -> Result<Vec<u8>, String> {
        match node {
            Some(id) => crate::controller::export::export_glb(&self.scene, Some(id)).await,
            None => crate::controller::export::export_scene_glb(self).await,
        }
    }

    /// Run a read-only [`EditorQuery`] and return a serializable result.
    /// Read-only: never mutates persisted state, never records undo, never
    /// broadcasts; the pinning handler saves + restores the transport.
    pub async fn query(&self, q: query::EditorQuery) -> query::QueryResult {
        use query::*;
        match q {
            EditorQuery::Snapshot => QueryResult::Snapshot(Box::new(self.snapshot())),
            EditorQuery::LastImportReport => {
                let mut entries = std::collections::BTreeMap::new();
                entries.insert(
                    "report".to_string(),
                    self.last_import_report
                        .get_cloned()
                        .unwrap_or(serde_json::Value::Null),
                );
                QueryResult::Map(MapResult {
                    kind: "import_report".to_string(),
                    entries,
                })
            }
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
            EditorQuery::ScenePng { width, height } => {
                // GPU-read the swapchain → PNG data URL (the same capture the MCP
                // screenshot_scene tool uses), returned as Text so the /debug
                // Query channel can surface it. `None` ⇒ the tab isn't presenting
                // frames (backgrounded / not yet rendered).
                match crate::engine::query::scene_png(width, height).await {
                    Some(data_url) => QueryResult::Text(data_url),
                    None => QueryResult::Error {
                        error: "scene_png: no frame captured (tab not presenting? \
                                foreground the editor + retry)"
                            .to_string(),
                    },
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
                        entries.insert("assigned".to_string(), json!(false));
                        entries.insert("kind".to_string(), json!(unassigned_material_kind(&kind)));
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
            EditorQuery::GetChildren { node } => {
                let Some(n) = mutate::find_by_id(&self.scene, node) else {
                    return QueryResult::Error {
                        error: format!("no node {node}"),
                    };
                };
                let children: Vec<serde_json::Value> = n
                    .children
                    .lock_ref()
                    .iter()
                    .map(|c| node_brief(c))
                    .collect();
                let mut entries = std::collections::BTreeMap::new();
                entries.insert("children".to_string(), serde_json::json!(children));
                QueryResult::Map(query::MapResult {
                    kind: "children".to_string(),
                    entries,
                })
            }
            EditorQuery::GetSubtree { root } => {
                let tree: Vec<serde_json::Value> = match root {
                    Some(id) => {
                        let Some(n) = mutate::find_by_id(&self.scene, id) else {
                            return QueryResult::Error {
                                error: format!("no node {id}"),
                            };
                        };
                        vec![node_subtree_json(&n)]
                    }
                    None => self
                        .scene
                        .nodes
                        .lock_ref()
                        .iter()
                        .map(|n| node_subtree_json(n))
                        .collect(),
                };
                let mut entries = std::collections::BTreeMap::new();
                entries.insert("tree".to_string(), serde_json::json!(tree));
                QueryResult::Map(query::MapResult {
                    kind: "subtree".to_string(),
                    entries,
                })
            }
            EditorQuery::SelectVerticesWhere {
                node,
                predicate,
                store,
                count_only,
                offset,
                limit,
            } => {
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
                        let idx = select_vertices_by_predicate(&mesh, &predicate);
                        let mut entries = std::collections::BTreeMap::new();
                        entries.insert("count".to_string(), json!(idx.len()));
                        if store {
                            // §10: keep the indices server-side, hand back a reusable
                            // handle — no array crosses the wire.
                            entries.insert("id".to_string(), json!(store_vertex_selection(idx)));
                        } else if !count_only {
                            // Optional pagination so a big raw read can be windowed.
                            let start = offset.unwrap_or(0) as usize;
                            let page: Vec<u32> = match limit {
                                Some(l) => {
                                    idx.iter().skip(start).take(l as usize).copied().collect()
                                }
                                None => idx.iter().skip(start).copied().collect(),
                            };
                            entries.insert("offset".to_string(), json!(start));
                            entries.insert("returned".to_string(), json!(page.len()));
                            entries.insert("indices".to_string(), json!(page));
                        }
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
                        let s = awsm_renderer_meshgen::mesh_stats(&mesh);
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
                        let profile = awsm_renderer_meshgen::cross_section_profile(
                            &mesh,
                            axis as usize,
                            samples,
                        );
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
            EditorQuery::GetVertexData {
                node,
                indices,
                selection,
                offset,
                limit,
                include_source,
            } => {
                use serde_json::json;
                if node_is_skinned(&self.scene, node) {
                    return QueryResult::Error {
                        error: skinned_edit_error(node),
                    };
                }
                // §P3: optionally tag each channel base-vs-override. Resolve the
                // mesh def's sparse override maps once (same node→mesh→def path as
                // get_mesh_layers); a vertex index present in a channel's map was
                // authored, otherwise it rides the evaluated base.
                let overrides = if include_source {
                    mutate::find_by_id(&self.scene, node)
                        .and_then(|n| match n.kind.get_cloned() {
                            NodeKind::Mesh { mesh, .. } => Some(mesh.0),
                            _ => None,
                        })
                        .and_then(|id| {
                            match self.scene.assets.lock().unwrap().get(id).map(|e| &e.source) {
                                Some(SceneAssetSource::Mesh(def)) => Some(def.overrides.clone()),
                                _ => None,
                            }
                        })
                } else {
                    None
                };
                // §10: a `selection` handle's indices win over an explicit list; a
                // dangling handle errors loudly.
                let target: Vec<u32> = match selection {
                    Some(id) => match lookup_vertex_selection(id) {
                        Some(v) => v,
                        None => {
                            return QueryResult::Error {
                                error: format!("no vertex-selection handle {id}"),
                            }
                        }
                    },
                    None => indices,
                };
                let mesh = mutate::find_by_id(&self.scene, node).and_then(|n| {
                    crate::controller::export::node_mesh(&self.scene, &n.kind.get_cloned())
                });
                match mesh {
                    Some(md) => {
                        // §10: window the (possibly large) selection so its per-vertex
                        // data doesn't overflow the token cap.
                        let start = offset.unwrap_or(0) as usize;
                        let selected = target.len();
                        let page: Vec<u32> = match limit {
                            Some(l) => target
                                .iter()
                                .skip(start)
                                .take(l as usize)
                                .copied()
                                .collect(),
                            None => target.iter().skip(start).copied().collect(),
                        };
                        let verts: Vec<serde_json::Value> = page
                            .iter()
                            .map(|&i| {
                                let idx = i as usize;
                                let mut v = json!({
                                    "index": i,
                                    "position": md.positions.get(idx),
                                    "normal": md.normals.as_ref().and_then(|n| n.get(idx)),
                                    "color": md.colors.as_ref().and_then(|c| c.get(idx)),
                                    "uv": md.uvs.first().and_then(|u| u.get(idx)),
                                });
                                if let Some(ov) = &overrides {
                                    let src =
                                        |present: bool| if present { "override" } else { "base" };
                                    v["source"] = json!({
                                        "position": src(ov.positions.contains_key(&i)),
                                        "normal": src(ov.normals.contains_key(&i)),
                                        "color": src(ov.colors.contains_key(&i)),
                                        "uv": src(ov.uvs.contains_key(&i)),
                                    });
                                }
                                v
                            })
                            .collect();
                        let mut entries = std::collections::BTreeMap::new();
                        entries.insert("vertex_count".to_string(), json!(md.positions.len()));
                        entries.insert("selected".to_string(), json!(selected));
                        entries.insert("offset".to_string(), json!(start));
                        entries.insert("returned".to_string(), json!(verts.len()));
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
                use awsm_renderer_editor_protocol::MeshBase;
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
            EditorQuery::GetMeshData {
                node,
                offset,
                limit,
            } => {
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
                        let tri_count = md.indices.len() / 3;
                        let start = offset.unwrap_or(0) as usize;
                        // Page over WHOLE triangles (the new payload). Each chunk is
                        // [a,b,c] vertex indices; per-vertex attrs come from
                        // get_vertex_data (kept out here to stay compact).
                        let tris_iter = md.indices.chunks_exact(3).skip(start);
                        let tris: Vec<[u32; 3]> = match limit {
                            Some(l) => tris_iter
                                .take(l as usize)
                                .map(|c| [c[0], c[1], c[2]])
                                .collect(),
                            None => tris_iter.map(|c| [c[0], c[1], c[2]]).collect(),
                        };
                        // Local-space bbox over positions.
                        let bbox = if md.positions.is_empty() {
                            json!(null)
                        } else {
                            let mut min = [f32::INFINITY; 3];
                            let mut max = [f32::NEG_INFINITY; 3];
                            for p in &md.positions {
                                for k in 0..3 {
                                    min[k] = min[k].min(p[k]);
                                    max[k] = max[k].max(p[k]);
                                }
                            }
                            json!({ "min": min, "max": max })
                        };
                        let mut entries = std::collections::BTreeMap::new();
                        entries.insert("vertex_count".to_string(), json!(md.positions.len()));
                        entries.insert("triangle_count".to_string(), json!(tri_count));
                        entries.insert("offset".to_string(), json!(start));
                        entries.insert("returned".to_string(), json!(tris.len()));
                        entries.insert("triangles".to_string(), json!(tris));
                        entries.insert("bbox".to_string(), bbox);
                        QueryResult::Map(query::MapResult {
                            kind: "mesh_data".to_string(),
                            entries,
                        })
                    }
                    None => QueryResult::Error {
                        error: format!("node {node} has no resolvable mesh"),
                    },
                }
            }
            EditorQuery::StripParameterize {
                node,
                selection,
                indices,
                axis,
            } => {
                use serde_json::json;
                if node_is_skinned(&self.scene, node) {
                    return QueryResult::Error {
                        error: skinned_edit_error(node),
                    };
                }
                let mesh = mutate::find_by_id(&self.scene, node).and_then(|n| {
                    crate::controller::export::node_mesh(&self.scene, &n.kind.get_cloned())
                });
                let Some(md) = mesh else {
                    return QueryResult::Error {
                        error: format!("node {node} has no resolvable mesh"),
                    };
                };
                // Resolve the band: selection handle > explicit indices > whole mesh.
                let target: Vec<u32> = match selection {
                    Some(id) => match lookup_vertex_selection(id) {
                        Some(v) => v,
                        None => {
                            return QueryResult::Error {
                                error: format!("no vertex-selection handle {id}"),
                            }
                        }
                    },
                    None if !indices.is_empty() => indices,
                    None => (0..md.positions.len() as u32).collect(),
                };
                // Gather the band's positions (skip any out-of-range index).
                let positions: Vec<[f32; 3]> = target
                    .iter()
                    .filter_map(|&i| md.positions.get(i as usize).copied())
                    .collect();
                let (resolved_axis, coords) =
                    awsm_renderer_meshgen::edit::strip_parameterize(&positions, axis);
                // Pair each in-range index with its (along, across).
                let verts: Vec<serde_json::Value> = target
                    .iter()
                    .filter(|&&i| (i as usize) < md.positions.len())
                    .zip(coords.iter())
                    .map(|(&i, c)| json!({ "index": i, "along": c[0], "across": c[1] }))
                    .collect();
                let mut entries = std::collections::BTreeMap::new();
                entries.insert("axis".to_string(), json!(resolved_axis));
                entries.insert("count".to_string(), json!(verts.len()));
                entries.insert("heuristic".to_string(), json!(true));
                entries.insert(
                    "note".to_string(),
                    json!("along=travel about axle [0,1); across=lateral along axle [0,1]; winding/polarity may be flipped — flip axis or use 1-coord if needed"),
                );
                entries.insert("vertices".to_string(), json!(verts));
                QueryResult::Map(query::MapResult {
                    kind: "strip_parameterize".to_string(),
                    entries,
                })
            }
            EditorQuery::UvLayout {
                node,
                uv_set,
                offset,
                limit,
            } => {
                use serde_json::json;
                if node_is_skinned(&self.scene, node) {
                    return QueryResult::Error {
                        error: skinned_edit_error(node),
                    };
                }
                let mesh = mutate::find_by_id(&self.scene, node).and_then(|n| {
                    crate::controller::export::node_mesh(&self.scene, &n.kind.get_cloned())
                });
                let Some(md) = mesh else {
                    return QueryResult::Error {
                        error: format!("node {node} has no resolvable mesh"),
                    };
                };
                let set = uv_set.unwrap_or(0) as usize;
                let Some(uvs) = md.uvs.get(set) else {
                    let mut entries = std::collections::BTreeMap::new();
                    entries.insert("has_uv".to_string(), json!(false));
                    entries.insert("uv_set".to_string(), json!(set));
                    return QueryResult::Map(query::MapResult {
                        kind: "uv_layout".to_string(),
                        entries,
                    });
                };
                let (island_of, count) = awsm_renderer_meshgen::edit::uv_islands(uvs, &md.indices);
                // Per-island vertex count + UV bounds; overall bounds.
                let mut isl_min = vec![[f32::INFINITY; 2]; count as usize];
                let mut isl_max = vec![[f32::NEG_INFINITY; 2]; count as usize];
                let mut isl_n = vec![0u32; count as usize];
                let (mut omin, mut omax) = ([f32::INFINITY; 2], [f32::NEG_INFINITY; 2]);
                for (i, uv) in uvs.iter().enumerate() {
                    let c = island_of[i] as usize;
                    isl_n[c] += 1;
                    for k in 0..2 {
                        isl_min[c][k] = isl_min[c][k].min(uv[k]);
                        isl_max[c][k] = isl_max[c][k].max(uv[k]);
                        omin[k] = omin[k].min(uv[k]);
                        omax[k] = omax[k].max(uv[k]);
                    }
                }
                let islands: Vec<serde_json::Value> = (0..count as usize)
                    .map(|c| json!({ "count": isl_n[c], "min": isl_min[c], "max": isl_max[c] }))
                    .collect();
                // Unique undirected UV edges (the wireframe), paged.
                let mut seen = std::collections::HashSet::new();
                let mut all_edges: Vec<[u32; 2]> = Vec::new();
                for tri in md.indices.chunks_exact(3) {
                    for &(a, b) in &[(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])] {
                        let e = if a < b { (a, b) } else { (b, a) };
                        if seen.insert(e) {
                            all_edges.push([e.0, e.1]);
                        }
                    }
                }
                let edge_count = all_edges.len();
                let start = offset.unwrap_or(0) as usize;
                // Bounded DEFAULT: an omitted `limit` used to return the ENTIRE UV
                // wireframe (7000+ edges on a dense mesh → a ~120 KB result that
                // overflows a normal tool budget). Cap the default page so a naive
                // call stays readable; callers paginate via offset/limit (and see
                // `edge_count` vs the returned length to know there's more).
                const DEFAULT_UV_EDGE_PAGE: usize = 1000;
                let take_n = limit.map(|l| l as usize).unwrap_or(DEFAULT_UV_EDGE_PAGE);
                let page = all_edges
                    .iter()
                    .skip(start)
                    .take(take_n)
                    .collect::<Vec<_>>();
                let edges: Vec<serde_json::Value> = page
                    .iter()
                    .filter_map(|e| {
                        let a = uvs.get(e[0] as usize)?;
                        let b = uvs.get(e[1] as usize)?;
                        Some(json!([a, b]))
                    })
                    .collect();
                let mut entries = std::collections::BTreeMap::new();
                entries.insert("has_uv".to_string(), json!(true));
                entries.insert("uv_set".to_string(), json!(set));
                entries.insert("island_count".to_string(), json!(count));
                entries.insert("bounds".to_string(), json!({ "min": omin, "max": omax }));
                entries.insert("islands".to_string(), json!(islands));
                entries.insert("edge_count".to_string(), json!(edge_count));
                entries.insert("offset".to_string(), json!(start));
                entries.insert("returned".to_string(), json!(edges.len()));
                entries.insert("edges".to_string(), json!(edges));
                QueryResult::Map(query::MapResult {
                    kind: "uv_layout".to_string(),
                    entries,
                })
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
            EditorQuery::SaveCensus => QueryResult::Text(
                serde_json::to_string(&crate::controller::persistence::save_census(self))
                    .unwrap_or_default(),
            ),
            EditorQuery::VerifyRoundtripReport => QueryResult::Text(
                self.verify_roundtrip_report
                    .get_cloned()
                    .unwrap_or(serde_json::Value::Null)
                    .to_string(),
            ),
            EditorQuery::PostProcess => QueryResult::Text(
                serde_json::to_string(&self.scene.post_process.get_cloned()).unwrap_or_default(),
            ),
            EditorQuery::Shadows => QueryResult::Text(
                serde_json::to_string(&self.scene.shadows.get_cloned()).unwrap_or_default(),
            ),
            EditorQuery::ViewOptions => {
                use serde_json::json;
                QueryResult::Text(
                    json!({
                        "grid": self.settings.grid.get(),
                        "gizmos": self.settings.gizmo.get(),
                        "light_gizmos": self.settings.light_gizmos.get(),
                        "skeleton_viz": self.settings.skeleton_viz.get(),
                        "follow_agent": crate::engine::activity_feed::follow_enabled().get(),
                        "activity_overlay": crate::engine::activity_feed::enabled().get(),
                        "mcp_notifications": crate::remote::show_notifications().get(),
                        "msaa": self.settings.msaa.get(),
                        "smaa": self.settings.smaa.get(),
                    })
                    .to_string(),
                )
            }
            EditorQuery::MemoryStats => {
                use serde_json::json;
                // Renderer-side object counts (under the renderer guard)…
                let (
                    meshes,
                    mesh_resources,
                    mesh_geometry_bytes,
                    transforms,
                    materials,
                    lines,
                    render_pipelines,
                    compute_pipelines,
                    shaders,
                    opaque_main,
                    edge_per_shader,
                    classify_dynamic,
                    visible_triangles,
                ) = crate::engine::context::with_renderer_mut(|r| {
                    (
                        r.meshes.len(),
                        r.meshes.resource_count(),
                        r.meshes.geometry_pool_used_bytes(),
                        r.transforms.len(),
                        r.materials.len(),
                        r.line_count(),
                        r.pipelines.render.len(),
                        r.pipelines.compute.len(),
                        r.shaders.len(),
                        r.render_passes.material_opaque.pipelines.main_len(),
                        r.render_passes
                            .material_opaque
                            .edge_pipelines
                            .per_shader_len(),
                        r.render_passes.material_classify.dynamic_cache_len(),
                        r.meshes.visible_triangle_count(),
                    )
                })
                .await;
                let (dynamic_materials, tex_pool, cubemaps, samplers) =
                    crate::engine::context::with_renderer_mut(|r| {
                        let (tp, cm, sm) = r.textures.resource_counts();
                        (r.dynamic_materials.len(), tp, cm, sm)
                    })
                    .await;
                let mut entries = std::collections::BTreeMap::new();
                entries.insert("meshes".to_string(), json!(meshes));
                // Shared-geometry census (axis 4): `meshes` counts INSTANCE
                // records; `mesh_resources` counts deduped geometry uploads and
                // `mesh_geometry_bytes` the pool bytes backing them. Prefab
                // duplicates grow `meshes` but leave these two flat.
                entries.insert("mesh_resources".to_string(), json!(mesh_resources));
                entries.insert(
                    "mesh_geometry_bytes".to_string(),
                    json!(mesh_geometry_bytes),
                );
                entries.insert("transforms".to_string(), json!(transforms));
                entries.insert("materials".to_string(), json!(materials));
                entries.insert("lines".to_string(), json!(lines));
                entries.insert("render_pipelines".to_string(), json!(render_pipelines));
                entries.insert("compute_pipelines".to_string(), json!(compute_pipelines));
                // Compute-pipeline-pool breakdown (dynamic-material leak diagnostics):
                // total shader modules + the per-pass typed caches that hold pool keys.
                // A `compute_pipelines` that exceeds `shaders` + the typed-cache sums
                // by a growing margin signals detached pool orphans.
                entries.insert("shaders".to_string(), json!(shaders));
                entries.insert("opaque_main_keys".to_string(), json!(opaque_main));
                entries.insert("edge_per_shader_keys".to_string(), json!(edge_per_shader));
                entries.insert("classify_dynamic_keys".to_string(), json!(classify_dynamic));
                // Submitted triangles across all visible meshes — the deterministic
                // discrete-LOD before/after metric (drops as instances pick coarser
                // levels at distance).
                entries.insert("visible_triangles".to_string(), json!(visible_triangles));
                entries.insert("dynamic_materials".to_string(), json!(dynamic_materials));
                // GPU texture-resource counts (leak diagnostics — the "Destroyed
                // texture"/"aw snap" blind spot). Growth under textured-material /
                // imported-model add+delete churn signals a texture/sampler leak.
                entries.insert("pool_textures".to_string(), json!(tex_pool));
                entries.insert("cubemaps".to_string(), json!(cubemaps));
                entries.insert("samplers".to_string(), json!(samplers));
                // Per-frame timing (rolling EMA, perf diagnostics): wall-clock frame
                // period (vsync-capped ~16.6ms at 60fps) + the CPU span building &
                // submitting the frame (the actionable "how heavy is this scene" number).
                let (frame_dt_ms, render_cpu_ms) = crate::engine::render_loop::frame_stats();
                entries.insert(
                    "frame_dt_ms".to_string(),
                    json!((frame_dt_ms * 100.0).round() / 100.0),
                );
                entries.insert(
                    "render_cpu_ms".to_string(),
                    json!((render_cpu_ms * 100.0).round() / 100.0),
                );
                // …plus Chrome's non-standard `performance.memory` (zeros
                // elsewhere). Read via Reflect — web_sys doesn't bind it.
                let mut heap_used = 0.0f64;
                let mut heap_total = 0.0f64;
                let mut heap_limit = 0.0f64;
                if let Some(perf) = web_sys::window().and_then(|w| w.performance()) {
                    if let Ok(mem) = js_sys::Reflect::get(&perf, &"memory".into()) {
                        if !mem.is_undefined() && !mem.is_null() {
                            let get = |k: &str| {
                                js_sys::Reflect::get(&mem, &k.into())
                                    .ok()
                                    .and_then(|v| v.as_f64())
                                    .unwrap_or(0.0)
                            };
                            heap_used = get("usedJSHeapSize");
                            heap_total = get("totalJSHeapSize");
                            heap_limit = get("jsHeapSizeLimit");
                        }
                    }
                }
                entries.insert("js_heap_used_bytes".to_string(), json!(heap_used));
                entries.insert("js_heap_total_bytes".to_string(), json!(heap_total));
                entries.insert("js_heap_limit_bytes".to_string(), json!(heap_limit));
                // Memory-leak soak instruments (docs/plans/crashes.md). Cumulative
                // (increment-only) GPU-buffer creation census: a rising slope on
                // either counter over an idle soak names the render loop as a
                // per-frame buffer-minting source (suspect #1/#5). NOT a live
                // count — there's no central destroy seam to decrement against, so
                // creation-rate (cross-referenced with the OS vmmap virtual size)
                // is the unambiguous signal.
                let (create_buffer_count, create_buffer_bytes) =
                    awsm_renderer_core::create_buffer_census();
                entries.insert(
                    "create_buffer_count".to_string(),
                    json!(create_buffer_count),
                );
                entries.insert(
                    "create_buffer_bytes".to_string(),
                    json!(create_buffer_bytes),
                );
                // Mapped-staging-ring rollup (suspect #1: map/unmap churn). The
                // ring is fixed-depth so it can't itself leak VA, but a climbing
                // `ring_fallback_count` / `ring_map_async_wait_ms` flags the ring
                // failing to release slots as fast as the CPU consumes them, and
                // `ring_bytes_uploaded` is the per-frame upload volume driving all
                // the map traffic. Folded across every subsystem's UploadStats.
                let (
                    ring_peak_depth,
                    ring_fallback_count,
                    ring_map_async_wait_ms,
                    ring_bytes_uploaded,
                    ring_resize_count,
                ) = crate::engine::context::with_renderer_mut(|r| {
                    let mut peak = 0usize;
                    let mut fallback = 0u64;
                    let mut wait_ms = 0.0f64;
                    let mut bytes = 0u64;
                    let mut resize = 0u64;
                    for (_label, s) in r.upload_ring_stats() {
                        peak = peak.max(s.peak_ring_depth_used);
                        fallback += s.fallback_count;
                        wait_ms += s.map_async_wait_ms;
                        bytes += s.bytes_uploaded_via_ring
                            + s.bytes_uploaded_via_fallback
                            + s.bytes_uploaded_via_writebuffer;
                        resize += s.resize_count;
                    }
                    (peak, fallback, wait_ms, bytes, resize)
                })
                .await;
                entries.insert("ring_peak_depth".to_string(), json!(ring_peak_depth));
                entries.insert(
                    "ring_fallback_count".to_string(),
                    json!(ring_fallback_count),
                );
                entries.insert(
                    "ring_map_async_wait_ms".to_string(),
                    json!((ring_map_async_wait_ms * 100.0).round() / 100.0),
                );
                entries.insert(
                    "ring_bytes_uploaded".to_string(),
                    json!(ring_bytes_uploaded),
                );
                entries.insert("ring_resize_count".to_string(), json!(ring_resize_count));
                // Opt-in hardening diagnostics (gated): the metrics the JS-heap
                // soak misses. `wasm_heap_bytes` is WASM linear-memory size — the
                // arena the unbounded-undo OOM actually grows. `undo_*`/`redo_*`
                // expose the bounded-history depth + its estimated retained bytes
                // (the same estimator the byte-budget cap uses), so a churn repro
                // can confirm the log PLATEAUS under budget instead of ramping
                // toward the ~2 GB realloc cliff.
                #[cfg(any(debug_assertions, feature = "harden-diag"))]
                {
                    let wasm_heap_bytes = wasm_bindgen::memory()
                        .dyn_into::<js_sys::WebAssembly::Memory>()
                        .ok()
                        .and_then(|m| m.buffer().dyn_into::<js_sys::ArrayBuffer>().ok())
                        .map(|b| b.byte_length())
                        .unwrap_or(0);
                    entries.insert("wasm_heap_bytes".to_string(), json!(wasm_heap_bytes));
                    let (undo_len, undo_bytes) = {
                        let u = self.undo.borrow();
                        (u.len(), u.bytes())
                    };
                    let (redo_len, redo_bytes) = {
                        let r = self.redo.borrow();
                        (r.len(), r.bytes())
                    };
                    entries.insert("undo_len".to_string(), json!(undo_len));
                    entries.insert("undo_bytes".to_string(), json!(undo_bytes));
                    entries.insert("redo_len".to_string(), json!(redo_len));
                    entries.insert("redo_bytes".to_string(), json!(redo_bytes));
                }
                QueryResult::Map(query::MapResult {
                    kind: "memory_stats".to_string(),
                    entries,
                })
            }
            EditorQuery::AnimationRuntime => {
                use serde_json::json;
                // Renderer-side lowered state: clip groups, resolved channels per
                // clip, rest-cache size, mixer layers.
                let (clip_count, total_channels, per_clip, rest_len, mixer_layers) =
                    crate::engine::context::with_renderer_mut(|r| {
                        let per_clip: Vec<serde_json::Value> = r
                            .animations
                            .clips_iter()
                            .map(|(_, g)| json!({"name": g.name, "channels": g.channels.len()}))
                            .collect();
                        let total: usize = r
                            .animations
                            .clips_iter()
                            .map(|(_, g)| g.channels.len())
                            .sum();
                        (
                            per_clip.len(),
                            total,
                            per_clip,
                            r.animations.rest_len(),
                            r.animations.mixer.layers.len(),
                        )
                    })
                    .await;
                // Controller-side: current clip + its authored track count (the
                // numerator the resolved channels should match).
                let current_clip = self.current_clip.get();
                let authored_tracks = current_clip
                    .and_then(|id| {
                        crate::controller::animation::find_clip(&self.custom_animations, id)
                    })
                    .map(|c| c.tracks.lock_ref().len())
                    .unwrap_or(0);
                let mut entries = std::collections::BTreeMap::new();
                entries.insert("clip_groups".to_string(), json!(clip_count));
                entries.insert("resolved_channels".to_string(), json!(total_channels));
                entries.insert("per_clip".to_string(), json!(per_clip));
                entries.insert("rest_entries".to_string(), json!(rest_len));
                entries.insert("mixer_layers".to_string(), json!(mixer_layers));
                entries.insert(
                    "current_clip".to_string(),
                    json!(current_clip.map(|id| id.to_string())),
                );
                entries.insert("authored_tracks".to_string(), json!(authored_tracks));
                entries.insert("playing".to_string(), json!(self.playing.get()));
                entries.insert("playhead".to_string(), json!(self.playhead.get()));
                QueryResult::Map(query::MapResult {
                    kind: "animation_runtime".to_string(),
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
                // Rig discovery: every skin's joint table, each joint resolved to
                // its live editor bone node (name + current local TRS). Joints are
                // ordinary scene nodes — SetTransform poses them and a Transform
                // animation track animates them; this query is just the map that
                // makes the rig reachable without walking the outliner.
                //
                // Grouped BY SKIN (source asset), not by skinned-mesh node: a
                // multi-material import splits into one skinned_mesh node per
                // primitive, all sharing one joint table — the old per-node shape
                // repeated the identical joint array once per primitive (4× the
                // payload for a 4-material rig; handoff #7). Each entry lists the
                // sharing mesh nodes instead.
                let ids = self.resolve_node_ids(&nodes);
                let bridge = crate::engine::bridge::bridge();
                let baked_map = bridge.skin_joint_baked.lock().unwrap().clone();
                // A bone is drivable through EITHER path: the legacy template
                // populate (baked-copy skin bridge) or the rig-decode
                // materializer, whose renderer skin reads the editor bone's own
                // TransformKey directly — for the latter (e.g. every DUPLICATED
                // rig) the bone just needs to be a live bridge node.
                let live_nodes: std::collections::HashSet<NodeId> =
                    bridge.nodes.lock().unwrap().keys().copied().collect();
                let mut entries = std::collections::BTreeMap::new();
                for id in ids {
                    let Some(n) = mutate::find_by_id(&self.scene, id) else {
                        continue;
                    };
                    let NodeKind::SkinnedMesh { skin, .. } = n.kind.get_cloned() else {
                        continue;
                    };
                    let mesh_entry = json!({
                        "node": id.to_string(),
                        "name": n.name.get_cloned(),
                        "primitive_index": skin.primitive_index,
                    });
                    // Group key: the first JOINT's node id — unique per rig
                    // INSTANCE (a duplicated rig shares the original's `source`
                    // asset but owns remapped joints), shared by all of one
                    // rig's per-primitive meshes. Fall back to the source for a
                    // joint-less skin.
                    let key = skin
                        .joints
                        .first()
                        .map(|j| j.node.to_string())
                        .unwrap_or_else(|| skin.source.to_string());
                    if let Some(existing) = entries.get_mut(&key) {
                        // Same skin — just record the additional sharing mesh.
                        let serde_json::Value::Object(map) = existing else {
                            continue;
                        };
                        if let Some(serde_json::Value::Array(meshes)) = map.get_mut("meshes") {
                            meshes.push(mesh_entry);
                        }
                        continue;
                    }
                    // `live`: the skin bridge holds a mirror→baked mapping for
                    // this bone, i.e. posing it actually deforms the skin. False
                    // means the rig is display-only (registration failed/skipped)
                    // — surfaced so an agent (and we) can SEE a broken chain.
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
                                "live": baked_map.contains_key(&j.node)
                                    || live_nodes.contains(&j.node),
                                "translation": trs.translation,
                                "rotation": trs.rotation,
                                "scale": trs.scale,
                            })
                        })
                        .collect();
                    entries.insert(
                        key,
                        json!({
                            "source": skin.source.to_string(),
                            "meshes": vec![mesh_entry],
                            "joints": joints,
                        }),
                    );
                }
                QueryResult::Map(query::MapResult {
                    kind: "skin_data".to_string(),
                    entries,
                })
            }
            EditorQuery::SolveIk {
                end_node,
                target,
                pole,
                root_node,
            } => {
                use serde_json::json;
                // Chain from the renderer's MIRROR hierarchy (bones are scene
                // nodes; mirrors are parented exactly like the scene tree):
                // auto = end → parent (mid) → grandparent (root). When `root_node`
                // is given, root is pinned to it and mid = root's child toward end
                // (§9 — pick the chain when the auto-walk picks wrong, e.g. fingers).
                let (tk_e, tk_root_opt, tk_to_node) = {
                    let b = crate::engine::bridge::bridge();
                    let nodes = b.nodes.lock().unwrap();
                    let Some(entry) = nodes.get(&end_node) else {
                        return QueryResult::Error {
                            error: format!("end_node {end_node} not materialized"),
                        };
                    };
                    let tk_root_opt = match root_node {
                        Some(rn) => match nodes.get(&rn) {
                            Some(re) => Some(re.transform_key),
                            None => {
                                return QueryResult::Error {
                                    error: format!("root_node {rn} not materialized"),
                                }
                            }
                        },
                        None => None,
                    };
                    let map: std::collections::HashMap<_, _> =
                        nodes.iter().map(|(id, n)| (n.transform_key, *id)).collect();
                    (entry.transform_key, tk_root_opt, map)
                };
                let solved = crate::engine::context::with_renderer_mut(move |r| {
                    // Resolve the 2-bone chain (mid, root). Auto: end's parent +
                    // grandparent. Explicit root: walk UP from end to root_node;
                    // mid = the joint just below root on that path.
                    let (tk_m, tk_r) = match tk_root_opt {
                        None => {
                            let tk_m = r.transforms.get_parent(tk_e).ok()?;
                            let tk_r = r.transforms.get_parent(tk_m).ok()?;
                            (tk_m, tk_r)
                        }
                        Some(tk_root) => {
                            let mut cur = tk_e;
                            let mut mid = None;
                            for _ in 0..128 {
                                let p = r.transforms.get_parent(cur).ok()?;
                                if p == tk_root {
                                    mid = Some(cur);
                                    break;
                                }
                                cur = p;
                            }
                            // mid = child of root toward end; None ⇒ root isn't an
                            // ancestor of end. mid==end ⇒ 1-bone (l2 check rejects).
                            (mid?, tk_root)
                        }
                    };
                    let mid_id = tk_to_node.get(&tk_m).copied()?;
                    let root_id = tk_to_node.get(&tk_r).copied()?;
                    let we = *r.transforms.get_world(tk_e).ok()?;
                    let wm = *r.transforms.get_world(tk_m).ok()?;
                    let wr = *r.transforms.get_world(tk_r).ok()?;
                    let wp = r
                        .transforms
                        .get_parent(tk_r)
                        .ok()
                        .and_then(|tk| r.transforms.get_world(tk).ok().copied())
                        .unwrap_or(glam::Mat4::IDENTITY);
                    let (_, q_r, a) = wr.to_scale_rotation_translation();
                    let (_, q_m, b) = wm.to_scale_rotation_translation();
                    let (_, _, c) = we.to_scale_rotation_translation();
                    let (_, q_p, _) = wp.to_scale_rotation_translation();
                    let t = glam::Vec3::from_array(target);

                    let l1 = (b - a).length();
                    let l2 = (c - b).length();
                    if l1 < 1e-5 || l2 < 1e-5 {
                        return None;
                    }
                    let dvec = t - a;
                    let dist = dvec.length();
                    if dist < 1e-5 {
                        return None;
                    }
                    let d = dist.clamp((l1 - l2).abs() + 1e-4, l1 + l2 - 1e-4);
                    let dir_t = dvec / dist;

                    // Bend-plane normal: toward the pole when given, else the
                    // chain's CURRENT bend plane, else character-forward (see
                    // `ik_bend_plane_normal`).
                    let n = ik_bend_plane_normal(
                        a,
                        b,
                        c,
                        dir_t,
                        pole.map(glam::Vec3::from_array),
                        q_p * glam::Vec3::NEG_Z,
                    );

                    // Law of cosines: angle at the root between the reach line
                    // and the upper bone.
                    let cos_a = ((l1 * l1 + d * d - l2 * l2) / (2.0 * l1 * d)).clamp(-1.0, 1.0);
                    let ang_a = cos_a.acos();
                    let dir_ab = glam::Quat::from_axis_angle(n, ang_a) * dir_t;

                    // Sequential rotation-arc deltas → new WORLD rotations.
                    let q_r_new = glam::Quat::from_rotation_arc((b - a).normalize(), dir_ab) * q_r;
                    let b_new = a + dir_ab * l1;
                    let dir_bc_new = (t - b_new).normalize();
                    let q_m_new =
                        glam::Quat::from_rotation_arc((c - b).normalize(), dir_bc_new) * q_m;

                    // World → LOCAL under the (new) parents.
                    let local_r = (q_p.inverse() * q_r_new).normalize();
                    let local_m = (q_r_new.inverse() * q_m_new).normalize();
                    Some((root_id, mid_id, local_r, local_m, d / dist))
                })
                .await;
                match solved {
                    Some((root_id, mid_id, lr, lm, reach)) => {
                        let mut entries = std::collections::BTreeMap::new();
                        entries.insert("root_node".into(), json!(root_id.to_string()));
                        entries.insert("mid_node".into(), json!(mid_id.to_string()));
                        entries.insert("root_rotation".into(), json!(lr.to_array()));
                        entries.insert("mid_rotation".into(), json!(lm.to_array()));
                        entries.insert("reach".into(), json!(reach.min(1.0)));
                        QueryResult::Map(query::MapResult {
                            kind: "ik_solution".to_string(),
                            entries,
                        })
                    }
                    None => QueryResult::Error {
                        error: "ik solve failed: chain not materialized or degenerate \
                                (zero-length bones / target at the root)"
                            .to_string(),
                    },
                }
            }
            EditorQuery::GetSkinWeights { node, indices } => {
                use serde_json::json;
                let meshes = renderer_meshes_for_node(node);
                let data = crate::engine::context::with_renderer_mut(move |r| {
                    let skin_key = meshes
                        .iter()
                        .find_map(|mk| r.meshes.mesh_skin_key(*mk).flatten())?;
                    let sets = r.meshes.skins.sets_len(skin_key).ok()?;
                    let stride = sets * 32;
                    let bytes = r.meshes.skins.read_joint_index_weights(skin_key).ok()?;
                    let vertex_count = bytes.len().checked_div(stride).unwrap_or(0);
                    let want: Vec<u32> = if indices.is_empty() {
                        (0..vertex_count as u32).collect()
                    } else {
                        indices
                    };
                    let mut weights = serde_json::Map::new();
                    for v in want {
                        let vu = v as usize;
                        if vu >= vertex_count {
                            continue;
                        }
                        let off = vu * stride;
                        let mut joints = [0u32; 4];
                        let mut ws = [0f32; 4];
                        for i in 0..4 {
                            let p = off + i * 8;
                            joints[i] = u32::from_le_bytes(bytes[p..p + 4].try_into().unwrap());
                            ws[i] = f32::from_le_bytes(bytes[p + 4..p + 8].try_into().unwrap());
                        }
                        weights.insert(v.to_string(), json!({ "joints": joints, "weights": ws }));
                    }
                    Some((vertex_count, sets, weights))
                })
                .await;
                match data {
                    Some((vertex_count, sets, weights)) => {
                        let mut entries = std::collections::BTreeMap::new();
                        entries.insert("vertex_count".into(), json!(vertex_count));
                        entries.insert("set_count".into(), json!(sets));
                        entries.insert("weights".into(), serde_json::Value::Object(weights));
                        QueryResult::Map(query::MapResult {
                            kind: "skin_weights".to_string(),
                            entries,
                        })
                    }
                    None => QueryResult::Error {
                        error: format!("node {node} has no materialized skin"),
                    },
                }
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
                    awsm_renderer_web_shared::logger::captured_logs(limit as usize)
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
                        let stored = awsm_renderer_editor_protocol::animation::StoredTrack {
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
        // (id, local-aabb, is_collider) tuples from the scene; world matrices from
        // the renderer. `is_collider` strips node scale below — a collider's size
        // is its shape extents, not its transform scale.
        let locals: Vec<(NodeId, Aabb3, bool)> = ids
            .iter()
            .filter_map(|id| {
                mutate::find_by_id(&self.scene, *id).map(|n| {
                    let kind = n.kind.get_cloned();
                    let is_collider = matches!(kind, NodeKind::Collider(_));
                    (*id, local_aabb(&kind), is_collider)
                })
            })
            .collect();
        // Resolve per-node renderer meshes + transform keys BEFORE taking the
        // renderer lock: renderer_meshes_for_node locks the bridge nodes map,
        // which must never nest inside a scope already holding that lock.
        // (id, local-aabb, is_collider, renderer meshes, transform key).
        type Resolved = (
            NodeId,
            Aabb3,
            bool,
            Vec<awsm_renderer::meshes::MeshKey>,
            Option<awsm_renderer::transforms::TransformKey>,
        );
        let resolved: Vec<Resolved> = {
            let bridge = crate::engine::bridge::bridge();
            locals
                .iter()
                .map(|(id, aabb, is_collider)| {
                    let meshes = renderer_meshes_for_node(*id);
                    let tk = bridge
                        .nodes
                        .lock()
                        .unwrap()
                        .get(id)
                        .map(|n| n.transform_key);
                    (*id, *aabb, *is_collider, meshes, tk)
                })
                .collect()
        };
        let entries = crate::engine::context::with_renderer_mut(move |r| {
            let mut m = std::collections::BTreeMap::new();
            for (id, (lmin, lmax), is_collider, meshes, tk) in &resolved {
                let mut world = tk
                    .and_then(|tk| r.transforms.get_world(tk).ok().copied())
                    .unwrap_or(glam::Mat4::IDENTITY);
                // Collider bounds compose the shape's local AABB with the node's
                // world translation + rotation only — scale (the node's own, or an
                // ancestor's) is not part of a Rapier collider. (FIXES.md #1.)
                if *is_collider {
                    let (_s, rot, trans) = world.to_scale_rotation_translation();
                    world = glam::Mat4::from_rotation_translation(rot, trans);
                }
                // §8 facing hint: the node's local axes in WORLD space (see
                // `world_forward_up_right`).
                let (forward, up, right) = world_forward_up_right(world);
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
                let (mn, mx) = match live {
                    Some(aabb) => (aabb.min.to_array(), aabb.max.to_array()),
                    None => transform_aabb(world, *lmin, *lmax),
                };
                m.insert(
                    id.to_string(),
                    json!({
                        "min": mn,
                        "max": mx,
                        "forward": forward,
                        "up": up,
                        "right": right,
                    }),
                );
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
    ///
    /// `compile_pending` covers custom-WGSL compiles AND (via [`CompileGuard`])
    /// every async re-materialization — the kind observer's `apply_kind` (a
    /// patch_kind / assign_material / builtin-variant edit) and the mesh-edit
    /// re-sync — so an edit that re-specializes a mesh's pipeline holds this
    /// barrier until its commit drains. The guard is raised on the microtask
    /// queue (signal delivery), strictly before this function's first 16 ms
    /// timer poll, so there is no observable gap after the triggering command.
    async fn wait_render_settled(&self, max_ms: u32) -> query::QueryResult {
        const INTERVAL_MS: u32 = 16;
        // Wall-clock budget + reporting: a poll can block far longer than the
        // 16 ms interval (its renderer read awaits the lock a re-materialize
        // holds across `commit_load` — that block IS the useful waiting), so
        // counting polls would both under-report `waited_ms` (48 ms reported
        // for a 6 s variant recompile) and overrun `max_ms`.
        let start = js_sys::Date::now();
        let elapsed = || (js_sys::Date::now() - start).max(0.0) as u32;
        let mut stable = 0u32;
        let mut settled = false;
        while elapsed() < max_ms {
            gloo_timers::future::TimeoutFuture::new(INTERVAL_MS).await;
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
            waited_ms: elapsed(),
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

/// Fetch + parse a pre-baked cluster-LOD ("nanite") DAG (`<id>.clusters.bin`, JSON)
/// from a URL — the `awsm-renderer-lod-bake` CLI output the `ImportNaniteAsset` command brings
/// into the editor as a view-only [`NodeKind::ClusterMesh`].
async fn fetch_cluster_mesh(url: &str) -> Result<awsm_renderer_lod_bake::ClusterMesh, String> {
    let resp = gloo_net::http::Request::get(url)
        .send()
        .await
        .map_err(|e| format!("fetch {url}: {e}"))?;
    if !resp.ok() {
        return Err(format!("fetch {url}: HTTP {}", resp.status()));
    }
    let bytes = resp
        .binary()
        .await
        .map_err(|e| format!("read {url}: {e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("parse {url}: {e}"))
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
/// AABB of a set of local-space positions, or `None` when empty.
fn aabb_from_positions(positions: &[[f32; 3]]) -> Option<Aabb3> {
    if positions.is_empty() {
        return None;
    }
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for p in positions {
        for i in 0..3 {
            min[i] = min[i].min(p[i]);
            max[i] = max[i].max(p[i]);
        }
    }
    Some((min, max))
}

fn local_aabb(kind: &NodeKind) -> Aabb3 {
    match kind {
        // A Mesh's true bounds come from its baked geometry in the mesh cache;
        // every procedural node (box / sphere / sweep / …) is a Mesh now.
        NodeKind::Mesh { mesh, .. } => {
            if let Some(raw) = crate::engine::bridge::mesh_cache::get_raw(mesh.0) {
                if let Some(b) = aabb_from_positions(&raw.positions) {
                    return b;
                }
            }
        }
        // A SkinnedMesh's bounds come from its bind-pose bake (cached at import +
        // persisted across reload), keyed by the same (source, node, primitive)
        // triple `drop_skinning` uses — so `frame_node` centers an imported
        // character instead of a unit box at its origin.
        NodeKind::SkinnedMesh { skin, .. } => {
            if let Some(md) = crate::engine::bridge::skinned_bake_cache::get(
                skin.source,
                skin.node_index,
                skin.primitive_index,
            ) {
                if let Some(b) = aabb_from_positions(&md.positions) {
                    return b;
                }
            }
        }
        // A collider's bounds come from its `ColliderShape` extents — never from
        // node scale (which a collider doesn't have; see `ColliderSpec::from_node`).
        // The shapes are centered on the node origin and Capsule/Cylinder/Cone are
        // Y-aligned in their local frame. (FIXES.md #1.)
        NodeKind::Collider(shape) => return collider_local_aabb(shape),
        // An explicit instancer's bounds span ALL its authored instances — the
        // def is self-contained (it owns the transform list), so the union of
        // (instance transform × instanced-mesh AABB) is computable right here.
        // Without this the fallback was a unit box, and framing / NodeBounds on
        // a spread-out instancer under-measured whenever the renderer's live
        // world AABB (which already unions instances) wasn't available.
        // (`InstancesAlongCurve` still falls through to the unit box: its
        // placement derives from ANOTHER node's curve, which this kind-only
        // helper can't see — its live renderer AABB is exact, though.)
        NodeKind::Instancer(def) => {
            let base = crate::engine::bridge::mesh_cache::get_raw(def.mesh.0)
                .and_then(|raw| aabb_from_positions(&raw.positions))
                .unwrap_or(([-0.5, -0.5, -0.5], [0.5, 0.5, 0.5]));
            return instancer_local_aabb(base, &def.transforms);
        }
        _ => {}
    }
    // Lights / cameras / empties / un-baked meshes: a small unit box centered on
    // the node.
    ([-0.5, -0.5, -0.5], [0.5, 0.5, 0.5])
}

/// Local-space AABB of a collider shape, in the collider's own frame (centered
/// on the node origin). Pair with the node's world translation + rotation — but
/// NOT its scale — to get world bounds. Mirrors the dimensions the wireframe
/// (`collider_wire`) and runtime (`ColliderSpec`) read.
fn collider_local_aabb(shape: &ColliderShape) -> Aabb3 {
    let half = |hx: f32, hy: f32, hz: f32| ([-hx, -hy, -hz], [hx, hy, hz]);
    match shape {
        ColliderShape::Box { half_extents } | ColliderShape::Ellipsoid { half_extents } => {
            half(half_extents[0], half_extents[1], half_extents[2])
        }
        ColliderShape::Sphere { radius } => half(*radius, *radius, *radius),
        // Capsule total half-height = half_height + radius; girth = radius on X/Z.
        ColliderShape::Capsule {
            half_height,
            radius,
        } => half(*radius, *half_height + *radius, *radius),
        // Cylinder / Cone span `half_height` on Y, `radius` on X/Z.
        ColliderShape::Cylinder {
            half_height,
            radius,
        }
        | ColliderShape::Cone {
            half_height,
            radius,
        } => half(*radius, *half_height, *radius),
    }
}

/// Node-local AABB of an explicit instancer: the union of
/// (instance transform × `base`) over every authored instance transform, where
/// `base` is the instanced mesh's local AABB. Instance transforms are relative
/// to the instancer node, so the union lives in the node's local frame — the
/// caller composes it with the node's world matrix like any other local AABB.
/// An empty transform list yields `base` unchanged (the "renders nothing"
/// authored state still frames as a unit-ish box at the node). Pure, so it is
/// unit-tested natively (see `instancer_aabb_tests`).
fn instancer_local_aabb(base: Aabb3, transforms: &[awsm_renderer_editor_protocol::Trs]) -> Aabb3 {
    let (bmin, bmax) = base;
    let mut acc: Option<Aabb3> = None;
    for trs in transforms {
        let m = glam::Mat4::from_scale_rotation_translation(
            glam::Vec3::from_array(trs.scale),
            glam::Quat::from_array(trs.rotation),
            glam::Vec3::from_array(trs.translation),
        );
        let (tmin, tmax) = transform_aabb(m, bmin, bmax);
        acc = Some(match acc {
            None => (tmin, tmax),
            Some((amin, amax)) => (
                [
                    amin[0].min(tmin[0]),
                    amin[1].min(tmin[1]),
                    amin[2].min(tmin[2]),
                ],
                [
                    amax[0].max(tmax[0]),
                    amax[1].max(tmax[1]),
                    amax[2].max(tmax[2]),
                ],
            ),
        });
    }
    acc.unwrap_or(base)
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

/// The LIVE value for an animation track target — what key-from-pose captures.
/// Transform targets read the editor node's authored `Trs` signal (the value
/// the gizmo + inspector write); everything else reads CPU-side renderer /
/// scene state through the same `read_readback_target` the timeseries query
/// uses. `None` when the target can't currently resolve — callers fall back
/// to sampling the track's own curve (the pre-key-from-pose behavior).
#[cfg_attr(test, allow(dead_code))] // caller (animation_mode UI) isn't built under cfg(test)
/// The two-bone-IK bend-plane normal — which SIDE the knee/elbow goes.
///
/// Priority (DCC semantics):
/// 1. **Pole given**: the plane through the reach line and the pole — the
///    joint bends toward the pole (`n = dir_t × (pole − a)`; Rodrigues with
///    `n ⊥ dir_t` rotates `dir_t` toward `n × dir_t`, which is the pole's
///    perpendicular component).
/// 2. **Chain already bent**: keep the CURRENT bend plane so the joint stays
///    on the side it's already on — normal `(c − b) × (b − a)`, sign-matched
///    to the rotate-toward geometry, orthogonalized against `dir_t`. "Bent"
///    means the sine of the joint angle clears `1e-3` (scale-free).
/// 3. **Straight chain**: bias to `forward` (the chain-root parent's −Z — the
///    character's facing, so a standing leg bends its knee FORWARD). The old
///    world-Y fallback was ~parallel to a downward reach and cascaded to
///    world-X: kicking the knee sideways.
/// 4. Degenerate forward (reach ∥ forward): world Y, then X.
///
/// `a`/`b`/`c` are the root/mid/end joint world positions, `dir_t` the
/// normalized root→target direction. Returns a normalized vector ⊥ `dir_t`.
fn ik_bend_plane_normal(
    a: glam::Vec3,
    b: glam::Vec3,
    c: glam::Vec3,
    dir_t: glam::Vec3,
    pole: Option<glam::Vec3>,
    forward: glam::Vec3,
) -> glam::Vec3 {
    if let Some(p) = pole {
        let n = dir_t.cross(p - a);
        if n.length_squared() > 1e-8 {
            return n.normalize();
        }
        // Pole on the reach line — fall through to the heuristics.
    }
    let l1 = (b - a).length();
    let l2 = (c - b).length();
    // Current bend plane, scale-free bent test: |(c−b)×(b−a)| = l1·l2·sin θ.
    let chain_n = (c - b).cross(b - a);
    if chain_n.length_squared() > (l1 * l2 * 1e-3).powi(2) {
        // Orthogonalize against the reach line (projection keeps the sign,
        // so the joint stays on its current side).
        let n = chain_n - dir_t * chain_n.dot(dir_t);
        if n.length_squared() > 1e-8 {
            return n.normalize();
        }
    }
    // Straight chain → character-forward, then world Y, then X.
    for f in [forward, glam::Vec3::Y, glam::Vec3::X] {
        let n = dir_t.cross(f);
        if n.length_squared() > 1e-8 {
            return n.normalize();
        }
    }
    glam::Vec3::X // unreachable in practice (dir_t can't be ∥ Y and X)
}

pub(crate) async fn live_track_value(
    ctrl: &EditorController,
    target: &TrackTarget,
) -> Option<TrackValue> {
    use crate::controller::animation::TransformProp;
    // Transform: the editor signal IS the live pose — sync, no renderer.
    if let TrackTarget::Transform { node, prop } = target {
        let n = mutate::find_by_id(&ctrl.scene, *node)?;
        let trs = n.transform.get();
        return Some(match prop {
            TransformProp::Translation => TrackValue::Vec3(trs.translation),
            TransformProp::Rotation => TrackValue::Quat(trs.rotation),
            TransformProp::Scale => TrackValue::Vec3(trs.scale),
        });
    }
    // Everything else: route through the readback machinery.
    use query::ReadbackTarget as R;
    let rt = match target.clone() {
        TrackTarget::Transform { .. } => unreachable!("handled above"),
        TrackTarget::Morph { node, index } => R::MorphWeight { node, index },
        TrackTarget::Uniform { material, name } => R::Uniform { material, name },
        TrackTarget::BuiltinParam { node, param } => R::BuiltinParam { node, param },
        TrackTarget::Light { node, param } => R::LightParam { node, param },
        TrackTarget::Camera { node, param } => R::CameraParam { node, param },
        // No readback target for a texture UV transform yet — the keyframe seeds
        // from `default_value_for` (zero offset / unit scale / 0 rotation) instead.
        TrackTarget::TextureTransform { .. } => return None,
    };
    let v = crate::engine::context::with_renderer_mut(move |r| read_readback_target(r, &rt)).await;
    // Shape the JSON by the track's expected kind (vec3 / quat / scalar).
    let expected = crate::controller::animation::default_value_for(target);
    match (expected, v) {
        (TrackValue::Scalar(_), serde_json::Value::Number(n)) => {
            Some(TrackValue::Scalar(n.as_f64()? as f32))
        }
        (TrackValue::Vec2(_), serde_json::Value::Array(a)) if a.len() >= 2 => {
            let mut out = [0.0f32; 2];
            for (i, x) in a.iter().take(2).enumerate() {
                out[i] = x.as_f64()? as f32;
            }
            Some(TrackValue::Vec2(out))
        }
        (TrackValue::Vec3(_), serde_json::Value::Array(a)) if a.len() >= 3 => {
            let mut out = [0.0f32; 3];
            for (i, x) in a.iter().take(3).enumerate() {
                out[i] = x.as_f64()? as f32;
            }
            Some(TrackValue::Vec3(out))
        }
        (TrackValue::Vec4(_), serde_json::Value::Array(a)) if a.len() >= 4 => {
            let mut out = [0.0f32; 4];
            for (i, x) in a.iter().take(4).enumerate() {
                out[i] = x.as_f64()? as f32;
            }
            Some(TrackValue::Vec4(out))
        }
        (TrackValue::Quat(_), serde_json::Value::Array(a)) if a.len() >= 4 => {
            let mut out = [0.0f32; 4];
            for (i, x) in a.iter().take(4).enumerate() {
                out[i] = x.as_f64()? as f32;
            }
            Some(TrackValue::Quat(out))
        }
        _ => None,
    }
}

/// Renderer mesh keys for a node, covering BOTH materialization paths: a
/// captured/editable node's own `model_meshes`, or — when that's empty — a
/// `SkinnedMesh` node's populate-baked keys resolved through the import
/// template (those keys are template-owned and deliberately never pushed to
/// `model_meshes`; see `materialize_skinned_mesh`). Morph-bearing imports ride
/// the SkinnedMesh path, so any morph resolution MUST use this, not
/// `model_meshes` alone. Empty when the node isn't materialized.
pub(crate) fn renderer_meshes_for_node(node: NodeId) -> Vec<awsm_renderer::meshes::MeshKey> {
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
            use awsm_renderer::materials::Material;
            use awsm_renderer_materials::dynamic_layout::UniformValue;
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
                P::SsrMask => match m {
                    Material::Pbr(p) => json!(p.ssr_mask),
                    _ => serde_json::Value::Null,
                },
                P::Roughness => match m {
                    Material::Pbr(p) => json!(p.roughness_factor),
                    _ => serde_json::Value::Null,
                },
                P::NormalScale => match m {
                    Material::Pbr(p) => json!(p.normal_scale),
                    _ => serde_json::Value::Null,
                },
                P::OcclusionStrength => match m {
                    Material::Pbr(p) => json!(p.occlusion_strength),
                    _ => serde_json::Value::Null,
                },
                P::EmissiveStrength => match m {
                    Material::Pbr(p) => {
                        json!(p
                            .emissive_strength
                            .as_ref()
                            .map(|e| e.strength)
                            .unwrap_or(1.0))
                    }
                    _ => serde_json::Value::Null,
                },
                P::AlphaCutoff => match m {
                    Material::Pbr(p) => json!(p.alpha_cutoff().unwrap_or(0.5)),
                    _ => serde_json::Value::Null,
                },
                P::ToonDiffuseBands => match m {
                    Material::Toon(t) => json!(t.diffuse_bands as f32),
                    _ => serde_json::Value::Null,
                },
                P::ToonSpecularSteps => match m {
                    Material::Toon(t) => json!(t.specular_steps as f32),
                    _ => serde_json::Value::Null,
                },
                P::ToonShininess => match m {
                    Material::Toon(t) => json!(t.shininess),
                    _ => serde_json::Value::Null,
                },
                P::ToonRimStrength => match m {
                    Material::Toon(t) => json!(t.rim_strength),
                    _ => serde_json::Value::Null,
                },
                P::ToonRimPower => match m {
                    Material::Toon(t) => json!(t.rim_power),
                    _ => serde_json::Value::Null,
                },
                P::FlipbookFps => match m {
                    Material::FlipBook(f) => json!(f.fps),
                    _ => serde_json::Value::Null,
                },
                P::FlipbookTimeOffset => match m {
                    Material::FlipBook(f) => json!(f.time_offset),
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
/// [`awsm_renderer_editor_protocol::CustomAlphaMode`] (folds in the mask cutoff).
fn custom_alpha_of(mat: &Arc<CM>) -> awsm_renderer_editor_protocol::CustomAlphaMode {
    use awsm_renderer_editor_protocol::CustomAlphaMode as M;
    match mat.alpha.get() {
        AlphaMode::Opaque => M::Opaque,
        AlphaMode::Mask => M::Mask {
            cutoff: mat.cutoff.get(),
        },
        AlphaMode::Blend => M::Blend,
    }
}

/// Project the editor's live `Slot`s into serializable `SlotSpec`s (and back).
fn slots_to_specs(slots: &[Slot]) -> Vec<awsm_renderer_editor_protocol::SlotSpec> {
    slots
        .iter()
        .map(|s| awsm_renderer_editor_protocol::SlotSpec {
            name: s.name.clone(),
            ty: s.ty.clone(),
            val: s.val.clone(),
            debug: s.debug.clone(),
            color_kind: s.color_kind,
        })
        .collect()
}

fn specs_to_slots(specs: &[awsm_renderer_editor_protocol::SlotSpec]) -> Vec<Slot> {
    specs
        .iter()
        .map(|s| Slot {
            name: s.name.clone(),
            ty: s.ty.clone(),
            val: s.val.clone(),
            debug: s.debug.clone(),
            color_kind: s.color_kind,
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
) -> Option<&awsm_renderer_editor_protocol::dynamic_material::MaterialInstance> {
    kind.selected_material()
}

thread_local! {
    /// §10 session-scoped vertex-selection store. `select_vertices_where { store }`
    /// puts a predicate's indices here under a fresh id and returns just
    /// `{ id, count }`; the paint/sculpt commands resolve `selection: <id>` back to
    /// the indices, so a full-resolution selection (tens of thousands of indices)
    /// drives many ops without ever crossing the MCP boundary. Cleared on reload
    /// (session-scoped, like the texture-key cache).
    static VERTEX_SELECTIONS: RefCell<std::collections::HashMap<u32, Vec<u32>>> =
        RefCell::new(std::collections::HashMap::new());
    static NEXT_VERTEX_SELECTION_ID: Cell<u32> = const { Cell::new(1) };
}

/// Store an index list and return its handle id (§10).
fn store_vertex_selection(indices: Vec<u32>) -> u32 {
    let id = NEXT_VERTEX_SELECTION_ID.with(|c| {
        let id = c.get();
        c.set(id.wrapping_add(1).max(1));
        id
    });
    VERTEX_SELECTIONS.with(|m| m.borrow_mut().insert(id, indices));
    id
}

/// The indices a selection handle holds, if it exists (§10).
fn lookup_vertex_selection(id: u32) -> Option<Vec<u32>> {
    VERTEX_SELECTIONS.with(|m| m.borrow().get(&id).cloned())
}

/// Sample an RGBA8 heightmap's perceptual luminance in `[0, 1]` at normalized
/// `(u, v)` (nearest, UV wraps) — the §16 displace-from-texture height source.
fn sample_heightmap_luminance(rgba: &[u8], w: u32, h: u32, u: f32, v: f32) -> f32 {
    if w == 0 || h == 0 || rgba.len() < (w * h * 4) as usize {
        return 0.0;
    }
    let x = ((u.rem_euclid(1.0)) * w as f32).floor().min(w as f32 - 1.0) as u32;
    let y = ((v.rem_euclid(1.0)) * h as f32).floor().min(h as f32 - 1.0) as u32;
    let o = ((y * w + x) * 4) as usize;
    let (r, g, b) = (rgba[o] as f32, rgba[o + 1] as f32, rgba[o + 2] as f32);
    (0.299 * r + 0.587 * g + 0.114 * b) / 255.0
}

/// Resolve a paint/sculpt command's target indices: a `selection` handle wins
/// over an explicit `indices` list; a dangling handle errors loudly (§10).
fn resolve_vertex_selection_or(
    selection: Option<u32>,
    indices: Vec<u32>,
) -> EditorResult<Vec<u32>> {
    match selection {
        Some(id) => lookup_vertex_selection(id).ok_or_else(|| {
            crate::error::EditorError::msg(format!(
                "no vertex-selection handle {id} (create one with select_vertices_where {{ store: true }})"
            ))
        }),
        None => Ok(indices),
    }
}

/// `resolve_node_material` kind for a node carrying **no** material (§5):
/// renderable geometry (Mesh / SkinnedMesh) is `"unassigned"` — it renders the
/// flat **magenta** missing-material sentinel (a visible "you forgot to assign",
/// NOT invisible); anything else is `"none"` (not a geometry node). The magenta
/// render itself lives in `node_sync::resolve_assigned_material` (`None` →
/// `insert_magenta`).
fn unassigned_material_kind(kind: &NodeKind) -> &'static str {
    if matches!(kind, NodeKind::Mesh { .. } | NodeKind::SkinnedMesh { .. }) {
        "unassigned"
    } else {
        "none"
    }
}

/// Select the vertex indices of `mesh` matching `predicate` — shared by the
/// `SelectVerticesWhere` query and the fused `PaintVerticesWhere` /
/// `TransformVerticesWhere` commands (§10), so a full-res selection can be acted
/// on server-side without round-tripping the (huge) index array through MCP.
fn select_vertices_by_predicate(
    mesh: &awsm_renderer_meshgen::MeshData,
    predicate: &awsm_renderer_editor_protocol::VertexPredicate,
) -> Vec<u32> {
    use awsm_renderer_editor_protocol::VertexPredicate as P;
    use awsm_renderer_meshgen::edit::{
        connected_component_of, select_by_axis, select_by_normal_dir, select_top_count_axis,
        select_top_percent_axis, select_within_aabb, select_within_radius, Cmp,
    };
    match predicate {
        P::ConnectedToSeed { seed } => connected_component_of(mesh, seed),
        P::NormalDir { dir, threshold } => select_by_normal_dir(mesh, *dir, *threshold),
        P::AxisGreater { axis, value } => {
            select_by_axis(mesh, *axis as usize, Cmp::Greater, *value)
        }
        P::AxisLess { axis, value } => select_by_axis(mesh, *axis as usize, Cmp::Less, *value),
        P::TopPercent { axis, percent } => {
            if !(0.0..=1.0).contains(percent) {
                // percent is a 0..1 FRACTION; out-of-range input silently clamps
                // in the selector, which reads as "selected everything" to a
                // confused caller.
                tracing::warn!(
                    "select_vertices_where top_percent: percent {percent} is outside 0..1 \
                     (it is a fraction, not a percentage) — clamping"
                );
            }
            select_top_percent_axis(mesh, *axis as usize, *percent)
        }
        P::TopCount { axis, count } => select_top_count_axis(mesh, *axis as usize, *count),
        P::WithinRadius { center, radius } => select_within_radius(mesh, *center, *radius),
        P::WithinAabb { min, max } => select_within_aabb(mesh, *min, *max),
    }
}

/// The node's local axes expressed in WORLD space, derived from its world matrix
/// (§8 facing hint): `(forward, up, right)` where `forward` is local **-Z** (the
/// project's "-Z forward" convention), `up` is local +Y, `right` is local +X —
/// each a unit vector. Lets an agent place things relative to a node's
/// orientation ("on the back" = `-forward`) without trial-and-error. This is the
/// node's TRANSFORM orientation; an imported model's *geometry* may face a
/// different way (the convention; verify visually).
fn world_forward_up_right(world: glam::Mat4) -> ([f32; 3], [f32; 3], [f32; 3]) {
    (
        (-world.z_axis.truncate()).normalize_or_zero().to_array(),
        world.y_axis.truncate().normalize_or_zero().to_array(),
        world.x_axis.truncate().normalize_or_zero().to_array(),
    )
}

/// Lightweight `{ id, name, kind }` for a node (no per-kind config blob) — the
/// row shape `get_children` / `get_subtree` return (§6).
fn node_brief(node: &crate::engine::scene::node::Node) -> serde_json::Value {
    serde_json::json!({
        "id": node.id.to_string(),
        "name": node.name.get_cloned(),
        "kind": awsm_renderer_editor_protocol::kind_tag(&node.kind.get_cloned()),
    })
}

/// `node_brief` plus a nested `children` array — the recursive subtree shape
/// `get_subtree` returns (§6).
fn node_subtree_json(node: &crate::engine::scene::node::Node) -> serde_json::Value {
    let children: Vec<serde_json::Value> = node
        .children
        .lock_ref()
        .iter()
        .map(|c| node_subtree_json(c))
        .collect();
    serde_json::json!({
        "id": node.id.to_string(),
        "name": node.name.get_cloned(),
        "kind": awsm_renderer_editor_protocol::kind_tag(&node.kind.get_cloned()),
        "children": children,
    })
}

/// Mutable variant of [`node_material_ref`].
fn node_material_mut(
    kind: &mut NodeKind,
) -> Option<&mut awsm_renderer_editor_protocol::dynamic_material::MaterialInstance> {
    kind.selected_material_mut()
}

/// Patch a built-in material factor on a node's per-mesh inline store. Returns
/// false if the node is unassigned (nothing to tweak on a magenta node) or
/// `value` is too short.
fn patch_builtin_param(
    kind: &mut NodeKind,
    param: awsm_renderer_editor_protocol::animation::BuiltinParamKind,
    value: &[f32],
) -> bool {
    use awsm_renderer_editor_protocol::animation::BuiltinParamKind as P;
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
            // §13: accept an optional 4th float as the base-color ALPHA (for a
            // sub-1 alpha on a Blend material — glass). 3 floats leaves alpha as-is.
            if let Some(&a) = value.get(3) {
                inline.base_color[3] = a;
            }
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
        P::NormalScale => match value.first() {
            Some(&v) => inline.normal_scale = v,
            None => return false,
        },
        P::OcclusionStrength => match value.first() {
            Some(&v) => inline.occlusion_strength = v,
            None => return false,
        },
        P::SsrMask => match value.first() {
            Some(&v) => inline.ssr_mask = v.clamp(0.0, 1.0),
            None => return false,
        },
        P::EmissiveStrength => match value.first() {
            // Enables the `KHR_materials_emissive_strength` extension on first set
            // (flips the feature → recompiles on re-register, like the material
            // studio's emissive-strength toggle), then writes the multiplier.
            Some(&v) => {
                inline
                    .extensions
                    .emissive_strength
                    .get_or_insert_with(Default::default)
                    .strength = v;
            }
            None => return false,
        },
        P::AlphaCutoff => match value.first() {
            // Only meaningful on a `Mask` material — the alpha MODE is a pipeline
            // choice set elsewhere; here we just tune the threshold (no-op otherwise).
            Some(&v) => {
                if let awsm_renderer_editor_protocol::MaterialAlphaMode::Mask { cutoff } =
                    &mut inline.alpha_mode
                {
                    *cutoff = v;
                }
            }
            None => return false,
        },
        // Toon / FlipBook knobs live inside the `shading` variant — tune them only
        // when the material is that kind (no-op otherwise).
        P::ToonDiffuseBands
        | P::ToonSpecularSteps
        | P::ToonShininess
        | P::ToonRimStrength
        | P::ToonRimPower
        | P::FlipbookFps
        | P::FlipbookTimeOffset => {
            use awsm_renderer_editor_protocol::MaterialShading as S;
            let Some(&v) = value.first() else {
                return false;
            };
            let count = (v.round() as i64).max(1) as u32;
            match (&mut inline.shading, param) {
                (S::Toon { diffuse_bands, .. }, P::ToonDiffuseBands) => *diffuse_bands = count,
                (S::Toon { specular_steps, .. }, P::ToonSpecularSteps) => *specular_steps = count,
                (S::Toon { shininess, .. }, P::ToonShininess) => *shininess = v,
                (S::Toon { rim_strength, .. }, P::ToonRimStrength) => *rim_strength = v,
                (S::Toon { rim_power, .. }, P::ToonRimPower) => *rim_power = v,
                (S::FlipBook { fps, .. }, P::FlipbookFps) => *fps = v,
                (S::FlipBook { time_offset, .. }, P::FlipbookTimeOffset) => *time_offset = v,
                _ => {} // material isn't the matching kind: no-op
            }
        }
    }
    true
}

/// Bind (or clear) a texture on a node's **built-in/inline** `MaterialDef` slot.
/// Returns false if the node is unassigned (no inline store to tweak).
fn patch_builtin_texture(
    kind: &mut NodeKind,
    slot: awsm_renderer_editor_protocol::BuiltinTextureSlot,
    texture: Option<AssetId>,
) -> bool {
    use awsm_renderer_editor_protocol::BuiltinTextureSlot as S;
    let Some(inst) = node_material_mut(kind) else {
        return false;
    };
    let inline = &mut inst.inline;
    let tref = texture.map(awsm_renderer_editor_protocol::TextureRef::new);
    match slot {
        S::BaseColor => inline.base_color_texture = tref,
        S::MetallicRoughness => inline.metallic_roughness_texture = tref,
        S::Normal => inline.normal_texture = tref,
        S::Occlusion => inline.occlusion_texture = tref,
        S::Emissive => inline.emissive_texture = tref,
    }
    true
}

/// Patch the UV transform / flow / sampler-wrap of a node's **built-in/inline**
/// `MaterialDef` texture slot (§1). Patch semantics — only provided fields
/// change. Returns `Err` (caller surfaces it as an MCP error) when there's no
/// inline material or the slot has no texture bound, so the op is never a silent
/// no-op (the original §1 trap).
#[allow(clippy::too_many_arguments)]
fn patch_builtin_texture_transform(
    kind: &mut NodeKind,
    slot: awsm_renderer_editor_protocol::BuiltinTextureSlot,
    offset: Option<[f32; 2]>,
    scale: Option<[f32; 2]>,
    rotation: Option<f32>,
    flow: Option<[f32; 2]>,
    wrap_u: Option<awsm_renderer_editor_protocol::TextureWrap>,
    wrap_v: Option<awsm_renderer_editor_protocol::TextureWrap>,
    uv_set: Option<u32>,
    mag_filter: Option<awsm_renderer_editor_protocol::TextureFilter>,
    min_filter: Option<awsm_renderer_editor_protocol::TextureFilter>,
    mipmap_filter: Option<awsm_renderer_editor_protocol::TextureFilter>,
) -> Result<(), String> {
    use awsm_renderer_editor_protocol::BuiltinTextureSlot as S;
    let Some(inst) = node_material_mut(kind) else {
        return Err(
            "node has no built-in material — assign one and bind a texture first".to_string(),
        );
    };
    let inline = &mut inst.inline;
    let tref = match slot {
        S::BaseColor => &mut inline.base_color_texture,
        S::MetallicRoughness => &mut inline.metallic_roughness_texture,
        S::Normal => &mut inline.normal_texture,
        S::Occlusion => &mut inline.occlusion_texture,
        S::Emissive => &mut inline.emissive_texture,
    };
    let Some(tref) = tref.as_mut() else {
        return Err(format!(
            "texture slot `{slot:?}` has no texture bound — bind one with set_node_texture first"
        ));
    };
    // Affine transform: touch it only when an affine field is supplied; seed an
    // identity transform first so a partial patch (e.g. offset only) keeps scale 1.
    if offset.is_some() || scale.is_some() || rotation.is_some() {
        let t = tref
            .transform
            .get_or_insert_with(awsm_renderer_editor_protocol::TextureTransform::default);
        if let Some(o) = offset {
            t.offset = o;
        }
        if let Some(s) = scale {
            t.scale = s;
        }
        if let Some(r) = rotation {
            t.rotation = r;
        }
    }
    if let Some(f) = flow {
        tref.flow = Some(f);
    }
    if wrap_u.is_some()
        || wrap_v.is_some()
        || mag_filter.is_some()
        || min_filter.is_some()
        || mipmap_filter.is_some()
    {
        let s = tref.sampler.get_or_insert_with(Default::default);
        if let Some(w) = wrap_u {
            s.wrap_u = w;
        }
        if let Some(w) = wrap_v {
            s.wrap_v = w;
        }
        if let Some(f) = mag_filter {
            s.mag_filter = f;
        }
        if let Some(f) = min_filter {
            s.min_filter = f;
        }
        if let Some(f) = mipmap_filter {
            s.mipmap_filter = f;
        }
    }
    if let Some(uv) = uv_set {
        tref.uv_index = uv;
    }
    Ok(())
}

/// Set (or clear) the per-USE bundle-export profile on a node's texture slot
/// ref: built-in slot names first, else a custom-material texture override
/// slot (docs/plans/compression.md F2). `Err` when no material is assigned or
/// nothing is bound at the slot — never a silent no-op.
fn patch_texture_use_profile(
    kind: &mut NodeKind,
    slot: &str,
    profile: Option<awsm_renderer_editor_protocol::TextureUseProfile>,
) -> Result<(), String> {
    let Some(inst) = node_material_mut(kind) else {
        return Err(
            "node has no material assigned — assign one and bind a texture first".to_string(),
        );
    };
    let tref = match slot {
        "base_color" => inst.inline.base_color_texture.as_mut(),
        "metallic_roughness" => inst.inline.metallic_roughness_texture.as_mut(),
        "normal" => inst.inline.normal_texture.as_mut(),
        "occlusion" => inst.inline.occlusion_texture.as_mut(),
        "emissive" => inst.inline.emissive_texture.as_mut(),
        custom => inst.texture_overrides.get_mut(custom),
    };
    match tref {
        Some(t) => {
            t.export_profile = profile;
            Ok(())
        }
        None => Err(format!(
            "texture slot `{slot}` has no texture bound — bind one first \
             (set_node_texture / set_material_texture)"
        )),
    }
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
                awsm_renderer_editor_protocol::TextureRef::new(asset),
            );
        }
        None => {
            inst.texture_overrides.remove(slot);
        }
    }
    true
}

/// Bind (or clear) a buffer-data override on a node's assigned custom material.
/// The `data` words are interned as a content-addressed [`AssetSource::Buffer`]
/// asset (see [`intern_buffer_asset`]) and referenced by id — so the binding
/// persists across Save/reload exactly like a texture override. Returns false if
/// the node has no custom-material instance.
fn patch_material_buffer(kind: &mut NodeKind, slot: &str, data: Option<Vec<u32>>) -> bool {
    let Some(inst) = node_material_mut(kind) else {
        return false;
    };
    match data {
        Some(words) => {
            let asset = intern_buffer_asset(words);
            inst.buffer_overrides.insert(
                slot.to_string(),
                awsm_renderer_editor_protocol::dynamic_material::BufferRef { asset },
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
    cfg: &mut awsm_renderer_editor_protocol::LightConfig,
    param: awsm_renderer_editor_protocol::animation::LightParamKind,
    value: &[f32],
) -> bool {
    use awsm_renderer_editor_protocol::animation::LightParamKind as P;
    use awsm_renderer_editor_protocol::LightConfig as L;
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

/// RAII [`compile_begin`]/[`compile_end`] pair. Holds the `WaitRenderSettled`
/// barrier open for the guard's lifetime — used around async re-materialization
/// (the kind observer's `apply_kind`, mesh re-sync) so a settled-wait issued
/// right after the triggering command can't slip through the gap before the
/// renderer's own compile counters become visible. Drop-based so a CANCELLED
/// re-materialization (node deleted mid-apply tears the observer down) still
/// releases the barrier instead of wedging `compile_pending` above zero.
pub(crate) struct CompileGuard;

impl CompileGuard {
    pub(crate) fn new() -> Self {
        compile_begin();
        Self
    }
}

impl Drop for CompileGuard {
    fn drop(&mut self) {
        compile_end();
    }
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
async fn register_material(mat: &Arc<CM>) -> bool {
    // A debounced register that lost the race with a delete (create→edit→delete
    // faster than the ~400ms debounce) must not re-register the deleted material —
    // it would leak its GPU pipelines forever (sub-second-churn "aw snap" tail).
    if crate::engine::bridge::dynamic::is_deleted(mat.id) {
        return false;
    }
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
            // skips synchronous shader validation), and that compile is of the
            // SHARED kernel — a WGSL error in the author body (undefined symbol,
            // type mismatch, …) fails the kernel without ever attributing back to
            // this material, so the old scheduler poll reported a silent `ok`
            // (D2b). Instead validate the assembled kernel with `naga` SYNCHRONOUSLY
            // here (the same front-end Tint mirrors for these classes) and report
            // the truth. Validation-only — it never gates a frame.
            let errors = crate::engine::context::with_renderer_mut(move |r| {
                r.validate_dynamic_material_wgsl(shader_id)
            })
            .await;
            if !errors.is_empty() {
                Toast::error(format!("Material compile failed:\n{}", errors.join("\n")));
                mat.last_diagnostics.set(
                    errors
                        .into_iter()
                        // naga line numbers index the ASSEMBLED module, not the
                        // author's snippet, so omit them (the message — e.g.
                        // "unresolved value 'foo'" — is the actionable part).
                        .map(|message| query::CompileError {
                            line: None,
                            message,
                        })
                        .collect(),
                );
                mat.registered.set_neq(false);
                return false;
            }
            mat.last_diagnostics.set(Vec::new());
            mat.registered.set_neq(true);
            crate::engine::bridge::rematerialize_for_material(mat.id);
            true
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

/// Whether the extension owning the `"<ext>.<field>"` slot is ENABLED
/// (`Some`) — the slot-visibility gate, distinct from "has a texture bound"
/// ([`get_ext_texture`] conflates the two: it returns `None` both for a
/// disabled extension and for an enabled one with no texture yet).
pub(crate) fn ext_slot_enabled(
    ext: &awsm_renderer_editor_protocol::PbrExtensions,
    slot: &str,
) -> bool {
    match slot.split('.').next().unwrap_or("") {
        "specular" => ext.specular.is_some(),
        "transmission" => ext.transmission.is_some(),
        "diffuse_transmission" => ext.diffuse_transmission.is_some(),
        "volume" => ext.volume.is_some(),
        "clearcoat" => ext.clearcoat.is_some(),
        "sheen" => ext.sheen.is_some(),
        "anisotropy" => ext.anisotropy.is_some(),
        "iridescence" => ext.iridescence.is_some(),
        _ => false,
    }
}

/// Read the `TextureRef` at an extension texture slot, keyed `"<ext>.<field>"`.
pub(crate) fn get_ext_texture(
    ext: &awsm_renderer_editor_protocol::PbrExtensions,
    slot: &str,
) -> Option<awsm_renderer_editor_protocol::TextureRef> {
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
    ext: &mut awsm_renderer_editor_protocol::PbrExtensions,
    slot: &str,
    tref: Option<awsm_renderer_editor_protocol::TextureRef>,
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
#[allow(clippy::type_complexity)]
fn ensure_import_texture(
    tex_for_key: &mut std::collections::HashMap<awsm_renderer::textures::TextureKey, AssetId>,
    texture_entries: &mut Vec<(
        AssetId,
        String,
        Option<(String, awsm_renderer_glb_export::ImageMime)>,
        TextureColorKind,
    )>,
    baked: Option<(
        awsm_renderer::textures::TextureKey,
        crate::engine::bridge::gltf::TexBinding,
    )>,
    name: &str,
    color_kind: TextureColorKind,
    texture_images: &std::collections::HashMap<
        awsm_renderer::textures::TextureKey,
        awsm_renderer_glb_export::ExportImage,
    >,
) -> Option<awsm_renderer_editor_protocol::TextureRef> {
    let (key, binding) = baked?;
    // The texture-asset id is deduped by baked key, but the binding (UV set +
    // transform) is per-slot, so it goes on the TextureRef, not the asset.
    let mk = |asset: AssetId| awsm_renderer_editor_protocol::TextureRef {
        asset,
        uv_index: binding.uv_index,
        transform: binding.transform,
        sampler: binding.sampler,
        flow: None,
        export_profile: None,
    };
    if let Some(id) = tex_for_key.get(&key) {
        return Some(mk(*id));
    }
    let id = AssetId::new();
    crate::engine::bridge::material::register_texture_key(id, key);
    tex_for_key.insert(key, id);
    // Capture the encoded source bytes for PERSISTENCE (when populate uploaded
    // this texture from an embedded / data-URI image). content_hash addresses the
    // on-disk side file `assets/<hash>.<ext>` (+ dedups identical textures); the
    // bytes live session-locally in texture_cache until Save reads them.
    let hash_mime = texture_images.get(&key).map(|img| {
        let hash = content_hash(&img.bytes);
        crate::engine::bridge::texture_cache::store(id, img.bytes.clone(), img.mime);
        (hash, img.mime)
    });
    texture_entries.push((id, name.to_string(), hash_mime, color_kind));
    Some(mk(id))
}

/// Map a KHR-extension texture slot name (the keys of `ExtractedMaterial::ext_textures`)
/// to its color kind, so persisted extension textures reload with the right color
/// space + mipmaps. Unknown slots default to `Albedo` (sRGB) — the safe default for a
/// color-ish map; the linear data-map extensions are matched explicitly.
fn ext_slot_color_kind(slot: &str) -> TextureColorKind {
    match slot {
        s if s.contains("clearcoat_normal") => TextureColorKind::Normal,
        s if s.contains("clearcoat") => TextureColorKind::MetallicRoughness, // roughness/factor maps — linear
        s if s.contains("specular_color") || s.contains("sheen_color") => {
            TextureColorKind::SpecularColor
        }
        s if s.contains("specular") || s.contains("sheen") => TextureColorKind::Specular,
        s if s.contains("transmission") => TextureColorKind::Transmission,
        s if s.contains("thickness") || s.contains("volume") => TextureColorKind::VolumeThickness,
        s if s.contains("iridescence") || s.contains("anisotropy") => TextureColorKind::Normal, // linear data
        _ => TextureColorKind::Albedo,
    }
}

/// SHA-256 hex of asset bytes — the `content_hash` that addresses the on-disk
/// `assets/<hash>.<ext>` side file (also dedups identical assets across the
/// project: textures, buffer-slot data, …).
/// The asset ids reachable from the live scene: walked from the node tree, the
/// environment, and every animation clip, then transitively through each reachable
/// asset entry (glTF child material/image ids, a captured mesh's source asset, a
/// material asset's own texture refs). Robust by construction — it serializes each
/// root/entry and keeps every UUID that is an asset-table key, so no
/// reference-carrying field can be forgotten: over-marking only ever KEEPS an
/// asset, never deletes a used one. Backs `PurgeUnusedAssets`.
fn reachable_assets(ctrl: &EditorController) -> std::collections::HashSet<AssetId> {
    use std::collections::HashSet;

    let keys: HashSet<AssetId> = ctrl
        .scene
        .assets
        .lock()
        .unwrap()
        .entries
        .keys()
        .copied()
        .collect();

    // Insert every asset-key UUID (a value string OR an object key) found in a
    // serde_json value.
    fn collect(v: &serde_json::Value, keys: &HashSet<AssetId>, out: &mut HashSet<AssetId>) {
        match v {
            serde_json::Value::String(s) => {
                if let Ok(u) = uuid::Uuid::parse_str(s) {
                    let id = AssetId(u);
                    if keys.contains(&id) {
                        out.insert(id);
                    }
                }
            }
            serde_json::Value::Array(a) => {
                for e in a {
                    collect(e, keys, out);
                }
            }
            serde_json::Value::Object(m) => {
                for (k, val) in m {
                    if let Ok(u) = uuid::Uuid::parse_str(k) {
                        let id = AssetId(u);
                        if keys.contains(&id) {
                            out.insert(id);
                        }
                    }
                    collect(val, keys, out);
                }
            }
            _ => {}
        }
    }
    fn scan<T: serde::Serialize>(val: &T, keys: &HashSet<AssetId>, out: &mut HashSet<AssetId>) {
        if let Ok(v) = serde_json::to_value(val) {
            collect(&v, keys, out);
        }
    }

    let mut reachable: HashSet<AssetId> = HashSet::new();

    // Roots: node tree (each node's kind carries its geometry/material/texture
    // refs), the environment, and every animation clip.
    fn walk_nodes(
        nodes: &[std::sync::Arc<crate::engine::scene::node::Node>],
        keys: &HashSet<AssetId>,
        out: &mut HashSet<AssetId>,
    ) {
        for n in nodes {
            scan(&n.kind.get_cloned(), keys, out);
            walk_nodes(&n.children.lock_ref(), keys, out);
        }
    }
    walk_nodes(&ctrl.scene.nodes.lock_ref(), &keys, &mut reachable);
    scan(&ctrl.scene.environment.get_cloned(), &keys, &mut reachable);
    for c in ctrl.custom_animations.lock_ref().iter() {
        scan(
            &crate::controller::animation::stored_from_live(c),
            &keys,
            &mut reachable,
        );
    }

    // Transitive: a reachable asset's entry may reference further assets.
    let mut frontier: Vec<AssetId> = reachable.iter().copied().collect();
    while let Some(id) = frontier.pop() {
        let entry = ctrl.scene.assets.lock().unwrap().entries.get(&id).cloned();
        if let Some(entry) = entry {
            let mut found = HashSet::new();
            scan(&entry, &keys, &mut found);
            for f in found {
                if reachable.insert(f) {
                    frontier.push(f);
                }
            }
        }
    }
    reachable
}

fn content_hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    let digest = h.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Intern raw buffer-slot words as a content-addressed [`AssetSource::Buffer`]
/// asset: dedups by SHA-256 (identical data across meshes shares one asset +
/// one `.bin`), caches the words in [`buffer_cache`](crate::engine::bridge::buffer_cache)
/// for Save, and returns the asset id to record in a `BufferRef`. The single bind
/// path for both the `SetMaterialBuffer` command and the inspector's "Load .bin".
pub(crate) fn intern_buffer_asset(words: Vec<u32>) -> AssetId {
    use awsm_renderer_editor_protocol::BufferDef;
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    let hash = content_hash(&bytes);
    let ctrl = controller();
    let id = {
        let mut assets = ctrl.scene.assets.lock().unwrap();
        // Reuse an existing BUFFER asset with identical bytes (a hash hit on a
        // different asset kind — astronomically unlikely — is ignored so we never
        // bind a texture id into a buffer slot).
        let existing = assets.entries.iter().find_map(|(id, e)| {
            (matches!(e.source, SceneAssetSource::Buffer(_)) && e.content_hash == hash)
                .then_some(*id)
        });
        existing.unwrap_or_else(|| {
            let id = AssetId::new();
            assets.entries.insert(
                id,
                AssetEntry::new_with_hash(
                    SceneAssetSource::Buffer(BufferDef {
                        word_len: words.len() as u32,
                    }),
                    hash,
                ),
            );
            id
        })
    };
    crate::engine::bridge::buffer_cache::store(id, words);
    id
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
    mesh: &awsm_renderer_glb_export::MeshData,
    tangents: Option<&Vec<[f32; 4]>>,
    source_asset: AssetId,
) -> awsm_renderer_editor_protocol::MeshRef {
    use crate::engine::bridge::mesh_cache;
    use awsm_renderer_editor_protocol::{CapturedSource, MeshDef, MeshRef};
    use awsm_renderer_editor_protocol::{MeshBase, ModifierStack};

    let mesh_id = AssetId(node_id.0);
    // `from_mesh_data` folds every UV set (incl. TEXCOORD_1) from `mesh.uvs`; attach
    // the authored glTF tangents (if any) so the captured mesh preserves the exact
    // basis a normal map was baked against across save→reload.
    let mut captured = mesh_cache::from_mesh_data(mesh.clone());
    captured.tangents = tangents.cloned();
    mesh_cache::store_with_id(mesh_id, captured);
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
) -> Vec<awsm_renderer_editor_protocol::SkinJoint> {
    let mut out = Vec::new();
    fn walk(
        nodes: &[crate::engine::bridge::asset_template::AssetTemplateNode],
        node_map: &std::collections::HashMap<u32, NodeId>,
        node_flat_indices: &std::collections::HashMap<u32, u32>,
        out: &mut Vec<awsm_renderer_editor_protocol::SkinJoint>,
    ) {
        for n in nodes {
            if n.is_skin_joint {
                if let (Some(&node), Some(&index)) = (
                    node_map.get(&n.gltf_node_index),
                    node_flat_indices.get(&n.gltf_node_index),
                ) {
                    out.push(awsm_renderer_editor_protocol::SkinJoint { node, index });
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
    joints: &[awsm_renderer_editor_protocol::SkinJoint],
) {
    use awsm_renderer_editor_protocol::NodeKind;
    let mut kind = node.kind.get_cloned();
    if let NodeKind::SkinnedMesh { skin, .. } = &mut kind {
        skin.joints = joints.to_vec();
        node.kind.set(kind);
    }
    for child in node.children.lock_ref().iter() {
        patch_skin_joints(child, joints);
    }
}

#[allow(clippy::too_many_arguments)]
fn build_editor_subtree(
    tn: &crate::engine::bridge::asset_template::AssetTemplateNode,
    asset_id: AssetId,
    mat_ids: &[AssetId],
    default_mat_id: Option<AssetId>,
    node_meshes: &crate::engine::bridge::gltf::NodeMeshMaps,
    node_flat_indices: &std::collections::HashMap<u32, u32>,
    fallback_name: Option<&str>,
    node_map: &mut std::collections::HashMap<u32, NodeId>,
) -> Arc<crate::engine::scene::node::Node> {
    use crate::engine::scene::node::Node;
    use awsm_renderer_editor_protocol::{
        dynamic_material::MaterialInstance, NodeKind, SkinnedMeshRef, Trs,
    };

    // This node's index in the clean rig glb (the DFS-flatten `reexport_clean`
    // assigns), the index space the MATERIALISER decodes the rig glb at. Falls
    // back to the original index when there's no rig glb (unskinned imports leave
    // `node_flat_indices` empty — the value is then unused).
    let rig_node_index = node_flat_indices
        .get(&tn.gltf_node_index)
        .copied()
        .unwrap_or(tn.gltf_node_index);

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
            if let Some((mesh, _tangents)) = node_meshes.get(&(tn.gltf_node_index, None)) {
                crate::engine::bridge::skinned_bake_cache::store(
                    asset_id,
                    tn.gltf_node_index,
                    None,
                    mesh.clone(),
                );
            }
            Node::new_with_transform_and_kind(name, trs, {
                let (material_variants, selected_variant) = palette_from_import(material);
                NodeKind::SkinnedMesh {
                    skin: SkinnedMeshRef {
                        source: asset_id,
                        node_index: tn.gltf_node_index,
                        rig_node_index,
                        primitive_index: None,
                        // Filled after the whole subtree is built (node_map
                        // complete) — see `assemble_skin_joints` / patch below.
                        joints: Vec::new(),
                    },
                    material_variants,
                    selected_variant,
                    shadow: Default::default(),
                    lod: Default::default(),
                }
            })
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
                if let Some((mesh, _tangents)) =
                    node_meshes.get(&(tn.gltf_node_index, Some(i as u32)))
                {
                    crate::engine::bridge::skinned_bake_cache::store(
                        asset_id,
                        tn.gltf_node_index,
                        Some(i as u32),
                        mesh.clone(),
                    );
                }
                let part = Node::new_with_transform_and_kind(part_label, Trs::IDENTITY, {
                    let (material_variants, selected_variant) = palette_from_import(material);
                    NodeKind::SkinnedMesh {
                        skin: SkinnedMeshRef {
                            source: asset_id,
                            node_index: tn.gltf_node_index,
                            rig_node_index,
                            primitive_index: Some(i as u32),
                            joints: Vec::new(),
                        },
                        material_variants,
                        selected_variant,
                        shadow: Default::default(),
                        lod: Default::default(),
                    }
                });
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
            if let Some((mesh, tangents)) = node_meshes.get(&(tn.gltf_node_index, None)) {
                let mesh_ref =
                    mint_imported_mesh(mesh_node.id, &name, mesh, tangents.as_ref(), asset_id);
                let (material_variants, selected_variant) = palette_from_import(material);
                mesh_node.kind.set(NodeKind::Mesh {
                    mesh: mesh_ref,
                    material_variants,
                    selected_variant,
                    shadow: Default::default(),
                    lod: Default::default(),
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
                if let Some((mesh, tangents)) =
                    node_meshes.get(&(tn.gltf_node_index, Some(i as u32)))
                {
                    let mesh_ref =
                        mint_imported_mesh(part.id, &part_label, mesh, tangents.as_ref(), asset_id);
                    let (material_variants, selected_variant) = palette_from_import(material);
                    part.kind.set(NodeKind::Mesh {
                        mesh: mesh_ref,
                        material_variants,
                        selected_variant,
                        shadow: Default::default(),
                        lod: Default::default(),
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
            node_flat_indices,
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
/// Wrap a glTF-imported material instance as the mesh's palette: ONE variant
/// (named after its library material) that is already selected — an imported
/// model must render its authored materials, not magenta. `None` → empty
/// palette, no selection.
fn palette_from_import(
    material: Option<awsm_renderer_editor_protocol::dynamic_material::MaterialInstance>,
) -> (
    Vec<awsm_renderer_editor_protocol::MaterialVariant>,
    Option<awsm_renderer_editor_protocol::VariantId>,
) {
    match material {
        Some(instance) => {
            let name = crate::controller::custom_material::find_material(
                &controller().custom_materials,
                instance.asset,
            )
            .map(|m| m.name.get_cloned())
            .unwrap_or_else(|| "Material".to_string());
            let v = awsm_renderer_editor_protocol::MaterialVariant {
                id: awsm_renderer_editor_protocol::VariantId::new(),
                name,
                instance,
            };
            let id = v.id;
            (vec![v], Some(id))
        }
        None => (Vec::new(), None),
    }
}

fn structure_key(kind: &NodeKind) -> String {
    use awsm_renderer_editor_protocol::{CameraProjection, LightConfig, MaterialShading};
    match kind {
        // The Mesh inspector rows depend on the assigned material's shading model
        // (its shared variant) — read it from the per-mesh inline store, which is
        // seeded from that variant. Unassigned → no material rows. (Geometry is no
        // longer edited inline — the base/stack display is informational — so the
        // structure key doesn't vary on the stack base.)
        NodeKind::Mesh { .. } => {
            let material = kind.selected_material();
            let shading = match material.map(|m| m.inline.shading) {
                Some(MaterialShading::Pbr) => "pbr",
                Some(MaterialShading::Unlit) => "unlit",
                Some(MaterialShading::Toon { .. }) => "toon",
                Some(MaterialShading::FlipBook { .. }) => "flipbook",
                None => "none",
            };
            // The palette (ids + names) is part of the structure: the Material
            // dropdown lists it, so adds/removes/renames must rebuild the rows.
            let palette = kind
                .material_variants()
                .map(|vs| {
                    vs.iter()
                        .map(|v| format!("{}:{}", v.id, v.name))
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .unwrap_or_default();
            format!("mesh/{shading}/{:?}/{palette}", kind.selected_variant_id())
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
    use awsm_renderer_editor_protocol::AssetId as Aid;
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

/// A distinct, deterministic cache id for a mesh's frozen `Captured` snapshot,
/// kept separate from the asset id. The asset id is the **render-bake** target
/// (`mesh_cache[mesh]`, what `node_mesh` reads); the snapshot id holds the
/// immutable frozen geometry a `Captured` base evaluates from. Keeping them
/// distinct is what stops a collapsed mesh's re-bake from reading its own output
/// and compounding. Derived from the mesh id (deterministic for replay /
/// persistence) but XOR-salted so it can never collide with it.
fn captured_snapshot_id(mesh: AssetId) -> AssetId {
    // Arbitrary fixed 128-bit salt; non-zero so the result differs from `mesh`,
    // and effectively collision-free against random v4 asset ids.
    const SNAPSHOT_SALT: u128 = 0x9E37_79B9_7F4A_7C15_F39C_C060_5CED_C835;
    AssetId(uuid::Uuid::from_u128(mesh.0.as_u128() ^ SNAPSHOT_SALT))
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

#[cfg(test)]
mod ik_tests {
    use super::ik_bend_plane_normal;
    use glam::Vec3;

    /// Rotating `dir_t` about the returned normal moves the joint toward
    /// `n × dir_t` (Rodrigues, n ⊥ dir_t) — assert that side matches `want`.
    fn bend_side(n: Vec3, dir_t: Vec3, want: Vec3) -> bool {
        n.cross(dir_t).dot(want) > 0.0
    }

    #[test]
    fn bent_chain_keeps_its_side() {
        // Leg down (−Y), knee bent FORWARD (+Z). Target straight below.
        let (a, b, c) = (
            Vec3::ZERO,
            Vec3::new(0.0, -1.0, 0.5),
            Vec3::new(0.0, -2.0, 0.0),
        );
        let dir_t = Vec3::NEG_Y;
        let n = ik_bend_plane_normal(a, b, c, dir_t, None, Vec3::NEG_Z);
        assert!(bend_side(n, dir_t, Vec3::Z), "knee must stay forward");
        // Mirrored: knee bent BACKWARD stays backward.
        let b2 = Vec3::new(0.0, -1.0, -0.5);
        let n2 = ik_bend_plane_normal(a, b2, c, dir_t, None, Vec3::NEG_Z);
        assert!(bend_side(n2, dir_t, Vec3::NEG_Z), "knee must stay backward");
    }

    #[test]
    fn straight_chain_bends_character_forward() {
        // Perfectly straight leg pointing down; character faces +Z (forward
        // here passed as the facing vector directly).
        let (a, b, c) = (
            Vec3::ZERO,
            Vec3::new(0.0, -1.0, 0.0),
            Vec3::new(0.0, -2.0, 0.0),
        );
        let dir_t = Vec3::NEG_Y;
        let n = ik_bend_plane_normal(a, b, c, dir_t, None, Vec3::Z);
        assert!(
            bend_side(n, dir_t, Vec3::Z),
            "straight knee must bend toward facing, not sideways"
        );
    }

    #[test]
    fn pole_wins_over_current_bend() {
        // Knee currently forward, pole placed BEHIND — pole must win.
        let (a, b, c) = (
            Vec3::ZERO,
            Vec3::new(0.0, -1.0, 0.5),
            Vec3::new(0.0, -2.0, 0.0),
        );
        let dir_t = Vec3::NEG_Y;
        let n = ik_bend_plane_normal(a, b, c, dir_t, Some(Vec3::new(0.0, -1.0, -5.0)), Vec3::Z);
        assert!(
            bend_side(n, dir_t, Vec3::NEG_Z),
            "joint must bend toward the pole"
        );
    }

    #[test]
    fn normal_is_unit_and_perpendicular() {
        let (a, b, c) = (
            Vec3::ZERO,
            Vec3::new(0.3, -1.0, 0.4),
            Vec3::new(0.1, -2.0, 0.1),
        );
        let dir_t = (Vec3::new(0.5, -1.8, 0.2) - a).normalize();
        let n = ik_bend_plane_normal(a, b, c, dir_t, None, Vec3::NEG_Z);
        assert!((n.length() - 1.0).abs() < 1e-5);
        assert!(n.dot(dir_t).abs() < 1e-5, "normal must be ⊥ the reach line");
    }
}

#[cfg(test)]
mod unassigned_material_tests {
    use super::unassigned_material_kind;
    use awsm_renderer_editor_protocol::{AssetId, MeshRef, NodeKind};

    // §5 regression guard: a geometry node with no material must report
    // `unassigned` (→ the visible magenta sentinel), never be treated as
    // non-geometry / invisible.
    #[test]
    fn unassigned_geometry_is_magenta_sentinel() {
        let mesh = NodeKind::Mesh {
            mesh: MeshRef(AssetId::new()),
            material_variants: Vec::new(),
            selected_variant: None,
            shadow: Default::default(),
            lod: Default::default(),
        };
        assert_eq!(unassigned_material_kind(&mesh), "unassigned");
    }

    #[test]
    fn non_geometry_is_none() {
        assert_eq!(unassigned_material_kind(&NodeKind::Group), "none");
    }
}

#[cfg(test)]
mod instancer_tests {
    use super::*;
    use awsm_renderer_editor_protocol::{InsertSpec, InstancerDef, Trs};
    use futures::executor::block_on;

    fn trs(t: [f32; 3]) -> Trs {
        Trs {
            translation: t,
            ..Trs::IDENTITY
        }
    }

    fn instancer_def(ctrl: &EditorController, node: NodeId) -> InstancerDef {
        match crate::engine::scene::mutate::find_by_id(&ctrl.scene, node)
            .expect("node exists")
            .kind
            .get_cloned()
        {
            NodeKind::Instancer(def) => def,
            other => panic!("expected Instancer, got {other:?}"),
        }
    }

    /// `SetInstancerTransforms` REPLACES the list wholesale, and applying the
    /// returned inverse restores the prior list exactly (the bulk-set undo
    /// contract, mirroring `SetTrackKeys`).
    #[test]
    fn set_instancer_transforms_replaces_and_undoes() {
        let ctrl = EditorController::new();
        let node = NodeId::new();
        block_on(ctrl.apply(EditorCommand::Insert {
            id: node,
            spec: InsertSpec::Instancer,
            parent: None,
        }))
        .unwrap();
        assert!(instancer_def(&ctrl, node).transforms.is_empty());

        // First bulk set: 3 transforms + colors.
        let first = vec![
            trs([1.0, 0.0, 0.0]),
            trs([2.0, 0.0, 0.0]),
            trs([3.0, 0.0, 0.0]),
        ];
        block_on(ctrl.apply(EditorCommand::SetInstancerTransforms {
            node,
            transforms: first.clone(),
            per_instance_colors: Some(vec![[1.0, 0.0, 0.0, 1.0]]),
        }))
        .unwrap();
        let def = instancer_def(&ctrl, node);
        assert_eq!(def.transforms, first);
        assert_eq!(def.per_instance_colors, vec![[1.0, 0.0, 0.0, 1.0]]);

        // Second bulk set REPLACES (not appends); colors=None keeps current.
        let second = vec![trs([9.0, 9.0, 9.0])];
        let inverse = block_on(ctrl.apply(EditorCommand::SetInstancerTransforms {
            node,
            transforms: second.clone(),
            per_instance_colors: None,
        }))
        .unwrap()
        .expect("undoable");
        let def = instancer_def(&ctrl, node);
        assert_eq!(def.transforms, second, "list replaced wholesale");
        assert_eq!(
            def.per_instance_colors,
            vec![[1.0, 0.0, 0.0, 1.0]],
            "colors untouched when None"
        );

        // Undo (apply the inverse) restores the prior list exactly.
        block_on(ctrl.apply(inverse)).unwrap();
        let def = instancer_def(&ctrl, node);
        assert_eq!(def.transforms, first, "undo restored the prior transforms");
    }

    /// The command rejects loudly on a non-instancer node (no silent no-op).
    #[test]
    fn set_instancer_transforms_rejects_wrong_kind() {
        let ctrl = EditorController::new();
        let node = NodeId::new();
        block_on(ctrl.apply(EditorCommand::Insert {
            id: node,
            spec: InsertSpec::Empty,
            parent: None,
        }))
        .unwrap();
        let r = block_on(ctrl.apply(EditorCommand::SetInstancerTransforms {
            node,
            transforms: vec![],
            per_instance_colors: None,
        }));
        assert!(r.is_err(), "group node must be rejected");
    }
}

#[cfg(test)]
mod instancer_aabb_tests {
    //! Native tests for the pure instancer-bounds fold used by `local_aabb`
    //! (frame_node / NodeBounds fallback): the node-local AABB of an explicit
    //! instancer is the UNION of (instance transform × instanced-mesh AABB)
    //! over all authored instances — not just one copy at the origin.
    use super::instancer_local_aabb;
    use awsm_renderer_editor_protocol::Trs;

    const UNIT: ([f32; 3], [f32; 3]) = ([-0.5, -0.5, -0.5], [0.5, 0.5, 0.5]);

    fn trs(t: [f32; 3]) -> Trs {
        Trs {
            translation: t,
            ..Trs::IDENTITY
        }
    }

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        (0..3).all(|i| (a[i] - b[i]).abs() < 1e-5)
    }

    #[test]
    fn empty_transforms_yield_base() {
        assert_eq!(instancer_local_aabb(UNIT, &[]), UNIT);
    }

    #[test]
    fn spread_instances_union_their_extents() {
        // Two unit boxes 10 apart on X: the union spans [-10.5, 10.5] on X and
        // stays [-0.5, 0.5] on Y/Z — the exact under-measure the unit-box
        // fallback had.
        let (min, max) =
            instancer_local_aabb(UNIT, &[trs([-10.0, 0.0, 0.0]), trs([10.0, 0.0, 0.0])]);
        assert!(close(min, [-10.5, -0.5, -0.5]), "min {min:?}");
        assert!(close(max, [10.5, 0.5, 0.5]), "max {max:?}");
    }

    #[test]
    fn instance_scale_expands_its_copy() {
        let big = Trs {
            translation: [0.0, 4.0, 0.0],
            scale: [3.0, 3.0, 3.0],
            ..Trs::IDENTITY
        };
        let (min, max) = instancer_local_aabb(UNIT, &[trs([0.0, 0.0, 0.0]), big]);
        assert!(close(min, [-1.5, -0.5, -1.5]), "min {min:?}");
        assert!(close(max, [1.5, 5.5, 1.5]), "max {max:?}");
    }

    #[test]
    fn rotation_encloses_the_rotated_box() {
        // A unit box rotated 45° about Y encloses within ±(√2/2) on X/Z.
        let rot = Trs {
            rotation: glam::Quat::from_rotation_y(std::f32::consts::FRAC_PI_4).to_array(),
            ..Trs::IDENTITY
        };
        let (min, max) = instancer_local_aabb(UNIT, &[rot]);
        let r = std::f32::consts::SQRT_2 / 2.0;
        assert!(close(min, [-r, -0.5, -r]), "min {min:?}");
        assert!(close(max, [r, 0.5, r]), "max {max:?}");
    }
}

#[cfg(test)]
mod facing_tests {
    use super::world_forward_up_right;
    use glam::Mat4;

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        (0..3).all(|i| (a[i] - b[i]).abs() < 1e-5)
    }

    #[test]
    fn identity_is_minus_z_forward() {
        let (f, u, r) = world_forward_up_right(Mat4::IDENTITY);
        assert!(close(f, [0.0, 0.0, -1.0]), "forward {f:?}");
        assert!(close(u, [0.0, 1.0, 0.0]), "up {u:?}");
        assert!(close(r, [1.0, 0.0, 0.0]), "right {r:?}");
    }

    #[test]
    fn rotation_tracks_orientation() {
        // Yaw 90° about +Y: local -Z forward swings to world -X.
        let (f, _u, _r) =
            world_forward_up_right(Mat4::from_rotation_y(std::f32::consts::FRAC_PI_2));
        assert!(close(f, [-1.0, 0.0, 0.0]), "forward {f:?}");
    }
}

#[cfg(test)]
mod mesh_rebake_tests {
    use super::*;
    use crate::engine::bridge::mesh_cache;
    use awsm_renderer_editor_protocol::{InsertSpec, MeshBase, Modifier, PrimitiveShape};
    use futures::executor::block_on;

    fn tris(mesh: AssetId) -> usize {
        mesh_cache::get_captured(mesh)
            .map(|c| c.indices.len() / 3)
            .unwrap_or(0)
    }

    fn surface_area(mesh: AssetId) -> f32 {
        let Some(c) = mesh_cache::get_captured(mesh) else {
            return 0.0;
        };
        let p = &c.positions;
        c.indices
            .chunks_exact(3)
            .map(|t| {
                let (a, b, d) = (
                    glam::Vec3::from(p[t[0] as usize]),
                    glam::Vec3::from(p[t[1] as usize]),
                    glam::Vec3::from(p[t[2] as usize]),
                );
                (b - a).cross(d - a).length() * 0.5
            })
            .sum()
    }

    fn sphere() -> InsertSpec {
        InsertSpec::Primitive(PrimitiveShape::Sphere {
            radius: 0.5,
            segments_long: 16,
            segments_lat: 12,
        })
    }

    fn base_of(ctrl: &EditorController, mesh: AssetId) -> MeshBase {
        let assets = ctrl.scene.assets.lock().unwrap();
        match assets.get(mesh).map(|e| &e.source) {
            Some(SceneAssetSource::Mesh(def)) => def.stack.base.clone(),
            _ => panic!("not a mesh"),
        }
    }

    /// Adding procedural modifiers to a collapsed (`Captured`-base) mesh must apply
    /// each one ONCE — not re-read its own bake output and re-apply the stack
    /// (which compounded geometry: 4096 → ×256 in the field report).
    #[test]
    fn modifiers_on_captured_base_do_not_compound() {
        let ctrl = EditorController::new();
        let node = NodeId::new();
        let mesh = AssetId(node.0);
        block_on(ctrl.apply(EditorCommand::Insert {
            id: node,
            spec: sphere(),
            parent: None,
        }))
        .unwrap();
        let base = tris(mesh);
        assert!(base > 0, "sphere baked");

        // Collapse to a Captured base by sculpting a vertex.
        block_on(ctrl.apply(EditorCommand::SoftTransformVertices {
            mesh,
            indices: vec![0],
            translation: [0.0, 0.2, 0.0],
            falloff: 0.3,
            selection: None,
        }))
        .unwrap();
        assert!(
            matches!(base_of(&ctrl, mesh), MeshBase::Captured(_)),
            "collapsed"
        );

        // subdivide(1) → exactly ×4.
        block_on(ctrl.apply(EditorCommand::AddModifier {
            mesh,
            modifier: Modifier::Subdivide { iterations: 1 },
        }))
        .unwrap();
        assert_eq!(tris(mesh), base * 4, "subdivide once = ×4");

        // smooth keeps the tri count — subdivide must NOT re-apply (no compounding).
        block_on(ctrl.apply(EditorCommand::AddModifier {
            mesh,
            modifier: Modifier::Smooth {
                iterations: 1,
                factor: 0.5,
            },
        }))
        .unwrap();
        assert_eq!(
            tris(mesh),
            base * 4,
            "no compounding — subdivide stayed applied once"
        );
    }

    /// Replacing the recipe with a fresh primitive base must regenerate from
    /// scratch — not re-apply the stale soft-transform overrides (which left a
    /// ghost tip at y ≈ 0.5 + 0.45 in the field report).
    #[test]
    fn set_mesh_modifiers_with_new_base_clears_stale_overrides() {
        let ctrl = EditorController::new();
        let node = NodeId::new();
        let mesh = AssetId(node.0);
        block_on(ctrl.apply(EditorCommand::Insert {
            id: node,
            spec: sphere(),
            parent: None,
        }))
        .unwrap();
        // Pull the top way up (collapses + records an override).
        block_on(ctrl.apply(EditorCommand::SoftTransformVertices {
            mesh,
            indices: vec![0],
            translation: [0.0, 5.0, 0.0],
            falloff: 0.2,
            selection: None,
        }))
        .unwrap();
        let pulled_max_y = mesh_cache::get_captured(mesh)
            .unwrap()
            .positions
            .iter()
            .map(|p| p[1])
            .fold(f32::MIN, f32::max);
        assert!(pulled_max_y > 1.0, "override pulled the tip up");

        // Replace the whole recipe with a clean unit sphere (radius 0.5).
        block_on(ctrl.apply(EditorCommand::SetMeshModifiers {
            mesh,
            stack: ModifierStack {
                base: MeshBase::Primitive(PrimitiveShape::Sphere {
                    radius: 0.5,
                    segments_long: 24,
                    segments_lat: 16,
                }),
                modifiers: vec![],
            },
        }))
        .unwrap();
        let new_max_y = mesh_cache::get_captured(mesh)
            .unwrap()
            .positions
            .iter()
            .map(|p| p[1])
            .fold(f32::MIN, f32::max);
        assert!(
            new_max_y < 0.6,
            "recipe replaced → clean sphere (max y ≈ 0.5), not a ghost tip (got {new_max_y})"
        );
        let _ = surface_area(mesh);
    }

    fn max_y(mesh: AssetId) -> f32 {
        mesh_cache::get_captured(mesh)
            .unwrap()
            .positions
            .iter()
            .map(|p| p[1])
            .fold(f32::MIN, f32::max)
    }

    /// Undo must walk back cleanly through collapse → add modifier → add modifier,
    /// restoring tri counts and erasing the sculpt — the inverses can't leave the
    /// frozen snapshot or the cache in a stale state.
    #[test]
    fn undo_walks_back_through_collapse_and_modifiers() {
        let ctrl = EditorController::new();
        let node = NodeId::new();
        let mesh = AssetId(node.0);
        block_on(ctrl.apply(EditorCommand::Insert {
            id: node,
            spec: sphere(),
            parent: None,
        }))
        .unwrap();
        let base = tris(mesh);
        let base_max_y = max_y(mesh);

        let inv_sculpt = block_on(ctrl.apply(EditorCommand::SoftTransformVertices {
            mesh,
            indices: vec![0],
            translation: [0.0, 3.0, 0.0],
            falloff: 0.25,
            selection: None,
        }))
        .unwrap()
        .expect("sculpt records an inverse");
        assert!(max_y(mesh) > 1.0, "sculpt raised the tip");

        let inv_subdiv = block_on(ctrl.apply(EditorCommand::AddModifier {
            mesh,
            modifier: Modifier::Subdivide { iterations: 1 },
        }))
        .unwrap()
        .expect("add modifier records an inverse");
        assert_eq!(tris(mesh), base * 4);

        let inv_smooth = block_on(ctrl.apply(EditorCommand::AddModifier {
            mesh,
            modifier: Modifier::Smooth {
                iterations: 1,
                factor: 0.5,
            },
        }))
        .unwrap()
        .expect("add modifier records an inverse");
        assert_eq!(tris(mesh), base * 4);

        // Undo, newest first.
        block_on(ctrl.apply(inv_smooth)).unwrap();
        assert_eq!(tris(mesh), base * 4, "undo smooth: still subdivided once");
        block_on(ctrl.apply(inv_subdiv)).unwrap();
        assert_eq!(
            tris(mesh),
            base,
            "undo subdivide: back to the collapsed base"
        );
        assert!(
            max_y(mesh) > 1.0,
            "sculpt still present after undoing modifiers"
        );
        block_on(ctrl.apply(inv_sculpt)).unwrap();
        assert_eq!(tris(mesh), base, "undo sculpt: tri count unchanged");
        assert!(
            (max_y(mesh) - base_max_y).abs() < 0.02,
            "undo sculpt: tip back to the original sphere (got {}, want {base_max_y})",
            max_y(mesh)
        );
    }

    /// A collapsed-then-modified mesh's frozen snapshot lives under a distinct id;
    /// it's non-regenerable, so Save must include it (else editing breaks after a
    /// reload).
    #[test]
    fn snapshot_id_is_saved_for_collapsed_meshes() {
        use awsm_renderer_editor_protocol::mesh_asset_filename;
        let ctrl = EditorController::new();
        let node = NodeId::new();
        let mesh = AssetId(node.0);
        block_on(ctrl.apply(EditorCommand::Insert {
            id: node,
            spec: sphere(),
            parent: None,
        }))
        .unwrap();
        block_on(ctrl.apply(EditorCommand::SoftTransformVertices {
            mesh,
            indices: vec![0],
            translation: [0.0, 0.3, 0.0],
            falloff: 0.25,
            selection: None,
        }))
        .unwrap();
        block_on(ctrl.apply(EditorCommand::AddModifier {
            mesh,
            modifier: Modifier::Subdivide { iterations: 1 },
        }))
        .unwrap();

        let snap = super::captured_snapshot_id(mesh);
        assert_ne!(snap, mesh, "snapshot id is distinct from the asset id");
        let files = crate::controller::persistence::mesh_files(&ctrl);
        let want = format!("assets/{}", mesh_asset_filename(snap));
        assert!(
            files.iter().any(|(p, _)| *p == want),
            "snapshot {snap} must be in the saved mesh files: {:?}",
            files.iter().map(|(p, _)| p).collect::<Vec<_>>()
        );
    }
}
