//! Mesh-related errors.

use awsm_renderer_core::error::AwsmCoreError;
use thiserror::Error;

use super::{skins::AwsmSkinError, MeshBufferInfoKey, MeshResourceKey};
use crate::{
    bind_groups::AwsmBindGroupError, materials::AwsmMaterialError, transforms::AwsmTransformError,
};

use super::MeshKey;
use crate::transforms::TransformKey;

/// Result type for mesh operations.
pub type Result<T> = std::result::Result<T, AwsmMeshError>;

/// Mesh-related errors.
#[derive(Error, Debug)]
pub enum AwsmMeshError {
    #[error("[mesh] not found: {0:?}")]
    MeshNotFound(MeshKey),

    #[error("[mesh] resource not found: {0:?}")]
    ResourceNotFound(MeshResourceKey),

    #[error("[mesh] instancing not enabled: {0:?}")]
    InstancingNotEnabled(MeshKey),

    #[error("[mesh] instancing already enabled: {0:?}")]
    InstancingAlreadyEnabled(MeshKey),

    #[error("[mesh] instance transforms missing or empty: {0:?}")]
    InstancingMissingTransforms(MeshKey),

    #[error("[mesh] instanced mesh unsupported for this operation: {0:?}")]
    InstancedMeshUnsupported(MeshKey),

    #[error("[mesh] transform has no meshes: {0:?}")]
    TransformHasNoMeshes(TransformKey),

    #[error("[mesh] mesh list is empty")]
    MeshListEmpty,

    #[error("[mesh] visibility geometry buffer not found: {0:?}")]
    VisibilityGeometryBufferNotFound(MeshKey),

    #[error("[mesh] transparency geometry buffer not found: {0:?}")]
    TransparencyGeometryBufferNotFound(MeshKey),

    #[error("[mesh] transparency geometry buffer info not found: {0:?}")]
    VisibilityGeometryBufferInfoNotFound(MeshBufferInfoKey),

    #[error("[mesh] custom attribute buffer not found: {0:?}")]
    CustomAttributeBufferNotFound(MeshKey),

    #[error("[mesh] metadata not found: {0:?}")]
    MetaNotFound(MeshKey),

    #[error("[mesh] {0:?}")]
    Core(#[from] AwsmCoreError),

    #[error("[mesh] {0:?}")]
    Transform(#[from] AwsmTransformError),

    #[error("[mesh] {0:?}")]
    Material(#[from] AwsmMaterialError),

    #[error("[mesh] {0:?}")]
    Skin(#[from] AwsmSkinError),

    #[error("[mesh] morph not found: {0}")]
    MorphNotFound(String),

    #[error("[mesh] morph must have same number of weights as targets: {weights} weights != {targets} targets")]
    MorphWeightsTargetsMismatch { weights: usize, targets: usize },

    #[error("[mesh] {0:?}")]
    BindGroup(#[from] AwsmBindGroupError),

    #[error("[mesh] buffer info not found: {0:?}")]
    BufferInfoNotFound(MeshBufferInfoKey),

    #[error("[mesh] buffer capacity overflow: {0}")]
    BufferCapacityOverflow(String),
}
