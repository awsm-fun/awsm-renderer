//! Editor-wide prelude: the web-shared widget/theme/signal surface plus the
//! controller + common std handles every editor module reaches for.

pub use awsm_renderer_web_shared::prelude::*;

pub use wasm_bindgen_futures::spawn_local;

pub use crate::controller::{controller, EditorCommand, EditorController, EditorMode};
