//! 3D viewport editing helpers — grid rendering + transform/point gizmos.
//!
//! Folded in from the former `awsm-renderer-editor` crate (which was eliminated):
//! these are frontend-facing helpers used by the editor and the model-tests
//! viewer, so they live in the shared frontend lib rather than a standalone
//! published crate.
pub mod grid;
pub mod point_handle;
pub mod transform_controller;
