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

mod command;
pub mod custom_material;
mod node_spec;
pub mod persistence;
mod query;
mod source;

pub use command::{CameraAxis, EditorCommand, EditorMode, ProceduralKind};
pub use custom_material::{compile_wgsl, AlphaMode, CustomMaterial, Slot};
// InsertSpec is dispatched by the ribbon (M4); NodeQuery is the snapshot
// projection — re-exported now for those consumers.
#[allow(unused_imports)]
pub use node_spec::{InsertSpec, NodeQuery, NodeSpec};
pub use query::{EditorSnapshot, ProjectSnapshot};
// The source/sink seam is wired into the loader/saver in M11; re-export now so
// the contract is reachable + documented.
#[allow(unused_imports)]
pub use source::{AssetSource, ProjectSink, ProjectSource};

use std::cell::{OnceCell, RefCell};
use std::rc::Rc;

use awsm_web_shared::prelude::{Mutable, MutableVec, Toast};
use wasm_bindgen_futures::spawn_local;

use self::custom_material::{find_material, CustomMaterial as CM};
use crate::engine::scene::{mutate, AssetId, NodeId, NodeKind, Scene};
use crate::error::EditorResult;
use awsm_scene_schema::{
    AssetEntry, AssetSource as SceneAssetSource, MaterialDef, ProceduralTextureDef, TextureDef,
};
use std::sync::Arc;

thread_local! {
    static CONTROLLER: OnceCell<EditorController> = const { OnceCell::new() };
}

/// Install the controller singleton. Call once at boot, before mounting the UI.
pub fn init() {
    CONTROLLER.with(|c| {
        let _ = c.set(EditorController::new());
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
        let payload =
            serde_json::to_string(cmd).unwrap_or_else(|_| format!("{cmd:?}"));
        tracing::info!("broadcasting {payload}");
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
                        let NodeKind::Primitive {
                            shape,
                            material: mref,
                            inline_material,
                            shadow,
                            ..
                        } = prev.clone()
                        else {
                            return Ok(None);
                        };
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
                        n.kind.set(NodeKind::Primitive {
                            shape,
                            material: mref,
                            inline_material,
                            custom_material: instance,
                            shadow,
                        });
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
        }
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

        let mut tex_for_key: std::collections::HashMap<awsm_renderer::textures::TextureKey, AssetId> =
            std::collections::HashMap::new();
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
            let mut entry = AssetEntry::new(SceneAssetSource::Filename(import.display_name.clone()));
            entry.gltf_material_asset_ids = mat_ids.clone();
            entry.gltf_image_asset_ids = img_ids;
            table.entries.insert(id, entry);
            id
        };
        let template = Arc::new(import.template);
        crate::engine::bridge::bridge().insert_template(asset_id, template.clone());

        // Mirror the glTF hierarchy as editor nodes under the scene root.
        for root in &template.roots {
            let node = build_editor_subtree(root, asset_id, Some(&import.display_name));
            mutate::insert_under(&self.scene, None, node);
        }
        self.scene.bump_revision();
        self.dirty.set_neq(true);
        Toast::info(format!("Imported {}", import.display_name));
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
        }
    }

    /// `snapshot()` as a JSON string (the shape an MCP/websocket transport would
    /// return). Used by headless tests + the future external transport.
    pub fn snapshot_json(&self) -> String {
        serde_json::to_string(&self.snapshot()).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
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

/// Create (or dedupe) a texture asset for a baked glTF texture key and return a
/// `TextureRef` to it. The asset id is pre-registered against the already-baked
/// renderer `TextureKey`, so when the material resolves this slot it reuses the
/// GPU texture rather than re-decoding (preserving the model's real textures).
fn ensure_import_texture(
    tex_for_key: &mut std::collections::HashMap<awsm_renderer::textures::TextureKey, AssetId>,
    texture_entries: &mut Vec<(AssetId, String)>,
    key: Option<awsm_renderer::textures::TextureKey>,
    name: &str,
) -> Option<awsm_scene_schema::TextureRef> {
    let key = key?;
    if let Some(id) = tex_for_key.get(&key) {
        return Some(awsm_scene_schema::TextureRef(*id));
    }
    let id = AssetId::new();
    crate::engine::bridge::material::register_texture_key(id, key);
    tex_for_key.insert(key, id);
    texture_entries.push((id, name.to_string()));
    Some(awsm_scene_schema::TextureRef(id))
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
    fallback_name: Option<&str>,
) -> Arc<crate::engine::scene::node::Node> {
    use crate::engine::scene::node::Node;
    use awsm_scene_schema::ModelRef;

    let name = tn.label.clone().unwrap_or_else(|| {
        fallback_name
            .map(str::to_string)
            .unwrap_or_else(|| format!("Node {}", tn.gltf_node_index))
    });

    let kind = if tn.mesh_keys.is_empty() {
        NodeKind::Group
    } else {
        NodeKind::Model(ModelRef {
            asset_id,
            node_index: tn.gltf_node_index,
            primitive_index: None,
            shadow: Default::default(),
        })
    };

    let trs = crate::engine::bridge::asset_template::transform_to_trs(&tn.local);
    let node = Node::new_with_transform_and_kind(name, trs, kind);

    for child in &tn.children {
        node.children
            .lock_mut()
            .push_cloned(build_editor_subtree(child, asset_id, None));
    }
    node
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

/// A coalescing key for continuous edits — consecutive commands with the same
/// key collapse into one undo step. `None` = never coalesce.
fn coalesce_key(cmd: &EditorCommand) -> Option<(u8, NodeId)> {
    match cmd {
        EditorCommand::SetTransform { id, .. } => Some((0, *id)),
        EditorCommand::Rename { id, .. } => Some((1, *id)),
        EditorCommand::SetKind { id, .. } => Some((2, *id)),
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
