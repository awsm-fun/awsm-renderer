//! Texture-image extraction — pulls the RAW encoded image bytes (PNG/JPEG) out
//! of a glTF, indexed by glTF image index (the index `materials::TexRef.image`
//! points at). Raw (not decoded) so the editor can write them straight to
//! `assets/<id>.png` and the player decodes on GPU upload, same as it would from
//! a bundle file. Pure data — no decode, no I/O.
//!
//! Covers images embedded as buffer **views** (the GLB-embedded case — what our
//! exporter and most GLBs produce) AND `data:` base64 URIs (self-contained
//! `.gltf`). External-file URIs are out of scope for a pure-data converter (no
//! I/O) — they come back with empty bytes.

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
            // `data:` base64 URI → decode the payload; external-file URI → empty.
            Source::Uri { uri, mime_type } => ImageData {
                mime_type: mime_type.map(String::from).or_else(|| data_uri_mime(uri)),
                bytes: decode_data_uri(uri).unwrap_or_default(),
            },
        })
        .collect()
}

/// Decode a `data:[<mime>][;base64],<payload>` URI's raw bytes (base64 only;
/// percent-encoded text payloads — rare for images — and external-file URIs
/// return `None`).
fn decode_data_uri(uri: &str) -> Option<Vec<u8>> {
    let rest = uri.strip_prefix("data:")?;
    let comma = rest.find(',')?;
    if rest[..comma].contains("base64") {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(&rest[comma + 1..])
            .ok()
    } else {
        None
    }
}

/// The mime type declared in a `data:` URI header (`data:image/png;base64,…`).
fn data_uri_mime(uri: &str) -> Option<String> {
    let meta = uri.strip_prefix("data:")?.split(',').next()?;
    let mime = meta.split(';').next()?.trim();
    (!mime.is_empty()).then(|| mime.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_data_uri_image() {
        use base64::Engine;
        // Arbitrary raw bytes (extract ships them raw; no decode happens).
        let raw: &[u8] = b"\x89PNG\r\n\x1a\nhello-image-bytes";
        let b64 = base64::engine::general_purpose::STANDARD.encode(raw);
        let json = format!(
            r#"{{"asset":{{"version":"2.0"}},"images":[{{"uri":"data:image/png;base64,{b64}"}}]}}"#
        );
        let g = gltf::Gltf::from_slice(json.as_bytes()).expect("parse");
        let imgs = extract_images(&g, &[]);
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0].bytes, raw);
        assert_eq!(imgs[0].mime_type.as_deref(), Some("image/png"));
    }
}
