//! Editor-wide state. Initialized once at startup (after the renderer
//! context is ready) and accessed by `actions/*` and the UI via
//! `with_app_state`.
//!
//! Separate from `context.rs` because that module's job is renderer setup;
//! this module is the editor's data layer.

#![allow(clippy::arc_with_non_send_sync)]

pub mod history;
pub mod project;

use crate::prelude::*;
use crate::renderer_bridge::gizmo::MoveAction;
use crate::renderer_bridge::Bridge;
use crate::scene::{AssetId, NodeId, Scene, SceneSnapshot};
use crate::tree::drag::DropZone;
use awsm_renderer::anti_alias::AntiAliasing;
use awsm_web_shared::util::free_camera::ProjectionMode;
use awsm_web_shared::viewport3d::{
    point_handle::PointHandleSet, transform_controller::TransformController,
};
use history::History;
use project::ProjectState;
use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};

/// How the rotation input is displayed in the properties panel. Storage is
/// always a quaternion on the node; this only affects the editor UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RotationDisplay {
    EulerDegrees,
    Quaternion,
}

/// Authored-node binding for the point-handle set. The handles edit one
/// of these kinds at a time; the bridge dispatches handle drags back into
/// the right `Mutable<NodeKind>` field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointHandleTarget {
    /// Edits `CurveDef::control_points`.
    Curve(NodeId),
    /// Edits `LineDef::points[i].pos`.
    Line(NodeId),
}

pub struct AppState {
    pub scene: Arc<Scene>,

    /// Multi-selection as an ordered-insertion-agnostic set. Rows observe
    /// membership via `.signal_ref(|set| set.contains(&id))`.
    pub selected: Mutable<HashSet<NodeId>>,
    /// Anchor node for Shift+click range selection. Updated on plain click
    /// and Ctrl/Cmd+click; stays put during Shift+click.
    pub selection_anchor: Mutable<Option<NodeId>>,
    /// Whether the current selection came from an explicit user action
    /// (tree click, viewport pick, keyboard nav, etc.) versus an
    /// internal action (e.g. Insert auto-selecting the new node).
    /// `parent_for_insert` only treats explicit selections as "insert
    /// under here" — without this, hammering `Insert > Empty` would
    /// nest each new node inside the previous one.
    pub selection_is_explicit: Mutable<bool>,

    pub history: Arc<Mutex<History>>,
    pub project: Arc<Mutex<ProjectState>>,

    /// Mirrored so `has_selection`, `can_undo`, `can_redo` are observable
    /// signals without holding the `Mutex` over an `await`.
    pub has_selection: Mutable<bool>,
    pub can_undo: Mutable<bool>,
    pub can_redo: Mutable<bool>,
    pub dirty: Mutable<bool>,
    pub project_name: Mutable<Option<String>>,

    /// Editor view prefs. Held here (not in `Scene`) because they're
    /// per-user UI state, not part of the project.
    pub grid_enabled: Mutable<bool>,
    /// Show/hide toggle for the on-canvas transform gizmo. Distinct
    /// from "no selection" — when this is `false` the gizmo stays
    /// hidden even with a selected node, and its handles aren't
    /// pickable (the GPU pick uses mesh visibility).
    pub gizmo_enabled: Mutable<bool>,
    /// Anti-aliasing settings mirror (MSAA + SMAA + mipmap). The
    /// Editor header's `MSAA Anti-Aliasing` checkbox writes here; an
    /// `actions::view::reset_anti_aliasing()` call pushes the current
    /// value into the renderer via `set_anti_aliasing`. Initialized
    /// to `AntiAliasing::default()` so the editor's boot matches the
    /// renderer's defaults.
    pub anti_aliasing: Mutable<AntiAliasing>,
    /// Dev toggle: light-culling debug heatmap (per-pixel applied-light
    /// count). Drives `AwsmRenderer::set_light_culling_debug_heatmap`.
    pub debug_light_heatmap: Mutable<bool>,
    pub rotation_display: Mutable<RotationDisplay>,
    /// Active viewport projection. The header `Camera` tab's dropdown is
    /// the single source of truth; `actions::camera::set_projection_mode`
    /// pushes changes into the `Camera` instance held in `context`.
    pub projection_mode: Mutable<ProjectionMode>,
    /// If `Some`, the viewport renders from this authored Camera node's
    /// behavior (mirrored from the player's camera driver) instead of
    /// the free-fly camera. `None` = the default free-fly mode. Picked
    /// via the dropdown in the header Camera tab.
    pub editor_camera_target: Mutable<Option<NodeId>>,

