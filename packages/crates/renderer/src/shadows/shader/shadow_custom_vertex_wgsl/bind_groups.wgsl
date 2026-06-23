// Bind groups + material-load helpers for the CUSTOM-VERTEX shadow caster.
//
// The custom-vertex shadow pipeline runs the SAME `custom_displace_vertex` hook
// the geometry custom-vertex pipeline does, so the displaced silhouette matches
// the lit geometry exactly (no detached shadow). The hook's `material_data_load`
// reads the `materials` storage buffer + samples the texture pool, and reads
// `frame_globals_raw` (animated displacement) — all in the VERTEX stage. The
// plain shadow bind groups (`shadow_wgsl/bind_groups.wgsl`) declare NONE of
// those, so this variant augments group 0, exactly like the masked-shadow group
// 0 — but with VERTEX visibility on the bindings the hook reads.
//
// Layout matches `ShadowMaskedBindGroup` (the masked-shadow group-0 is reused at
// draw time): shadow_view (0) + materials (1) + material_mesh_metas (2) +
// visibility_data (3) + texture_transforms (4) + frame_globals_raw (5) + the
// texture pool. Keep the binding indices in lock-step with
// `shadow_masked_wgsl/bind_groups.wgsl` and `ShadowMaskedBindGroup`.
//
// Type definitions: the shadow vertex include already provides `GeometryMeshMeta`
// (via `shared_wgsl/vertex/geometry_mesh_meta.wgsl`). We pull in the material /
// texture type defs the loader references here, plus `frame_globals.wgsl` (struct
// + `frame_globals_from_raw`) — so the shadow vertex.wgsl must NOT double-include
// it for this variant. WGSL resolves module-scope identifiers order-independently.
{% include "shared_wgsl/material_mesh_meta.wgsl" %}
{% include "shared_wgsl/textures.wgsl" %}
{% include "shared_wgsl/frame_globals.wgsl" %}

struct ShadowView {
    view_projection: mat4x4<f32>,
    bias: vec4<f32>,
};
@group(0) @binding(0) var<uniform> shadow_view: ShadowView;
// Renderer-wide per-material data pool (raw u32 words). Read at
// `material_mesh_meta.material_offset` via the `material_load_*` helpers, by the
// VERTEX hook.
@group(0) @binding(1) var<storage, read> materials: array<u32>;
// Per-mesh material meta — declared for binding-index parity with the masked
// group; the custom-vertex VERTEX hook doesn't read it (it loads MaterialData
// at `geometry_mesh_meta.material_mesh_meta_offset`).
@group(0) @binding(2) var<storage, read> material_mesh_metas: array<MaterialMeshMeta>;
// Merged geometry pool — declared for binding-index parity; unused by the hook.
@group(0) @binding(3) var<storage, read> visibility_data: array<f32>;
// Per-material UV transforms (KHR_texture_transform). Referenced by
// `texture_transform_uvs` (in textures.wgsl) when a custom material's vertex
// hook samples a texture.
@group(0) @binding(4) var<storage, read> texture_transforms: array<TextureTransform>;
// Per-frame uniform — `time` for animated displacement (read by the VERTEX hook
// via `frame_globals_from_raw`).
@group(0) @binding(5) var<uniform> frame_globals_raw: FrameGlobalsRaw;
{% for i in 0..texture_pool_arrays_len %}
@group(0) @binding({{ 6 + i }}u) var pool_tex_{{ i }}: texture_2d_array<f32>;
{% endfor %}
{% for i in 0..texture_pool_samplers_len %}
@group(0) @binding({{ 6 + texture_pool_arrays_len + i }}u) var pool_sampler_{{ i }}: sampler;
{% endfor %}

// === Group 1: transforms (vertex) — mirrors geometry/shadow bind_groups ===
struct TransformPacked {
    model_world: mat4x4<f32>,
    normal_world: mat3x3<f32>,
};
@group(1) @binding(0) var<storage, read> transforms: array<TransformPacked>;

// === Group 2: per-mesh geometry meta (vertex) — forked by instancing ===
{% if instancing_transforms %}
@group(2) @binding(0) var<uniform> geometry_mesh_meta: GeometryMeshMeta;
{% else %}
@group(2) @binding(0) var<storage, read> geometry_mesh_metas: array<GeometryMeshMeta>;
var<private> geometry_mesh_meta: GeometryMeshMeta;
{% endif %}

// === Group 3: morph + skin animation (vertex) — mirrors geometry bind_groups ===
@group(3) @binding(0) var<storage, read> geometry_morph_weights: array<f32>;
@group(3) @binding(1) var<storage, read> geometry_morph_values: array<f32>;
@group(3) @binding(2) var<storage, read> skin_joint_matrices: array<mat4x4<f32>>;
@group(3) @binding(3) var<storage, read> skin_joint_index_weights: array<f32>;

// Minimal material-buffer load helpers (mirrors shared_wgsl/masked_alpha.wgsl +
// the geometry custom-vertex bind_groups). The generated `material_data_load`
// (loader_decl) references these; the plain shadow pass has no fragment to pull
// `masked_alpha.wgsl` in, so declare them here for the VERTEX stage.
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

// LOD-0 texture-pool sampler so the generated `material_sample_<name>` helpers
// resolve. The vertex stage has no auto-derivatives, so LOD 0 is the correct,
// cheap choice.
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
