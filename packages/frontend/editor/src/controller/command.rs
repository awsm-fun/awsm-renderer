//! `EditorCommand` — the single serializable enum covering every editor
//! mutation. The UI never mutates editor state directly; it
//! builds a command and dispatches it through the [`super::EditorController`].
//! Commands are **data** (no closures) so they serialize, and non-transient
//! ones are invertible — the inverse is captured at apply-time and pushed onto
//! the undo log (command-sourcing, replacing the old snapshot history).

use serde::{Deserialize, Serialize};

use super::animation::{
    AnimSel, AnimView, ClipDirection, ClipLoop, Interp, LayerModeDoc, SamplerKind, StepKind,
    TrackTarget, TrackValue,
};
use super::node_spec::{InsertSpec, NodeSpec};
use crate::engine::scene::types::Trs;
use crate::engine::scene::{AssetId, EnvironmentConfig, NodeId, NodeKind};
use awsm_scene_schema::{AssetEntry, MaterialShading};

/// A procedural texture generator the Content Browser can author.
/// Maps to `ProceduralTextureDef` with sensible defaults at apply-time.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProceduralKind {
    Checker,
    Gradient,
    Noise,
}

/// A world axis to snap the viewport camera to (the nav-cube directions). The
/// camera ends up on that axis looking back at the orbit target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraAxis {
    PosX,
    NegX,
    PosY,
    NegY,
    PosZ,
    NegZ,
}

/// Top-level workspace mode (the Scene/Material switch in the top bar).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditorMode {
    #[default]
    Scene,
    Material,
    Animation,
}

