/*************** START camera.wgsl ******************/
{% include "shared_wgsl/camera.wgsl" %}
/*************** END camera.wgsl ******************/

/*************** START frame_globals.wgsl ******************/
{% include "shared_wgsl/frame_globals.wgsl" %}
/*************** END frame_globals.wgsl ******************/

/*************** START math.wgsl ******************/
{% include "shared_wgsl/math.wgsl" %}
/*************** END math.wgsl ******************/

/*************** START color_space.wgsl ******************/
{% include "shared_wgsl/color_space.wgsl" %}
/*************** END color_space.wgsl ******************/

{% if smaa_anti_alias %}
    /*************** START smaa.wgsl ******************/
    {% include "effects_wgsl/helpers/smaa.wgsl" %}
    /*************** END smaa.wgsl ******************/
{% endif %}

{% if bloom %}
    /*************** START bloom.wgsl ******************/
    {% include "effects_wgsl/helpers/bloom.wgsl" %}
    /*************** END bloom.wgsl ******************/
{% endif %}

{% if dof %}
    /*************** START dof.wgsl ******************/
    {% include "effects_wgsl/helpers/dof.wgsl" %}
    /*************** END dof.wgsl ******************/
{% endif %}



@compute @workgroup_size(8, 8)
fn main(
    @builtin(global_invocation_id) gid: vec3<u32>
) {
    let coords = vec2<i32>(gid.xy);
    let screen_dims = textureDimensions(composite_tex);
    let screen_dims_i32 = vec2<i32>(i32(screen_dims.x), i32(screen_dims.y));
    let screen_dims_f32 = vec2<f32>(f32(screen_dims.x), f32(screen_dims.y));
    let pixel_center = vec2<f32>(f32(coords.x) + 0.5, f32(coords.y) + 0.5);

    // Bounds check
    if (coords.x >= screen_dims_i32.x || coords.y >= screen_dims_i32.y) {
        return;
    }

    let camera = camera_from_raw(camera_raw);
    let frame_globals = frame_globals_from_raw(frame_globals_raw);

    let composite_color = textureLoad(composite_tex, coords, 0);

    {% if smaa_anti_alias %}
        var rgb = apply_smaa(composite_color, coords).rgb;
    {% else %}
        var rgb = composite_color.rgb;
    {% endif %}

    {% if bloom %}
        rgb = apply_bloom(rgb, coords, screen_dims_i32);
    {% endif %}

    {% if dof %}
        rgb = apply_dof(rgb, coords, screen_dims_i32, camera);
    {% endif %}

    {% if !ping_pong %}
        textureStore(effects_tex, coords, vec4<f32>(rgb, 1.0));
    {% else %}
        textureStore(bloom_tex, coords, vec4<f32>(rgb, 1.0));
    {% endif %}
}
