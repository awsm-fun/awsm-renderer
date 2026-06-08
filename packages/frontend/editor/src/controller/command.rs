//! The serializable `EditorCommand` vocabulary — every editor mutation as data.
//!
//! The types now live in the shared [`awsm_editor_protocol`] crate (so the
//! native MCP server constructs the exact same values the editor applies); this
//! module re-exports them at their established path. The apply/inverse
//! *interpreter* — the half that mutates the live controller state — stays in
//! [`super::state`].
pub use awsm_editor_protocol::{CameraAxis, EditorCommand, EditorMode, ProceduralKind};
