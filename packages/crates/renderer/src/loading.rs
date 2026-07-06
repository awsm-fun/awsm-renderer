//! The load-transaction progress surface.
//!
//! [`LoadingStats`] is the single struct both [`crate::AwsmRenderer::commit_load`]'s
//! `on_progress` callback and the imperative [`crate::AwsmRenderer::loading_stats`]
//! poller report. It supersedes the ad-hoc `CompileProgress`-only progress that the
//! old `wait_for_pipelines_ready_with_progress` surfaced: one struct carries the
//! texture-upload phase AND the pipeline-compile phase of a commit, so a loader can
//! drive a single progress bar across the whole transaction.
//!
//! (The coarser, per-load-step [`crate::load_phase::LoadPhase`] a scene loader emits
//! while *building* a scene is a different, higher-level thing — it brackets the adds
//! that precede the commit; `LoadingStats` describes the commit itself.)

use crate::pipeline_scheduler::CompileProgress;

/// Which phase of a [`crate::AwsmRenderer::commit_load`] is in flight.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LoadPhase {
    /// No commit has run yet (or the renderer is between commits).
    #[default]
    Idle,
    /// Deriving + uploading each registered geometry's needed pass representations
    /// (visibility / transparency) from its retained source — the first commit phase.
    UploadingGeometry,
    /// Finalizing the texture pool — the one batched GPU upload of every staged image.
    FinalizingTextures,
    /// Reconciling material variants against the final pool (synchronous WGSL
    /// codegen + kicking the scene's pipeline compiles). Reported so this CPU
    /// work isn't misattributed to the texture phase's last snapshot — on slow
    /// machines it can dominate the commit's wall clock.
    PreparingMaterials,
    /// Driving the scene's pipeline compiles to completion.
    Compiling,
    /// The commit landed; the scene is committed and renders this frame on.
    Ready,
}

/// Snapshot of a load transaction's progress. Reported by `commit_load`'s
/// `on_progress` (live, per resolution) and by `loading_stats()` (imperative poll).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoadingStats {
    /// Current commit phase.
    pub phase: LoadPhase,
    /// Total geometries whose representations this commit will (re)build.
    pub geometry_total: usize,
    /// Geometries resolved (representations uploaded) so far.
    pub geometry_uploaded: usize,
    /// Total textures the pool will upload this commit.
    pub textures_total: usize,
    /// Textures uploaded so far.
    pub textures_uploaded: usize,
    /// Materials still compiling (`CompileProgress::materials_pending`).
    pub pipelines_pending: usize,
    /// Materials fully resolved (`CompileProgress::materials_ready`).
    pub pipelines_ready: usize,
    /// Materials whose compile failed (`CompileProgress::materials_failed`).
    pub pipelines_failed: usize,
    /// In-flight sub-pipeline compiles summed across pending materials.
    pub in_flight_subcompiles: u32,
}

impl LoadingStats {
    /// Pipelines still in flight this commit (materials pending + their summed
    /// sub-pipeline compiles) — the single "how much compile is left" number.
    pub fn pipelines_remaining(&self) -> usize {
        self.pipelines_pending + self.in_flight_subcompiles as usize
    }

    /// Human-facing progress line for the active commit phase — the SHARED mapping
    /// both viewers' loading overlays render, so geometry/texture/pipeline progress
    /// reads identically everywhere. `None` for `Idle` /
    /// `Ready` (no banner needed).
    pub fn phase_label(&self) -> Option<String> {
        match self.phase {
            LoadPhase::Idle | LoadPhase::Ready => None,
            LoadPhase::UploadingGeometry => Some(format!(
                "Uploading geometry {}/{}",
                self.geometry_uploaded, self.geometry_total
            )),
            LoadPhase::FinalizingTextures => Some(format!(
                "Uploading textures {}/{}",
                self.textures_uploaded, self.textures_total
            )),
            LoadPhase::PreparingMaterials => Some("Preparing materials".to_string()),
            LoadPhase::Compiling => Some(format!(
                "Compiling pipelines ({} remaining)",
                self.pipelines_remaining()
            )),
        }
    }

    /// Build a snapshot from a [`CompileProgress`] plus the commit's texture counts
    /// and phase. The compile drain calls this per resolution to map the scheduler's
    /// `CompileProgress` into the unified `LoadingStats` shape.
    pub(crate) fn from_parts(
        phase: LoadPhase,
        geometry_total: usize,
        geometry_uploaded: usize,
        textures_total: usize,
        textures_uploaded: usize,
        cp: CompileProgress,
    ) -> Self {
        Self {
            phase,
            geometry_total,
            geometry_uploaded,
            textures_total,
            textures_uploaded,
            pipelines_pending: cp.materials_pending,
            pipelines_ready: cp.materials_ready,
            pipelines_failed: cp.materials_failed,
            in_flight_subcompiles: cp.in_flight_subcompiles,
        }
    }
}
