//! Encode→decode roundtrips, native AND wasm32. The encode paths exercise
//! meshoptimizer's internal allocator (decode writes into caller memory and
//! never allocates), so this is the test that proves the C library's
//! allocation story works on wasm32-unknown-unknown — a lazy archive link can
//! hide an unresolved `operator new` until an encode object gets pulled in.

use awsm_renderer_codec_meshopt::{decode_buffer_view, Filter, Mode};

#[cfg(target_arch = "wasm32")]
use wasm_bindgen_test::wasm_bindgen_test;

#[derive(Clone, Copy, Default, PartialEq, Debug)]
#[repr(C)]
struct Vertex([f32; 3]);

fn grid_vertices() -> Vec<Vertex> {
    // 17×17 grid — enough data that the encoder actually compresses.
    let mut v = Vec::new();
    for y in 0..17 {
        for x in 0..17 {
            v.push(Vertex([
                x as f32 / 16.0,
                y as f32 / 16.0,
                ((x * y) % 5) as f32 * 0.1,
            ]));
        }
    }
    v
}

#[cfg_attr(not(target_arch = "wasm32"), test)]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
fn vertex_buffer_roundtrip() {
    let verts = grid_vertices();
    let encoded = awsm_renderer_codec_meshopt::meshopt::encode_vertex_buffer(&verts).unwrap();
    assert!(
        encoded.len() < verts.len() * 12,
        "encoder should compress a regular grid"
    );
    let decoded =
        decode_buffer_view(&encoded, verts.len(), 12, Mode::Attributes, Filter::None).unwrap();
    let out: &[Vertex] =
        unsafe { std::slice::from_raw_parts(decoded.as_ptr().cast(), verts.len()) };
    assert_eq!(out, verts.as_slice());
}

#[cfg_attr(not(target_arch = "wasm32"), test)]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
fn index_buffer_roundtrip() {
    // Triangulate the 17×17 grid.
    let mut indices: Vec<u32> = Vec::new();
    for y in 0..16u32 {
        for x in 0..16u32 {
            let a = y * 17 + x;
            indices.extend_from_slice(&[a, a + 1, a + 17, a + 17, a + 1, a + 18]);
        }
    }
    let encoded =
        awsm_renderer_codec_meshopt::meshopt::encode_index_buffer(&indices, 17 * 17).unwrap();
    let decoded =
        decode_buffer_view(&encoded, indices.len(), 4, Mode::Triangles, Filter::None).unwrap();
    let out: &[u32] = unsafe { std::slice::from_raw_parts(decoded.as_ptr().cast(), indices.len()) };
    // The index codec is lossless up to per-triangle ROTATION (a,b,c → b,c,a;
    // winding preserved) — compare rotation-normalized triangles.
    fn normalize(t: &[u32]) -> [u32; 3] {
        let m = (0..3).min_by_key(|&i| t[i]).unwrap();
        [t[m], t[(m + 1) % 3], t[(m + 2) % 3]]
    }
    for (i, (a, b)) in out.chunks_exact(3).zip(indices.chunks_exact(3)).enumerate() {
        assert_eq!(normalize(a), normalize(b), "triangle {i}");
    }
}

#[cfg_attr(not(target_arch = "wasm32"), test)]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
fn octahedral_filter_roundtrip() {
    // Unit normals as f32x4 (encodeFilterOct input layout), 8-bit snorm x/y,
    // stride 4 output — the gltfpack normal encoding our fixtures use.
    let normals: Vec<[f32; 4]> = (0..64)
        .map(|i| {
            let t = i as f32 * 0.1;
            let v = [
                t.sin() * 0.6,
                t.cos() * 0.6,
                (1.0 - 0.72 * t.sin() * t.sin() * 0.0).max(0.1),
                0.0,
            ];
            let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            [v[0] / len, v[1] / len, v[2] / len, 0.0]
        })
        .collect();

    let mut packed = vec![0u8; normals.len() * 4];
    unsafe {
        awsm_renderer_codec_meshopt::meshopt::ffi::meshopt_encodeFilterOct(
            packed.as_mut_ptr().cast(),
            normals.len(),
            4,
            8,
            normals.as_ptr().cast(),
        );
    }
    #[derive(Clone, Copy, Default)]
    #[repr(C)]
    struct P([u8; 4]);
    let packed_t: &[P] =
        unsafe { std::slice::from_raw_parts(packed.as_ptr().cast(), normals.len()) };
    let encoded = awsm_renderer_codec_meshopt::meshopt::encode_vertex_buffer(packed_t).unwrap();

    let decoded = decode_buffer_view(
        &encoded,
        normals.len(),
        4,
        Mode::Attributes,
        Filter::Octahedral,
    )
    .unwrap();

    for (i, (chunk, orig)) in decoded.chunks_exact(4).zip(&normals).enumerate() {
        let v = [
            chunk[0] as i8 as f32 / 127.0,
            chunk[1] as i8 as f32 / 127.0,
            chunk[2] as i8 as f32 / 127.0,
        ];
        let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        assert!((0.99..=1.01).contains(&len), "normal {i} not unit ({len})");
        let dot = v[0] * orig[0] + v[1] * orig[1] + v[2] * orig[2];
        assert!(dot > 0.98, "normal {i} deviates from source (dot {dot})");
    }
}
