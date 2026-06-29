//! Session-local store of custom-material **buffer-slot** data (raw little-endian
//! `u32` words), keyed by the buffer's [`AssetId`].
//!
//! Buffer data bound via `set_material_buffer` is a first-class content-addressed
//! asset (`AssetSource::Buffer`), exactly like an imported texture: the renderer
//! only keeps the words packed into its extras pool, so the originals live here
//! until Save. Persistence (`controller::persistence`) writes each entry to the
//! project's `assets/<content_hash>.bin` side file on Save (via [`get`]) so a
//! buffer override survives Save → reload; on Load the words come back from the
//! file into this cache (`restore_buffers`), NOT preserved across the reset — same
//! session-local caveat as [`texture_cache`](super::texture_cache) / `mesh_cache`.

use std::cell::RefCell;
use std::collections::HashMap;

use awsm_renderer_editor_protocol::AssetId;

thread_local! {
    static BUFFER_WORDS: RefCell<HashMap<AssetId, Vec<u32>>> = RefCell::new(HashMap::new());
}

/// Stash a buffer asset's words under its id (idempotent — re-storing replaces).
pub fn store(id: AssetId, words: Vec<u32>) {
    BUFFER_WORDS.with(|c| c.borrow_mut().insert(id, words));
}

/// The cached words for a buffer asset, if present.
pub fn get(id: AssetId) -> Option<Vec<u32>> {
    BUFFER_WORDS.with(|c| c.borrow().get(&id).cloned())
}

/// Whether a buffer asset's words are cached (used by the save-completeness census).
pub fn contains(id: AssetId) -> bool {
    BUFFER_WORDS.with(|c| c.borrow().contains_key(&id))
}

/// Drop every cached buffer (project reset).
pub fn clear() {
    BUFFER_WORDS.with(|c| c.borrow_mut().clear());
}
