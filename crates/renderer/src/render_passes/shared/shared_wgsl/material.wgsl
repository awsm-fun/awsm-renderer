// Material storage layout + per-material WGSL fragments.
//
// `materials_wgsl` and `shader_id_consts` are populated by the renderer at
// shader-template construction time from the `awsm-materials` registry —
// each enabled material (gated by a Cargo feature in `awsm-materials`)
// contributes its `wgsl_fragment()` and `shader_id`. Adding a new material
// is one new module + one feature entry + one `MaterialEntry` push in the
// registry; no edits to this file or to `awsm-renderer` are needed.

{{ materials_wgsl|safe }}

// must match MaterialAlphaMode::variant_as_u32
const ALPHA_MODE_OPAQUE: u32 = 0u;
const ALPHA_MODE_MASK: u32 = 1u;
const ALPHA_MODE_BLEND: u32 = 2u;

// Generated shader-id constants — one per enabled material in `awsm-materials`.
{{ shader_id_consts|safe }}

fn material_load_shader_id(byte_offset: u32) -> u32 {
    // shader_id is stored as the first u32 at the material's byte offset
    let index = byte_offset / 4u;
    return material_load_u32(index);
}

fn material_load_u32(index: u32) -> u32 {
    return bitcast<u32>(materials[index]);
}
fn material_load_f32(index: u32) -> f32 {
    return bitcast<f32>(materials[index]);
}

fn material_load_texture_info(index: u32) -> TextureInfo {
    return convert_texture_info(material_load_texture_info_raw(index));
}

fn material_load_texture_info_raw(index: u32) -> TextureInfoRaw {
    return TextureInfoRaw(
        material_load_u32(index + 0u),
        material_load_u32(index + 1u),
        material_load_u32(index + 2u),
        material_load_u32(index + 3u),
        material_load_u32(index + 4u),
    );
}
