// Minimal material-buffer load helpers + a LOD-0 texture-pool sampler, shared by
// the alpha-tested (MASK) and custom-vertex raster variants.
//
// Three places need this exact helper set: the masked alpha fragment
// (`shared_wgsl/masked_alpha.wgsl`), the geometry custom-vertex bind groups, and
// the shadow custom-vertex bind groups. They all read the `materials` storage
// buffer (raw u32 words) at a `material_offset` and sample the texture pool at
// LOD 0 (the visibility/cutout/vertex stages have no auto-derivatives). Factoring
// the helpers here lets a COMBINED masked + custom-vertex module include both the
// masked fragment AND the custom-vertex bind groups without redefining them.
//
// Deliberately does NOT pull in `shared_wgsl/material.wgsl` (which would emit the
// full `materials_wgsl` blob referencing opaque-only contract types) nor
// `shared_wgsl/textures.wgsl` (the including template already provides the
// texture type defs + `texture_transform_uvs`). It references — but does not
// declare — `materials`, the `pool_tex_*` / `pool_sampler_*` bindings, `TextureInfo`,
// `TextureInfoRaw`, `convert_texture_info`, and `texture_transform_uvs`; WGSL
// resolves module-scope identifiers order-independently, so the including
// template must declare/include those exactly once (the masked + custom-vertex
// templates already do).
//
// The including template MUST also define these context fields (for the pool
// switch below): texture_pool_arrays_len, texture_pool_samplers_len: u32.

// Read one UV set for a single vertex from the merged geometry pool
// (`visibility_data`). Shared by the masked fragment (per-vertex UV for the
// barycentric reconstruction) AND the custom-vertex VERTEX hooks (geometry +
// shadow), which build the per-vertex `input.uv` array from it. The including
// template MUST declare the `visibility_data: array<f32>` storage binding
// (VERTEX-visible for the vertex hooks). `attribute_data_offset`,
// `vertex_attribute_stride`, `uv_sets_index` are in FLOATS (the callers convert
// the byte values from `MaterialMeshMeta` by /4u where needed).
fn _mask_uv_per_vertex(attribute_data_offset: u32, set_index: u32, vertex_index: u32, vertex_attribute_stride: u32, uv_sets_index: u32) -> vec2<f32> {
    let vertex_start = attribute_data_offset + (vertex_index * vertex_attribute_stride);
    let uv_offset = uv_sets_index + (set_index * 2u);
    let index = vertex_start + uv_offset;
    return vec2<f32>(visibility_data[index], visibility_data[index + 1u]);
}

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

// LOD-0 texture-pool sampler. The visibility-pass discard, the vertex hook, and
// the shadow vertex hook all run without auto-derivatives, so LOD 0 is the
// correct, cheap choice. The generated `material_sample_<name>` helpers (custom)
// + the masked base-color / flipbook cutout both call this.
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
