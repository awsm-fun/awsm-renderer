//! `create_texture` payload decoding — the pure, host-testable half of the
//! generic raw-texture-upload primitive (★ in `docs/plans/mcp-improvements.md`).
//!
//! The MCP `create_texture` tool lets the agent author *any* texture itself —
//! a soft particle sprite, an fbm height/normal map, a gradient, a cubemap face
//! — instead of choosing from a fixed procedural menu. The agent ships the
//! pixels (or an encoded image) as base64; this module turns that wire string
//! into bytes the GPU-upload bridge consumes. The actual upload is wasm-only
//! (`engine::bridge::material::create_texture`); everything here is plain data
//! so it unit-tests natively.

use base64::Engine;

/// Decoded `create_texture` input, ready for the GPU-upload bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TexturePayload {
    /// Raw RGBA8 pixels, row-major, top-left origin. `bytes.len() ==
    /// width * height * 4`.
    RawRgba8 {
        bytes: Vec<u8>,
        width: u32,
        height: u32,
    },
    /// Encoded image bytes (PNG/JPEG/WebP) for the browser to decode. `mime` is
    /// the source mime when a `data:` URI carried one (decoders sniff content
    /// regardless, so it is only a hint).
    Encoded {
        bytes: Vec<u8>,
        mime: Option<String>,
    },
}

/// Parse a `create_texture` request into a [`TexturePayload`]. Pure (no GPU), so
/// it is unit-tested natively. Two shapes, selected by `format`:
///
/// - `format = "rgba8"` → **raw pixels**: `data` is base64 of exactly
///   `width * height * 4` bytes; both dimensions are required and the byte
///   length must match (rejected loudly otherwise).
/// - `format` omitted (or a named image codec) → **encoded image**: `data` is a
///   `data:` URI (`data:image/png;base64,…`) or bare base64 of a PNG/JPEG/WebP;
///   the mime is taken from the URI when present.
pub fn decode_texture_payload(
    data: &str,
    width: Option<u32>,
    height: Option<u32>,
    format: Option<&str>,
) -> Result<TexturePayload, String> {
    match format.map(|f| f.trim().to_ascii_lowercase()) {
        // Raw pixel path — the caller explicitly asked for a pixel format.
        Some(fmt) if matches!(fmt.as_str(), "rgba8" | "rgba8unorm" | "rgba") => {
            let w = width.ok_or("rgba8 requires `width`")?;
            let h = height.ok_or("rgba8 requires `height`")?;
            let expected = (w as usize)
                .checked_mul(h as usize)
                .and_then(|n| n.checked_mul(4))
                .ok_or("width * height * 4 overflows")?;
            let (bytes, _) = decode_b64_or_datauri(data)?;
            if bytes.len() != expected {
                return Err(format!(
                    "rgba8 data is {} bytes, expected width*height*4 = {expected}",
                    bytes.len()
                ));
            }
            Ok(TexturePayload::RawRgba8 {
                bytes,
                width: w,
                height: h,
            })
        }
        // Named encoded codecs — decode + carry a mime hint.
        Some(fmt) if matches!(fmt.as_str(), "png" | "jpeg" | "jpg" | "webp") => {
            let (bytes, mime) = decode_b64_or_datauri(data)?;
            let mime = mime.or_else(|| {
                Some(
                    match fmt.as_str() {
                        "jpg" | "jpeg" => "image/jpeg",
                        "webp" => "image/webp",
                        _ => "image/png",
                    }
                    .to_string(),
                )
            });
            Ok(TexturePayload::Encoded { bytes, mime })
        }
        Some(other) => Err(format!(
            "unknown texture format `{other}` (use `rgba8` for raw pixels, or omit `format` for an encoded image)"
        )),
        // No format → encoded image (data: URI or bare base64).
        None => {
            let (bytes, mime) = decode_b64_or_datauri(data)?;
            Ok(TexturePayload::Encoded { bytes, mime })
        }
    }
}

/// Decode `data` that is either a `data:[<mime>][;base64],<payload>` URI or bare
/// standard base64. Returns the bytes + the URI's mime (if present). Whitespace
/// in the payload (line-wrapped base64) is tolerated. Only base64 `data:` URIs
/// are supported — the common case for image payloads.
fn decode_b64_or_datauri(data: &str) -> Result<(Vec<u8>, Option<String>), String> {
    let b64 = base64::engine::general_purpose::STANDARD;
    let trimmed = data.trim();
    if let Some(rest) = trimmed.strip_prefix("data:") {
        // rest = "<mime>[;base64],<payload>" (mime may be empty).
        let (meta, payload) = rest
            .split_once(',')
            .ok_or("malformed data: URI (no comma)")?;
        if !meta.contains("base64") {
            return Err("only base64 data: URIs are supported".to_string());
        }
        let mime = meta
            .split(';')
            .next()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let payload: String = payload.split_whitespace().collect();
        let bytes = b64
            .decode(payload)
            .map_err(|e| format!("base64 decode: {e}"))?;
        Ok((bytes, mime))
    } else {
        let payload: String = trimmed.split_whitespace().collect();
        let bytes = b64
            .decode(payload)
            .map_err(|e| format!("base64 decode: {e}"))?;
        Ok((bytes, None))
    }
}

