//! The export itself, as a library.
//!
//! `main.rs` is only argument parsing and file writing; everything that reads
//! `mjModel` lives here so the integration tests can drive it against real models
//! without shelling out to the binary.

pub mod mesh;
pub mod sidecar;
