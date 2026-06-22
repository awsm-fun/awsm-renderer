//! M5 — the Layer 1 remote-renderer protocol (`docs/plans/multithreading.md`,
//! D4): a typed command/event channel so a main-thread DOM driver fully
//! controls a worker-hosted renderer.
//!
//! `RenderCommand` (main → worker) and `RenderEvent` (worker → main) are
//! `serde` / `serde_wasm_bindgen` values. Geometry payloads are NOT
//! serialized into the command — they ride alongside as **Transferable**
//! `ArrayBuffer`s (zero-copy) and the command references them by index (the
//! transfer rule from the plan's API audit).

use serde::{Deserialize, Serialize};

/// Commands the main-thread driver sends to the render worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "kebab-case")]
pub enum RenderCommand {
    /// Load a model as a set of meshes. Each mesh's geometry bytes are in the
    /// transfer list; `ModelDesc` indexes into it.
    Load { models: Vec<ModelDesc> },
    /// Load a real glTF/GLB from a same-origin URL (the worker fetches it,
    /// parses, and runs the load transaction streaming `Loading` events).
    LoadGltf { url: String },
    /// Re-orient the orbit camera (radians + distance).
    UpdateCamera { yaw: f32, pitch: f32, distance: f32 },
    /// Pick the mesh under a canvas pixel (request → `RenderEvent::PickResult`).
    Pick { x: i32, y: i32 },
}

/// One mesh in a [`RenderCommand::Load`]. `positions_buf`/`indices_buf` index
/// into the transferred buffer array (positions = `f32` xyz triples, indices =
/// `u32`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDesc {
    pub positions_buf: u32,
    pub indices_buf: u32,
    pub translation: [f32; 3],
    pub color: [f32; 4],
}

/// Events the render worker streams back to the driver.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "evt", rename_all = "kebab-case")]
pub enum RenderEvent {
    /// Renderer built; the driver may now send commands.
    Initialized,
    /// A `commit_load` progress tick — drives the driver's progress bar. The
    /// fields mirror `awsm_renderer::loading::LoadingStats`.
    Loading {
        phase: u8,
        phase_label: String,
        geometry_uploaded: usize,
        geometry_total: usize,
        textures_uploaded: usize,
        textures_total: usize,
        pipelines_pending: usize,
        pipelines_ready: usize,
    },
    /// The load transaction committed; the model is on screen.
    Ready,
    /// Reply to a `Pick` command.
    PickResult { hit: bool, mesh_id: f64 },
    /// Something failed.
    Error { message: String },
}

/// `LoadPhase` → wire discriminant + a 0..=1 overall progress fraction, so the
/// driver can render a single bar across the whole commit.
pub fn phase_fraction(stats: &awsm_renderer::loading::LoadingStats) -> (u8, f32) {
    use awsm_renderer::loading::LoadPhase;
    let frac = |done: usize, total: usize| {
        if total == 0 {
            1.0
        } else {
            done as f32 / total as f32
        }
    };
    match stats.phase {
        LoadPhase::Idle => (0, 0.0),
        LoadPhase::UploadingGeometry => (
            1,
            0.40 * frac(stats.geometry_uploaded, stats.geometry_total),
        ),
        LoadPhase::FinalizingTextures => (
            2,
            0.40 + 0.10 * frac(stats.textures_uploaded, stats.textures_total),
        ),
        LoadPhase::Compiling => {
            let total = stats.pipelines_pending + stats.pipelines_ready;
            (3, 0.50 + 0.50 * frac(stats.pipelines_ready, total))
        }
        LoadPhase::Ready => (4, 1.0),
    }
}
