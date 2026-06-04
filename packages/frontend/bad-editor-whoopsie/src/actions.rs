//! Editor action handlers. Every UI click flows through here; the UI layer
//! itself is action-free.

pub mod camera;
pub mod history;
pub mod insert;
#[cfg(debug_assertions)]
pub mod measurement;
pub mod object;
pub mod project;
pub mod view;
