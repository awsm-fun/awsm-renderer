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

    /// Minimal-cube golden: 8 verts / 12 tris, exact corner positions.
    #[test]
    fn cube_roundtrip() {
        let mut mesh = MeshData::default();
        for z in [-1.0f32, 1.0] {
            for y in [-1.0f32, 1.0] {
                for x in [-1.0f32, 1.0] {
                    mesh.positions.push([x * 0.5, y * 0.5 + 2.0, z * 0.5]);
                }
            }
        }
        mesh.indices = vec![
            0, 1, 2, 2, 1, 3, 4, 6, 5, 5, 6, 7, 0, 4, 1, 1, 4, 5, 2, 3, 6, 6, 3, 7, 0, 2, 4, 4, 2,
            6, 1, 5, 3, 3, 5, 7,
        ];
        mesh.compute_vertex_normals();
        let scene = GlbScene {
            nodes: vec![ExportNode::new("cube").with_mesh(mesh.clone())],
            animations: vec![],
            skins: vec![],
            images: vec![],
            env: None,
        };
        let compressed = compress_glb(&write_glb(&scene)).unwrap();
        let gltf = parse_gltf_lenient(&compressed).unwrap();
        let doc = &gltf.document;
        let blob = gltf.blob.clone().unwrap();
        let mut buffers = materialize(doc, &blob);
        decode_meshopt_buffer_views(doc, &mut buffers).unwrap();

        let wrapper = doc
            .nodes()
            .find(|n| n.name() == Some("dequant") && n.mesh().is_some())
            .expect("dequant wrapper");
        let (t, _r, s) = wrapper.transform().decomposed();
        let prim = wrapper.mesh().unwrap().primitives().next().unwrap();
        let positions =
            crate::populate::mesh::read_vec3_dequant(&prim, &gltf::Semantic::Positions, &buffers)
                .unwrap();
        let tol = s[0] / 32767.0 * 2.0;
        let world: Vec<[f32; 3]> = positions
            .iter()
            .map(|g| [g[0] * s[0] + t[0], g[1] * s[1] + t[1], g[2] * s[2] + t[2]])
            .collect();
        pair_by_position(&world, &mesh.positions, tol);
    }

    /// Skinned round-trip: the dequant transform must fold into the skin's
    /// IBMs (NOT a wrapper node — skinned vertices ignore node TRS), and
    /// `IBM' × v_quant` must reproduce the source within tolerance at bind
    /// pose (identity joints, identity original IBMs ⇒ IBM' IS the dequant).
    #[test]
    fn skinned_roundtrip_folds_dequant_into_ibms() {
        use awsm_renderer_glb_export::ExportSkin;

        let mut mesh = MeshData::default();
        for i in 0..12u32 {
            let f = i as f32;
            mesh.positions
                .push([f * 0.3 - 1.5, (f * 0.7).sin() * 2.0, f * 0.1 + 5.0]);
        }
        mesh.indices = (0..12u32).collect();
        mesh.compute_vertex_normals();
        let n = mesh.positions.len();

        let mut skinned = ExportNode::new("skinned").with_mesh(mesh.clone());
        skinned.skin = Some(0);
        skinned.joints = Some(vec![[0u16, 0, 0, 0]; n]);
        skinned.weights = Some(vec![[1.0f32, 0.0, 0.0, 0.0]; n]);

        let identity: [f32; 16] = glam::Mat4::IDENTITY.to_cols_array();
        let scene = GlbScene {
            // node 0 = the joint (identity transform), node 1 = skinned mesh
            nodes: vec![ExportNode::new("joint0"), skinned],
            animations: vec![],
            skins: vec![ExportSkin {
                // Explicit identity IBMs so the accessor EXISTS — the dequant
                // folds into it. (A skin with NO IBM accessor must skip
                // quantization instead; see the sibling test below.)
                joints: vec![0],
                inverse_bind_matrices: vec![identity],
                skeleton: Some(0),
            }],
            images: vec![],
            env: None,
        };
        let compressed = compress_glb(&write_glb(&scene)).unwrap();
        let gltf = parse_gltf_lenient(&compressed).unwrap();
        let doc = &gltf.document;
        let blob = gltf.blob.clone().unwrap();
        let mut buffers = materialize(doc, &blob);
        decode_meshopt_buffer_views(doc, &mut buffers).unwrap();

        // No wrapper node for skinned meshes.
        assert!(
            !doc.nodes().any(|nd| nd.name() == Some("dequant")),
            "skinned dequant must ride the IBMs, not a wrapper node"
        );

        let skinned_node = doc
            .nodes()
            .find(|nd| nd.mesh().is_some() && nd.skin().is_some())
            .expect("skinned node");
        let prim = skinned_node.mesh().unwrap().primitives().next().unwrap();
        let positions =
            crate::populate::mesh::read_vec3_dequant(&prim, &gltf::Semantic::Positions, &buffers)
                .unwrap();

        // Patched IBM (one joint) from the (uncompressed) IBM accessor.
        let skin = skinned_node.skin().unwrap();
        let ibm_acc = skin.inverse_bind_matrices().expect("IBM accessor");
        let view = ibm_acc.view().unwrap();
        let raw = &buffers[view.buffer().index()]
            [view.offset() + ibm_acc.offset()..view.offset() + ibm_acc.offset() + 64];
        let mut cols = [0f32; 16];
        for (i, c) in raw.chunks_exact(4).enumerate() {
            cols[i] = f32::from_le_bytes(c.try_into().unwrap());
        }
        let ibm = glam::Mat4::from_cols_array(&cols);

        // Bind pose with identity joint world: position = IBM' × v_quant.
        let scale = ibm.x_axis.x; // uniform dequant scale
        let tol = scale / 32767.0 * 2.0;
        let world: Vec<[f32; 3]> = positions
            .iter()
            .map(|q| ibm.transform_point3(glam::Vec3::from_array(*q)).to_array())
            .collect();
        pair_by_position(&world, &mesh.positions, tol);
    }

    /// Skin WEIGHTS quantize to u8-normalized with each vertex renormalized
    /// to sum exactly 255, and JOINTS drop to u8 when they fit (gltfpack
    /// parity, plan F5 — this was 70% of the rig-size gap).
    #[test]
    fn skin_weights_and_joints_quantize() {
        use awsm_renderer_glb_export::ExportSkin;

        let mut mesh = MeshData::default();
        for i in 0..12u32 {
            let f = i as f32;
            mesh.positions.push([f * 0.2 - 1.0, (f * 0.5).sin(), 0.5]);
        }
        mesh.indices = (0..12u32).collect();
        mesh.compute_vertex_normals();
        let n = mesh.positions.len();

        let mut skinned = ExportNode::new("skinned").with_mesh(mesh.clone());
        skinned.skin = Some(0);
        skinned.joints = Some(vec![[0u16, 1, 0, 0]; n]);
        // Deliberately awkward weights: rounding must renormalize to 255.
        skinned.weights = Some(vec![[0.7f32, 0.2, 0.06, 0.04]; n]);

        let identity: [f32; 16] = glam::Mat4::IDENTITY.to_cols_array();
        let scene = GlbScene {
            nodes: vec![ExportNode::new("j0"), ExportNode::new("j1"), skinned],
            animations: vec![],
            skins: vec![ExportSkin {
                joints: vec![0, 1],
                inverse_bind_matrices: vec![identity, identity],
                skeleton: Some(0),
            }],
            images: vec![],
            env: None,
        };
        let compressed = compress_glb(&write_glb(&scene)).unwrap();
        let gltf = parse_gltf_lenient(&compressed).unwrap();
        let doc = &gltf.document;
        let blob = gltf.blob.clone().unwrap();
        let mut buffers = materialize(doc, &blob);
        decode_meshopt_buffer_views(doc, &mut buffers).unwrap();

        let prim = doc.meshes().next().unwrap().primitives().next().unwrap();
        let w_acc = prim.get(&gltf::Semantic::Weights(0)).unwrap();
        assert_eq!(w_acc.data_type(), gltf::accessor::DataType::U8);
        assert!(w_acc.normalized());
        let j_acc = prim.get(&gltf::Semantic::Joints(0)).unwrap();
        assert_eq!(j_acc.data_type(), gltf::accessor::DataType::U8);

        let reader = prim.reader(|b| buffers.get(b.index()).map(|v| &v[..]));
        for w in reader.read_weights(0).unwrap().into_u8() {
            // Renormalized: quantized weights sum to exactly 255 (partition
            // of unity), each within 1 step of the source.
            assert_eq!(w.iter().map(|&v| v as u32).sum::<u32>(), 255, "{w:?}");
            let src = [0.7f32, 0.2, 0.06, 0.04];
            for k in 0..4 {
                assert!((w[k] as f32 / 255.0 - src[k]).abs() <= 1.5 / 255.0);
            }
        }
        for j in reader.read_joints(0).unwrap().into_u16() {
            assert_eq!(j, [0, 1, 0, 0]);
        }
    }

    /// The pre-encode reorder (plan F5) permutes vertex order; JOINTS/WEIGHTS
    /// ride the SAME remap as POSITION, so a vertex's skin data must still
    /// track its position after the permutation. The sibling quantize tests
    /// use uniform skin data (any permutation looks identical) — this one
    /// varies JOINTS per vertex so a stream desync would actually corrupt the
    /// output. Joints and weights go through one identical remap loop, so
    /// pinning joints guards the whole mechanism.
    #[test]
    fn reorder_keeps_skin_streams_synced_with_position() {
        use awsm_renderer_glb_export::ExportSkin;

        // 24 verts / 8 triangles. Distinct positions; triangles emitted in
        // REVERSE so vertex-cache/fetch optimization produces a non-identity
        // remap (an identity remap would make this test vacuous).
        let n = 24usize;
        let mut mesh = MeshData::default();
        for i in 0..n as u32 {
            let f = i as f32;
            mesh.positions
                .push([f * 0.13 - 1.5, (f * 0.6).sin() * 1.5, f * 0.07 + 4.0]);
        }
        mesh.indices = (0..8u32)
            .rev()
            .flat_map(|t| [t * 3, t * 3 + 1, t * 3 + 2])
            .collect();
        mesh.compute_vertex_normals();

        // JOINTS vary per vertex over a 4-joint skin (all identity IBMs, so
        // bind-pose position recovery stays joint-independent: world = dequant
        // × q regardless of which joints a vertex references).
        let joints: Vec<[u16; 4]> = (0..n)
            .map(|i| [(i % 4) as u16, ((i + 1) % 4) as u16, 0, 0])
            .collect();
        let mut skinned = ExportNode::new("skinned").with_mesh(mesh.clone());
        skinned.skin = Some(0);
        skinned.joints = Some(joints.clone());
        skinned.weights = Some(vec![[1.0f32, 0.0, 0.0, 0.0]; n]);

        let identity: [f32; 16] = glam::Mat4::IDENTITY.to_cols_array();
        let scene = GlbScene {
            nodes: vec![
                ExportNode::new("j0"),
                ExportNode::new("j1"),
                ExportNode::new("j2"),
                ExportNode::new("j3"),
                skinned,
            ],
            animations: vec![],
            skins: vec![ExportSkin {
                joints: vec![0, 1, 2, 3],
                inverse_bind_matrices: vec![identity; 4],
                skeleton: Some(0),
            }],
            images: vec![],
            env: None,
        };

        let compressed = compress_glb(&write_glb(&scene)).unwrap();
        let gltf = parse_gltf_lenient(&compressed).unwrap();
        let doc = &gltf.document;
        let blob = gltf.blob.clone().unwrap();
        let mut buffers = materialize(doc, &blob);
        decode_meshopt_buffer_views(doc, &mut buffers).unwrap();

        let skinned_node = doc
            .nodes()
            .find(|nd| nd.mesh().is_some() && nd.skin().is_some())
            .expect("skinned node");
        let prim = skinned_node.mesh().unwrap().primitives().next().unwrap();
        let positions =
            crate::populate::mesh::read_vec3_dequant(&prim, &gltf::Semantic::Positions, &buffers)
                .unwrap();

        // Recover world bind position through the (dequant-folded) IBM — all
        // IBMs identical, so any joint works.
        let skin = skinned_node.skin().unwrap();
        let ibm_acc = skin.inverse_bind_matrices().expect("IBM accessor");
        let view = ibm_acc.view().unwrap();
        let base = view.offset() + ibm_acc.offset();
        let mut cols = [0f32; 16];
        for (i, c) in buffers[view.buffer().index()][base..base + 64]
            .chunks_exact(4)
            .enumerate()
        {
            cols[i] = f32::from_le_bytes(c.try_into().unwrap());
        }
        let ibm = glam::Mat4::from_cols_array(&cols);
        let tol = ibm.x_axis.x / 32767.0 * 2.0;
        let world: Vec<[f32; 3]> = positions
            .iter()
            .map(|q| ibm.transform_point3(glam::Vec3::from_array(*q)).to_array())
            .collect();

        // pairing[source_idx] = decoded_idx.
        let pairing = pair_by_position(&world, &mesh.positions, tol);
        assert!(
            pairing.iter().enumerate().any(|(i, &d)| i != d),
            "reorder should have permuted vertex order; got identity — test is vacuous"
        );

        let reader = prim.reader(|b| buffers.get(b.index()).map(|v| &v[..]));
        let decoded_joints: Vec<[u16; 4]> = reader.read_joints(0).unwrap().into_u16().collect();
        for (src_idx, &dec_idx) in pairing.iter().enumerate() {
            assert_eq!(
                decoded_joints[dec_idx], joints[src_idx],
                "vertex at source position {src_idx} (decoded {dec_idx}) lost its joints in the reorder"
            );
        }
    }

    /// A skin with NO inverseBindMatrices accessor has nowhere to fold the
    /// dequant — its meshes must skip quantization (positions stay F32),
    /// never quantize-and-corrupt.
    #[test]
    fn ibm_less_skin_skips_quantization() {
        let mut mesh = MeshData::default();
        for i in 0..6u32 {
            mesh.positions.push([i as f32, 1.0, -2.0]);
        }
        mesh.indices = vec![0, 1, 2, 3, 4, 5];
        mesh.compute_vertex_normals();
        let n = mesh.positions.len();
        let mut skinned = ExportNode::new("skinned").with_mesh(mesh);
        skinned.skin = Some(0);
        skinned.joints = Some(vec![[0u16, 0, 0, 0]; n]);
        skinned.weights = Some(vec![[1.0f32, 0.0, 0.0, 0.0]; n]);
        let scene = GlbScene {
            nodes: vec![ExportNode::new("joint0"), skinned],
            animations: vec![],
            skins: vec![awsm_renderer_glb_export::ExportSkin {
                joints: vec![0],
                inverse_bind_matrices: Vec::new(), // NO accessor
                skeleton: Some(0),
            }],
            images: vec![],
            env: None,
        };
        let compressed = compress_glb(&write_glb(&scene)).unwrap();
        let gltf = parse_gltf_lenient(&compressed).unwrap();
        let prim_acc = gltf
            .document
            .meshes()
            .next()
            .unwrap()
            .primitives()
            .next()
            .unwrap()
            .get(&gltf::Semantic::Positions)
            .unwrap();
        assert_eq!(
            prim_acc.data_type(),
            gltf::accessor::DataType::F32,
            "IBM-less skinned mesh must keep F32 positions"
        );
    }

    /// Bundle-rig shape: `strip_materials_and_images` + `compress_glb` must
    /// drop the embedded image BYTES (not just the JSON refs), remove
    /// materials/textures, keep the geometry decodable, and shrink the file.
    #[test]
    fn strip_and_compress_drops_embedded_image_bytes() {
        use awsm_renderer_glb_export::{
            strip_materials_and_images, ExportImage, ExportMaterial, ImageMime, PbrMaterial, TexRef,
        };

        let (mut scene, source) = grid_scene();
        // A fat fake "texture" so the byte-drop is unmistakable.
        scene.images.push(ExportImage {
            name: "fake".into(),
            bytes: vec![0xAB; 512 * 1024],
            mime: ImageMime::Png,
        });
        scene.nodes[0].material = Some(ExportMaterial::Pbr(PbrMaterial {
            base_color_texture: Some(TexRef {
                image: 0,
                tex_coord: 0,
                transform: None,
            }),
            ..Default::default()
        }));

        let plain = write_glb(&scene);
        assert!(plain.len() > 512 * 1024, "image embedded in the plain glb");
        let bundle = compress_glb(&strip_materials_and_images(&plain).unwrap()).unwrap();
        assert!(
            bundle.len() < 100 * 1024,
            "image bytes must be gone ({} bytes left)",
            bundle.len()
        );

        let gltf = parse_gltf_lenient(&bundle).unwrap();
        let doc = &gltf.document;
        assert_eq!(doc.images().count(), 0);
        assert_eq!(doc.textures().count(), 0);
        assert_eq!(doc.materials().count(), 0);
        assert!(doc
            .meshes()
            .flat_map(|m| m.primitives())
            .all(|p| p.material().index().is_none()));

        // Geometry still round-trips through the decode path.
        let blob = gltf.blob.clone().unwrap();
        let mut buffers = materialize(doc, &blob);
        assert!(decode_meshopt_buffer_views(doc, &mut buffers).unwrap() > 0);
        let wrapper = doc
            .nodes()
            .find(|n| n.name() == Some("dequant") && n.mesh().is_some())
            .unwrap();
        let prim = wrapper.mesh().unwrap().primitives().next().unwrap();
        let positions =
            crate::populate::mesh::read_vec3_dequant(&prim, &gltf::Semantic::Positions, &buffers)
                .unwrap();
        assert_eq!(positions.len(), source.positions.len());
    }

    /// Rig glbs re-exported with embedded KTX2 textures declare
    /// `KHR_texture_basisu` in `extensionsRequired`. strip+compress must
    /// tolerate that: the F4 on-device run caught the strict parse REJECTING
    /// such rigs, silently shipping the uncompressed ~29MB original.
    #[test]
    fn strip_and_compress_tolerate_basisu_required() {
        use awsm_renderer_glb_export::{
            strip_materials_and_images, ExportImage, ExportMaterial, ImageMime, PbrMaterial, TexRef,
        };

        let (mut scene, _) = grid_scene();
        scene.images.push(ExportImage {
            name: "fake-ktx".into(),
            bytes: vec![0xCD; 256 * 1024],
            mime: ImageMime::Png,
        });
        scene.nodes[0].material = Some(ExportMaterial::Pbr(PbrMaterial {
            base_color_texture: Some(TexRef {
                image: 0,
                tex_coord: 0,
                transform: None,
            }),
            ..Default::default()
        }));
        let plain = write_glb(&scene);

        // Inject the extension declaration into the raw JSON chunk, then
        // rebuild the GLB container around the edited JSON + original BIN.
        let json_len = u32::from_le_bytes(plain[12..16].try_into().unwrap()) as usize;
        let mut root: gltf::json::Value =
            gltf::json::deserialize::from_slice(&plain[20..20 + json_len]).unwrap();
        root["extensionsUsed"] = gltf::json::Value::from(vec!["KHR_texture_basisu"]);
        root["extensionsRequired"] = gltf::json::Value::from(vec!["KHR_texture_basisu"]);
        let mut json = gltf::json::serialize::to_vec(&root).unwrap();
        while json.len() % 4 != 0 {
            json.push(b' ');
        }
        let bin_start = 20 + json_len;
        let bin_chunk = &plain[bin_start..]; // 8-byte chunk header + data
        let total = 12 + 8 + json.len() + bin_chunk.len();
        let mut glb = Vec::with_capacity(total);
        glb.extend_from_slice(&plain[0..8]); // magic + version
        glb.extend_from_slice(&(total as u32).to_le_bytes());
        glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
        glb.extend_from_slice(&plain[16..20]); // JSON chunk type
        glb.extend_from_slice(&json);
        glb.extend_from_slice(bin_chunk);

        // Sanity: the strict parse rejects this container (the original bug's
        // trigger) — the pipeline must succeed anyway.
        assert!(gltf::Gltf::from_slice(&glb).is_err());
        let bundle = compress_glb(&strip_materials_and_images(&glb).unwrap()).unwrap();
        assert!(
            bundle.len() < 100 * 1024,
            "strip+compress must shrink the basisu-required glb ({} bytes left)",
            bundle.len()
        );
        let required = required_extensions(&bundle);
        assert!(
            !required.contains(&"KHR_texture_basisu".to_string()),
            "stripped output must not require the dead texture extension"
        );
    }

    fn materialize(doc: &gltf::Document, blob: &[u8]) -> Vec<Vec<u8>> {
        doc.buffers()
            .map(|b| {
                if buffer_is_meshopt_fallback(&b) {
                    vec![0u8; b.length()]
                } else {
                    let mut bin = blob.to_vec();
                    while bin.len() % 4 != 0 {
                        bin.push(0);
                    }
                    bin
                }
            })
            .collect()
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
        let world: Vec<[f32; 3]> = positions
            .iter()
            .map(|g| {
                [
                    g[0] * scale[0] + translation[0],
                    g[1] * scale[1] + translation[1],
                    g[2] * scale[2] + translation[2],
                ]
            })
            .collect();
        let pairing = pair_by_position(&world, &source.positions, tolerance);

        // Normals: octahedral round-trip stays within ~1 degree (paired by
        // position — the reorder pass permutes vertex order).
        let normals =
            crate::populate::mesh::read_vec3_dequant(&prim, &gltf::Semantic::Normals, &buffers)
                .expect("quantized normals readable");
        let source_normals = source.normals.as_ref().unwrap();
        for (i, want) in source_normals.iter().enumerate() {
            let got = normals[pairing[i]];
            let len = (got[0] * got[0] + got[1] * got[1] + got[2] * got[2]).sqrt();
            let dot = (got[0] * want[0] + got[1] * want[1] + got[2] * want[2]) / len.max(1e-6);
            assert!(dot > 0.98, "normal deviated (dot {dot})");
        }

        // Indices: same triangle multiset in SOURCE vertex ids (winding
        // preserved; triangle + vertex order both permuted by the reorder).
        let reader = prim.reader(|b| buffers.get(b.index()).map(|v| &v[..]));
        let indices: Vec<u32> = reader.read_indices().unwrap().into_u32().collect();
        assert_eq!(indices.len(), source.indices.len());
        let remapped = remap_to_source_ids(&indices, &pairing, world.len());
        assert_eq!(
            triangle_multiset(&remapped),
            triangle_multiset(&source.indices)
        );

        // UVs (u16-normalized): the gltf typed reader handles unorm16.
        let uvs: Vec<[f32; 2]> = reader.read_tex_coords(0).unwrap().into_f32().collect();
        for (i, want) in source.uvs[0].iter().enumerate() {
            let got = uvs[pairing[i]];
            for k in 0..2 {
                assert!((got[k] - want[k]).abs() <= 2.0 / 65535.0);
            }
        }
    }

    /// The pre-encode reorder pass (gltfpack parity, plan F5) permutes vertex
    /// AND triangle order, so fidelity is checked by VALUE: pair each source
    /// vertex with the decoded vertex whose (world-space) position matches
    /// within `tol` per component. Panics when a source vertex has no match.
    fn pair_by_position(decoded: &[[f32; 3]], source: &[[f32; 3]], tol: f32) -> Vec<usize> {
        source
            .iter()
            .map(|want| {
                decoded
                    .iter()
                    .position(|got| (0..3).all(|k| (got[k] - want[k]).abs() <= tol))
                    .unwrap_or_else(|| panic!("no decoded vertex within {tol} of {want:?}"))
            })
            .collect()
    }

    /// Rotation-normalized triangle multiset over vertex ids — winding
    /// preserved, triangle order ignored.
    fn triangle_multiset(indices: &[u32]) -> std::collections::BTreeMap<[u32; 3], usize> {
        let mut out = std::collections::BTreeMap::new();
        for t in indices.chunks_exact(3) {
            let m = (0..3).min_by_key(|&i| t[i]).unwrap();
            *out.entry([t[m], t[(m + 1) % 3], t[(m + 2) % 3]])
                .or_default() += 1;
        }
        out
    }

    /// Decoded indices remapped into SOURCE vertex ids via the position
    /// pairing, for triangle-set comparison against the source indices.
    fn remap_to_source_ids(indices: &[u32], pairing: &[usize], decoded_len: usize) -> Vec<u32> {
        let mut inverse = vec![u32::MAX; decoded_len];
        for (source_id, &decoded_id) in pairing.iter().enumerate() {
            inverse[decoded_id] = source_id as u32;
        }
        indices.iter().map(|&i| inverse[i as usize]).collect()
    }

    fn required_extensions(glb: &[u8]) -> Vec<String> {
        let json_len = u32::from_le_bytes(glb[12..16].try_into().unwrap()) as usize;
        let raw: gltf::json::Value =
            gltf::json::deserialize::from_slice(&glb[20..20 + json_len]).unwrap();
        raw["extensionsRequired"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Quantization WITHOUT meshopt: `KHR_mesh_quantization` alone, quantized
    /// accessors in plain bufferViews (no decode pass needed), normals as
    /// direct i16-normalized (octahedral is meshopt-filter-only).
    #[test]
    fn quantize_only_emits_plain_views() {
        use awsm_renderer_glb_export::{compress_glb_with, CompressOptions, Quantization};

        let (scene, source) = grid_scene();
        let compressed = compress_glb_with(
            &write_glb(&scene),
            &CompressOptions {
                meshopt: false,
                quantization: Quantization::Always,
            },
        )
        .unwrap();

        let required = required_extensions(&compressed);
        assert!(required.contains(&"KHR_mesh_quantization".to_string()));
        assert!(!required.contains(&"EXT_meshopt_compression".to_string()));

        let gltf = parse_gltf_lenient(&compressed).unwrap();
        let doc = &gltf.document;
        let blob = gltf.blob.clone().unwrap();
        let mut buffers = materialize(doc, &blob);
        assert_eq!(
            decode_meshopt_buffer_views(doc, &mut buffers).unwrap(),
            0,
            "plain-view output must not carry meshopt views"
        );

        let wrapper = doc
            .nodes()
            .find(|n| n.name() == Some("dequant") && n.mesh().is_some())
            .expect("dequant wrapper");
        let (t, _r, s) = wrapper.transform().decomposed();
        let prim = wrapper.mesh().unwrap().primitives().next().unwrap();

        let pos_acc = prim.get(&gltf::Semantic::Positions).unwrap();
        assert_eq!(pos_acc.data_type(), gltf::accessor::DataType::I16);
        let positions =
            crate::populate::mesh::read_vec3_dequant(&prim, &gltf::Semantic::Positions, &buffers)
                .unwrap();
        let tol = s[0] / 32767.0 * 2.0;
        let world: Vec<[f32; 3]> = positions
            .iter()
            .map(|g| [g[0] * s[0] + t[0], g[1] * s[1] + t[1], g[2] * s[2] + t[2]])
            .collect();
        let pairing = pair_by_position(&world, &source.positions, tol);

        // Normals: direct i16-normalized, no filter — near-exact.
        let norm_acc = prim.get(&gltf::Semantic::Normals).unwrap();
        assert_eq!(norm_acc.data_type(), gltf::accessor::DataType::I16);
        let normals =
            crate::populate::mesh::read_vec3_dequant(&prim, &gltf::Semantic::Normals, &buffers)
                .unwrap();
        for (i, want) in source.normals.as_ref().unwrap().iter().enumerate() {
            let got = normals[pairing[i]];
            let dot = got[0] * want[0] + got[1] * want[1] + got[2] * want[2];
            assert!(dot > 0.9999, "snorm16 normal deviated (dot {dot})");
        }

        // Indices stay raw bytes (index codec is meshopt-only) but the
        // reorder pass still permutes them — compare triangle multisets.
        let reader = prim.reader(|b| buffers.get(b.index()).map(|v| &v[..]));
        let indices: Vec<u32> = reader.read_indices().unwrap().into_u32().collect();
        let remapped = remap_to_source_ids(&indices, &pairing, world.len());
        assert_eq!(
            triangle_multiset(&remapped),
            triangle_multiset(&source.indices)
        );
    }

    /// meshopt WITHOUT quantization: streams encode losslessly, accessors stay
    /// F32, and `KHR_mesh_quantization` is NOT declared.
    #[test]
    fn meshopt_only_keeps_f32_accessors() {
        use awsm_renderer_glb_export::{compress_glb_with, CompressOptions, Quantization};

        let (scene, source) = grid_scene();
        let compressed = compress_glb_with(
            &write_glb(&scene),
            &CompressOptions {
                meshopt: true,
                quantization: Quantization::Off,
            },
        )
        .unwrap();

        let required = required_extensions(&compressed);
        assert!(required.contains(&"EXT_meshopt_compression".to_string()));
        assert!(!required.contains(&"KHR_mesh_quantization".to_string()));

        let gltf = parse_gltf_lenient(&compressed).unwrap();
        let doc = &gltf.document;
        let blob = gltf.blob.clone().unwrap();
        let mut buffers = materialize(doc, &blob);
        assert!(decode_meshopt_buffer_views(doc, &mut buffers).unwrap() > 0);

        assert!(
            !doc.nodes().any(|n| n.name() == Some("dequant")),
            "no quantization ⇒ no dequant wrapper"
        );
        let prim = doc.meshes().next().unwrap().primitives().next().unwrap();
        let pos_acc = prim.get(&gltf::Semantic::Positions).unwrap();
        assert_eq!(pos_acc.data_type(), gltf::accessor::DataType::F32);

        // meshopt alone is lossless: the exact f32 VALUES survive (the
        // reorder pass permutes order — compare as sorted multisets).
        let reader = prim.reader(|b| buffers.get(b.index()).map(|v| &v[..]));
        let positions: Vec<[f32; 3]> = reader.read_positions().unwrap().collect();
        let key = |p: &[f32; 3]| (p[0].to_bits(), p[1].to_bits(), p[2].to_bits());
        let mut got = positions.clone();
        let mut want = source.positions.clone();
        got.sort_by_key(key);
        want.sort_by_key(key);
        assert_eq!(got, want);
    }

    /// Smart mode: a mesh whose grid step exceeds the threshold keeps F32
    /// positions (still meshopt-encoded); a small mesh under the same options
    /// quantizes.
    #[test]
    fn smart_threshold_demotes_large_extents() {
        use awsm_renderer_glb_export::{compress_glb_with, CompressOptions, Quantization};

        // grid_scene spans x ∈ [-2, 2] ⇒ half-extent 2m ⇒ step ~0.061mm.
        let (small_scene, _) = grid_scene();
        // Scale ×4 ⇒ half-extent 8m ⇒ step ~0.24mm > 0.1mm threshold.
        let (mut big_scene, _) = grid_scene();
        for node in &mut big_scene.nodes {
            if let Some(mesh) = &mut node.mesh {
                for p in &mut mesh.positions {
                    for v in p.iter_mut() {
                        *v *= 4.0;
                    }
                }
            }
        }

        let options = CompressOptions {
            meshopt: true,
            quantization: Quantization::Smart { threshold_mm: 0.1 },
        };
        let position_type = |glb: &[u8]| {
            let gltf = parse_gltf_lenient(glb).unwrap();
            let prim = gltf
                .document
                .meshes()
                .next()
                .unwrap()
                .primitives()
                .next()
                .unwrap();
            prim.get(&gltf::Semantic::Positions).unwrap().data_type()
        };

        let small = compress_glb_with(&write_glb(&small_scene), &options).unwrap();
        assert_eq!(position_type(&small), gltf::accessor::DataType::I16);

        let big = compress_glb_with(&write_glb(&big_scene), &options).unwrap();
        assert_eq!(
            position_type(&big),
            gltf::accessor::DataType::F32,
            "8m half-extent must demote under a 0.1mm Smart threshold"
        );
        assert!(
            required_extensions(&big).contains(&"EXT_meshopt_compression".to_string()),
            "demoted mesh still meshopt-encodes"
        );
        assert!(
            !required_extensions(&big).contains(&"KHR_mesh_quantization".to_string()),
            "nothing quantized ⇒ KHR_mesh_quantization must not be required"
        );
    }

    /// Both knobs off: byte-identical passthrough.
    #[test]
    fn both_off_is_passthrough() {
        use awsm_renderer_glb_export::{compress_glb_with, CompressOptions, Quantization};

        let (scene, _) = grid_scene();
        let plain = write_glb(&scene);
        let out = compress_glb_with(
            &plain,
            &CompressOptions {
                meshopt: false,
                quantization: Quantization::Off,
            },
        )
        .unwrap();
        assert_eq!(plain, out);
    }
}

/// Astrabot — the second paid robot — through the same decode plumbing, so
/// both acceptance fixtures are locked when present locally.
#[cfg(all(test, has_local_fixtures_astrabot))]
mod astrabot_fixture_tests {
    use super::*;
    use crate::loader::parse_gltf_lenient;

    const ASTRABOT_GLB: &[u8] = include_bytes!("../../../../fixtures/local/astrabot-meshopt.glb");

    #[test]
    fn astrabot_decode_pass_and_accessor_sanity() {
        let gltf = parse_gltf_lenient(ASTRABOT_GLB).unwrap();
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
        let decoded = decode_meshopt_buffer_views(doc, &mut buffers).unwrap();
        assert!(decoded > 0, "astrabot must carry meshopt bufferViews");
        for mesh in doc.meshes() {
            for prim in mesh.primitives() {
                let reader = prim.reader(|b| buffers.get(b.index()).map(|v| &v[..]));
                let vcount = prim.get(&gltf::Semantic::Positions).unwrap().count();
                if let Some(indices) = reader.read_indices() {
                    let max = indices.into_u32().max().unwrap_or(0) as usize;
                    assert!(max < vcount, "index {max} out of range ({vcount} verts)");
                }
            }
        }
    }
}

/// gltfpack parity (plan F5): the RAW Blender export, pushed through OUR
/// pipeline (clean-rig re-export → strip → compress), must land close to the
/// gltfpack artifact's geometry size. Gated on both local fixtures.
#[cfg(all(test, has_local_fixtures_astrabot_large))]
mod parity_fixture_tests {
    use crate::loader::parse_gltf_lenient;

    const LARGE: &[u8] = include_bytes!("../../../../fixtures/local/astrabot-large.glb");
    const PACKED: &[u8] = include_bytes!("../../../../fixtures/local/astrabot-meshopt.glb");

    /// Total bytes of bufferViews referenced by images (the embedded texture
    /// payload) — subtracted to compare GEOMETRY against geometry.
    fn image_bytes(glb: &[u8]) -> usize {
        let json_len = u32::from_le_bytes(glb[12..16].try_into().unwrap()) as usize;
        let raw: gltf::json::Value =
            gltf::json::deserialize::from_slice(&glb[20..20 + json_len]).unwrap();
        let views = raw["bufferViews"].as_array().cloned().unwrap_or_default();
        raw["images"]
            .as_array()
            .map(|imgs| {
                imgs.iter()
                    .filter_map(|img| img["bufferView"].as_u64())
                    .filter_map(|v| views.get(v as usize))
                    .filter_map(|v| v["byteLength"].as_u64())
                    .sum::<u64>() as usize
            })
            .unwrap_or(0)
    }

    #[test]
    fn raw_export_compresses_close_to_gltfpack() {
        use awsm_renderer_glb_export::{
            compress_glb, reexport_clean_scene, strip_materials_and_images, write_glb,
        };

        // Parse WITHOUT decoding the (huge) embedded textures.
        let gltf = parse_gltf_lenient(LARGE).expect("large fixture parses");
        let blob = gltf.blob.clone().expect("GLB BIN");
        let buffers = vec![blob];
        let scene = reexport_clean_scene(&gltf.document, &buffers).expect("clean-rig re-export");
        let ours = compress_glb(&strip_materials_and_images(&write_glb(&scene)).unwrap())
            .expect("strip+compress");

        let gltfpack_geometry = PACKED.len() - image_bytes(PACKED);
        let ratio = ours.len() as f64 / gltfpack_geometry as f64;
        println!(
            "parity: ours {} bytes vs gltfpack geometry {} bytes (ratio {ratio:.3})",
            ours.len(),
            gltfpack_geometry
        );
        assert!(
            ratio <= 1.25,
            "our pipeline must land within 25% of gltfpack's geometry \
             ({} vs {gltfpack_geometry}, ratio {ratio:.3})",
            ours.len()
        );
        assert!(
            ratio >= 0.4,
            "suspiciously small output ({} bytes) — did the re-export drop meshes?",
            ours.len()
        );
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
