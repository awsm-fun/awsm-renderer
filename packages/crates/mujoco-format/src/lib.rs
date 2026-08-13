//! Frozen interchange schemas for MuJoCo support.
//!
//! These are *stable seams*, not internal types: a third-party pipeline (a Python
//! RL shop, a CI job) must be able to produce a valid sidecar without any of our
//! code. Our exporter is merely the first producer. That has consequences the rest
//! of the codebase does not share:
//!
//! - **Nothing MuJoCo-specific is compiled in.** No `libmujoco`, no FFI — just
//!   serde. The editor and scene-loader (wasm) depend on this crate; the native
//!   exporter depends on it *and* on `mujoco-sys`.
//! - **Enums serialize as documented strings, not discriminants**, so a
//!   hand-written JSON file is readable and MuJoCo's own numbering never leaks.
//! - **Unknown fields are accepted and new fields default**, so a v1 reader
//!   survives a v1.x producer.
//!
//! ## The two formats
//!
//! - [`sidecar`] — what a compiled model *is*: geoms, materials, meshes, groups,
//!   the source fingerprint. Written once at export, imported once by the editor.
//! - [`capture`] — what a simulation *did*: a sequence of stream frames. Written
//!   by whatever ran the sim, baked into ordinary animation clips, then thrown
//!   away.
//!
//! ## Coordinates
//!
//! Everything here is **raw MuJoCo**: Z-up, right-handed, metres, quaternions as
//! `[w, x, y, z]`. There is deliberately no conversion at this layer — the Y-up
//! flip lives once, on the sim instance's root node in the scene (see
//! `docs/mujoco.md`, "Conventions"). Converting here as well would apply it
//! twice, and would put a second copy of the convention in a format third parties
//! write by hand.

pub mod capture;
pub mod sidecar;

pub use capture::Capture;
pub use sidecar::Sidecar;
