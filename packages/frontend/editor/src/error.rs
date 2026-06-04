//! Editor-wide error type. Most user-facing errors flow through `anyhow` /
//! `String` (action handlers throw modal/toast-friendly messages);
//! `EditorError` carries the structured cases (renderer startup, dispatch).

use thiserror::Error;

pub type EditorResult<T> = Result<T, EditorError>;

// The error variants/constructor are exercised as dispatch grows fallible
// commands in M4+ (insert/import/load can fail); defined now so `dispatch`'s
// `EditorResult` signature is stable.
#[allow(dead_code)]
#[derive(Debug, Error)]
pub enum EditorError {
    #[error("{0}")]
    Msg(String),
    #[error(transparent)]
    Anyhow(#[from] anyhow::Error),
}

impl EditorError {
    #[allow(dead_code)]
    pub fn msg(s: impl Into<String>) -> Self {
        EditorError::Msg(s.into())
    }
}
