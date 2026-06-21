// opaque_kernel_includes.wgsl — the shared include/preamble block for the
// opaque material kernel. Used by BOTH compute.wgsl (the material kernel) and
// skybox_primary.wgsl (the dedicated skybox writer for the canonical skybox
// bucket). Heavy shading includes (brdf/apply_lighting/material_color_calc) gate
// themselves out via inc.* — so the skybox kernel (inc = skybox_only) gets only
// the binding-struct + camera/math/skybox scaffolding.
//
// Module Tier A (generic) / Tier B (model-internal) / scaffold classification +
// current-vs-target gating: see the taxonomy table in
// `awsm-materials::shader_includes` (materials/src/shader_includes.rs).

/*************** START color_space.wgsl ******************/
{% include "shared_wgsl/color_space.wgsl" %}
/*************** END color_space.wgsl ******************/

/*************** START debug.wgsl ******************/
{% include "shared_wgsl/debug.wgsl" %}
/*************** END debug.wgsl ******************/

/*************** START camera.wgsl ******************/
{% include "shared_wgsl/camera.wgsl" %}
/*************** END camera.wgsl ******************/

/*************** START frame_globals.wgsl ******************/
{% include "shared_wgsl/frame_globals.wgsl" %}
/*************** END frame_globals.wgsl ******************/

/*************** START math.wgsl ******************/
{% include "shared_wgsl/math.wgsl" %}
/*************** END math.wgsl ******************/

/*************** START mesh_meta.wgsl ******************/
{% include "shared_wgsl/material_mesh_meta.wgsl" %}
/*************** END mesh_meta.wgsl ******************/

// instance_attrs.wgsl is already included via bind_groups.wgsl above (the
// `InstanceAttr` struct must be declared before binding 23 references it).

/*************** START textures.wgsl ******************/
{% include "shared_wgsl/textures.wgsl" %}
/*************** END textures.wgsl ******************/

{% if inc.vertex_color %}
/*************** START vertex_color.wgsl ******************/
{% include "shared_wgsl/vertex_color.wgsl" %}
/*************** END vertex_color.wgsl ******************/

/*************** START vertex_color_attrib.wgsl ******************/
{% include "material_opaque_wgsl/helpers/vertex_color_attrib.wgsl" %}
/*************** END vertex_color_attrib.wgsl ******************/
{% endif %}

/*************** START transforms.wgsl ******************/
{% include "shared_wgsl/transforms.wgsl" %}
/*************** END transforms.wgsl ******************/

/*************** START light_access_types.wgsl (always — ABI) ******************/
{% include "shared_wgsl/lighting/light_access_types.wgsl" %}
/*************** END light_access_types.wgsl ******************/
{% if inc.light_access %}
/*************** START light_access.wgsl ******************/
{% include "shared_wgsl/lighting/light_access.wgsl" %}
/*************** END light_access.wgsl ******************/
{% endif %}

{% if inc.light_access && !inc.apply_lighting %}
/*************** START froxel_walk.wgsl (Stage 7 — custom froxel-culled lights) ******************/
// Custom materials can declare LIGHT_ACCESS but never APPLY_LIGHTING (that is
// PBR-internal). apply_lighting.wgsl is what normally pulls in froxel_walk, so a
// custom material that lights itself would otherwise lack the per-froxel cull and
// iterate ALL n_lights. Include the SSOT directly here for that case (cull_params
// + lights_storage are already bound in the opaque lights group; froxel_slice_count
// is in the compute template context). When apply_lighting IS present it brings
// froxel_walk itself, so the `&& !inc.apply_lighting` guard avoids a double-include.
{% include "shared_wgsl/lighting/froxel_walk.wgsl" %}
/*************** END froxel_walk.wgsl ******************/
{% endif %}

{% if inc.apply_lighting %}
/*************** START apply_lighting.wgsl ******************/
{% include "shared_wgsl/lighting/apply_lighting.wgsl" %}
/*************** END apply_lighting.wgsl ******************/
{% endif %}

{% if inc.brdf %}
/*************** START brdf.wgsl ******************/
{% include "shared_wgsl/lighting/brdf.wgsl" %}
/*************** END brdf.wgsl ******************/
{% endif %}

{% if inc.ibl %}
/*************** START ibl.wgsl (Tier-A image-based-lighting primitive for custom materials) ******************/
{% include "shared_wgsl/lighting/ibl.wgsl" %}
/*************** END ibl.wgsl ******************/
{% endif %}


/*************** START material.wgsl ******************/
{% include "shared_wgsl/material.wgsl" %}
/*************** END material.wgsl ******************/

