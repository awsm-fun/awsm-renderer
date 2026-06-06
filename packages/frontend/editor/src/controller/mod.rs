//! `EditorController` — the single command/query authority (decision 8 / §5.5).
//!
//! All editor/project state is governed here. The UI is just one driver: event
//! handlers translate gestures → [`EditorCommand`]s → [`EditorController::dispatch`];
//! they never mutate editor state directly. Non-transient commands record an
//! inverse and form the undo/redo log (command-sourcing). A serializable
//! [`EditorSnapshot`] read API exists for external inspection + headless tests.
//!
//! A future MCP/websocket transport is a thin adapter over `dispatch`/`snapshot`
//! — designed for now (the URL load/import command variants + source seam), not
//! built now.

pub mod animation;
mod command;
pub mod custom_material;
mod node_spec;
pub mod persistence;
pub mod query;
mod source;

// The animation model + transport/mixer doc types. Several are consumed only by
// the Animation-mode UI panels (M-A2+); re-exported now so the contract is
// reachable + the command/query/persistence layers use them.
#[allow(unused_imports)]
pub use animation::{
    AnimSel, AnimView, ClipDirection, ClipLoop, CustomAnimation, Interp, MixerDoc, SamplerKind,
    StepKind, Track, TrackTarget, TrackValue,
};
pub use command::{CameraAxis, EditorCommand, EditorMode, ProceduralKind};
pub use custom_material::{compile_wgsl, AlphaMode, CustomMaterial, Slot};
// InsertSpec is dispatched by the ribbon (M4); NodeQuery is the snapshot
// projection — re-exported now for those consumers.
#[allow(unused_imports)]
pub use node_spec::{InsertSpec, NodeQuery, NodeSpec};
pub use query::{EditorSnapshot, ProjectSnapshot};
// The query read-surface (§6.8) — consumed by the `editor_query_json` wasm seam
// + the future MCP transport.
#[allow(unused_imports)]
pub use query::{EditorQuery, QueryResult, ReadbackTarget};
// The source/sink seam is wired into the loader/saver in M11; re-export now so
// the contract is reachable + documented.
#[allow(unused_imports)]
pub use source::{AssetSource, ProjectSink, ProjectSource};

use std::cell::{OnceCell, RefCell};
use std::rc::Rc;

use awsm_web_shared::prelude::{Mutable, MutableVec, Toast};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::spawn_local;

use self::animation::{find_clip, CustomAnimation as CA};
use self::custom_material::{find_material, CustomMaterial as CM};
use crate::engine::scene::{mutate, AssetId, NodeId, NodeKind, Scene};
use crate::error::EditorResult;
use awsm_scene_schema::{
    AssetEntry, AssetSource as SceneAssetSource, MaterialDef, ProceduralTextureDef, TextureDef,
};
use std::sync::Arc;

thread_local! {
    static CONTROLLER: OnceCell<EditorController> = const { OnceCell::new() };
    /// The cross-tab relay channel (§9). `None` until `init`, or if the browser
    /// lacks `BroadcastChannel` (cross-tab then simply disabled — the editor still
    /// works). Every non-tab-local dispatched command is posted here; other tabs
    /// apply it. `BroadcastChannel` does not deliver to the posting context, so
    /// there is no echo to guard against.
    static SYNC_CHANNEL: RefCell<Option<web_sys::BroadcastChannel>> = const { RefCell::new(None) };
}

/// Install the controller singleton. Call once at boot, before mounting the UI.
pub fn init() {
    CONTROLLER.with(|c| {
        let _ = c.set(EditorController::new());
    });
    init_cross_tab_sync();
}

/// Wire the cross-tab relay (§9): a `BroadcastChannel` whose incoming commands
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
    /// The custom WGSL materials authored in the Material-mode Studio (decision
    /// 3). Reactive — the Studio edits their bodies/slots live.
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
    /// Solo-subtree focus: only tracks under this node advance (decision #6).
    pub anim_solo_root: Mutable<Option<NodeId>>,
    /// The selected timeline element (track / keyframe).
    pub anim_selection: Mutable<Option<AnimSel>>,
    /// The NLA mixer document (layers / strips / masks / weights, by clip id).
    pub anim_mixer: Mutable<MixerDoc>,
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
}

