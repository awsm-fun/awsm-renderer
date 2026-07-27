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

{# Both haze paths read depth, not just the analytic one: the froxel path needs
   it to find which slice of the scattering volume a pixel sits in. #}
{% if dof || atmosphere || atmosphere_volumetric %}
    /*************** START depth.wgsl ******************/
    {% include "effects_wgsl/helpers/depth.wgsl" %}
    /*************** END depth.wgsl ******************/
{% endif %}

{% if dof %}
    /*************** START dof.wgsl ******************/
    {% include "effects_wgsl/helpers/dof.wgsl" %}
    /*************** END dof.wgsl ******************/
{% endif %}

{# The MEDIUM math is shared: the analytic path integrates the whole view ray
   with it, the volumetric path integrates only the segment BEYOND its froxel
   volume. Exactly one copy, for either consumer — same shape as depth.wgsl
   above, and for the same reason. #}
{% if atmosphere || atmosphere_volumetric %}
    /*************** START atmosphere_medium.wgsl ******************/
    {% include "effects_wgsl/helpers/atmosphere_medium.wgsl" %}
    /*************** END atmosphere_medium.wgsl ******************/
{% endif %}

{% if atmosphere %}
    /*************** START atmosphere.wgsl ******************/
    {% include "effects_wgsl/helpers/atmosphere.wgsl" %}
    /*************** END atmosphere.wgsl ******************/
{% endif %}

{% if atmosphere_volumetric %}
    /*************** START atmosphere_volumetric.wgsl ******************/
    {% include "effects_wgsl/helpers/atmosphere_volumetric.wgsl" %}
    /*************** END atmosphere_volumetric.wgsl ******************/
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

    // Haze goes in before the bloom composite, and the composite scales the
    // ADDED glow by the pixel's haze transmittance (recorded by the apply_*
    // helpers in `atmosphere_scene_transmittance`). The pyramid itself is
    // built by the dedicated bloom pass from the PRE-haze composite — it runs
    // before this shader — so scaling here is what keeps an emitter buried in
    // dense haze from blooming at full strength through the fog. Residual
    // approximation: the glow is attenuated by the RECEIVING pixel's
    // transmittance (not the source's), and the in-scattered haze itself
    // doesn't bloom.
    {% if atmosphere %}
        rgb = apply_atmosphere(rgb, coords, pixel_center, screen_dims_f32, camera);
    {% endif %}

    {% if atmosphere_volumetric %}
        rgb = apply_atmosphere_volumetric(rgb, coords, pixel_center, screen_dims_f32, camera);
    {% endif %}

    {% if bloom %}
        {% if atmosphere || atmosphere_volumetric %}
            let pre_bloom_rgb = rgb;
            rgb = apply_bloom(rgb, coords, screen_dims_i32);
            rgb = pre_bloom_rgb + (rgb - pre_bloom_rgb) * atmosphere_scene_transmittance;
        {% else %}
            rgb = apply_bloom(rgb, coords, screen_dims_i32);
        {% endif %}
    {% endif %}

    {% if dof %}
        rgb = apply_dof(rgb, coords, screen_dims_i32, camera);
    {% endif %}

    textureStore(effects_tex, coords, vec4<f32>(rgb, 1.0));
}
