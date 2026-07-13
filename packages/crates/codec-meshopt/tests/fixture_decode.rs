//! Phase-1 gate: decode every `EXT_meshopt_compression` bufferView of the
//! real gltfpack-exported robot, natively AND on wasm32-unknown-unknown (the
//! same test runs under `wasm-bindgen-test-runner`, proving the cross-compiled
//! C library links and executes correctly in wasm).
//!
//! Only compiled when the gitignored paid fixture exists locally
//! (`has_local_fixtures` cfg from build.rs).
#![cfg(has_local_fixtures)]

use awsm_renderer_codec_meshopt::{decode_buffer_view, Filter, Mode};

#[cfg(target_arch = "wasm32")]
use wasm_bindgen_test::wasm_bindgen_test;

const POLICE_GLB: &[u8] = include_bytes!("../../../../fixtures/local/police-meshopt.glb");

/// Minimal GLB container parse: (JSON chunk, BIN chunk).
fn parse_glb(bytes: &[u8]) -> (serde_json::Value, &[u8]) {
    let u32_at = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap()) as usize;
    assert_eq!(&bytes[0..4], b"glTF", "GLB magic");
    assert_eq!(u32_at(4), 2, "GLB version");
    let mut offset = 12;
    let mut json = None;
    let mut bin: &[u8] = &[];
    while offset + 8 <= bytes.len() {
        let len = u32_at(offset);
        let kind = &bytes[offset + 4..offset + 8];
        let body = &bytes[offset + 8..offset + 8 + len];
        match kind {
            b"JSON" => json = Some(serde_json::from_slice(body).unwrap()),
            b"BIN\0" => bin = body,
            _ => {}
        }
        offset += 8 + len + (4 - len % 4) % 4;
    }
    (json.expect("JSON chunk"), bin)
}

#[cfg_attr(not(target_arch = "wasm32"), test)]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
fn decodes_all_meshopt_buffer_views_of_police_robot() {
    let (json, bin) = parse_glb(POLICE_GLB);

    let required = json["extensionsRequired"]
        .as_array()
        .expect("extensionsRequired");
    assert!(
        required.iter().any(|e| e == "EXT_meshopt_compression"),
        "fixture must actually use EXT_meshopt_compression"
    );

    // The ext's own `buffer` must be the BIN buffer; parents point at the
    // fallback buffer, which we must never read. Identify the fallback.
    let buffers = json["buffers"].as_array().unwrap();
    let fallback_indices: Vec<usize> = buffers
        .iter()
        .enumerate()
        .filter(|(_, b)| {
            b["extensions"]["EXT_meshopt_compression"]["fallback"]
                .as_bool()
                .unwrap_or(false)
        })
        .map(|(i, _)| i)
        .collect();

    let mut decoded_views = 0usize;
    let mut compressed_total = 0usize;
    let mut decoded_total = 0usize;
    let mut modes_seen = Vec::new();
    let mut filters_seen = Vec::new();

    for (i, view) in json["bufferViews"].as_array().unwrap().iter().enumerate() {
        let ext = &view["extensions"]["EXT_meshopt_compression"];
        if ext.is_null() {
            continue;
        }
        let buffer = ext["buffer"].as_u64().unwrap() as usize;
        assert!(
            !fallback_indices.contains(&buffer),
            "bufferView {i}: ext must reference real bytes, not the fallback buffer"
        );
        let byte_offset = ext["byteOffset"].as_u64().unwrap_or(0) as usize;
        let byte_length = ext["byteLength"].as_u64().unwrap() as usize;
        let count = ext["count"].as_u64().unwrap() as usize;
        let stride = ext["byteStride"].as_u64().unwrap() as usize;
        let mode = Mode::from_gltf(ext["mode"].as_str().unwrap()).unwrap();
        let filter = ext["filter"]
            .as_str()
            .map(|f| Filter::from_gltf(f).unwrap())
            .unwrap_or(Filter::None);

        let data = &bin[byte_offset..byte_offset + byte_length];
        let out = decode_buffer_view(data, count, stride, mode, filter)
            .unwrap_or_else(|e| panic!("bufferView {i} ({mode:?}/{filter:?}) failed: {e}"));
        assert_eq!(out.len(), count * stride, "bufferView {i} size");

        // Octahedral-filtered views hold unit normals: spot-check magnitude.
        if filter == Filter::Octahedral && stride == 4 {
            let n = &out[0..4];
            let v = [
                n[0] as i8 as f32 / 127.0,
                n[1] as i8 as f32 / 127.0,
                n[2] as i8 as f32 / 127.0,
            ];
            let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            assert!(
                (0.9..=1.1).contains(&len),
                "bufferView {i}: octahedral output not unit-length ({len})"
            );
        }

        decoded_views += 1;
        compressed_total += byte_length;
        decoded_total += out.len();
        modes_seen.push(format!("{mode:?}"));
        filters_seen.push(format!("{filter:?}"));
    }

    assert!(decoded_views > 0, "no meshopt bufferViews found");
    // Surfaces in native test output and in the wasm runner's console.
    println!(
        "police-meshopt.glb: {decoded_views} meshopt bufferViews decoded, {compressed_total} compressed bytes -> {decoded_total} logical bytes (modes: {modes_seen:?}, filters: {filters_seen:?})"
    );
}
