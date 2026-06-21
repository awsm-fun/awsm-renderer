//! Session-local store of **encoded** imported-texture bytes (the original
//! PNG/JPEG), keyed by the texture's [`AssetId`].
//!
//! The renderer keeps only DECODED pixels, so the originals are grabbed off the
//! glTF document at import (`awsm_glb_export::extract_texture_images`) and stashed
//! here. Persistence (`controller::persistence`) writes each entry to the
//! project's `assets/<content_hash>.<ext>` side file on Save (via [`get`]) so
//! imported textures survive Save → reload; on Load the bytes come back from the
//! file (decoded + re-uploaded), NOT from this cache (it's session-local — same
//! caveat as `skinned_bake_cache` / `mesh_cache`).

use std::cell::RefCell;
use std::collections::HashMap;

use awsm_editor_protocol::AssetId;
use awsm_glb_export::ImageMime;

thread_local! {
    static TEXTURE_BYTES: RefCell<HashMap<AssetId, (Vec<u8>, ImageMime)>> =
        RefCell::new(HashMap::new());
}

/// Stash an imported texture's encoded bytes + mime under its asset id
/// (idempotent — re-storing replaces).
pub fn store(id: AssetId, bytes: Vec<u8>, mime: ImageMime) {
    TEXTURE_BYTES.with(|c| c.borrow_mut().insert(id, (bytes, mime)));
}

/// The cached encoded bytes + mime for a texture asset, if present.
pub fn get(id: AssetId) -> Option<(Vec<u8>, ImageMime)> {
    TEXTURE_BYTES.with(|c| c.borrow().get(&id).cloned())
}

/// Drop every cached texture (project reset).
pub fn clear() {
    TEXTURE_BYTES.with(|c| c.borrow_mut().clear());
}
