//! `EditorQuery` / `EditorSnapshot` — a serializable read of editor state for
//! external inspection + headless tests. The MCP/WebTransport transport
//! `serde`-encodes these back to the caller. A flat, view-agnostic projection of
//! the controller's state, not the live model.

use serde::{Deserialize, Serialize};

use awsm_scene_schema::animation::{BuiltinParamKind, CameraParamKind, LightParamKind};
use awsm_scene_schema::{AssetId, NodeId};

use crate::command::EditorMode;
use crate::node_spec::NodeQuery;

/// A serializable snapshot of editor state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditorSnapshot {
    pub mode: EditorMode,
    pub project: ProjectSnapshot,
    /// The scene tree (id / name / kind / children), top-level first.
    pub scene_tree: Vec<NodeQuery>,
    /// Selected node ids (ordered; last = primary).
    pub selection: Vec<String>,
    pub undo_depth: usize,
    pub redo_depth: usize,
    /// Animation-mode state (clip library + transport). Lets a driver discover
    /// clip ids + verify transport without the UI.
    pub animation: AnimationSnapshot,
    /// Custom (dynamic-WGSL) material assets — id / name / registered / declared
    /// uniform slot names. Lets a driver discover material ids + uniform slots
    /// (e.g. to author/verify a Uniform animation track).
    #[serde(default)]
    pub materials: Vec<MaterialSnapshot>,
    /// Texture assets in the project (id / name / kind / dims). Lets a driver
    /// discover texture ids to bind into material slots.
    #[serde(default)]
    pub textures: Vec<TextureSnapshot>,
}

/// Serializable projection of a texture asset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextureSnapshot {
    pub id: String,
    pub name: String,
    /// `"procedural"` | `"raster"`.
    pub kind: String,
}

/// Serializable projection of a custom material asset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterialSnapshot {
    pub id: String,
    pub name: String,
    pub registered: bool,
    pub builtin: bool,
    pub uniforms: Vec<String>,
    /// True when the material has no outstanding compile errors (always true for
    /// built-ins, which need no compile). Closes the old `query.rs` TODO.
    #[serde(default = "default_true")]
    pub compile_ok: bool,
    /// Outstanding compile diagnostics (empty when `compile_ok`).
    #[serde(default)]
    pub errors: Vec<CompileError>,
}

fn default_true() -> bool {
    true
}

/// One compile diagnostic for a custom material's WGSL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompileError {
    /// 1-based line in the author's WGSL body, when known. The lightweight
    /// in-editor syntax check reports author-relative lines; GPU/naga errors
    /// carry only a message (their line numbers index the assembled module, not
    /// the author's snippet, so they're omitted rather than mislead).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    pub message: String,
}

/// The compile status of a custom (dynamic-WGSL) material — the answer to
/// [`EditorQuery::MaterialDiagnostics`]. Lets an MCP caller tell a compile
/// failure from a successful-but-dark shader (the original §A failure).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompileDiagnostics {
    /// Whether the material is currently registered (compiled into a renderer
    /// bucket and live on assigned meshes).
    pub registered: bool,
    /// True when there are no compile errors.
    pub ok: bool,
    pub errors: Vec<CompileError>,
}