/// Editor view-only settings (viewport toggles + units). Reactive; each field is
/// a shared `Mutable`. Not persisted into the project file.
#[derive(Clone)]
pub struct Settings {
    pub grid: Mutable<bool>,
    pub gizmo: Mutable<bool>,
    pub msaa: Mutable<bool>,
    pub heatmap: Mutable<bool>,
    pub snap: Mutable<bool>,
    pub units: Mutable<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            grid: Mutable::new(true),
            gizmo: Mutable::new(true),
            msaa: Mutable::new(true),
            heatmap: Mutable::new(false),
            snap: Mutable::new(false),
            units: Mutable::new("meters".to_string()),
        }
    }
}

impl EditorController {
    fn new() -> Self {
        Self {
            scene: Scene::new(),
            selected: Mutable::new(Vec::new()),
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
            anim_view: Mutable::new(AnimView::default()),
            cmdk_open: Mutable::new(false),
            settings: Settings::default(),
            settings_open: Mutable::new(false),
            undo: Rc::new(RefCell::new(Vec::new())),
            redo: Rc::new(RefCell::new(Vec::new())),
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
        // broadcast — a second window keeps its own view (§9).
        if cmd.is_tab_local() {
            return;
        }
        let payload = serde_json::to_string(cmd).unwrap_or_else(|_| format!("{cmd:?}"));
        tracing::info!("broadcasting {payload}");
        SYNC_CHANNEL.with(|c| {
            if let Some(bc) = c.borrow().as_ref() {
                let _ = bc.post_message(&JsValue::from_str(&payload));
            }
        });
    }

    /// Apply a command that arrived from ANOTHER tab via the cross-tab relay
    /// (§9). Goes straight to `apply` — the replay path: it does NOT re-broadcast
    /// (only `dispatch` broadcasts) and does NOT record undo (the inverse is
    /// discarded), so a relayed edit isn't mistaken for a fresh local one.
    async fn apply_remote(&self, cmd: EditorCommand) -> EditorResult<()> {
        let _ = self.apply(cmd).await?;
        Ok(())
    }

    /// Apply a command's effect and return its inverse (for the undo log), or
    /// `None` if the command is not undoable. The undoable per-node mutation
    /// commands return `Some(inverse)` here as they land in M4+.
    async fn apply(&self, cmd: EditorCommand) -> EditorResult<Option<EditorCommand>> {
        match cmd {
            EditorCommand::SwitchMode { mode } => {
                self.mode.set_neq(mode);
                Ok(None)
            }
            EditorCommand::SetSelection { ids } => {
                self.selected.set(ids);
                Ok(None)
            }
            EditorCommand::SetKind { id, kind } => match mutate::find_by_id(&self.scene, id) {
                Some(node) => {
                    let prev = node.kind.get_cloned();
                    if structure_key(&prev) != structure_key(&kind) {
                        self.structure_rev
                            .set(self.structure_rev.get().wrapping_add(1));
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
                self.scene.bump_revision();
                self.project_name.set("untitled.awsm".to_string());
                self.missing_assets.set(Vec::new());
                self.dirty.set_neq(false);
                self.undo.borrow_mut().clear();
                self.redo.borrow_mut().clear();
                self.refresh_history_signals();
                Toast::info("New project");
                Ok(None)
            }
            EditorCommand::Insert { spec, parent } => {
                let node = spec.build();
                let id = node.id;
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
                let arc = node.to_node();
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
                        let spec = NodeSpec::from_node(&node);
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
            EditorCommand::AddMaterialAsset { shading } => {
                let id = AssetId::new();
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
            EditorCommand::AddTextureAsset { proc } => {
                let id = AssetId::new();
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
            EditorCommand::SetAssetSelection { id } => {
                self.asset_selection.set(id);
                Ok(None)
            }
            EditorCommand::AddCustomMaterial => {
                let id = AssetId::new();
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
            EditorCommand::AddBuiltinMaterial { shading } => {
                let id = AssetId::new();
                let n = self.custom_materials.lock_ref().len() + 1;
                let label = match shading {
                    awsm_scene_schema::MaterialShading::Pbr => "PBR",
                    awsm_scene_schema::MaterialShading::Unlit => "Unlit",
                    awsm_scene_schema::MaterialShading::Toon { .. } => "Toon",
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
                    let errs = compile_wgsl(&mat.wgsl.get_cloned());
                    if !errs.is_empty() {
                        Toast::error(format!(
                            "Can't register \u{2014} {} compile error(s).",
                            errs.len()
                        ));
                    } else {
                        let was = mat.registered.get();
                        let name = mat.name.get_cloned();
                        // Real GPU registration: compile the material into a
                        // renderer bucket. On success flag it registered + re-
                        // materialize any mesh it's assigned to so it renders.
                        match crate::engine::bridge::dynamic::register(&mat).await {
                            Ok(_) => {
                                mat.registered.set_neq(true);
                                crate::engine::bridge::rematerialize_for_material(mat.id);
                                Toast::info(if was {
                                    format!("Recompiled \u{201c}{name}\u{201d} \u{2014} bucket refreshed.")
                                } else {
                                    format!("Registered \u{201c}{name}\u{201d}.")
                                });
                            }
                            Err(e) => Toast::error(format!("Register failed: {e}")),
                        }
                    }
                }
                Ok(None)
            }
            EditorCommand::AssignMaterial { node, material } => {
                match mutate::find_by_id(&self.scene, node) {
                    Some(n) => {
                        let prev = n.kind.get_cloned();
                        // Id-keyed assignment: store the material's stable id (so
                        // renaming it never orphans this mesh). Validate the id
                        // exists in the custom-material list.
                        let instance = material
                            .filter(|id| find_material(&self.custom_materials, *id).is_some())
                            .map(|id| awsm_scene_schema::CustomMaterialInstance {
                                material: id,
                                uniform_overrides: Default::default(),
                                texture_overrides: Default::default(),
                                buffer_overrides: Default::default(),
                            });
                        // Assigning a material adopts its *defaults* (the full
                        // uniform surface — factors, extension params, Toon knobs,
                        // cutoff) into this mesh's inline store, so the mesh starts
                        // looking like the material; the user then customizes
                        // per-mesh from there. (A dynamic material has no built-in
                        // defaults → keep the existing inline, which it ignores.)
                        let seeded_inline = instance.as_ref().and_then(|inst| {
                            find_material(&self.custom_materials, inst.material)
                                .and_then(|m| m.builtin.get_cloned())
                        });
                        let next = match prev.clone() {
                            NodeKind::Primitive {
                                shape,
                                material: mref,
                                inline_material,
                                shadow,
                                ..
                            } => NodeKind::Primitive {
                                shape,
                                material: mref,
                                inline_material: seeded_inline.unwrap_or(inline_material),
                                custom_material: instance,
                                shadow,
                            },
                            // A Model node carries one assigned material (the
                            // same model as a Primitive); `None` = unassigned →
                            // magenta.
                            NodeKind::Model(mut r) => {
                                if let Some(inline) = seeded_inline {
                                    r.inline_material = inline;
                                }
                                r.material = instance;
                                NodeKind::Model(r)
                            }
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
                let NodeKind::Primitive {
                    inline_material: src_inline,
                    custom_material: src_cm,
                    ..
                } = src.kind.get_cloned()
                else {
                    return Ok(None);
                };
                let prev = dst.kind.get_cloned();
                let NodeKind::Primitive {
                    shape,
                    material,
                    custom_material: dst_cm,
                    shadow,
                    ..
                } = prev.clone()
                else {
                    return Ok(None);
                };
                // Only copy between meshes that reference the same material.
                if src_cm.as_ref().map(|i| i.material) != dst_cm.as_ref().map(|i| i.material) {
                    return Ok(None);
                }
                dst.kind.set(NodeKind::Primitive {
                    shape,
                    material,
                    inline_material: src_inline,
                    custom_material: dst_cm,
                    shadow,
                });
                self.structure_rev
                    .set(self.structure_rev.get().wrapping_add(1));
                self.scene.bump_revision();
                Ok(Some(EditorCommand::SetKind {
                    id: to,
                    kind: Box::new(prev),
                }))
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
            EditorCommand::ImportTextureFromUrl { url } => {
                Toast::info(format!("Import texture from {url} — lands in M11"));
                Ok(None)
            }
            // ───────────────────── Animation: clip lifecycle ─────────────────
            EditorCommand::AddClip { id } => {
                // Idempotent: a cross-tab relay (§9) replays this; if the clip id
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
        use awsm_scene_schema::MaterialShading;

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
        crate::engine::bridge::bridge().insert_template(asset_id, template.clone());

        // glTF primitives with no material use glTF's default material — white,
        // metallic 1.0, roughness 1.0 (NOT the editor's magenta sentinel, which is
        // for deliberately-unassigned meshes). Create one shared "Default"
        // library material iff the model actually has unmaterialed primitives.
        let default_mat_id = if template.roots.iter().any(template_needs_default_material) {
            let id = AssetId::new();
            let def = awsm_scene_schema::MaterialDef {
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
        for root in &template.roots {
            let node = build_editor_subtree(
                root,
                asset_id,
                &mat_ids,
                default_mat_id,
                Some(&import.display_name),
                &mut node_map,
            );
            mutate::insert_under(&self.scene, None, node);
        }
        self.scene.bump_revision();
        self.dirty.set_neq(true);

        // Convert each extracted glTF animation → a library clip bound to the
        // freshly-instantiated nodes (channels for un-instantiated nodes skip).
        let clip_count = self.import_animations(&import.animations, &node_map);

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
            let name = anim
                .name
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| format!("Animation {}", anim_i + 1));
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

    /// A serializable read of editor state (§5.5) for external inspection.
    pub fn snapshot(&self) -> EditorSnapshot {
        let scene_tree = self
            .scene
            .nodes
            .lock_ref()
            .iter()
            .map(|n| NodeSpec::from_node(n).to_query())
            .collect();
        EditorSnapshot {
            mode: self.mode.get(),
            project: ProjectSnapshot {
                name: self.project_name.get_cloned(),
                dirty: self.dirty.get(),
                missing_assets: self.missing_assets.get_cloned(),
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
                .map(|m| query::MaterialSnapshot {
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
                })
                .collect(),
        }
    }

    /// The Animation-mode projection of `snapshot()` (§6.2).
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

    /// Run a read-only [`EditorQuery`] (§6.8) and return a serializable result.
    /// Read-only: never mutates persisted state, never records undo, never
    /// broadcasts; the pinning handler saves + restores the transport.
    pub async fn query(&self, q: query::EditorQuery) -> query::QueryResult {
        use query::*;
        match q {
            EditorQuery::Snapshot => QueryResult::Snapshot(self.snapshot()),
            EditorQuery::SampleClipTimeseries {
                clip,
                times,
                targets,
            } => self.sample_clip_timeseries(clip, times, targets).await,
            EditorQuery::CanvasPixels { coords } => {
                match crate::engine::query::canvas_pixels(&coords) {
                    Ok(pixels) => QueryResult::Pixels(PixelsResult { pixels }),
                    Err(e) => QueryResult::Error { error: e },
                }
            }
            EditorQuery::CanvasStats { region } => {
                match crate::engine::query::canvas_stats(region) {
                    Ok(s) => QueryResult::Stats(s),
                    Err(e) => QueryResult::Error { error: e },
                }
            }
        }
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
            let mesh = crate::engine::bridge::bridge()
                .nodes
                .lock()
                .unwrap()
                .get(node)
                .and_then(|n| n.model_meshes.lock().unwrap().first().copied());
            let weight = mesh
                .and_then(|mesh| r.meshes.geometry_morph_key_for_mesh(mesh))
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

/// Compile + register a dynamic material into a renderer bucket, then
/// re-materialize meshes using it. Returns true on success; leaves
/// `registered = false` on a compile error (the code pane surfaces the problems).
async fn register_material(mat: &Arc<CM>) -> bool {
    if !compile_wgsl(&mat.wgsl.get_cloned()).is_empty() {
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
        Ok(_) => {
            mat.registered.set_neq(true);
            crate::engine::bridge::rematerialize_for_material(mat.id);
            true
        }
        Err(e) => {
            Toast::error(format!("Material compile failed: {e}"));
            mat.registered.set_neq(false);
            false
        }
    }
}

/// Auto-register a dynamic material: compile it now, then re-compile (debounced
/// ~400 ms) on any WGSL edit — so it's always live without a manual Register step.
fn spawn_auto_register(mat: Arc<CM>) {
    use futures_signals::signal::SignalExt;
    let first_mat = mat.clone();
    spawn_local(async move {
        // A fresh material must come up READY (not "draft"). Compile now; if the
        // very first attempt fails (e.g. the renderer's pipeline scheduler is still
        // warming up on a cold load), retry a few times so it doesn't get stuck as
        // a draft requiring a manual edit to recompile.
        for attempt in 0..4 {
            if register_material(&first_mat).await {
                break;
            }
            if attempt < 3 {
                gloo_timers::future::TimeoutFuture::new(300).await;
            }
        }
    });
    spawn_local(async move {
        let gen = std::rc::Rc::new(std::cell::Cell::new(0u64));
        let sig = mat.wgsl.signal_cloned();
        let mut first = true;
        sig.for_each(move |_| {
            let fire = !first;
            first = false;
            let g = gen.get().wrapping_add(1);
            gen.set(g);
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
            }
        })
        .await;
    });
}

/// Re-materialize meshes using a **built-in** material whenever its shared
/// variant settings change (node_sync re-merges the variant with each mesh's
/// per-mesh uniforms).
fn spawn_builtin_resync(mat: Arc<CM>) {
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
    ext: &awsm_scene_schema::PbrExtensions,
    slot: &str,
) -> Option<awsm_scene_schema::TextureRef> {
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
    ext: &mut awsm_scene_schema::PbrExtensions,
    slot: &str,
    tref: Option<awsm_scene_schema::TextureRef>,
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
) -> Option<awsm_scene_schema::TextureRef> {
    let (key, binding) = baked?;
    // The texture-asset id is deduped by baked key, but the binding (UV set +
    // transform) is per-slot, so it goes on the TextureRef, not the asset.
    let mk = |asset: AssetId| awsm_scene_schema::TextureRef {
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

/// Recursively mirror one glTF template node as an editor `Node`. Mesh-bearing
/// nodes become `Model` nodes (which duplicate the template's meshes under
/// their own transform); pure transform/bone nodes become `Group`s. The local
/// transform is carried over so the reconstructed hierarchy matches the glTF.
/// `fallback_name` only labels an unnamed *top-level* node (so a single-root
/// import shows the file name); children fall back to `Node {index}`.
fn build_editor_subtree(
    tn: &crate::engine::bridge::asset_template::AssetTemplateNode,
    asset_id: AssetId,
    mat_ids: &[AssetId],
    default_mat_id: Option<AssetId>,
    fallback_name: Option<&str>,
    node_map: &mut std::collections::HashMap<u32, NodeId>,
) -> Arc<crate::engine::scene::node::Node> {
    use crate::engine::scene::node::Node;
    use awsm_scene_schema::{dynamic_material::CustomMaterialInstance, MaterialDef, ModelRef, Trs};

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
    let instance_for = |mi: Option<usize>| -> Option<CustomMaterialInstance> {
        // A primitive's glTF material index → its library material; a primitive
        // with NO material (`None`) uses glTF's default material (white,
        // metallic=1, roughness=1) rather than the editor's magenta sentinel.
        let id = match mi {
            Some(i) => mat_ids.get(i).copied(),
            None => default_mat_id,
        };
        id.map(|id| CustomMaterialInstance {
            material: id,
            ..Default::default()
        })
    };
    // The per-mesh inline store, seeded as a *clone of the assigned material's
    // defaults*. `builtin_merged` then layers its uniform-class fields (factors,
    // extension params, Toon knobs, mask cutoff) over the shared variant, so
    // editing it customizes this node without touching the shared material.
    let inline_for = |inst: &Option<CustomMaterialInstance>| -> MaterialDef {
        inst.as_ref()
            .and_then(|i| {
                crate::controller::custom_material::find_material(
                    &controller().custom_materials,
                    i.material,
                )
            })
            .and_then(|m| m.builtin.get_cloned())
            .unwrap_or_default()
    };

    let node = if tn.mesh_keys.is_empty() {
        Node::new_with_transform_and_kind(name, trs, NodeKind::Group)
    } else {
        // With one material per node, a node whose primitives all share a
        // material (the common case) maps 1:1. A node whose primitives use
        // *different* materials is destructured: a Group keeps the transform +
        // glTF children, and one Model child per primitive carries its own
        // `primitive_index` + assigned material.
        let mat_indices = &tn.mesh_gltf_material_indices;
        let distinct: std::collections::BTreeSet<Option<usize>> =
            mat_indices.iter().copied().collect();
        if distinct.len() <= 1 {
            let material = instance_for(mat_indices.first().copied().flatten());
            let inline_material = inline_for(&material);
            Node::new_with_transform_and_kind(
                name,
                trs,
                NodeKind::Model(ModelRef {
                    asset_id,
                    node_index: tn.gltf_node_index,
                    primitive_index: None,
                    material,
                    inline_material,
                    shadow: Default::default(),
                }),
            )
        } else {
            let group = Node::new_with_transform_and_kind(name.clone(), trs, NodeKind::Group);
            for (i, mi) in mat_indices.iter().enumerate() {
                let material = instance_for(*mi);
                let inline_material = inline_for(&material);
                let part_label = material
                    .as_ref()
                    .and_then(|inst| {
                        crate::controller::custom_material::find_material(
                            &controller().custom_materials,
                            inst.material,
                        )
                        .map(|m| m.name.get_cloned())
                    })
                    .unwrap_or_else(|| format!("{name} · part {i}"));
                group
                    .children
                    .lock_mut()
                    .push_cloned(Node::new_with_transform_and_kind(
                        part_label,
                        Trs::IDENTITY,
                        NodeKind::Model(ModelRef {
                            asset_id,
                            node_index: tn.gltf_node_index,
                            primitive_index: Some(i as u32),
                            material,
                            inline_material,
                            shadow: Default::default(),
                        }),
                    ));
            }
            group
        }
    };

    // Record this glTF node index → its minted editor `NodeId`, so imported
    // animation channels (keyed by glTF node index) can resolve their target.
    // For a destructured multi-material node, the transform-bearing Group keeps
    // the glTF index (its Model-child parts are unindexed primitive splits).
    node_map.insert(tn.gltf_node_index, node.id);

    for child in &tn.children {
        node.children.lock_mut().push_cloned(build_editor_subtree(
            child,
            asset_id,
            mat_ids,
            default_mat_id,
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
    use awsm_scene_schema::{CameraProjection, LightConfig, MaterialShading, PrimitiveShape};
    match kind {
        NodeKind::Primitive {
            shape,
            inline_material,
            custom_material,
            ..
        } => {
            let shp = match shape {
                PrimitiveShape::Plane { .. } => "plane",
                PrimitiveShape::Box { .. } => "box",
                PrimitiveShape::Sphere { .. } => "sphere",
                PrimitiveShape::Cylinder { .. } => "cylinder",
                PrimitiveShape::Cone { .. } => "cone",
                PrimitiveShape::Torus { .. } => "torus",
            };
            let shading = match inline_material.shading {
                MaterialShading::Pbr => "pbr",
                MaterialShading::Unlit => "unlit",
                MaterialShading::Toon { .. } => "toon",
            };
            format!("prim/{shp}/{shading}/{}", custom_material.is_some())
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
    use awsm_scene_schema::AssetId as Aid;
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
