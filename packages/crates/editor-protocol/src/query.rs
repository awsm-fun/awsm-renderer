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
    // materials / compile_errors land as those models arrive.
}

/// Serializable projection of a custom material asset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterialSnapshot {
    pub id: String,
    pub name: String,
    pub registered: bool,
    pub builtin: bool,
    pub uniforms: Vec<String>,
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
    /// A camera parameter on a node (DEFERRED — resolves to null for now).
    CameraParam {
        node: NodeId,
        param: CameraParamKind,
    },
}

/// The result of a query (serialized back to the caller).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum QueryResult {
    Snapshot(EditorSnapshot),
    Timeseries(TimeseriesResult),
    Pixels(PixelsResult),
    Stats(StatsResult),
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
