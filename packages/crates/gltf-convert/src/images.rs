//! Texture-image extraction — pulls the RAW encoded image bytes (PNG/JPEG) out
//! of a glTF, indexed by glTF image index (the index `materials::TexRef.image`
//! points at). Raw (not decoded) so the editor can write them straight to
//! `assets/<id>.png` and the player decodes on GPU upload, same as it would from
//! a bundle file. Pure data — no decode, no I/O.
//!
//! Covers glated images embedded as buffer **views** (the GLB-embedded case —
//! what our exporter and most GLBs produce). FOLLOW-ON: `data:` URI images
//! (self-contained `.gltf`) need base64 decoding (not a dep yet); external-file
//! URIs are out of scope for a pure-data converter (no I/O).

/// One texture image's raw encoded bytes + mime type, index-aligned with
/// `doc.images()`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ImageData {
    /// e.g. `"image/png"` / `"image/jpeg"`. `None` if the source didn't declare one.
    pub mime_type: Option<String>,
    /// Raw encoded bytes (PNG/JPEG). Empty when the source is an unsupported
    /// `data:`/external URI (see module docs) — a follow-on.
    pub bytes: Vec<u8>,
}

/// Extract every image's raw bytes, index-aligned with `doc.images()`.
/// `buffers` is the resolved glTF buffer-bytes (as from `gltf::import`).
pub fn extract_images(doc: &gltf::Document, buffers: &[Vec<u8>]) -> Vec<ImageData> {
    use gltf::image::Source;
    doc.images()
        .map(|img| match img.source() {
            Source::View { view, mime_type } => {
                let buf = view.buffer().index();
                let start = view.offset();
                let end = start + view.length();
                let bytes = buffers
                    .get(buf)
                    .and_then(|b| b.get(start..end))
                    .map(<[u8]>::to_vec)
                    .unwrap_or_default();
                ImageData {
                    mime_type: Some(mime_type.to_string()),
                    bytes,
                }
            }
            // data:/external URI — bytes left empty (follow-on; needs base64 / I/O).
            Source::Uri { mime_type, .. } => ImageData {
                mime_type: mime_type.map(String::from),
                bytes: Vec::new(),
            },
        })
        .collect()
}
