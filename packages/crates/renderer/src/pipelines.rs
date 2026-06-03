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
}

impl Default for Pipelines {
    fn default() -> Self {
        Self::new()
    }
}
