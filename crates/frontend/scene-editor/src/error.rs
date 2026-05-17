//! Editor-wide error type.
//!
//! Most user-facing errors flow through `anyhow` / `String` (action
//! handlers throw modal-friendly messages). `EditorError` is used by the
//! parts of the editor where structured errors carry useful info — today
//! the renderer-startup path.

use thiserror::Error;

pub type EditorResult<T> = Result<T, EditorError>;

#[derive(Debug, Error)]
pub enum EditorError {
    #[error("AwsmRenderer: {0}")]
    Awsm(#[from] awsm_renderer::error::AwsmError),
}
