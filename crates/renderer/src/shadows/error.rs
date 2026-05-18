//! Errors returned by the shadow subsystem.

use awsm_renderer_core::error::AwsmCoreError;
use thiserror::Error;

use crate::{
    bind_group_layout::AwsmBindGroupLayoutError, bind_groups::AwsmBindGroupError,
    pipeline_layouts::AwsmPipelineLayoutError, pipelines::render_pipeline::AwsmRenderPipelineError,
    shaders::AwsmShaderError,
};

/// Errors produced by the shadow subsystem.
///
/// Surfaces from `AwsmRenderer::set_light_shadow_params`,
/// `AwsmRenderer::set_mesh_shadow_flags`, atlas allocation, and the
/// cube-pool slot allocator. Wraps `AwsmCoreError` for low-level GPU
/// failures so the rest of the renderer can convert into `AwsmError`
/// via `?` with no boilerplate.
#[derive(Error, Debug)]
pub enum AwsmShadowError {
    /// The light key passed to a setter does not exist in `Lights`.
    #[error("[shadow] unknown light key")]
    UnknownLight,
    /// The mesh key passed to a setter does not exist in `Meshes`.
    #[error("[shadow] unknown mesh key")]
    UnknownMesh,
    /// More shadow-casting point lights were requested than there are
    /// slots in the cube pool. The numeric argument is the current
    /// capacity; raise `ShadowsConfig::max_point_shadows` to grow it.
    #[error("[shadow] point-light cube pool exhausted (capacity {0}); raise `max_point_shadows`")]
    CubePoolExhausted(u32),
    /// The combined size of all shadow rects exceeds the 2D atlas.
    #[error("[shadow] atlas too small for requested resolutions ({need} > {have})")]
    AtlasTooSmall {
        /// Total area (or required dimension) the caller asked for.
        need: u32,
        /// What the atlas can currently accommodate.
        have: u32,
    },
    /// Pass-through for GPU-side failures.
    #[error("[shadow] {0}")]
    Core(#[from] AwsmCoreError),
    /// Shader compilation / template error.
    #[error("[shadow] {0}")]
    Shader(#[from] AwsmShaderError),
    /// Bind-group layout failure.
    #[error("[shadow] {0}")]
    BindGroupLayout(#[from] AwsmBindGroupLayoutError),
    /// Bind-group lookup failure.
    #[error("[shadow] {0}")]
    BindGroup(#[from] AwsmBindGroupError),
    /// Pipeline layout failure.
    #[error("[shadow] {0}")]
    PipelineLayout(#[from] AwsmPipelineLayoutError),
    /// Render pipeline failure.
    #[error("[shadow] {0}")]
    RenderPipeline(#[from] AwsmRenderPipelineError),
}
