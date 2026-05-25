//! Renderer error types and results.

use awsm_renderer_core::{error::AwsmCoreError, pipeline::primitive::CullMode};
use thiserror::Error;

use crate::{
    bind_group_layout::AwsmBindGroupLayoutError,
    bind_groups::AwsmBindGroupError,
    camera::AwsmCameraError,
    instances::AwsmInstanceError,
    lights::AwsmLightError,
    materials::AwsmMaterialError,
    meshes::{error::AwsmMeshError, skins::AwsmSkinError},
    pipeline_layouts::AwsmPipelineLayoutError,
    pipelines::{
        compute_pipeline::AwsmComputePipelineError, render_pipeline::AwsmRenderPipelineError,
    },
    render_textures::AwsmRenderTextureError,
    shaders::AwsmShaderError,
    shadows::AwsmShadowError,
    textures::AwsmTextureError,
    transforms::AwsmTransformError,
};

/// Errors returned by the renderer crate.
#[derive(Error, Debug)]
pub enum AwsmError {
    #[error("{0}")]
    Core(#[from] AwsmCoreError),

    #[error("{0}")]
    Camera(#[from] AwsmCameraError),

    #[error("{0}")]
    Mesh(#[from] AwsmMeshError),

    #[error("{0}")]
    Transform(#[from] AwsmTransformError),

    #[cfg(feature = "animation")]
    #[error("{0}")]
    Animation(#[from] crate::animation::AwsmAnimationError),

    #[error("{0}")]
    Skin(#[from] AwsmSkinError),

    #[error("{0}")]
    BindGroup(#[from] AwsmBindGroupError),

    #[error("{0}")]
    BindGroupLayout(#[from] AwsmBindGroupLayoutError),

    #[error("{0}")]
    Shader(#[from] AwsmShaderError),

    #[error("{0}")]
    Instance(#[from] AwsmInstanceError),

    #[error("{0}")]
    Material(#[from] AwsmMaterialError),

    #[error("{0}")]
    PipelineLayout(#[from] AwsmPipelineLayoutError),

    #[error("{0}")]
    RenderPipeline(#[from] AwsmRenderPipelineError),

    #[error("{0}")]
    ComputePipeline(#[from] AwsmComputePipelineError),

    #[error("{0}")]
    Light(#[from] AwsmLightError),

    #[error("{0}")]
    RenderTexture(#[from] AwsmRenderTextureError),

    #[error("Unregistered Msaa count: {0}")]
    UnsupportedMsaaCount(u32),

    #[error("Unregistered Cull Mode: {0:?}")]
    UnsupportedCullMode(CullMode),

    #[error("{0}")]
    Texture(#[from] AwsmTextureError),

    #[error("{0}")]
    OpaqueMipgen(#[from] crate::opaque_mipgen::AwsmOpaqueMipgenError),

    #[error("{0}")]
    Shadow(#[from] AwsmShadowError),

    #[error("{0}")]
    FrameGlobals(#[from] crate::frame_globals::AwsmFrameGlobalsError),
}

/// Renderer result type.
pub type Result<T> = std::result::Result<T, AwsmError>;