/// Serializable projection of Animation-mode state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnimationSnapshot {
    pub clips: Vec<ClipSnapshot>,
    pub current_clip: Option<String>,
    pub playhead: f64,
    pub playing: bool,
    pub fps: u32,
    pub solo_root: Option<String>,
    pub mixer_layers: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipSnapshot {
    pub id: String,
    pub name: String,
    pub duration: f64,
    pub tracks: Vec<TrackSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackSnapshot {
    /// Human-readable target summary (e.g. `transform:rotation`).
    pub target: String,
    pub keys: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectSnapshot {
    pub name: String,
    pub dirty: bool,
    pub missing_assets: Vec<String>,
    /// Coordinate-system description (handedness / up-axis / units) so a driver
    /// doesn't have to guess the frame. Constant for now.
    #[serde(default = "default_coordinate_system")]
    pub coordinate_system: String,
    /// World units. Constant ("meters") for now.
    #[serde(default = "default_units")]
    pub units: String,
}

fn default_coordinate_system() -> String {
    "right-handed, Y-up, -Z forward".to_string()
}

fn default_units() -> String {
    "meters".to_string()
}

// ─────────────────────────────── query surface ──────────────────────────────
// The READ half of the controller — serializable, read-only (never mutates
// persisted state, never records undo, never broadcasts; any handler that pins
// the playhead saves + restores the transport). The MCP/WebTransport transport
// `serde`-decodes a query → `query()` → encodes the result.

/// A read/verification query against editor + renderer state.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "query", rename_all = "snake_case")]
pub enum EditorQuery {
    /// The existing editor snapshot.
    Snapshot,
    /// Sample a clip's targets across a set of pinned times — *video as numbers*.
    /// GPU-independent (reads CPU-side renderer state after `update_animations(0.0)`).
    SampleClipTimeseries {
        clip: AssetId,
        /// Seconds; the playhead is pinned at each (Animation-pin).
        times: Vec<f64>,
        /// What to read at every pinned time.
        targets: Vec<ReadbackTarget>,
    },
    /// Exact RGBA at canvas points (drawImage→getImageData in-page).
    CanvasPixels { coords: Vec<(u32, u32)> },
    /// Mean / min / max luma over a region (or the whole canvas when `None`).
    CanvasStats { region: Option<[u32; 4]> },
    /// The WGSL source of a custom (dynamic) material. Dedicated query (not a
    /// snapshot field) so potentially-large shader bodies stay out of every
    /// snapshot.
    CustomMaterialWgsl { material: AssetId },
    /// Compile diagnostics for a custom (dynamic) material — registered flag, an
    /// `ok` bool, and the outstanding errors. The answer to "did my last
    /// `set_material_wgsl` actually compile?".
    MaterialDiagnostics { material: AssetId },
    /// Local TRS + world matrix for each node (empty `nodes` = all nodes). Reads
    /// the live scene — no animation-clip pin hack needed.
    NodeTransforms {
        #[serde(default)]
        nodes: Vec<NodeId>,
    },
    /// The full per-kind config (primitive shape, light/camera config, assigned +
    /// inline material) for each node, as the serialized `NodeKind` (empty
    /// `nodes` = all nodes).
    NodeKindDetails {
        #[serde(default)]
        nodes: Vec<NodeId>,
    },
    /// World-space axis-aligned bounding box `{min,max}` for each node (empty
    /// `nodes` = all nodes). CPU-estimated from primitive dims + world transform;
    /// used to frame the camera (`FrameNode`) and size objects.
    NodeBounds {
        #[serde(default)]
        nodes: Vec<NodeId>,
    },
    /// The full stored data for one animation track (target, sampler, mute/solo,
    /// times, keyframes incl. interp/tangents) — lets a driver verify what it
    /// authored. `SampleClipTimeseries` samples rendered output; this returns the
    /// keyframes themselves.
    GetTrackData { clip: AssetId, track: usize },
    /// The renderer's current frame globals: `time`, `delta_time`, `frame_count`,
    /// `resolution`. Reflects a `SetFrameTime` pin.
    FrameGlobals,
    /// The last `limit` editor notices (toasts: info/warning/error) from an
    /// in-process ring buffer — surfaces runtime errors otherwise invisible over
    /// MCP. Material compile errors have a dedicated path (`MaterialDiagnostics`).
    ConsoleLogs {
        #[serde(default = "default_log_limit")]
        limit: u32,
    },
    /// Block until no material recompile is pending **and** the renderer's
    /// pipeline scheduler has drained **and** a fresh frame has presented (or
    /// `max_ms` elapses). The deterministic barrier between an edit and a
    /// screenshot — defeats the `set → screenshot` race against the ~400ms
    /// debounced recompile + RAF present.
    WaitRenderSettled {
        #[serde(default = "default_settle_ms")]
        max_ms: u32,
    },
}

fn default_log_limit() -> u32 {
    50
}

fn default_settle_ms() -> u32 {
    4000
}

/// What a [`EditorQuery::SampleClipTimeseries`] frame reads.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "target", rename_all = "snake_case")]
pub enum ReadbackTarget {
    /// A node's local TRS (translation, rotation xyzw, scale). Struct variant
    /// (not a newtype) so the internally-tagged enum round-trips — a tagged
    /// newtype over a string id errors at runtime (same trap as `TrackValue`).
    NodeLocalTrs { node: NodeId },
    /// A node's world matrix (16 floats, column-major).
    NodeWorldMatrix { node: NodeId },
    /// A morph weight on a node.
    MorphWeight { node: NodeId, index: usize },
    /// A custom-material uniform value (by material asset + slot name).
    Uniform { material: AssetId, name: String },
    /// A built-in material factor on a node.
    BuiltinParam {
        node: NodeId,
        param: BuiltinParamKind,
    },
    /// A light parameter on a node.
    LightParam { node: NodeId, param: LightParamKind },
    /// A camera parameter on a node — resolves to the live renderer camera's
    /// value (fov_y, near, far, aperture, focus_distance). Null if the camera
    /// slot isn't materialized yet, or fov_y on an orthographic camera.
    CameraParam {
        node: NodeId,
        param: CameraParamKind,
    },
}

/// The result of a query (serialized back to the caller).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum QueryResult {
    // Boxed — `EditorSnapshot` is by far the largest variant; serde boxes
    // transparently so the JSON wire form is unchanged.
    Snapshot(Box<EditorSnapshot>),
    Timeseries(TimeseriesResult),
    Pixels(PixelsResult),
    Stats(StatsResult),
    /// Compile diagnostics for a custom material.
    Diagnostics(CompileDiagnostics),
    /// Render-settle barrier outcome.
    Settled(SettledResult),
    /// A keyed map result (node transforms / kind details / bounds / track data).
    /// `kind` discriminates; `entries` maps id (or index) → arbitrary JSON.
    Map(MapResult),
    /// A plain text payload (e.g. a custom material's WGSL source).
    Text(String),
    Error {
        error: String,
    },
}

/// The numeric time-series result (one frame per pinned time).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeseriesResult {
    /// The targets, echoed back as stable string keys (the `values` map keys).
    pub targets: Vec<String>,
    pub frames: Vec<TimeseriesFrame>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeseriesFrame {
    pub t: f64,
    /// target-key → number | array of numbers (null when unreadable).
    pub values: std::collections::BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PixelsResult {
    /// One `[r,g,b,a]` per requested coordinate (0–255).
    pub pixels: Vec<[u8; 4]>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsResult {
    pub mean_luma: f64,
    pub min_luma: f64,
    pub max_luma: f64,
    pub pixel_count: u64,
}

/// A keyed-map query result (see [`QueryResult::Map`]). The `kind` field is what
/// lets the `untagged` `QueryResult` tell this apart from the other variants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapResult {
    /// Discriminator: `"transforms"` | `"kind_details"` | `"bounds"` | `"track"`.
    pub kind: String,
    /// id (or index) → value.
    pub entries: std::collections::BTreeMap<String, serde_json::Value>,
}

/// The outcome of a [`EditorQuery::WaitRenderSettled`] barrier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettledResult {
    /// True if the scene settled before `max_ms`; false on timeout.
    pub settled: bool,
    /// How long the barrier actually waited (ms).
    pub waited_ms: u32,
}