/// Every editor mutation, as serializable data.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum EditorCommand {
    /// Switch the workspace mode. **Transient** — dispatched but not recorded in
    /// the undo log.
    SwitchMode { mode: EditorMode },

    /// Set the current selection (ordered; last = primary/anchor). **Transient**
    /// — the UI computes single/ctrl-toggle/shift-range and dispatches the
    /// resulting set.
    SetSelection { ids: Vec<NodeId> },

    /// Replace a node's kind config (per-kind property edits — light color/
    /// intensity, geometry params, camera fov, …). The bridge re-materializes on
    /// kind change, so geometry/material edits update live. Boxed (NodeKind is
    /// the largest payload). Inverse: restore the prior kind. Coalesces.
    SetKind { id: NodeId, kind: Box<NodeKind> },

    /// Set a node's local transform (TRS). Inverse: restore the prior transform.
    /// Consecutive `SetTransform`s on the same node coalesce into one undo step
    /// (so a drag-scrub is a single undo).
    SetTransform { id: NodeId, transform: Trs },

    /// Rename a node. Inverse: rename back to the prior name.
    Rename { id: NodeId, name: String },

    /// Set a node's visibility (eye toggle). Inverse: restore prior value.
    SetVisible { id: NodeId, visible: bool },

    /// Set a node's locked flag. Inverse: restore prior value.
    SetLocked { id: NodeId, locked: bool },

    /// Set a node's prefab-root flag. Inverse: restore prior value.
    SetPrefab { id: NodeId, prefab: bool },

    /// Duplicate a node (deep clone, fresh ids) as a following sibling. Inverse:
    /// delete the clone.
    Duplicate { id: NodeId },

    /// Reparent a node under `new_parent` at `index` (root when `None`).
    /// Inverse: reparent back to its prior parent + index.
    Reparent {
        id: NodeId,
        new_parent: Option<NodeId>,
        index: Option<usize>,
    },

    /// Start a fresh, empty project.
    NewProject,

    /// Insert a fresh node (from a ribbon Insert action) under `parent` (root
    /// when `None`). Inverse: `Delete` of the new node.
    Insert {
        spec: InsertSpec,
        parent: Option<NodeId>,
    },

    /// Re-insert a captured node subtree at `index` under `parent` (preserving
    /// ids). This is the inverse of `Delete` — undoing a delete restores the
    /// exact subtree. `node` is boxed (it's the largest variant payload).
    InsertTree {
        node: Box<NodeSpec>,
        parent: Option<NodeId>,
        index: Option<usize>,
    },

    /// Remove the node with `id` (and its subtree). Inverse: `InsertTree` of the
    /// captured subtree at its original position.
    Delete { id: NodeId },

    /// Load a project from a base URL (gesture-free; fetches `<base>/project.toml`
    /// and the referenced material/asset files). The external/MCP + headless-test
    /// entry point. Full implementation is future work; the seam exists now.
    LoadProjectFromUrl { base_url: String },

    /// Import a glTF model from a URL (gesture-free). Pairs with the file-picker
    /// variant `ImportModelFromFile`.
    ImportModelFromUrl { url: String },

    /// Import a glTF model from a locally-picked file. `url` is a `blob:` object
    /// URL minted from the picked `File`; `name` is the real filename (used for
    /// `.glb`/`.gltf` type inference — blob URLs have no extension — and the
    /// Outliner label). Not serialized into project history (the blob URL is
    /// session-local); treated as transient for undo.
    ImportModelFromFile { name: String, url: String },

    /// Import a texture from a URL (gesture-free).
    ImportTextureFromUrl { url: String },

    /// Create a fresh custom material asset (Content Browser "+ Material") of the
    /// given shading family. Inserts a `MaterialDef` into the project asset table
    /// and selects it. Inverse: `DeleteAsset` of the new id.
    AddMaterialAsset { shading: MaterialShading },

    /// Create a fresh procedural texture asset (Content Browser "+ Texture").
    /// Inverse: `DeleteAsset` of the new id.
    AddTextureAsset { proc: ProceduralKind },

    /// Remove an asset from the project asset table. Inverse: `RestoreAsset` with
    /// the captured entry (so undo round-trips the exact asset + id).
    DeleteAsset { id: AssetId },

    /// Re-insert a captured asset entry at its original id (the inverse of
    /// `DeleteAsset`). `entry` is boxed — `AssetEntry` is a large payload.
    RestoreAsset { id: AssetId, entry: Box<AssetEntry> },

    /// Select an asset in the Content Browser (routes the right rail to the Asset
    /// Inspector). **Transient** — `None` clears back to the node inspector.
    SetAssetSelection { id: Option<AssetId> },

    /// Create a fresh custom WGSL (dynamic) material and make it the current
    /// Studio material. Auto-registers on create + on edit (no manual Register).
    AddCustomMaterial,

    /// Create a fresh **built-in** library material (PBR / Unlit / Toon) and make
    /// it current. Carries shared variant settings; per-mesh uniform values are
    /// set on each assigned mesh. Needs no compile.
    AddBuiltinMaterial { shading: MaterialShading },

    /// Delete a custom WGSL material.
    DeleteCustomMaterial { id: AssetId },

    /// Set the material the Studio is editing. **Transient**.
    SetCurrentMaterial { id: Option<AssetId> },

    /// Register (compile to a renderer bucket) the current custom material.
    /// Validates the WGSL and flips the `registered` flag; the real GPU
    /// registration + bucket accounting is future work.
    RegisterMaterial { id: AssetId },

    /// Set the scene environment (skybox + IBL). Stored in `scene.environment`
    /// (serialized to TOML); the `env_sync` bridge uploads the cubemaps as a
    /// side effect. Inverse: restore the prior environment.
    SetEnvironment { env: EnvironmentConfig },

    /// Snap the viewport camera to a world axis (the nav-cube directions).
    /// **Transient** — camera/view state, not recorded in the undo log.
    SnapCameraToAxis { axis: CameraAxis },

    /// Reset the viewport camera to its default framing ("Reset View").
    /// **Transient** — camera/view state, not recorded in the undo log.
    ResetCamera,

    /// Assign a custom WGSL material (by id) to a scene node's mesh, or clear it
    /// (`material: None`). Sets the node's `custom_material` reference. Inverse:
    /// restore the node's prior kind (a `SetKind`). The bridge renders the
    /// assigned material once it's registered with the renderer.
    AssignMaterial {
        node: NodeId,
        material: Option<AssetId>,
    },

    /// Copy a mesh's per-mesh material *instance* (its inline uniform values:
    /// base color / metallic / roughness / emissive / …) onto another mesh that
    /// references the **same** assigned material. Controller-only (no UI) — the
    /// MCP path for "paste these material settings onto that mesh". No-op when the
    /// two meshes don't share the same material. Inverse: restore `to`'s prior kind.
    CopyMaterialInstance { from: NodeId, to: NodeId },

    // ───────────────────────── Animation: clip lifecycle ─────────────────────
    /// Create a fresh empty animation clip and make it current. Lifecycle (no
    /// inverse recorded). **Carries its `id`** (minted by the dispatcher, not in
    /// `apply`) so the command is deterministic data — a cross-tab relay that
    /// replays it produces the *same* clip id in every tab. Idempotent: applying
    /// it when the id already exists is a no-op.
    AddClip { id: AssetId },
    /// Delete a clip from the library. Lifecycle.
    DeleteClip { id: AssetId },
    /// Duplicate a clip (deep copy, fresh id) and select it. Lifecycle.
    DuplicateClip { id: AssetId },
    /// Set the clip Animation mode is editing. **Transient**.
    SetCurrentClip { id: Option<AssetId> },

    // ───────────────────────── Animation: clip props ─────────────────────────
    /// Rename a clip. Inverse: rename back.
    RenameClip { id: AssetId, name: String },
    /// Set a clip's duration (seconds). Inverse: restore prior. Coalesces.
    SetClipDuration { id: AssetId, duration: f64 },
    /// Set a clip's loop style. Inverse: restore prior.
    SetClipLoop { id: AssetId, loop_style: ClipLoop },
    /// Set a clip's speed multiplier. Inverse: restore prior. Coalesces.
    SetClipSpeed { id: AssetId, speed: f64 },
    /// Set a clip's default play direction. Inverse: restore prior.
    SetClipDirection {
        id: AssetId,
        direction: ClipDirection,
    },
    /// Set a clip's library color (`#rrggbb`). Inverse: restore prior.
    SetClipColor { id: AssetId, color: String },

    // ───────────────────────── Animation: tracks ─────────────────────────────
    /// Add a track to a clip, bound to `target`. Inverse: `DeleteTrack`.
    AddTrack { clip: AssetId, target: TrackTarget },
    /// Delete a track (by index) from a clip. Inverse: re-insert the captured track.
    DeleteTrack { clip: AssetId, track: usize },
    /// Re-insert a captured track at its original index (the inverse of
    /// `DeleteTrack`). `track` is boxed (the full stored track is a large payload).
    RestoreTrack {
        clip: AssetId,
        index: usize,
        track: Box<super::animation::StoredTrack>,
    },
    /// Set a track's sampler kind. Inverse: restore prior.
    SetTrackSampler {
        clip: AssetId,
        track: usize,
        sampler: SamplerKind,
    },
    /// Set a track's mute flag. Inverse: restore prior.
    SetTrackMute {
        clip: AssetId,
        track: usize,
        mute: bool,
    },
    /// Set a track's solo flag. Inverse: restore prior.
    SetTrackSolo {
        clip: AssetId,
        track: usize,
        solo: bool,
    },

    // ───────────────────────── Animation: keyframes ──────────────────────────
    /// Insert a keyframe at time `t` (seconds) with `value` on a track (sorted by
    /// time; an existing key at `t` is replaced). Inverse: `DeleteKeyframe` /
    /// restore.
    AddKeyframe {
        clip: AssetId,
        track: usize,
        t: f64,
        value: TrackValue,
    },
    /// Delete a keyframe (by index). Inverse: `InsertKeyframe` of the captured key.
    DeleteKeyframe {
        clip: AssetId,
        track: usize,
        index: usize,
    },
    /// Re-insert a captured keyframe at its original index + time (the inverse of
    /// `DeleteKeyframe`). `key` is boxed.
    InsertKeyframe {
        clip: AssetId,
        track: usize,
        index: usize,
        t: f64,
        key: Box<super::animation::Keyframe>,
    },
    /// Patch a keyframe (partial: any subset of time/value/interp/tangents).
    /// Inverse: restore the prior keyframe (+ its time). Coalesces per
    /// (clip, track, index).
    SetKeyframe {
        clip: AssetId,
        track: usize,
        index: usize,
        #[serde(default)]
        t: Option<f64>,
        #[serde(default)]
        value: Option<TrackValue>,
        #[serde(default)]
        interp: Option<Interp>,
        #[serde(default)]
        in_tangent: Option<TrackValue>,
        #[serde(default)]
        out_tangent: Option<TrackValue>,
    },

    // ───────────────────────── Animation: transport ──────────────────────────
    /// Set the playhead (seconds). **Transient**.
    SetPlayhead { t: f64 },
    /// Set play/pause. **Transient**.
    SetPlaying { on: bool },
    /// Step the playhead (home / prev-key / next-key / end). **Transient**.
    StepPlayhead { kind: StepKind },
    /// Set the display frame rate. **Transient**.
    SetAnimFps { fps: u32 },
    /// Set the Solo-subtree focus node (or clear). **Transient**.
    SetSoloRoot { id: Option<NodeId> },
    /// Set the selected timeline element. **Transient**.
    SetAnimSelection { sel: Option<AnimSel> },
    /// Set which timeline editor the dock shows. **Transient**.
    SetAnimView { view: AnimView },

    // ───────────────────────── Animation: mixer (NLA) ────────────────────────
    /// Add a fresh (Replace, weight 1) layer to the mixer. Inverse: `DeleteLayer`.
    AddLayer,
    /// Delete a mixer layer (by index). Inverse: `RestoreLayer` of the captured layer.
    DeleteLayer { layer: usize },
    /// Re-insert a captured layer at its original index (inverse of `DeleteLayer`).
    RestoreLayer {
        layer: usize,
        doc: Box<super::animation::LayerDoc>,
    },
    /// Set a layer's composite mode (+ optional additive base clip). Inverse:
    /// restore prior.
    SetLayerMode { layer: usize, mode: LayerModeDoc },
    /// Set a layer's blend weight. Inverse: restore prior. Coalesces.
    SetLayerWeight { layer: usize, weight: f64 },
    /// Set a layer's node mask (+ include-descendants). Inverse: restore prior.
    SetLayerMask {
        layer: usize,
        nodes: Vec<NodeId>,
        include_descendants: bool,
    },
    /// Add a clip strip to a layer at `[start, start+len]`. Inverse: `DeleteStrip`.
    AddStrip {
        layer: usize,
        clip: AssetId,
        start: f64,
        len: f64,
    },
    /// Delete a strip (by index) from a layer. Inverse: `RestoreStrip`.
    DeleteStrip { layer: usize, strip: usize },
    /// Re-insert a captured strip at its original index (inverse of `DeleteStrip`).
    RestoreStrip {
        layer: usize,
        strip: usize,
        doc: Box<super::animation::StripDoc>,
    },
    /// Move a strip's start on the timeline. Inverse: restore prior. Coalesces.
    MoveStrip {
        layer: usize,
        strip: usize,
        start: f64,
    },
    /// Trim a strip's start + length. Inverse: restore prior. Coalesces.
    TrimStrip {
        layer: usize,
        strip: usize,
        start: f64,
        len: f64,
    },
    /// Set a strip's repeat (wrap) flag. Inverse: restore prior.
    SetStripRepeat {
        layer: usize,
        strip: usize,
        repeat: bool,
    },
}

