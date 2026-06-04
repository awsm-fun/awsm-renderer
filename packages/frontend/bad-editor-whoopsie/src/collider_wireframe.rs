//! Editor-only overlay wireframes — collider shapes, camera frustums,
//! selection gizmos. Drawn through the renderer's fat-line pipeline
//! (B-2) via `sync_editor_wireframes`, called once per frame from the
//! render loop before `renderer.render(...)`.

pub mod render;

pub use render::sync_editor_wireframes;
