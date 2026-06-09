//! `EditorCommand` — the single serializable enum covering every editor
//! mutation. The UI is read-only/informational and never mutates editor state
//! directly; every change is a command (from the MCP agent, or UI affordances)
//! dispatched through the `EditorController`.
//! Commands are **data** (no closures) so they serialize, and non-transient
//! ones are invertible — the inverse is captured at apply-time and pushed onto
//! the undo log (command-sourcing, replacing the old snapshot history).

use serde::{Deserialize, Serialize};

use awsm_scene::animation::{
    BuiltinParamKind, ClipDirection, ClipLoop, Interp, Keyframe, LayerDoc, LayerModeDoc,
    LightParamKind, SamplerKind, StoredTrack, StripDoc, TrackTarget, TrackValue,
};
use awsm_scene::{AssetId, EnvironmentConfig, MaterialShading, NodeId, NodeKind, Trs};

use awsm_meshgen::recipe::{Modifier, ModifierStack};

use crate::assets::AssetEntry;
use crate::mesh_def::{CapturedMesh, VertexOverrides};

use crate::anim_ui::{AnimSel, AnimView, StepKind};
use crate::node_spec::{InsertSpec, NodeSpec};

/// A procedural texture generator the Content Browser can author.
/// Maps to `ProceduralTextureDef` with sensible defaults at apply-time.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProceduralKind {
    Checker,
    Gradient,
    Noise,
}

/// Alpha/surface mode a custom (dynamic-WGSL) material compiles for. `Mask`
/// carries its alpha cutoff. Mirrors the editor's `AlphaMode` + cutoff pair.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CustomAlphaMode {
    Opaque,
    Mask { cutoff: f64 },
    Blend,
}

/// One declared slot in a custom material's layout (uniform / texture / buffer).
/// A string-typed mirror of the editor's live `Slot` — `val` is the uniform's
/// default (comma-separated for vectors, e.g. `"0.6, 0.7, 1.0"`); `debug` is the
/// texture/buffer debug-preview source. Used by `SetCustomMaterialLayout`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlotSpec {
    pub name: String,
    /// WGSL type, e.g. `"f32"`, `"vec3<f32>"`, `"texture_2d<f32>"`,
    /// `"array<vec4<f32>>"`.
    pub ty: String,
    #[serde(default)]
    pub val: String,
    #[serde(default)]
    pub debug: String,
}

