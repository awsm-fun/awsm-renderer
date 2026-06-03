/*************** START tonemap.wgsl ******************/
{% include "display_wgsl/helpers/tonemap.wgsl" %}
/*************** END tonemap.wgsl ******************/

/*************** START color_space.wgsl ******************/
{% include "shared_wgsl/color_space.wgsl" %}
/*************** END color_space.wgsl ******************/

struct FragmentInput {
    @builtin(position) full_screen_quad_position: vec4<f32>,
}

@fragment
fn frag_main(in: FragmentInput) -> @location(0) vec4<f32> {
    let coords = vec2<i32>(in.full_screen_quad_position.xy);

    var color: vec4<f32> = textureLoad(composite_texture, coords, 0);

    // Apply scene exposure BEFORE tonemapping. The renderer treats
    // KHR_lights_punctual intensities as already-radiometric (the spec
    // leaves the lumens→watts conversion implementation-defined), so
    // assets authored at real candela/lux values (e.g. PlaysetLightTest's
    // 1500 cd LED) need to be pulled into the tonemapper's responsive
    // range somewhere. We do it here with a single linear multiplier,
    // exp2(EV), so the UI exposure slider behaves like a photo stop.
    let exposed = color.rgb * display_uniform.exposure_scale;

    // Apply tone mapping to compress HDR to displayable range
    {% match tonemapping %}
        {% when ToneMapping::KhronosNeutralPbr %}
            let rgb = khronos_pbr_neutral_tonemap(exposed);
        {% when ToneMapping::Aces %}
            let rgb = aces_tonemap(exposed);
        {% when _ %}
            let rgb = exposed;
    {% endmatch %}

    return vec4<f32>(linear_to_srgb(rgb), color.a);
}
