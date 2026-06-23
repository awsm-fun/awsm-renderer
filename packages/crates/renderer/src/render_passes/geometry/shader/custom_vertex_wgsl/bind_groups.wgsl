// Bind groups + material-load helpers for the CUSTOM-VERTEX geometry variant.
//
// Same augmented group 0 + reused groups 1-3 as the MASKED variant. The
// custom-vertex hook's material_data_load reads the materials storage buffer
// those bind groups declare (the plain geometry bind groups lack it). We reuse
// the masked bind-group declarations verbatim via an include.
//
// The masked variant gets its type definitions + the minimal material-load
// helpers from its FRAGMENT includes (shared_wgsl/masked_alpha.wgsl); this
// variant pairs the masked bind groups with the PLAIN geometry fragment (which
// includes none of that), so we provide them here. We deliberately do NOT
// include shared_wgsl/material.wgsl: it emits the full materials_wgsl blob,
// whose dynamic-material color fragments reference opaque-only contract types.
// Instead, mirror masked_alpha's minimal helper set (the same the generated
// material_data_load / material_sample_<name> reference). WGSL resolves
// module-scope identifiers order-independently, so declaring these alongside
// the bindings is fine.
{% include "shared_wgsl/material_mesh_meta.wgsl" %}
{% include "shared_wgsl/textures.wgsl" %}
{% include "masked_wgsl/bind_groups.wgsl" %}

// Minimal material-buffer load helpers (mirrors shared_wgsl/masked_alpha.wgsl).
fn material_load_u32(index: u32) -> u32 { return bitcast<u32>(materials[index]); }
fn material_load_f32(index: u32) -> f32 { return bitcast<f32>(materials[index]); }
fn material_load_texture_info_raw(index: u32) -> TextureInfoRaw {
    return TextureInfoRaw(
        material_load_u32(index + 0u),
        material_load_u32(index + 1u),
        material_load_u32(index + 2u),
        material_load_u32(index + 3u),
        material_load_u32(index + 4u),
    );
}
fn material_load_texture_info(index: u32) -> TextureInfo {
    return convert_texture_info(material_load_texture_info_raw(index));
}

// LOD-0 texture-pool sampler so the generated material_sample_<name> helpers
// (appended to the loader when the material declares textures) resolve. The
// vertex stage has no auto-derivatives, so LOD 0 is the correct, cheap choice.
fn texture_pool_sample(info: TextureInfo, attribute_uv: vec2<f32>) -> vec4<f32> {
    let uv = texture_transform_uvs(attribute_uv, info);
    var color = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    switch info.array_index {
        {% for i in 0..texture_pool_arrays_len %}
        case {{ i }}u: {
            switch info.sampler_index {
                {% for j in 0..texture_pool_samplers_len %}
                case {{ j }}u: {
                    color = textureSampleLevel(pool_tex_{{ i }}, pool_sampler_{{ j }}, uv, i32(info.layer_index), 0);
                }
                {% endfor %}
                default: {}
            }
        }
        {% endfor %}
        default: {}
    }
    return color;
}
