//! Pipeline caches for render and compute passes.

pub mod compute_pipeline;
pub mod render_pipeline;

use crate::pipelines::{compute_pipeline::ComputePipelines, render_pipeline::RenderPipelines};

/// Combined render and compute pipeline caches.
pub struct Pipelines {
    pub render: RenderPipelines,
    pub compute: ComputePipelines,
}

impl Pipelines {
    /// Creates empty pipeline caches.
    pub fn new() -> Self {
        Self {
            render: RenderPipelines::new(),
            compute: ComputePipelines::new(),
        }
    }

    /// Put both caches in or out of DEFERRED mode (see
    /// `ComputePipelines::set_deferred`).
    pub fn set_deferred(&mut self, deferred: bool) {
        self.render.set_deferred(deferred);
        self.compute.set_deferred(deferred);
    }

    /// Whether the caches are in DEFERRED mode.
    pub fn is_deferred(&self) -> bool {
        self.compute.is_deferred()
    }
}

impl Default for Pipelines {
    fn default() -> Self {
        Self::new()
    }
}
