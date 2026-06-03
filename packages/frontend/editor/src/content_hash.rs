//! SHA-256 of asset bytes, lowercase hex.
//!
//! Used by every upload path (image picker, glb importer, glTF
//! embedded-image extractor, KTX picker) to compute the
//! `AssetEntry::content_hash`. The hash addresses the on-disk file
//! and dedups uploads — two identical files share one `AssetId`.

use sha2::{Digest, Sha256};

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}
