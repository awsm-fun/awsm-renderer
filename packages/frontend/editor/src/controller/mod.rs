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
mod query;
mod source;

pub use command::{EditorCommand, EditorMode, ProceduralKind};
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
    /// The asset selected in the Content Browser, if any. When `Some`, the right
    /// rail shows the Asset Inspector instead of the node inspector. Set via the
    /// transient `SetAssetSelection` command.
    pub asset_selection: Mutable<Option<AssetId>>,
    /// The custom WGSL materials authored in the Material-mode Studio (decision
    /// 3). Reactive — the Studio edits their bodies/slots live.
    pub custom_materials: MutableVec<Arc<CM>>,
    /// The material the Studio is currently editing.
    pub current_material: Mutable<Option<AssetId>>,
    /// Inverses of applied commands, newest last (the undo log).
    undo: Rc<RefCell<Vec<EditorCommand>>>,
    /// Inverses popped by undo, re-appliable by redo.
    redo: Rc<RefCell<Vec<EditorCommand>>>,
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
            asset_selection: Mutable::new(None),
            custom_materials: MutableVec::new(),
            current_material: Mutable::new(None),
            undo: Rc::new(RefCell::new(Vec::new())),
            redo: Rc::new(RefCell::new(Vec::new())),
        }
    }

    /// The single entry point. UI handlers build a command and dispatch it here;
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
                self.custom_materials.lock_mut().push_cloned(mat);
                self.current_material.set(Some(id));
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
                    if errs.is_empty() {
                        let was = mat.registered.get();
                        mat.registered.set_neq(true);
                        let name = mat.name.get_cloned();
                        Toast::info(if was {
                            format!("Recompiled \u{201c}{name}\u{201d} \u{2014} bucket refreshed.")
                        } else {
                            format!("Registered \u{201c}{name}\u{201d}.")
                        });
                    } else {
                        Toast::error(format!(
                            "Can't register \u{2014} {} compile error(s).",
                            errs.len()
                        ));
                    }
                }
                Ok(None)
            }
            EditorCommand::LoadProjectFromUrl { base_url } => {
                // Seam present; the fetch + TOML deserialize lands in M11.
                Toast::info(format!("Load project from {base_url} — lands in M11"));
                Ok(None)
            }
            EditorCommand::ImportModelFromUrl { url } => {
                Toast::info(format!("Import model from {url} — lands in M4/M11"));
                Ok(None)
            }
            EditorCommand::ImportTextureFromUrl { url } => {
                Toast::info(format!("Import texture from {url} — lands in M11"));
                Ok(None)
            }
        }
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
