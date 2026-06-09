//! Re-exports of the asset-table types from `lockstep-game-data`. The
//! editor never owns a separate copy — `Scene::assets` is the live form
//! of the same type that ends up in `EditorProject`.

pub use awsm_editor_protocol::{AssetId, AssetSource, AssetTable};
