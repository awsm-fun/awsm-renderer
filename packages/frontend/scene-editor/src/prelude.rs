//! Glob-import surface shared by every editor module.
//!
//! Re-exports the frontend-shared prelude (`Mutable`, `Arc`, dominator
//! macros, theme atoms, etc.) plus the editor's own context handles + the
//! `EditorError` / `EditorResult` aliases.

#[allow(unused_imports)]
pub use crate::context::*;
#[allow(unused_imports)]
pub use crate::error::*;
pub use awsm_web_shared::prelude::*;
