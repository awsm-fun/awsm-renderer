/*************** START bind_groups.wgsl ******************/
{% include "material_opaque_wgsl/bind_groups.wgsl" %}
/*************** END bind_groups.wgsl ******************/

/*************** START math.wgsl ******************/
{% include "shared_wgsl/math.wgsl" %}
/*************** END math.wgsl ******************/

/*************** START camera.wgsl ******************/
{% include "shared_wgsl/camera.wgsl" %}
/*************** END camera.wgsl ******************/

/*************** START frame_globals.wgsl ******************/
{% include "shared_wgsl/frame_globals.wgsl" %}
/*************** END frame_globals.wgsl ******************/

/*************** START skybox.wgsl ******************/
{% include "material_opaque_wgsl/helpers/skybox.wgsl" %}
/*************** END skybox.wgsl ******************/

/*************** START mesh_meta.wgsl ******************/
{% include "shared_wgsl/material_mesh_meta.wgsl" %}
/*************** END mesh_meta.wgsl ******************/

/*************** START textures.wgsl ******************/
{% include "shared_wgsl/textures.wgsl" %}
/*************** END textures.wgsl ******************/

/*************** START material.wgsl ******************/
{% include "shared_wgsl/material.wgsl" %}
/*************** END material.wgsl ******************/

/*************** START vertex_color.wgsl ******************/
{% include "shared_wgsl/vertex_color.wgsl" %}
/*************** END vertex_color.wgsl ******************/

// light_access types (ABI structs for the group(1) light bindings) + accessor
// functions. The empty kernel's `main` only writes the skybox, BUT it still
// embeds `materials_wgsl` (every first-party fragment), and toon's
// `compute_toon_lit_color` calls `get_light` / `light_sample` — so the accessor
// functions must be present here too (they're dead but referenced). Not gated:
// empty is a single shader, and the per-material light_access opt-out (Phase 4)
// targets the per-material Custom kernels, not this one.
/*************** START light_access_types.wgsl ******************/
{% include "shared_wgsl/lighting/light_access_types.wgsl" %}
/*************** END light_access_types.wgsl ******************/
/*************** START light_access.wgsl ******************/
{% include "shared_wgsl/lighting/light_access.wgsl" %}
/*************** END light_access.wgsl ******************/


@compute @workgroup_size(8, 8)
fn main(
    @builtin(global_invocation_id) gid: vec3<u32>
) {
    let coords = vec2<i32>(gid.xy);
    let screen_dims = textureDimensions(opaque_tex);
    let screen_dims_i32 = vec2<i32>(i32(screen_dims.x), i32(screen_dims.y));
    let screen_dims_f32 = vec2<f32>(f32(screen_dims.x), f32(screen_dims.y));
    let pixel_center = vec2<f32>(f32(coords.x) + 0.5, f32(coords.y) + 0.5);

    // Bounds check
    if (coords.x >= screen_dims_i32.x || coords.y >= screen_dims_i32.y) {
        return;
    }

    let camera = camera_from_raw(camera_raw);

    let skybox_col = sample_skybox(coords, screen_dims_f32, camera, skybox_tex, skybox_sampler);

    textureStore(opaque_tex, coords, skybox_col);

}
