//! Animation error types and results.

use awsm_renderer_core::error::AwsmCoreError;
use thiserror::Error;

use crate::{meshes::error::AwsmMeshError, transforms::AwsmTransformError};

use super::AnimationKey;

/// Animation result type.
pub type Result<T> = std::result::Result<T, AwsmAnimationError>;

/// Errors related to animation playback and data.
#[derive(Error, Debug)]
pub enum AwsmAnimationError {
    #[error("[animation] {0:?}")]
    Core(#[from] AwsmCoreError),

    #[error("[animation] {0:?}")]
    Transform(#[from] AwsmTransformError),

    #[error("[animation] {0:?}")]
    Mesh(#[from] AwsmMeshError),

    #[error("[animation] {0}")]
    WrongKind(String),

    #[error("[animation] missing animation key {0:?}")]
    MissingKey(AnimationKey),
}