/// Which texture slot of a built-in/inline `MaterialDef` a `SetBuiltinTexture`
/// targets (mirrors the glTF PBR texture set).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum BuiltinTextureSlot {
    BaseColor,
    MetallicRoughness,
    Normal,
    Occlusion,
    Emissive,
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
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

    /// Record a read-only **vertex-selection highlight**: "these vertices of
    /// this node are selected". **Transient** observability (like
    /// [`SetSelection`]) — session-local view state, never recorded in the undo
    /// log and never mutating geometry. The bridge draws a small marker at each
    /// selected vertex in the viewport. An empty `indices` clears the highlight.
    SetVertexSelection { node: NodeId, indices: Vec<u32> },

    /// Apply a list of commands as one atomic step: they run in order and
    /// collapse into a **single undo entry** (so undo reverses the whole batch).
    /// The MCP `dispatch_batch` round-trips here. Inverse: a `Batch` of the
    /// sub-inverses, reversed.
    Batch(Vec<EditorCommand>),

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
    /// when `None`). **Carries its `id`** (minted by the dispatcher, not in
    /// `apply`) so the command is deterministic data — the MCP path can echo the
    /// new id without a snapshot round-trip, and a cross-tab replay produces the
    /// *same* id in every tab. Idempotent: applying it when the id already exists
    /// is a no-op. Inverse: `Delete` of the new node.
    Insert {
        id: NodeId,
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

    /// Import a raster texture from a URL (gesture-free): fetch + decode + upload
    /// to the GPU, then add a `TextureDef::Raster` asset. **Carries its `id`**
    /// (caller-minted, idempotent) so the MCP path can echo it. Inverse:
    /// `DeleteAsset` of the new id.
    ImportTextureFromUrl { id: AssetId, url: String },

    /// Create a fresh custom material asset (Content Browser "+ Material") of the
    /// given shading family. Inserts a `MaterialDef` into the project asset table
    /// and selects it. **Carries its `id`** (caller-minted, idempotent) so the
    /// MCP path can echo it. Inverse: `DeleteAsset` of the new id.
    AddMaterialAsset {
        id: AssetId,
        shading: MaterialShading,
    },

    /// Create a fresh procedural texture asset (Content Browser "+ Texture").
    /// **Carries its `id`** (caller-minted, idempotent). Inverse: `DeleteAsset`
    /// of the new id.
    AddTextureAsset { id: AssetId, proc: ProceduralKind },

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
    /// **Carries its `id`** (caller-minted, idempotent) so the MCP path can echo
    /// it without a snapshot round-trip.
    AddCustomMaterial { id: AssetId },

    /// Create a fresh **built-in** library material (PBR / Unlit / Toon) and make
    /// it current. Carries shared variant settings; per-mesh uniform values are
    /// set on each assigned mesh. Needs no compile. **Carries its `id`**
    /// (caller-minted, idempotent).
    AddBuiltinMaterial {
        id: AssetId,
        shading: MaterialShading,
    },

    /// Delete a custom WGSL material.
    DeleteCustomMaterial { id: AssetId },

    /// Set the material the Studio is editing. **Transient**.
    SetCurrentMaterial { id: Option<AssetId> },

    /// Register (compile to a renderer bucket) the current custom material.
    /// Validates the WGSL and flips the `registered` flag; the real GPU
    /// registration + bucket accounting is future work.
    RegisterMaterial { id: AssetId },

    /// Replace a custom (dynamic-WGSL) material's shader source. The handler sets
    /// the live `wgsl` field; the controller-owned auto-register pipeline observes
    /// it and recompiles (debounced) — so this works headlessly, with no Studio
    /// UI mounted. Inverse: restore the prior source. The remote/MCP authoring
    /// path (the Studio textarea writes the live model directly).
    SetCustomMaterialWgsl { id: AssetId, wgsl: String },

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

    /// Set the orbit camera's full pose: `yaw`/`pitch` (radians), `radius`
    /// (distance from look-at), and the `look_at` point. **Transient** (view
    /// state). Convention: yaw 0 looks down -Z, π/2 down -X; pitch > 0 raises
    /// the camera (looks down).
    SetCameraOrbit {
        yaw: f32,
        pitch: f32,
        radius: f32,
        look_at: [f32; 3],
    },
    /// Switch the viewport projection (perspective vs orthographic), with an
    /// optional perspective vertical FOV (radians). **Transient** (view state).
    SetCameraProjection {
        perspective: bool,
        #[serde(default)]
        fov_y: Option<f32>,
    },
    /// Frame a node in the viewport — fit its world-space bounds with `padding`
    /// (0 = tight, 0.2 = 20% margin). **Transient** (view state).
    FrameNode { node: NodeId, padding: f32 },

    /// Pin the renderer's `frame_globals.time` to `seconds` (overrides the
    /// wall-clock). A temporal material (`sin(time*f)`) then screenshots the same
    /// phase every call. Separate from the animation playhead. **Transient**.
    SetFrameTime { seconds: f32 },
    /// Clear the pinned frame time — back to the wall-clock source. **Transient**.
    ClearFrameTime,

    /// Assign a library material (built-in or custom WGSL, by id) to a scene
    /// node's mesh, or clear it (`material: None` → magenta). Sets the node's
    /// single `material: Option<MaterialInstance>` field. Inverse: restore the
    /// node's prior kind (a `SetKind`). The bridge renders the assigned material
    /// once it's registered with the renderer.
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

    /// Bake a **skinned** mesh node to a static **editable** mesh: discard the
    /// skin (JOINTS/WEIGHTS + skeleton), capture the bind-pose geometry into a
    /// new captured `MeshDef{ stack:{ base: Captured } }` asset, and swap the
    /// node's kind from `SkinnedMesh` to `Mesh` (carrying the material + shadow
    /// across). The explicit, **terminal** bridge that makes a skinned import
    /// editable — a hard prerequisite for any mesh-editing op on it. Errors if
    /// the node isn't a `SkinnedMesh`. Inverse: restore the prior `SkinnedMesh`
    /// kind (the captured asset is left behind, harmlessly unreferenced).
    DropSkinning { node: NodeId },

    // ─────────────────────────── Mesh editing ────────────────────────────────
    /// **Retired / no-op.** Procedural-geometry nodes are now unified on
    /// `NodeKind::Mesh`, each already backed by an editable `MeshDef` carrying a
    /// `ModifierStack` — so there is nothing to convert. The variant is kept for
    /// protocol stability; `apply` does nothing (not undoable) and the MCP tool
    /// echoes the node's existing mesh id instead of the (ignored) caller-minted
    /// `mesh`.
    ConvertToEditableMesh { node: NodeId, mesh: AssetId },
    /// Replace an editable mesh's geometry wholesale (raw per-vertex editing / a
    /// collapsed modifier bake). The bridge re-materializes every referencing
    /// `NodeKind::Mesh` node via the mesh-revision observer. Inverse: restore the
    /// prior geometry (a `SetMeshData` carrying the previous `CapturedMesh`).
    SetMeshData { mesh: AssetId, data: CapturedMesh },
    /// Replace an editable mesh's procedural **recipe** wholesale (modifier
    /// stack: base + ordered deformers) — the idempotent, coalescing idiom of
    /// `SetCustomMaterialLayout`. The handler re-evaluates the stack to triangles
    /// (resolving `Sweep`/`Captured` bases against the scene) and re-bakes the
    /// `.mesh.bin` cache; the bridge re-materializes referencing nodes. Add /
    /// remove / reorder / param-tweak are all the UI/agent sending a new whole
    /// stack. Inverse: restore the prior stack (or prior bytes if there was none).
    SetMeshModifiers { mesh: AssetId, stack: ModifierStack },
    /// Append one `Modifier` to the **end** of a mesh's existing modifier stack
    /// (convenience over resending the whole stack). The mesh must already carry a
    /// recipe (`set_mesh_modifiers` first); errors otherwise. Re-bakes + the bridge
    /// re-materializes referencing nodes. Inverse: `SetMeshModifiers(prior_stack)`.
    AddModifier { mesh: AssetId, modifier: Modifier },
    /// Replace the modifier at `index` in a mesh's existing stack. The mesh must
    /// already carry a recipe; `index` must be in range — errors otherwise.
    /// Inverse: `SetMeshModifiers(prior_stack)`.
    SetModifier {
        mesh: AssetId,
        index: u32,
        modifier: Modifier,
    },
    /// Remove the modifier at `index` from a mesh's existing stack. The mesh must
    /// already carry a recipe; `index` must be in range — errors otherwise.
    /// Inverse: `SetMeshModifiers(prior_stack)`.
    RemoveModifier { mesh: AssetId, index: u32 },
    /// Replace the positions of specific vertices (raw editing). `indices[k]`
    /// gets `positions[k]`; normals are recomputed. Inverse: a `SetVertexPositions`
    /// carrying the **prior** positions of the same indices (sparse — never a
    /// whole-mesh snapshot).
    SetVertexPositions {
        mesh: AssetId,
        indices: Vec<u32>,
        positions: Vec<[f32; 3]>,
    },
    /// Translate a vertex selection with a smooth radial falloff (server computes
    /// the per-vertex weights via `meshgen::edit::soft_transform_positions`).
    /// Inverse: a sparse `SetVertexPositions` of every vertex the move touched.
    SoftTransformVertices {
        mesh: AssetId,
        indices: Vec<u32>,
        translation: [f32; 3],
        falloff: f32,
    },
    /// Bake an editable mesh's modifier stack into raw triangles and clear the
    /// recipe (the deliberate heavy snapshot). Inverse:
    /// `Batch[SetMeshModifiers(prior), SetMeshData(prior_bytes)]`.
    CollapseMeshStack { mesh: AssetId },

    // ───────────────────── Per-vertex attribute authoring ────────────────────
    // Per-vertex authoring is **index-based on a frozen topology** → terminal:
    // the first authoring op collapses the procedural stack to a `Captured`-self
    // base (locking topology), after which edits are a sparse per-vertex override
    // layer (`MeshDef::overrides`). Each command below collapses-first
    // (`ensure_authorable`), writes the override, re-bakes the `.mesh.bin` cache
    // (base+modifiers+overrides), and bumps `mesh_revision`. The inverse restores
    // the prior overrides (and, if the collapse fired, the prior stack too — a
    // `Batch`).
    /// Set the per-vertex **color** override of `indices` to `color` (RGBA). The
    /// painted colors only *display* under a material that reads vertex colors —
    /// built-in PBR with `vertex_colors_enabled`, or a custom material that
    /// samples them. Inverse: restore the prior overrides (`SetVertexOverrides`,
    /// possibly batched with a stack restore).
    PaintVertexColors {
        mesh: AssetId,
        indices: Vec<u32>,
        color: [f32; 4],
    },
    /// Set the per-vertex **normal** override of `indices` to `normal`. An
    /// explicit normal override always wins over the eval/recompute. Inverse:
    /// restore the prior overrides.
    SetVertexNormals {
        mesh: AssetId,
        indices: Vec<u32>,
        normal: [f32; 3],
    },
    /// Replace a mesh's entire sparse [`VertexOverrides`] map wholesale (the
    /// idempotent setter, used as the universal inverse of the authoring
    /// commands and by `BakeAll` undo). Collapses-first, re-bakes. Inverse:
    /// `SetVertexOverrides(prior_overrides)`.
    SetVertexOverrides {
        mesh: AssetId,
        overrides: VertexOverrides,
    },
    /// Project-wide finalize: collapse **every** Mesh asset's stack (freeze all
    /// topology, bake all overrides into the cache, then flatten recipes to
    /// `Captured`-self). Inverse: a `Batch` restoring each mesh's prior stack.
    BakeAll {},

    /// Bind (or clear) a texture on a mesh node's **built-in/inline** material
    /// slot — the counterpart of `SetMaterialTexture` (which targets custom-WGSL
    /// materials). `texture: None` clears the slot. Inverse: restore the node's
    /// prior kind.
    SetBuiltinTexture {
        node: NodeId,
        slot: BuiltinTextureSlot,
        texture: Option<AssetId>,
    },

    // ─────────────────── Custom (dynamic-WGSL) material authoring ─────────────
    // The Studio surface that used to mutate the reactive `CustomMaterial`
    // `Mutable`s directly now routes through these commands (the "all via
    // controller" rule), so each edit is undoable, cross-tab-broadcast, and
    // reachable over MCP. Each flips the material back to draft (`registered =
    // false`); the debounced auto-register recompiles. Inverse: restore prior.
    /// Set a custom material's alpha/surface mode (`Mask` carries its cutoff).
    SetCustomMaterialAlphaMode { id: AssetId, mode: CustomAlphaMode },
    /// Set a custom material's double-sided flag.
    SetCustomMaterialDoubleSided { id: AssetId, double_sided: bool },
    /// Set a custom material's debug base color (`#rrggbb`, preview-only).
    SetCustomMaterialDebugColor { id: AssetId, hex: String },
    /// Replace a custom material's declared slot layout (uniforms / textures /
    /// buffers). The full lists are sent (not a delta) so it's a single
    /// idempotent edit. Re-registration re-derives the WGSL `MaterialData` struct.
    SetCustomMaterialLayout {
        id: AssetId,
        uniforms: Vec<SlotSpec>,
        textures: Vec<SlotSpec>,
        buffers: Vec<SlotSpec>,
    },
    /// Set the declared `ShaderIncludes` keys a custom material's WGSL needs
    /// (validated against `SHADER_INCLUDE_KEYS`; unknown keys are dropped).
    SetCustomMaterialShaderIncludes { id: AssetId, includes: Vec<String> },
    /// Set the declared `FragmentInputs` keys (interpolants the fragment reads;
    /// validated against `FRAGMENT_INPUT_KEYS`).
    SetCustomMaterialFragmentInputs { id: AssetId, inputs: Vec<String> },
    /// Set the default value of a custom material's declared uniform slot (by
    /// slot name). `value` is the comma-separated form the layout uses (e.g.
    /// `"0.6, 0.7, 1.0"`). The writable counterpart of `ReadbackTarget::Uniform`.
    SetMaterialUniform {
        material: AssetId,
        name: String,
        value: String,
    },
    /// Set a built-in material factor on a mesh node's inline material (the
    /// writable counterpart of `ReadbackTarget::BuiltinParam`). `value` is 1
    /// element for `Metallic`/`Roughness`, 3 for `BaseColor`/`Emissive`. Inverse:
    /// restore the node's prior kind.
    SetBuiltinParam {
        node: NodeId,
        param: BuiltinParamKind,
        value: Vec<f32>,
    },
    /// Set a light parameter on a light node (writable counterpart of
    /// `ReadbackTarget::LightParam`). `value` is 3 floats for `Color`, 1 for
    /// `Intensity`/`Range`/`InnerAngle`/`OuterAngle`. Range/angles only apply to
    /// the relevant light kind. Inverse: restore the node's prior kind.
    SetLightParam {
        node: NodeId,
        param: LightParamKind,
        value: Vec<f32>,
    },
    /// Bind a texture asset into a mesh node's assigned custom-material texture
    /// slot (by slot name), or clear it (`texture: None`). Writes the node's
    /// `MaterialInstance::texture_overrides`. The node must already have a
    /// custom material assigned with a matching declared texture slot. Inverse:
    /// restore the node's prior kind.
    SetMaterialTexture {
        node: NodeId,
        slot: String,
        texture: Option<AssetId>,
    },

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
        track: Box<StoredTrack>,
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
        key: Box<Keyframe>,
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
    RestoreLayer { layer: usize, doc: Box<LayerDoc> },
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
        doc: Box<StripDoc>,
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
                | EditorCommand::SetVertexSelection { .. }
                | EditorCommand::SetAssetSelection { .. }
                | EditorCommand::SetCurrentMaterial { .. }
                | EditorCommand::SnapCameraToAxis { .. }
                | EditorCommand::ResetCamera
                | EditorCommand::SetCameraOrbit { .. }
                | EditorCommand::SetCameraProjection { .. }
                | EditorCommand::FrameNode { .. }
                | EditorCommand::SetFrameTime { .. }
                | EditorCommand::ClearFrameTime
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
                | EditorCommand::SetVertexSelection { .. }
                | EditorCommand::SetAssetSelection { .. }
                | EditorCommand::SnapCameraToAxis { .. }
                | EditorCommand::ResetCamera
                | EditorCommand::SetCameraOrbit { .. }
                | EditorCommand::SetCameraProjection { .. }
                | EditorCommand::FrameNode { .. }
                | EditorCommand::SetFrameTime { .. }
                | EditorCommand::ClearFrameTime
                | EditorCommand::SetAnimSelection { .. }
                | EditorCommand::SetSoloRoot { .. }
        )
    }

    /// Does applying this command change what the renderer must re-lower for
    /// animation playback — the active clip set, a clip's params, a track's
    /// sampler/mute/solo/keyframes, the mixer, the solo subtree, or the whole
    /// project (reset / load / model import that carries clips)?
    ///
    /// The bridge (`animation_sync`) observes a single revision counter the
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

    /// Does applying this command change captured-mesh geometry the bridge must
    /// re-materialize? The bridge (`mesh_sync`) observes a single revision counter
    /// the controller bumps for exactly these commands (mirrors
    /// [`affects_animation`](Self::affects_animation)) — `SetMeshData` replaces an
    /// editable mesh's bytes without changing the node kind, so the per-node
    /// `node.kind` observer wouldn't otherwise re-fire. (`ConvertToEditableMesh`
    /// changes the node kind too, so its `SetKind` already re-materializes; it's
    /// listed for symmetry.)
    pub fn affects_mesh(&self) -> bool {
        matches!(
            self,
            EditorCommand::SetMeshData { .. }
                | EditorCommand::ConvertToEditableMesh { .. }
                | EditorCommand::SetMeshModifiers { .. }
                | EditorCommand::AddModifier { .. }
                | EditorCommand::SetModifier { .. }
                | EditorCommand::RemoveModifier { .. }
                | EditorCommand::SetVertexPositions { .. }
                | EditorCommand::SoftTransformVertices { .. }
                | EditorCommand::CollapseMeshStack { .. }
                | EditorCommand::PaintVertexColors { .. }
                | EditorCommand::SetVertexNormals { .. }
                | EditorCommand::SetVertexOverrides { .. }
                | EditorCommand::BakeAll {}
        )
    }

    /// A short human-readable label (used in toasts / telemetry / the eventual
    /// undo-history UI).
    #[allow(dead_code)]
    pub fn label(&self) -> &'static str {
        match self {
            EditorCommand::SwitchMode { .. } => "Switch mode",
            EditorCommand::SetSelection { .. } => "Select",
            EditorCommand::SetVertexSelection { .. } => "Select vertices",
            EditorCommand::Batch(_) => "Batch edit",
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
            EditorCommand::AddCustomMaterial { .. } => "New material",
            EditorCommand::AddBuiltinMaterial { .. } => "New material",
            EditorCommand::DeleteCustomMaterial { .. } => "Delete material",
            EditorCommand::SetCurrentMaterial { .. } => "Select material",
            EditorCommand::RegisterMaterial { .. } => "Register material",
            EditorCommand::SetCustomMaterialWgsl { .. } => "Edit shader",
            EditorCommand::AssignMaterial { .. } => "Assign material",
            EditorCommand::CopyMaterialInstance { .. } => "Copy material settings",
            EditorCommand::DropSkinning { .. } => "Drop skinning",
            EditorCommand::ConvertToEditableMesh { .. } => "Convert to editable mesh",
            EditorCommand::SetMeshData { .. } => "Edit mesh",
            EditorCommand::SetMeshModifiers { .. } => "Edit modifiers",
            EditorCommand::AddModifier { .. } => "Add modifier",
            EditorCommand::SetModifier { .. } => "Set modifier",
            EditorCommand::RemoveModifier { .. } => "Remove modifier",
            EditorCommand::SetVertexPositions { .. } => "Move vertices",
            EditorCommand::SoftTransformVertices { .. } => "Soft-transform vertices",
            EditorCommand::CollapseMeshStack { .. } => "Collapse mesh stack",
            EditorCommand::PaintVertexColors { .. } => "Paint vertex colors",
            EditorCommand::SetVertexNormals { .. } => "Set vertex normals",
            EditorCommand::SetVertexOverrides { .. } => "Set vertex overrides",
            EditorCommand::BakeAll {} => "Bake all meshes",
            EditorCommand::SetBuiltinTexture { .. } => "Bind texture",
            EditorCommand::SetCustomMaterialAlphaMode { .. } => "Set alpha mode",
            EditorCommand::SetCustomMaterialDoubleSided { .. } => "Set double-sided",
            EditorCommand::SetCustomMaterialDebugColor { .. } => "Set base color",
            EditorCommand::SetCustomMaterialLayout { .. } => "Edit material layout",
            EditorCommand::SetCustomMaterialShaderIncludes { .. } => "Set shader includes",
            EditorCommand::SetCustomMaterialFragmentInputs { .. } => "Set fragment inputs",
            EditorCommand::SetMaterialUniform { .. } => "Set uniform",
            EditorCommand::SetBuiltinParam { .. } => "Set material param",
            EditorCommand::SetLightParam { .. } => "Set light param",
            EditorCommand::SetMaterialTexture { .. } => "Bind texture",
            EditorCommand::SetEnvironment { .. } => "Set environment",
            EditorCommand::SnapCameraToAxis { .. } => "Snap camera",
            EditorCommand::ResetCamera => "Reset view",
            EditorCommand::SetCameraOrbit { .. } => "Orbit camera",
            EditorCommand::SetCameraProjection { .. } => "Set projection",
            EditorCommand::FrameNode { .. } => "Frame node",
            EditorCommand::SetFrameTime { .. } => "Pin frame time",
            EditorCommand::ClearFrameTime => "Clear frame time",
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