    /// Currently-selected assets in the Assets panel. Non-empty
    /// takes priority over `selected` (nodes) when routing the
    /// right-sidebar inspector — exactly one selected opens the
    /// asset editor; multiple opens a batch summary + Delete button.
    /// Clear (empty the set) to fall back to the node-based inspector.
    /// `IndexSet` (not `HashSet`) so the batch inspector lists entries
    /// in the order the user clicked them — more predictable than
    /// random hash iteration, and the cost of indexed iteration is
    /// indistinguishable for ~tens of entries.
    pub selected_assets: Mutable<indexmap::IndexSet<AssetId>>,

    /// Reactive list of imported custom materials (each one a folder
    /// pointer under `<project>/assets/materials/<name>/`). The
    /// Materials pane (`properties::custom_materials_pane`) appends
    /// to this on successful Import; the bridge consumes it on
    /// renderer registration.
    pub custom_materials: std::rc::Rc<
        futures_signals::signal_vec::MutableVec<
            awsm_scene_schema::dynamic_material::CustomMaterialRef,
        >,
    >,
    /// Status of the in-flight Import Material flow. Drives the inline
    /// status line under the Import button.
    pub custom_materials_import_status: std::sync::Arc<
        futures_signals::signal::Mutable<crate::properties::custom_materials_pane::ImportStatus>,
    >,

    /// Tree-view drag state. Non-empty `tree_drag_ids` means a drag is in
    /// progress; `tree_drag_target` tracks what the pointer is hovering.
    pub tree_drag_ids: Arc<Mutex<Vec<NodeId>>>,
    pub tree_drag_target: Mutable<Option<(NodeId, DropZone)>>,
    pub tree_is_dragging: Mutable<bool>,

    /// Asset bytes that haven't been written to disk yet, keyed by
    /// `AssetId`. Populated by `Insert Model` / KTX picker; flushed on
    /// `Save`. Assets already present on disk (e.g. loaded from an existing
    /// project) are NOT kept here — only newly-inserted ones.
    pub pending_assets: Arc<Mutex<HashMap<AssetId, Vec<u8>>>>,

    /// Scene ↔ renderer bridge. Holds the per-node `RendererNode`
    /// entries + the asset cache.
    pub renderer_bridge: Arc<Bridge>,

    /// On-canvas transform gizmo. `None` until gizmo.glb finishes loading.
    pub transform_controller: Arc<Mutex<Option<TransformController>>>,
    /// Point-handle gizmo set — translation-only handles, one per control
    /// point of the currently-selected Curve / Line node. Created eagerly
    /// (empty) and populated by the selection observer.
    pub point_handles: Arc<Mutex<PointHandleSet>>,
    /// Authored-node id the point handles are currently bound to (if any),
    /// and which authored field they edit. The bridge needs this to
    /// translate handle drags back into `CurveDef::control_points` /
    /// `LineDef::points` mutations.
    pub point_handle_target: Mutable<Option<PointHandleTarget>>,
    /// Per-emitter "play preview" toggle. Keyed by the emitter's NodeId;
    /// the inspector's Play/Stop button writes into this and a bridge
    /// observer materializes / tears down the runtime simulator.
    pub playing_emitters: Arc<Mutex<HashMap<NodeId, Mutable<bool>>>>,
    /// Which kind of pointer drag is currently active, if any.
    pub move_action: Mutable<Option<MoveAction>>,
    /// Pre-drag scene snapshot, captured at the start of a gizmo drag
    /// so we can commit a single history entry when the drag ends.
    pub pending_transform_snapshot: Arc<Mutex<Option<SceneSnapshot>>>,