impl EditorCommand {
    /// Transient commands are applied but never recorded in the undo log
    /// (mode switches, selection, camera orbit, panel toggles). Everything else
    /// records its inverse and participates in undo/redo.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            EditorCommand::SwitchMode { .. }
                | EditorCommand::SetSelection { .. }
                | EditorCommand::SetAssetSelection { .. }
                | EditorCommand::SetCurrentMaterial { .. }
                | EditorCommand::SnapCameraToAxis { .. }
                | EditorCommand::ResetCamera
                | EditorCommand::SetCurrentClip { .. }
                | EditorCommand::SetPlayhead { .. }
                | EditorCommand::SetPlaying { .. }
                | EditorCommand::StepPlayhead { .. }
                | EditorCommand::SetAnimFps { .. }
                | EditorCommand::SetSoloRoot { .. }
                | EditorCommand::SetAnimSelection { .. }
                | EditorCommand::SetAnimView { .. }
        )
    }

    /// Per-tab **view-local** commands that must NOT cross-tab broadcast: a
    /// second window framing its own camera / with its own selection / mode must
    /// not be yanked when the first edits. Everything else (clip/track/keyframe/
    /// mixer edits + the shared transport playhead) DOES broadcast so two tabs on
    /// the same project stay in lock-step.
    pub fn is_tab_local(&self) -> bool {
        matches!(
            self,
            EditorCommand::SwitchMode { .. }
                | EditorCommand::SetSelection { .. }
                | EditorCommand::SetAssetSelection { .. }
                | EditorCommand::SnapCameraToAxis { .. }
                | EditorCommand::ResetCamera
                | EditorCommand::SetAnimSelection { .. }
                | EditorCommand::SetSoloRoot { .. }
        )
    }

    /// Does applying this command change what the renderer must re-lower for
    /// animation playback — the active clip set, a clip's params, a track's
    /// sampler/mute/solo/keyframes, the mixer, the solo subtree, or the whole
    /// project (reset / load / model import that carries clips)?
    ///
    /// The bridge ([`animation_sync`]) observes a single revision counter the
    /// controller bumps for exactly these commands, then debounced-re-lowers.
    /// Routing through ONE counter (rather than per-field signal observers) means
    /// no edit can silently skip a re-lower — the bug where `SetTrackSampler` /
    /// time-only `SetKeyframe` / `SetClipDuration` left a stale lowered channel.
    ///
    /// Pure transport (playhead / play / step / fps) and view-only state
    /// (selection / view / clip color / rename) are EXCLUDED — they never change
    /// the lowered channels (the playhead is pinned by the render loop directly).
    pub fn affects_animation(&self) -> bool {
        matches!(
            self,
            // Project-level resets / loads / imports that replace the clip set.
            EditorCommand::NewProject
                | EditorCommand::LoadProjectFromUrl { .. }
                | EditorCommand::ImportModelFromUrl { .. }
                | EditorCommand::ImportModelFromFile { .. }
                // Active clip set + clip params that the group lowers.
                | EditorCommand::AddClip { .. }
                | EditorCommand::DeleteClip { .. }
                | EditorCommand::DuplicateClip { .. }
                | EditorCommand::SetCurrentClip { .. }
                | EditorCommand::SetClipDuration { .. }
                | EditorCommand::SetClipLoop { .. }
                | EditorCommand::SetClipSpeed { .. }
                | EditorCommand::SetClipDirection { .. }
                // Tracks.
                | EditorCommand::AddTrack { .. }
                | EditorCommand::DeleteTrack { .. }
                | EditorCommand::RestoreTrack { .. }
                | EditorCommand::SetTrackSampler { .. }
                | EditorCommand::SetTrackMute { .. }
                | EditorCommand::SetTrackSolo { .. }
                // Keyframes.
                | EditorCommand::AddKeyframe { .. }
                | EditorCommand::DeleteKeyframe { .. }
                | EditorCommand::InsertKeyframe { .. }
                | EditorCommand::SetKeyframe { .. }
                // Solo subtree focus.
                | EditorCommand::SetSoloRoot { .. }
                // Mixer / NLA.
                | EditorCommand::AddLayer
                | EditorCommand::DeleteLayer { .. }
                | EditorCommand::RestoreLayer { .. }
                | EditorCommand::SetLayerMode { .. }
                | EditorCommand::SetLayerWeight { .. }
                | EditorCommand::SetLayerMask { .. }
                | EditorCommand::AddStrip { .. }
                | EditorCommand::DeleteStrip { .. }
                | EditorCommand::RestoreStrip { .. }
                | EditorCommand::MoveStrip { .. }
                | EditorCommand::TrimStrip { .. }
                | EditorCommand::SetStripRepeat { .. }
        )
    }

    /// A short human-readable label (used in toasts / telemetry / the eventual
    /// undo-history UI).
    #[allow(dead_code)]
    pub fn label(&self) -> &'static str {
        match self {
            EditorCommand::SwitchMode { .. } => "Switch mode",
            EditorCommand::SetSelection { .. } => "Select",
            EditorCommand::NewProject => "New project",
            EditorCommand::Insert { .. } | EditorCommand::InsertTree { .. } => "Insert node",
            EditorCommand::Delete { .. } => "Delete node",
            EditorCommand::SetKind { .. } => "Edit properties",
            EditorCommand::SetTransform { .. } => "Transform",
            EditorCommand::Rename { .. } => "Rename",
            EditorCommand::SetVisible { .. } => "Toggle visibility",
            EditorCommand::SetLocked { .. } => "Toggle lock",
            EditorCommand::SetPrefab { .. } => "Toggle prefab",
            EditorCommand::Duplicate { .. } => "Duplicate",
            EditorCommand::Reparent { .. } => "Reparent",
            EditorCommand::LoadProjectFromUrl { .. } => "Load project",
            EditorCommand::ImportModelFromUrl { .. } => "Import model",
            EditorCommand::ImportModelFromFile { .. } => "Import model",
            EditorCommand::ImportTextureFromUrl { .. } => "Import texture",
            EditorCommand::AddMaterialAsset { .. } => "Add material",
            EditorCommand::AddTextureAsset { .. } => "Add texture",
            EditorCommand::DeleteAsset { .. } | EditorCommand::RestoreAsset { .. } => {
                "Delete asset"
            }
            EditorCommand::SetAssetSelection { .. } => "Select asset",
            EditorCommand::AddCustomMaterial => "New material",
            EditorCommand::AddBuiltinMaterial { .. } => "New material",
            EditorCommand::DeleteCustomMaterial { .. } => "Delete material",
            EditorCommand::SetCurrentMaterial { .. } => "Select material",
            EditorCommand::RegisterMaterial { .. } => "Register material",
            EditorCommand::AssignMaterial { .. } => "Assign material",
            EditorCommand::CopyMaterialInstance { .. } => "Copy material settings",
            EditorCommand::SetEnvironment { .. } => "Set environment",
            EditorCommand::SnapCameraToAxis { .. } => "Snap camera",
            EditorCommand::ResetCamera => "Reset view",
            EditorCommand::AddClip { .. } => "New clip",
            EditorCommand::DeleteClip { .. } => "Delete clip",
            EditorCommand::DuplicateClip { .. } => "Duplicate clip",
            EditorCommand::SetCurrentClip { .. } => "Select clip",
            EditorCommand::RenameClip { .. } => "Rename clip",
            EditorCommand::SetClipDuration { .. } => "Set duration",
            EditorCommand::SetClipLoop { .. } => "Set loop",
            EditorCommand::SetClipSpeed { .. } => "Set speed",
            EditorCommand::SetClipDirection { .. } => "Set direction",
            EditorCommand::SetClipColor { .. } => "Set clip color",
            EditorCommand::AddTrack { .. } => "Add track",
            EditorCommand::DeleteTrack { .. } | EditorCommand::RestoreTrack { .. } => {
                "Delete track"
            }
            EditorCommand::SetTrackSampler { .. } => "Set sampler",
            EditorCommand::SetTrackMute { .. } => "Mute track",
            EditorCommand::SetTrackSolo { .. } => "Solo track",
            EditorCommand::AddKeyframe { .. } => "Add keyframe",
            EditorCommand::DeleteKeyframe { .. } | EditorCommand::InsertKeyframe { .. } => {
                "Delete keyframe"
            }
            EditorCommand::SetKeyframe { .. } => "Edit keyframe",
            EditorCommand::SetPlayhead { .. } => "Scrub",
            EditorCommand::SetPlaying { .. } => "Play/pause",
            EditorCommand::StepPlayhead { .. } => "Step playhead",
            EditorCommand::SetAnimFps { .. } => "Set FPS",
            EditorCommand::SetSoloRoot { .. } => "Solo subtree",
            EditorCommand::SetAnimSelection { .. } => "Select",
            EditorCommand::SetAnimView { .. } => "Switch view",
            EditorCommand::AddLayer => "Add layer",
            EditorCommand::DeleteLayer { .. } | EditorCommand::RestoreLayer { .. } => {
                "Delete layer"
            }
            EditorCommand::SetLayerMode { .. } => "Set layer mode",
            EditorCommand::SetLayerWeight { .. } => "Set layer weight",
            EditorCommand::SetLayerMask { .. } => "Set layer mask",
            EditorCommand::AddStrip { .. } => "Add strip",
            EditorCommand::DeleteStrip { .. } | EditorCommand::RestoreStrip { .. } => {
                "Delete strip"
            }
            EditorCommand::MoveStrip { .. } => "Move strip",
            EditorCommand::TrimStrip { .. } => "Trim strip",
            EditorCommand::SetStripRepeat { .. } => "Set strip repeat",
        }
    }
}
