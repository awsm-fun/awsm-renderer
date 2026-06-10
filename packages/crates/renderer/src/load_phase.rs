//! Coarse load-time progress phases a scene/asset loader reports so a host can
//! show "what's happening now" while a bundle or model materializes.
//!
//! Player-agnostic: the loader (`populate_awsm_scene`, and in time the glTF
//! loader) emits these through a caller-supplied callback; the editor maps them
//! to its activity pill, a headless player can log them. Driving it by callback
//! (rather than a render-loop-polled field) means the host sees live updates
//! even while a loader holds the renderer lock across its awaits — the awaits
//! yield to the event loop, so a reactive UI signal the callback updates still
//! renders.

use crate::pipeline_scheduler::CompileProgress;

/// One stage of a scene/asset load, in the order a phased loader runs them.
#[derive(Clone, Debug)]
pub enum LoadPhase {
    /// Lowering authored materials to renderer materials + inserting them.
    BuildingMaterials { done: usize, total: usize },
    /// Committing all staged texture images to the GPU (one batched upload).
    UploadingTextures,
    /// Uploading mesh geometry (+ skins) referencing the already-built materials.
    UploadingMeshes { done: usize, total: usize },
    /// Driving pipeline compilation to completion (wraps the renderer's
    /// [`CompileProgress`] snapshot).
    CompilingPipelines(CompileProgress),
}

impl LoadPhase {
    /// A short human label for an activity indicator / log line.
    pub fn label(&self) -> String {
        match self {
            LoadPhase::BuildingMaterials { done, total } => {
                format!("Building materials {done}/{total}…")
            }
            LoadPhase::UploadingTextures => "Uploading textures…".to_string(),
            LoadPhase::UploadingMeshes { done, total } => {
                format!("Uploading meshes {done}/{total}…")
            }
            LoadPhase::CompilingPipelines(p) => {
                let n = p.materials_pending + p.in_flight_subcompiles as usize;
                format!("Compiling pipelines ({n})…")
            }
        }
    }
}