{% if inc.extras %}
/*************** START extras.wgsl ******************/
{% include "shared_wgsl/extras.wgsl" %}
/*************** END extras.wgsl ******************/
{% endif %}


{% if inc.textures %}
{% match mipmap %}
    {% when MipmapMode::Gradient %}
/*************** START mipmap.wgsl ******************/
{% include "material_opaque_wgsl/helpers/mipmap.wgsl" %}
/*************** END mipmap.wgsl ******************/
    {% when _ %}
{% endmatch %}

/*************** START texture_uvs.wgsl ******************/
{% include "material_opaque_wgsl/helpers/texture_uvs.wgsl" %}
/*************** END texture_uvs.wgsl ******************/
{% endif %}

/*************** START standard.wgsl ******************/
{% include "material_opaque_wgsl/helpers/standard.wgsl" %}
/*************** END standard.wgsl ******************/

/*************** START material_color.wgsl ******************/
{% include "material_opaque_wgsl/helpers/material_color_calc.wgsl" %}
/*************** END material_color.wgsl ******************/

/*************** START positions.wgsl ******************/
{% include "material_opaque_wgsl/helpers/positions.wgsl" %}
/*************** END positions.wgsl ******************/

{% if inc.skybox %}
/*************** START skybox.wgsl ******************/
{% include "material_opaque_wgsl/helpers/skybox.wgsl" %}
/*************** END skybox.wgsl ******************/
{% endif %}

{% if multisampled_geometry %}
/*************** START msaa.wgsl ******************/
{% include "material_opaque_wgsl/helpers/msaa.wgsl" %}
/*************** END msaa.wgsl ******************/
{% endif %}

/*************** START material_shading.wgsl ******************/
{% include "material_opaque_wgsl/helpers/material_shading.wgsl" %}
/*************** END material_shading.wgsl ******************/

{% if base == ShadingBase::Custom %}
/*************** START dynamic-material wrapper ******************/
// Auto-generated per the registered material's layout, implementing the
// `OpaqueShadingInput` / `OpaqueShadingOutput` / `MaterialData`
// contract.
//
// The contract types are declared here (inline rather than in
// shared_wgsl/) because they exist exclusively for the wrapper —
// first-party materials read their inputs from the kernel directly.

// MaterialData struct — auto-generated from the registered layout.
{{ dynamic_struct_decl|safe }}

// MaterialData accessor — auto-generated to walk the layout's byte
// offsets, reading values out of `materials: array<u32>` (from
// shared_wgsl/material.wgsl). The wrapper calls this once per pixel
// using `input.material_offset` and stuffs the result into
// `input.material`.
{{ dynamic_loader_decl|safe }}

struct OpaqueShadingInput {
    coords: vec2<i32>,
    screen_dims: vec2<u32>,
    triangle_index: u32,
    barycentric: vec3<f32>,
    main_instance_id: u32,
    world_normal: vec3<f32>,
    world_position: vec3<f32>,
    surface_to_camera: vec3<f32>,
    // Per-vertex attribute access (so a custom material can read COLOR_n / future
    // named streams the way built-in PBR does). The kernel computes these per
    // pixel anyway; we forward them rather than make the author recompute. Use
    // `material_vertex_color(input, set)`.
    triangle_indices: vec3<u32>,    // the 3 vertex indices of this pixel's triangle
    attribute_data_offset: u32,     // base offset into the packed per-vertex attr stream
    vertex_attribute_stride: u32,   // floats per vertex in that stream
    color_sets_index: u32,          // float offset to COLOR_0 within that stream
    uv_sets_index: u32,             // float offset to TEXCOORD_0 within that stream
    color_set_count: u32,           // number of COLOR sets present (out-of-range clamp)
    uv_set_count: u32,              // number of UV sets present (out-of-range clamp)
    material_offset: u32,
    material: MaterialData,
};
struct OpaqueShadingOutput {
    color: vec3<f32>,
    alpha: f32,
};