    /// Aggregated, deduped, sorted list of asset filenames whose load
    /// failed. Drives the header "missing assets" indicator + overflow
    /// item. Computed reactively via `report_asset_failed` /
    /// `clear_asset_failure` from `failed_assets_by_node`.
    pub missing_assets: Mutable<Vec<String>>,
    /// Per-node failed-asset map. Source of truth for `missing_assets`.
    /// Tracking by node id (not just filename) means deleting a failed
    /// node correctly retracts its entry, even when multiple nodes
    /// share the same broken file.
    failed_assets_by_node: Arc<Mutex<HashMap<NodeId, String>>>,
}

impl AppState {
    fn new() -> Self {
        Self {
            scene: Scene::new(),
            selected: Mutable::new(HashSet::new()),
            selection_anchor: Mutable::new(None),
            selection_is_explicit: Mutable::new(false),
            history: Arc::new(Mutex::new(History::new())),
            project: Arc::new(Mutex::new(ProjectState::new())),
            has_selection: Mutable::new(false),
            can_undo: Mutable::new(false),
            can_redo: Mutable::new(false),
            dirty: Mutable::new(false),
            project_name: Mutable::new(None),
            grid_enabled: Mutable::new(true),
            gizmo_enabled: Mutable::new(true),
            anti_aliasing: Mutable::new(AntiAliasing::default()),
            debug_light_heatmap: Mutable::new(false),
            rotation_display: Mutable::new(RotationDisplay::EulerDegrees),
            projection_mode: Mutable::new(ProjectionMode::Perspective),
            editor_camera_target: Mutable::new(None),
            selected_assets: Mutable::new(indexmap::IndexSet::new()),
            custom_materials: std::rc::Rc::new(futures_signals::signal_vec::MutableVec::new()),
            custom_materials_import_status: std::sync::Arc::new(
                futures_signals::signal::Mutable::new(
                    crate::properties::custom_materials_pane::ImportStatus::Idle,
                ),
            ),
            tree_drag_ids: Arc::new(Mutex::new(Vec::new())),
            tree_drag_target: Mutable::new(None),
            tree_is_dragging: Mutable::new(false),
            pending_assets: Arc::new(Mutex::new(HashMap::new())),
            renderer_bridge: Bridge::new(),
            transform_controller: Arc::new(Mutex::new(None)),
            point_handles: Arc::new(Mutex::new(PointHandleSet::new())),
            point_handle_target: Mutable::new(None),
            playing_emitters: Arc::new(Mutex::new(HashMap::new())),
            move_action: Mutable::new(None),
            pending_transform_snapshot: Arc::new(Mutex::new(None)),
            missing_assets: Mutable::new(Vec::new()),
            failed_assets_by_node: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Mark `node_id` as having failed to load `filename`. The node may
    /// already be tracked (re-load on the same node); we just overwrite.
    pub fn report_asset_failed(&self, node_id: NodeId, filename: String) {
        {
            let mut map = self.failed_assets_by_node.lock().unwrap();
            map.insert(node_id, filename);
        }
        self.refresh_missing_assets();
    }

    /// Drop `node_id` from the failed map (asset became Ready, node was
    /// removed, kind changed away from Model, etc.). No-op if absent.
    pub fn clear_asset_failure(&self, node_id: NodeId) {
        let removed = {
            let mut map = self.failed_assets_by_node.lock().unwrap();
            map.remove(&node_id).is_some()
        };
        if removed {
            self.refresh_missing_assets();
        }
    }

    /// Recompute the deduped, sorted `missing_assets` list from the map.
    fn refresh_missing_assets(&self) {
        let map = self.failed_assets_by_node.lock().unwrap();
        let mut names: Vec<String> = map.values().cloned().collect();
        names.sort();
        names.dedup();
        drop(map);
        // Avoid spurious re-renders when the deduped list hasn't changed
        // (multiple nodes referencing the same broken file → one entry).
        if *self.missing_assets.lock_ref() != names {
            self.missing_assets.set(names);
        }
    }

    /// Capture the current scene into a snapshot. Used by history commit
    /// and Save. Includes the imported custom-material list so undo /
    /// redo / save / reload all preserve it.
    pub fn snapshot_scene(&self) -> SceneSnapshot {
        crate::scene::snapshot::capture(&self.scene, &self.custom_materials)
    }

    /// Push the pre-mutation snapshot onto the undo stack. Call *before*
    /// you apply a mutation; then call `bump_revision` on the scene after.
    pub fn commit_history(&self, previous: SceneSnapshot) {
        self.history.lock().unwrap().commit(previous);
        self.refresh_history_signals();
        self.mark_dirty();
    }

    pub fn mark_dirty(&self) {
        self.dirty.set_neq(true);
        self.project.lock().unwrap().dirty = true;
    }

    pub fn mark_clean(&self) {
        self.dirty.set_neq(false);
        self.project.lock().unwrap().dirty = false;
    }

    pub fn refresh_history_signals(&self) {
        let history = self.history.lock().unwrap();
        self.can_undo.set_neq(history.can_undo());
        self.can_redo.set_neq(history.can_redo());
    }

    pub fn refresh_selection_signal(&self) {
        let has = !self.selected.lock_ref().is_empty();
        self.has_selection.set_neq(has);
    }

    pub fn clear_selection(&self) {
        self.selected.lock_mut().clear();
        self.selection_anchor.set(None);
        self.selection_is_explicit.set_neq(false);
        self.refresh_selection_signal();
    }

    /// Replace the selection with a single node from an explicit user
    /// action — tree click, viewport pick, keyboard nav. Marks the
    /// selection as explicit so future inserts treat the node as their
    /// parent.
    pub fn select_only(&self, id: NodeId) {
        {
            let mut set = self.selected.lock_mut();
            set.clear();
            set.insert(id);
        }
        self.selection_anchor.set(Some(id));
        self.selection_is_explicit.set_neq(true);
        self.refresh_selection_signal();
    }

    /// Same as `select_only`, but for selections triggered by an
    /// internal action (currently: `Insert` auto-selecting the new
    /// node so the user can see what they just added). Marks the
    /// selection as *implicit* so the next `Insert` doesn't nest its
    /// new node under this one.
    pub fn select_only_implicit(&self, id: NodeId) {
        {
            let mut set = self.selected.lock_mut();
            set.clear();
            set.insert(id);
        }
        self.selection_anchor.set(Some(id));
        self.selection_is_explicit.set_neq(false);
        self.refresh_selection_signal();
    }

    /// Toggle a node's membership in the selection (Ctrl/Cmd+click).
    pub fn toggle_selection(&self, id: NodeId) {
        let now_selected = {
            let mut set = self.selected.lock_mut();
            if set.contains(&id) {
                set.remove(&id);
                false
            } else {
                set.insert(id);
                true
            }
        };
        self.selection_anchor
            .set(if now_selected { Some(id) } else { None });
        self.selection_is_explicit.set_neq(true);
        self.refresh_selection_signal();
    }

    /// Replace the selection with an explicit set (used by Shift+click range).
    pub fn set_selection<I: IntoIterator<Item = NodeId>>(&self, ids: I, anchor: Option<NodeId>) {
        {
            let mut set = self.selected.lock_mut();
            set.clear();
            set.extend(ids);
        }
        self.selection_anchor.set(anchor);
        self.selection_is_explicit.set_neq(true);
        self.refresh_selection_signal();
    }

    // ---- tree drag helpers ----

    pub fn begin_tree_drag(&self, ids: Vec<NodeId>) {
        *self.tree_drag_ids.lock().unwrap() = ids;
        self.tree_drag_target.set(None);
        self.tree_is_dragging.set_neq(true);
    }

    /// Returns `(dragged_ids, final_target)` and clears drag state.
    pub fn end_tree_drag(&self) -> (Vec<NodeId>, Option<(NodeId, DropZone)>) {
        let ids = std::mem::take(&mut *self.tree_drag_ids.lock().unwrap());
        let target = self.tree_drag_target.get_cloned();
        self.tree_drag_target.set(None);
        self.tree_is_dragging.set_neq(false);
        (ids, target)
    }

    pub fn dragged_node_ids(&self) -> Vec<NodeId> {
        self.tree_drag_ids.lock().unwrap().clone()
    }

    /// Signal producing the drop zone for `node_id` iff the drag target
    /// currently lands on that row. Rows use this to draw the indicator.
    pub fn tree_drop_zone_signal(&self, node_id: NodeId) -> impl Signal<Item = Option<DropZone>> {
        self.tree_drag_target.signal_ref(move |target| {
            target
                .as_ref()
                .filter(|(id, _)| *id == node_id)
                .map(|(_, zone)| *zone)
        })
    }

    /// True if the current selection contains at least one node `Split`
    /// can act on — i.e. a `Model` whose underlying gltf has >1 mesh
    /// primitives at its `node_index`, and which isn't already pinned to
    /// a single primitive via `ModelRef::primitive_index`.
    ///
    /// Driven by selection changes + bridge `nodes_revision` (which bumps
    /// when a Model finishes loading and `model_meshes` becomes known).
    pub fn can_split_signal(&self) -> impl Signal<Item = bool> {
        let bridge = self.renderer_bridge.clone();
        let bridge_for_compute = bridge.clone();
        map_ref! {
            // We don't actually need the ids out — we just want a tick
            // each time the selection changes. The compute path reads the
            // live selection back from `app_state()`.
            let _selected_len = self.selected.signal_ref(|set| set.len()),
            let _rev = bridge.nodes_revision.signal() => {
                compute_can_split(&bridge_for_compute)
            }
        }
    }
}

thread_local! {
    static APP_STATE: OnceCell<Arc<AppState>> = const { OnceCell::new() };
}

pub fn init() {
    APP_STATE.with(|cell| {
        let state = Arc::new(AppState::new());
        let _ = cell.set(state);
    });
    wire_document_title();
}

/// Keep `document.title` in sync with the active project name + dirty flag.
fn wire_document_title() {
    use futures_signals::signal::SignalExt;
    wasm_bindgen_futures::spawn_local(async move {
        let state = app_state();
        map_ref! {
            let name = state.project_name.signal_cloned(),
            let dirty = state.dirty.signal() => {
                let base = match name.as_ref() {
                    Some(n) if !n.is_empty() => format!("{n} — awsm scene editor"),
                    _ => "awsm scene editor".to_string(),
                };
                if *dirty { format!("• {base}") } else { base }
            }
        }
        .for_each(|title| {
            if let Some(document) = web_sys::window().and_then(|w| w.document()) {
                document.set_title(&title);
            }
            async {}
        })
        .await;
    });
}

#[allow(dead_code)] // Sync-access variant; `app_state()` covers most call sites today.
pub fn with_app_state<T>(f: impl FnOnce(&AppState) -> T) -> T {
    APP_STATE.with(|cell| {
        let state = cell.get().expect("AppState accessed before init()");
        f(state)
    })
}

/// Same as `with_app_state`, but returns an owned `Arc<AppState>` so the
/// caller can hold it across `await` points.
pub fn app_state() -> Arc<AppState> {
    APP_STATE.with(|cell| cell.get().expect("AppState accessed before init()").clone())
}

/// Walk the live selection + bridge state and decide whether `Split`
/// has any work to do. Pulled out of `can_split_signal` so the body
/// can use early `return`s without confusing the `map_ref!` macro.
fn compute_can_split(bridge: &Arc<Bridge>) -> bool {
    let state = app_state();
    let selected: Vec<NodeId> = state.selected.lock_ref().iter().copied().collect();
    if selected.is_empty() {
        return false;
    }
    let nodes = bridge.nodes.lock().unwrap();
    selected.iter().any(|id| {
        let Some(entry) = nodes.get(id) else {
            return false;
        };
        // Already pinned to one primitive → not splittable further.
        let already_split = matches!(
            &*entry.node.kind.lock_ref(),
            crate::scene::NodeKind::Model(r) if r.primitive_index.is_some()
        );
        if already_split {
            return false;
        }
        entry.model_meshes.lock().unwrap().len() > 1
    })
}
