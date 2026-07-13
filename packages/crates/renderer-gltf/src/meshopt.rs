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

/// Encode→decode round-trip over the FULL pipeline: glb-export's
/// `compress_glb` (quantize + meshopt) → this crate's parse + decode pass +
/// quantization-aware reads. Synthetic data — always runs.
#[cfg(test)]
mod roundtrip_tests {
    use super::*;
    use crate::loader::parse_gltf_lenient;
    use awsm_renderer_glb_export::{compress_glb, write_glb, ExportNode, GlbScene, MeshData};

    fn grid_scene() -> (GlbScene, MeshData) {
        // 9×9 grid with wavy z, normals up-ish, UVs in [0,1].
        let mut mesh = MeshData::default();
        for y in 0..9 {
            for x in 0..9 {
                let (fx, fy) = (x as f32 / 8.0, y as f32 / 8.0);
                mesh.positions
                    .push([fx * 4.0 - 2.0, fy * 2.0 + 1.0, (fx * 7.0).sin() * 0.3]);
                mesh.uvs
                    .get_mut(0)
                    .map(|_| ())
                    .unwrap_or_else(|| mesh.uvs.push(Vec::new()));
                mesh.uvs[0].push([fx, fy]);
            }
        }
        for y in 0..8u32 {
            for x in 0..8u32 {
                let a = y * 9 + x;
                mesh.indices
                    .extend_from_slice(&[a, a + 1, a + 9, a + 9, a + 1, a + 10]);
            }
        }
        mesh.compute_vertex_normals();
        let scene = GlbScene {
            nodes: vec![ExportNode::new("grid").with_mesh(mesh.clone())],
            animations: vec![],
            skins: vec![],
            images: vec![],
            env: None,
        };
        (scene, mesh)
    }

    #[test]
    fn compressed_glb_roundtrips_through_the_import_decode_path() {
        let (scene, source) = grid_scene();
        let plain = write_glb(&scene);
        let compressed = compress_glb(&plain).expect("compress");
        assert!(
            compressed.len() < plain.len(),
            "compression should shrink the grid ({} -> {})",
            plain.len(),
            compressed.len()
        );

        // extensionsRequired must be on the WIRE (parse_gltf_lenient strips
        // the supported ones before re-validating, so check raw JSON).
        let json_len = u32::from_le_bytes(compressed[12..16].try_into().unwrap()) as usize;
        let raw_json: gltf::json::Value =
            gltf::json::deserialize::from_slice(&compressed[20..20 + json_len]).unwrap();
        let required: Vec<&str> = raw_json["extensionsRequired"]
            .as_array()
            .expect("extensionsRequired present")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(required.contains(&"EXT_meshopt_compression"));
        assert!(required.contains(&"KHR_mesh_quantization"));

        let gltf = parse_gltf_lenient(&compressed).expect("compressed glb parses");
        let doc = &gltf.document;

        // Materialize buffers the way the loader does.
        let blob = gltf.blob.clone().expect("BIN");
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
        let decoded = decode_meshopt_buffer_views(doc, &mut buffers).expect("decode pass");
        assert!(decoded >= 3, "positions + normals + uvs + indices views");

        // The quantized mesh hangs off a "dequant" wrapper node.
        let wrapper = doc
            .nodes()
            .find(|n| n.name() == Some("dequant") && n.mesh().is_some())
            .expect("dequant wrapper node");
        let (translation, _r, scale) = wrapper.transform().decomposed();
        let prim = wrapper.mesh().unwrap().primitives().next().unwrap();

        // Positions: dequantize + wrapper TRS must reproduce the source
        // within quantization tolerance.
        let positions =
            crate::populate::mesh::read_vec3_dequant(&prim, &gltf::Semantic::Positions, &buffers)
                .expect("quantized positions readable");
        assert_eq!(positions.len(), source.positions.len());
        let tolerance = scale[0] / 32767.0 * 2.0;
        for (got, want) in positions.iter().zip(&source.positions) {
            for k in 0..3 {
                let world = got[k] * scale[k] + translation[k];
                assert!(
                    (world - want[k]).abs() <= tolerance,
                    "position {world} vs {} (tol {tolerance})",
                    want[k]
                );
            }
        }

        // Normals: octahedral round-trip stays within ~1 degree.
        let normals =
            crate::populate::mesh::read_vec3_dequant(&prim, &gltf::Semantic::Normals, &buffers)
                .expect("quantized normals readable");
        let source_normals = source.normals.as_ref().unwrap();
        for (got, want) in normals.iter().zip(source_normals) {
            let len = (got[0] * got[0] + got[1] * got[1] + got[2] * got[2]).sqrt();
            let dot = (got[0] * want[0] + got[1] * want[1] + got[2] * want[2]) / len.max(1e-6);
            assert!(dot > 0.98, "normal deviated (dot {dot})");
        }

        // Indices: same triangles (index codec preserves order for sequential
        // grid strips; compare as sets of rotation-normalized triangles).
        let reader = prim.reader(|b| buffers.get(b.index()).map(|v| &v[..]));
        let indices: Vec<u32> = reader.read_indices().unwrap().into_u32().collect();
        assert_eq!(indices.len(), source.indices.len());
        fn norm(t: &[u32]) -> [u32; 3] {
            let m = (0..3).min_by_key(|&i| t[i]).unwrap();
            [t[m], t[(m + 1) % 3], t[(m + 2) % 3]]
        }
        for (a, b) in indices.chunks_exact(3).zip(source.indices.chunks_exact(3)) {
            assert_eq!(norm(a), norm(b));
        }

        // UVs (u16-normalized): the gltf typed reader handles unorm16.
        let uvs: Vec<[f32; 2]> = reader.read_tex_coords(0).unwrap().into_f32().collect();
        for (got, want) in uvs.iter().zip(&source.uvs[0]) {
            for k in 0..2 {
                assert!((got[k] - want[k]).abs() <= 2.0 / 65535.0);
            }
        }
    }
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
