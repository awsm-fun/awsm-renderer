// opaque_kernel_includes.wgsl — the shared include/preamble block for the
// opaque material kernel. Used by BOTH compute.wgsl (the material kernel) and
// skybox_primary.wgsl (the dedicated skybox writer for the canonical skybox
// bucket). Heavy shading includes (brdf/apply_lighting/material_color_calc) gate
// themselves out via inc.* — so the skybox kernel (inc = skybox_only) gets only
// the binding-struct + camera/math/skybox scaffolding.

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

/*************** START vertex_color.wgsl ******************/
{% include "shared_wgsl/vertex_color.wgsl" %}
/*************** END vertex_color.wgsl ******************/

/*************** START vertex_color_attrib.wgsl ******************/
{% include "material_opaque_wgsl/helpers/vertex_color_attrib.wgsl" %}
/*************** END vertex_color_attrib.wgsl ******************/

/*************** START transforms.wgsl ******************/
{% include "shared_wgsl/transforms.wgsl" %}
/*************** END transforms.wgsl ******************/

/*************** START light_access.wgsl ******************/
{% include "shared_wgsl/lighting/light_access.wgsl" %}
/*************** END light_access.wgsl ******************/

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


/*************** START material.wgsl ******************/
{% include "shared_wgsl/material.wgsl" %}
/*************** END material.wgsl ******************/

/*************** START extras.wgsl ******************/
{% include "shared_wgsl/extras.wgsl" %}
/*************** END extras.wgsl ******************/


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

/*************** START standard.wgsl ******************/
{% include "material_opaque_wgsl/helpers/standard.wgsl" %}
/*************** END standard.wgsl ******************/

/*************** START material_color.wgsl ******************/
{% include "material_opaque_wgsl/helpers/material_color_calc.wgsl" %}
/*************** END material_color.wgsl ******************/

/*************** START positions.wgsl ******************/
{% include "material_opaque_wgsl/helpers/positions.wgsl" %}
/*************** END positions.wgsl ******************/

/*************** START skybox.wgsl ******************/
{% include "material_opaque_wgsl/helpers/skybox.wgsl" %}
/*************** END skybox.wgsl ******************/

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
    material_offset: u32,
    material: MaterialData,
};
struct OpaqueShadingOutput {
    color: vec3<f32>,
    alpha: f32,
};

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
