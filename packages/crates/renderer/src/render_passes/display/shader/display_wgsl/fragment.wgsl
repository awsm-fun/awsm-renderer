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

    {% if supersample %}
    // Supersampled composite: the render targets are `render_scale` times
    // the swap-chain size — downsample with a manual bilinear at the scaled
    // sample point. At exactly 2.0 the sample point lands on the corner
    // between a 2x2 block, so the bilinear IS the box average; the manual
    // 4-tap (instead of a sampler) keeps the bind-group layout identical to
    // the 1:1 variant. `display_uniform.scale_*` = composite_size / target.
    let dims = vec2<i32>(textureDimensions(composite_texture));
    let src = in.full_screen_quad_position.xy
        * vec2<f32>(display_uniform.scale_x, display_uniform.scale_y);
    let base = src - vec2<f32>(0.5);
    let i0 = vec2<i32>(floor(base));
    let f = base - floor(base);
    let c00 = clamp(i0, vec2<i32>(0), dims - 1);
    let c10 = clamp(i0 + vec2<i32>(1, 0), vec2<i32>(0), dims - 1);
    let c01 = clamp(i0 + vec2<i32>(0, 1), vec2<i32>(0), dims - 1);
    let c11 = clamp(i0 + vec2<i32>(1, 1), vec2<i32>(0), dims - 1);
    let s00 = textureLoad(composite_texture, c00, 0);
    let s10 = textureLoad(composite_texture, c10, 0);
    let s01 = textureLoad(composite_texture, c01, 0);
    let s11 = textureLoad(composite_texture, c11, 0);
    var color: vec4<f32> = mix(mix(s00, s10, f.x), mix(s01, s11, f.x), f.y);
    {% else %}
    var color: vec4<f32> = textureLoad(composite_texture, coords, 0);
    {% endif %}

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
