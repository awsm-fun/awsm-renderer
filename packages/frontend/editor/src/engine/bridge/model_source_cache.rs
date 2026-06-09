//! In-memory cache of imported model **source bytes**, keyed by the model's
//! `AssetId`.
//!
//! A glTF/GLB imported from a file arrives as a `blob:` object URL that is
//! revoked right after the load (see `ImportModelFromFile`), and a URL import
//! keeps only the file's *display name* in the asset table — neither leaves a
//! re-loadable source behind. GLB **export** of a `Model` node needs to re-read
//! that node's geometry from the original file, so at import time we stash the
//! raw bytes here, keyed by the minted model `asset_id`. The exporter consults
//! this cache first (see `controller::export::resolve_model_meshes`).
//!
//! TODO(cross-reload persistence): this cache is session-local — it does NOT
//! survive a project Save → reload. Model source bytes are not yet written to
//! the project's `assets/` directory on Save (unlike captured meshes in
//! `mesh_cache`, which persist via `assets/<id>.mesh.bin`). Wiring full
//! persistence means: on Save, write each cached model's bytes to
//! `asset_disk_path(id, entry)` (the entry already carries the original filename
//! to derive the extension; it needs a real `content_hash`), and on Load, read
//! them back into this cache before nodes materialize. Until then, export only
//! recovers Model geometry **within the same editing session** as the import.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use awsm_scene_schema::AssetId;

thread_local! {
    static MODEL_BYTES: RefCell<HashMap<AssetId, Arc<Vec<u8>>>> = RefCell::new(HashMap::new());
}

/// Stash a model's source glTF/GLB bytes under its `asset_id` (idempotent —
/// re-storing the same id replaces). Called at import time, before the blob URL
/// is revoked.
pub fn store(id: AssetId, bytes: Vec<u8>) {
    MODEL_BYTES.with(|c| c.borrow_mut().insert(id, Arc::new(bytes)));
}

/// The cached source bytes for a model `asset_id`, if present.
pub fn get(id: AssetId) -> Option<Arc<Vec<u8>>> {
    MODEL_BYTES.with(|c| c.borrow().get(&id).cloned())
}
