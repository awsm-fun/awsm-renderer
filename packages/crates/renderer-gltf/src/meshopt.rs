//! `EXT_meshopt_compression` decode pass — runs right after buffer bytes are
//! materialized and BEFORE any accessor is read.
//!
//! The extension compresses whole bufferViews: the parent bufferView points at
//! a `fallback: true` buffer (allocated zeroed, never fetched — see
//! [`buffer_is_meshopt_fallback`]), while the extension object carries its own
//! `buffer`/`byteOffset`/`byteLength` naming the REAL compressed bytes plus
//! the decode parameters. This pass meshopt-decodes every such view and writes
//! the reconstructed logical `byteStride × count` bytes into the parent view's
//! range in the fallback buffer, so the entire downstream accessor path
//! (quantized attributes included) reads through unchanged.

use awsm_renderer_codec_meshopt::{decode_buffer_view, Filter, Mode};
use gltf::Document;

const EXT: &str = "EXT_meshopt_compression";

/// A buffer flagged `extensions.EXT_meshopt_compression.fallback: true` holds
/// no data on the wire (no uri, not the GLB BIN chunk); loaders allocate it
/// zeroed at its declared `byteLength` for the decode pass to fill.
pub(crate) fn buffer_is_meshopt_fallback(buffer: &gltf::Buffer) -> bool {
    buffer
        .extension_value(EXT)
        .and_then(|ext| ext.get("fallback"))
        .and_then(|fallback| fallback.as_bool())
        .unwrap_or(false)
}

/// Decode every meshopt-compressed bufferView of `document` in place,
/// returning how many views were decoded (0 = the model doesn't use the
/// extension; the pass is a no-op walk).
pub(crate) fn decode_meshopt_buffer_views(
    document: &Document,
    buffers: &mut [Vec<u8>],
) -> anyhow::Result<usize> {
    let mut decoded_views = 0usize;
    let mut compressed_bytes = 0usize;
    let mut logical_bytes = 0usize;

    for view in document.views() {
        let Some(ext) = view.extension_value(EXT) else {
            continue;
        };
        let view_index = view.index();
        let get = |key: &str| ext.get(key).and_then(|v| v.as_u64());
        let missing =
            |key: &str| anyhow::anyhow!("bufferView {view_index}: {EXT} missing required `{key}`");

        let src_buffer = get("buffer").ok_or_else(|| missing("buffer"))? as usize;
        let src_offset = get("byteOffset").unwrap_or(0) as usize;
        let src_len = get("byteLength").ok_or_else(|| missing("byteLength"))? as usize;
        let count = get("count").ok_or_else(|| missing("count"))? as usize;
        let stride = get("byteStride").ok_or_else(|| missing("byteStride"))? as usize;
        let mode = ext
            .get("mode")
            .and_then(|v| v.as_str())
            .and_then(Mode::from_gltf)
            .ok_or_else(|| missing("mode"))?;
        let filter = match ext.get("filter").and_then(|v| v.as_str()) {
            Some(name) => Filter::from_gltf(name).ok_or_else(|| {
                anyhow::anyhow!("bufferView {view_index}: unknown {EXT} filter `{name}`")
            })?,
            None => Filter::None,
        };

        let src = buffers
            .get(src_buffer)
            .and_then(|b| b.get(src_offset..src_offset + src_len))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "bufferView {view_index}: {EXT} source range {src_offset}+{src_len} exceeds buffer {src_buffer}"
                )
            })?;

        let out = decode_buffer_view(src, count, stride, mode, filter)
            .map_err(|e| anyhow::anyhow!("bufferView {view_index}: meshopt decode failed: {e}"))?;

        let dst_buffer = view.buffer().index();
        let dst_offset = view.offset();
        let dst = buffers
            .get_mut(dst_buffer)
            .and_then(|b| b.get_mut(dst_offset..dst_offset + out.len()))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "bufferView {view_index}: decoded {} bytes exceed parent range at {dst_offset} in buffer {dst_buffer}",
                    out.len()
                )
            })?;
        dst.copy_from_slice(&out);

        decoded_views += 1;
        compressed_bytes += src_len;
        logical_bytes += out.len();
    }

    if decoded_views > 0 {
        tracing::info!(
            "meshopt decode pass: {decoded_views} bufferViews, {compressed_bytes} compressed → {logical_bytes} logical bytes"
        );
    }
    Ok(decoded_views)
}

#[cfg(all(test, has_local_fixtures))]
mod fixture_tests {
    use super::*;
    use crate::loader::parse_gltf_lenient;

    const POLICE_GLB: &[u8] = include_bytes!("../../../../fixtures/local/police-meshopt.glb");

    /// Full decode pass over the real robot: allocate the fallback buffer
    /// zeroed (as the loader does), decode all views, and sanity-check that
    /// accessor-visible data materialized (indices in range, quantized
    /// positions in the normalized-i16 domain).
    #[test]
    fn decode_pass_fills_the_fallback_buffer() {
        let gltf = parse_gltf_lenient(POLICE_GLB).unwrap();
        let doc = &gltf.document;
        let blob = gltf.blob.expect("GLB BIN");

        let mut buffers: Vec<Vec<u8>> = doc
            .buffers()
            .map(|b| {
                if buffer_is_meshopt_fallback(&b) {
                    vec![0u8; b.length()]
                } else {
                    let mut bin = blob.clone();
                    while bin.len() % 4 != 0 {
                        bin.push(0);
                    }
                    bin
                }
            })
            .collect();
        assert!(
            doc.buffers().any(|b| buffer_is_meshopt_fallback(&b)),
            "robot must carry a fallback buffer"
        );

        let decoded = decode_meshopt_buffer_views(doc, &mut buffers).unwrap();
        assert_eq!(decoded, 82, "the robot's 82 meshopt bufferViews");

        // Accessor-level sanity through the standard gltf reader path.
        let mut checked_indices = 0usize;
        let mut checked_positions = 0usize;
        for mesh in doc.meshes() {
            for prim in mesh.primitives() {
                let reader = prim.reader(|buffer| buffers.get(buffer.index()).map(|v| &v[..]));
                let vertex_count = prim.get(&gltf::Semantic::Positions).unwrap().count();
                if let Some(indices) = reader.read_indices() {
                    let max = indices.into_u32().max().unwrap_or(0) as usize;
                    assert!(
                        max < vertex_count,
                        "max index {max} out of range ({vertex_count} verts)"
                    );
                    checked_indices += 1;
                }
                // Quantized POSITION (i16 normalized): raw values must be
                // within the signed-16 domain and not all zero (the fallback
                // buffer started zeroed — all-zero means the pass missed it).
                let acc = prim.get(&gltf::Semantic::Positions).unwrap();
                if acc.data_type() == gltf::accessor::DataType::I16 {
                    let view = acc.view().unwrap();
                    let data = &buffers[view.buffer().index()]
                        [view.offset() + acc.offset()..view.offset() + view.length()];
                    let any_nonzero = data.iter().any(|&b| b != 0);
                    assert!(any_nonzero, "quantized POSITION region left zeroed");
                    checked_positions += 1;
                }
            }
        }
        assert!(checked_indices > 0 && checked_positions > 0);
    }
}