// Interpolated per-vertex `COLOR_<set_index>` at this pixel (barycentric-blended
// across the triangle). Mirrors built-in PBR's vertex-colour read. Only
// meaningful when the mesh actually carries that colour set — declare
// `vertex_color` in the material's includes and author against a painted mesh;
// on a mesh without the set there is no presence guard on the custom path.
// Gated on `inc.vertex_color` (builds on `vertex_color()` from
// vertex_color_attrib.wgsl) — a custom material reading vertex colour declares it.
{% if inc.vertex_color %}
fn material_vertex_color(input: OpaqueShadingInput, set_index: u32) -> vec4<f32> {
    // Out-of-range clamp: a set the mesh lacks reads a benign default rather than
    // an adjacent vertex's floats from the shared attribute pool (index-driven
    // fetch — no automatic bounds guard).
    if (set_index >= input.color_set_count) { return vec4<f32>(1.0); }
    return vertex_color(
        input.attribute_data_offset,
        input.triangle_indices,
        input.barycentric,
        VertexColorInfo(set_index),
        input.vertex_attribute_stride,
        input.color_sets_index,
    );
}
{% endif %}{# inc.vertex_color (material_vertex_color accessor) #}

// Interpolated `TEXCOORD_<set_index>` at this pixel (barycentric-blended across
// the triangle) — the raw-attribute companion to `material_vertex_color`. Lets a
// custom material read a NON-ZERO UV set directly, the same multi-set data the
// built-in PBR `uv_index` samples. As with `material_vertex_color`, there is no
// presence guard on the custom path — only meaningful when the mesh actually
// carries that UV set.
// Gated on `inc.textures` (it builds on `_texture_uv_per_vertex` from
// texture_uvs.wgsl) — a custom material that reads UVs declares `TEXTURES`.
// (A finer UV-without-sampling split could ride FragmentInputs::UV later.)
{% if inc.textures %}
fn material_uv(input: OpaqueShadingInput, set_index: u32) -> vec2<f32> {
    if (set_index >= input.uv_set_count) { return vec2<f32>(0.0); }
    let uv0 = _texture_uv_per_vertex(input.attribute_data_offset, set_index, input.triangle_indices.x, input.vertex_attribute_stride, input.uv_sets_index);
    let uv1 = _texture_uv_per_vertex(input.attribute_data_offset, set_index, input.triangle_indices.y, input.vertex_attribute_stride, input.uv_sets_index);
    let uv2 = _texture_uv_per_vertex(input.attribute_data_offset, set_index, input.triangle_indices.z, input.vertex_attribute_stride, input.uv_sets_index);
    return input.barycentric.x * uv0 + input.barycentric.y * uv1 + input.barycentric.z * uv2;
}
{% endif %}{# inc.textures (material_uv accessor) #}

{% if inc.light_access %}
// ── Froxel-culled per-pixel lights for custom materials (Stage 7) ────────────
// A custom material that lights itself should iterate ONLY the lights affecting
// this pixel — the deferred froxel cull built-ins get — instead of scanning all
// `n_lights`. These wrap the SAME `froxel_walk.wgsl` SSOT the built-in
// `apply_lighting_per_froxel` walks, so custom + built-in enumerate identically:
// the directional prefix first (indices `0..get_n_directional()`), then this
// pixel's froxel punctual slice. Recipe:
//   for (var i = 0u; i < material_pixel_light_count(input); i = i + 1u) {
//       let light = material_pixel_light(input, i);
//       let s = light_sample(light, input.world_normal, input.world_position);
//       // accumulate s.radiance * s.n_dot_l * brdf(...)
//   }
fn _material_froxel_base(input: OpaqueShadingInput) -> u32 {
    let view_z = -(camera_from_raw(camera_raw).view * vec4<f32>(input.world_position, 1.0)).z;
    return froxel_base_for_pixel(vec2<f32>(f32(input.coords.x), f32(input.coords.y)), view_z);
}

// Total lights affecting this pixel = directional prefix + this froxel's punctual count.
fn material_pixel_light_count(input: OpaqueShadingInput) -> u32 {
    return get_n_directional() + froxel_light_count(_material_froxel_base(input));
}

// The i-th light in canonical order (directionals first, then froxel punctuals).
fn material_pixel_light(input: OpaqueShadingInput, i: u32) -> Light {
    let n_dir = get_n_directional();
    if (i < n_dir) {
        return get_light(get_directional_light_index(i));
    }
    let base = _material_froxel_base(input);
    return get_light(lights_storage[base + 1u + (i - n_dir)]);
}
{% endif %}{# inc.light_access (custom froxel-light accessors) #}

fn custom_shade_dynamic(input: OpaqueShadingInput) -> OpaqueShadingOutput {
{{ dynamic_wgsl_fragment|safe }}
}
/*************** END dynamic-material wrapper ******************/
{% endif %}

{% if debug.any() %}
/*************** START debug.wgsl ******************/
{% include "material_opaque_wgsl/helpers/debug.wgsl" %}
/*************** END debug.wgsl ******************/
{% endif %}
