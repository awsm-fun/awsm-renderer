// Scanline material — first-party port of the dynamic-material
// `scanline` worked example.
//
// Reads:
//   - tint (vec3): base overlay color
//   - scan_freq (f32): vertical scanline frequency in screen pixels
//   - scan_speed (f32): scanline scroll rate in cycles/sec
//   - scan_strength (f32): overlay intensity 0..1
//   - base_tex (TextureInfo): sampled with quad UV
//
// CPU layout — see `ScanlineMaterial::write_uniform_buffer`. Field
// offsets are u32-words counted from the byte_offset arg (which points
// at the material's shader_id word):
//
//   word 0       shader_id
//   word 1..3    leading pad (vec3<f32> 16-byte align)
//   word 4..6    tint (no trailing pad — next field is f32, 4-byte aligned)
//   word 7       scan_freq
//   word 8       scan_speed
//   word 9       scan_strength
//   word 10..14  base_tex_info (TextureInfoRaw, 5 u32s)
//
// This file is included verbatim into both the opaque and transparent
// material templates via `build_materials_wgsl()`. It can NOT use
// askama template syntax — only the parent shader's template is
// rendered. Per-pass texture sampling (mip-mode branching) happens at
// the call site.

struct ScanlineMaterialRaw {
    _pad: vec3<u32>,
    tint_r: f32,
    tint_g: f32,
    tint_b: f32,
    scan_freq: f32,
    scan_speed: f32,
    scan_strength: f32,
    base_tex_info: TextureInfoRaw,
};

struct ScanlineMaterial {
    tint: vec3<f32>,
    scan_freq: f32,
    scan_speed: f32,
    scan_strength: f32,
    base_tex_info: TextureInfo,
};

fn scanline_get_material(byte_offset: u32) -> ScanlineMaterial {
    let base_index = (byte_offset / 4u) + 1u; // skip shader_id

    // Skip the leading 3-word pad (vec3 alignment ahead of the tint
    // vec3<f32> field).
    let tint_base = base_index + 3u;
    let tint_r = material_load_f32(tint_base + 0u);
    let tint_g = material_load_f32(tint_base + 1u);
    let tint_b = material_load_f32(tint_base + 2u);
    // NOTE: no trailing vec3 pad — next field scan_freq is f32 (4-byte
    // align) so we immediately read it.
    let scan_freq = material_load_f32(tint_base + 3u);
    let scan_speed = material_load_f32(tint_base + 4u);
    let scan_strength = material_load_f32(tint_base + 5u);
    let base_tex_raw = material_load_texture_info_raw(tint_base + 6u);

    return ScanlineMaterial(
        vec3<f32>(tint_r, tint_g, tint_b),
        scan_freq,
        scan_speed,
        scan_strength,
        convert_texture_info(base_tex_raw),
    );
}

// Convenience helper used by the opaque + transparent dispatch arms.
// Produces the moving-scanline overlay color given a screen-space UV
// (in [0,1]) and the current frame_globals.time.
fn scanline_compute_overlay(material: ScanlineMaterial, uv: vec2<f32>, time: f32) -> vec3<f32> {
    let scan = sin(uv.y * material.scan_freq + time * material.scan_speed);
    return mix(vec3<f32>(0.0), material.tint, scan * material.scan_strength);
}