/// Build a solid-color `width × height` RGBA8 buffer (row-major, top-left
/// origin) — each channel of `color` (`[0,1]` linear) clamped + quantized to a
/// byte. The placeholder fill for `BakeMaterialToTexture` (a real material bake
/// renders the shaded surface in UV space; that is the deferred GPU work — see
/// the command docs). `bytes.len() == width * height * 4`.
pub fn solid_rgba8(width: u32, height: u32, color: [f32; 4]) -> Vec<u8> {
    let px: [u8; 4] = [
        (color[0].clamp(0.0, 1.0) * 255.0).round() as u8,
        (color[1].clamp(0.0, 1.0) * 255.0).round() as u8,
        (color[2].clamp(0.0, 1.0) * 255.0).round() as u8,
        (color[3].clamp(0.0, 1.0) * 255.0).round() as u8,
    ];
    let count = (width as usize) * (height as usize);
    let mut out = Vec::with_capacity(count * 4);
    for _ in 0..count {
        out.extend_from_slice(&px);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn solid_rgba8_fills_uniformly() {
        let bytes = solid_rgba8(4, 2, [1.0, 0.0, 0.5, 1.0]);
        assert_eq!(bytes.len(), 4 * 2 * 4);
        // Every pixel is the same RGBA (red=255, green=0, blue≈128, alpha=255).
        for px in bytes.chunks_exact(4) {
            assert_eq!(px[0], 255);
            assert_eq!(px[1], 0);
            assert_eq!(px[2], 128);
            assert_eq!(px[3], 255);
        }
        // Out-of-range channels clamp.
        let clamped = solid_rgba8(1, 1, [2.0, -1.0, 0.0, 5.0]);
        assert_eq!(clamped, vec![255, 0, 0, 255]);
    }

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    #[test]
    fn raw_rgba8_roundtrips() {
        // 2x2 RGBA = 16 bytes.
        let pixels: Vec<u8> = (0..16).collect();
        let got = decode_texture_payload(&b64(&pixels), Some(2), Some(2), Some("rgba8")).unwrap();
        assert_eq!(
            got,
            TexturePayload::RawRgba8 {
                bytes: pixels,
                width: 2,
                height: 2
            }
        );
    }

    #[test]
    fn raw_rgba8_aliases_accepted() {
        let pixels = vec![0u8; 4];
        for fmt in ["rgba8", "RGBA8", "rgba8unorm", "rgba"] {
            let got = decode_texture_payload(&b64(&pixels), Some(1), Some(1), Some(fmt)).unwrap();
            assert!(matches!(
                got,
                TexturePayload::RawRgba8 {
                    width: 1,
                    height: 1,
                    ..
                }
            ));
        }
    }

    #[test]
    fn raw_rgba8_wrong_length_rejected_loudly() {
        // 3 bytes can't be a 1x1 RGBA (needs 4).
        let err =
            decode_texture_payload(&b64(&[1, 2, 3]), Some(1), Some(1), Some("rgba8")).unwrap_err();
        assert!(err.contains("expected width*height*4"), "{err}");
    }

    #[test]
    fn raw_rgba8_missing_dims_rejected() {
        let err = decode_texture_payload(&b64(&[0; 4]), None, Some(1), Some("rgba8")).unwrap_err();
        assert!(err.contains("`width`"), "{err}");
        let err = decode_texture_payload(&b64(&[0; 4]), Some(1), None, Some("rgba8")).unwrap_err();
        assert!(err.contains("`height`"), "{err}");
    }

    #[test]
    fn data_uri_png_decodes_with_mime() {
        let bytes = vec![0x89, b'P', b'N', b'G', 1, 2, 3];
        let uri = format!("data:image/png;base64,{}", b64(&bytes));
        let got = decode_texture_payload(&uri, None, None, None).unwrap();
        assert_eq!(
            got,
            TexturePayload::Encoded {
                bytes,
                mime: Some("image/png".to_string())
            }
        );
    }

    #[test]
    fn bare_base64_no_format_is_encoded() {
        let bytes = vec![1u8, 2, 3, 4, 5];
        let got = decode_texture_payload(&b64(&bytes), None, None, None).unwrap();
        assert_eq!(got, TexturePayload::Encoded { bytes, mime: None });
    }

    #[test]
    fn named_codec_format_carries_mime_hint() {
        let bytes = vec![0xFF, 0xD8, 0xFF];
        let got = decode_texture_payload(&b64(&bytes), None, None, Some("jpeg")).unwrap();
        assert_eq!(
            got,
            TexturePayload::Encoded {
                bytes,
                mime: Some("image/jpeg".to_string())
            }
        );
    }

    #[test]
    fn unknown_format_rejected() {
        let err = decode_texture_payload("AAAA", Some(1), Some(1), Some("rgba16f")).unwrap_err();
        assert!(err.contains("unknown texture format"), "{err}");
    }

    #[test]
    fn non_base64_data_uri_rejected() {
        let err =
            decode_texture_payload("data:image/png,rawnotbase64", None, None, None).unwrap_err();
        assert!(err.contains("base64 data: URIs"), "{err}");
    }

    #[test]
    fn line_wrapped_base64_tolerated() {
        let bytes = vec![7u8; 64];
        let wrapped = b64(&bytes)
            .as_bytes()
            .chunks(16)
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .collect::<Vec<_>>()
            .join("\n");
        let got = decode_texture_payload(&wrapped, None, None, None).unwrap();
        assert_eq!(got, TexturePayload::Encoded { bytes, mime: None });
    }
}
