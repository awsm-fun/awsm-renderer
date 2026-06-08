//! The serializable `EditorQuery` / `EditorSnapshot` read surface.
//!
//! The types now live in the shared [`awsm_editor_protocol`] crate; this module
//! re-exports them at their established path. The `query()` *interpreter* (which
//! reads live controller + renderer state) stays in [`super::state`].
pub use awsm_editor_protocol::{
    AnimationSnapshot, ClipSnapshot, CompileDiagnostics, CompileError, EditorQuery, EditorSnapshot,
    MapResult, MaterialSnapshot, PixelsResult, ProjectSnapshot, QueryResult, ReadbackTarget,
    SettledResult, StatsResult, TextureSnapshot, TimeseriesFrame, TimeseriesResult, TrackSnapshot,
};
