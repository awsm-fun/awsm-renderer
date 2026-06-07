//! Material-mode **Studio** (material-mode.jsx + material-shell.jsx) — the
//! custom-WGSL authoring workspace. A 3-column grid:
//! Library (the custom material list) · Definition (surface + declared uniforms/
//! textures/buffers) · main (a code pane + preview placeholder). The Material
//! Contract is a dismissible help drawer.
//!
//! Delivers the full authoring surface, a lightweight in-editor WGSL check, the
//! register/draft lifecycle, the live 2nd-renderer preview, and real GPU
//! registration.

mod studio;

pub use studio::*;
