//! Animation mode — the third editor workspace (Scene · Material · **Animation**),
//! a clip-authoring studio.
//!
//! Layout lives in [`workspace`]: the **ribbon** (active-clip header) over a
//! `248px · 1fr` grid — the left column stacks the **ClipLibrary** over the
//! **KeyInspector**, the right column the real-scene **viewport** over the
//! **timeline dock** (transport · ruler · Dope Sheet / Curves / Mixer).
//!
//! Load-bearing rule: every animation mutation is a serializable `EditorCommand`
//! dispatched through the one `EditorController` — the UI never mutates animation
//! state directly.

mod add_track;
mod inspector;
mod library;
mod ribbon;
mod timeline;
mod viewport;
mod workspace;

pub use workspace::render;
