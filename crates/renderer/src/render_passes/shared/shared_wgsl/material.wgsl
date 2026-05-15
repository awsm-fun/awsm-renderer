/*************** START pbr_material_color.wgsl ******************/
{% include "shared_wgsl/pbr/pbr_material.wgsl" %}
/*************** END pbr_material_color.wgsl ******************/

/*************** START pbr_material_color.wgsl ******************/
{% include "shared_wgsl/pbr/pbr_material_color.wgsl" %}
/*************** END pbr_material_color.wgsl ******************/

/*************** START unlit_material_color.wgsl ******************/
{% include "shared_wgsl/unlit/unlit_material.wgsl" %}
/*************** END unlit_material_color.wgsl ******************/

/*************** START toon_material.wgsl ******************/
{% include "shared_wgsl/toon/toon_material.wgsl" %}
/*************** END toon_material.wgsl ******************/

// must match MaterialAlphaMode::variant_as_u32
const ALPHA_MODE_OPAQUE: u32 = 0u;
const ALPHA_MODE_MASK: u32 = 1u;
const ALPHA_MODE_BLEND: u32 = 2u;

const SHADER_ID_PBR: u32 = 1u;
const SHADER_ID_UNLIT: u32 = 2u;
const SHADER_ID_TOON: u32 = 3u;

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
