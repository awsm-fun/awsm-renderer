//! glTF ingestion for `awsm-renderer`.
//!
//! **Status:** scaffolding only — the full extraction (moving `gltf/*` from
//! `awsm-renderer` into this crate, converting `impl AwsmRenderer` methods
//! to free functions / extension traits, promoting `pub(crate)` items, and
//! removing the `AwsmRenderer.gltf` field) is tracked as C-2 in
//! `docs/plans/editor-renderer-overhaul-progress.md` and is the next-session
//! work order for this crate.
//!
//! For now this crate exists so:
//! 1. The workspace layout matches the target topology.
//! 2. Callers can be migrated incrementally — they'll switch from
//!    `awsm_renderer::gltf::*` → `awsm_renderer_gltf::*` after the move.
//! 3. The README documents the design.
//!
//! Until the move lands, glTF ingestion still lives in `awsm-renderer` behind
//! its `gltf` feature flag; downstream code continues to import from
//! `awsm_renderer::gltf::*`.
