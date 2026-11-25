// TAA reactive seed (plan 03 §5.2, §5.4): a fullscreen draw at the start of
// the world transparent pass.
// - HUD pixels drawn with the scene (the opaque-path HUD; its depth is in
//   `hud_depth`) get `reactive = 1.0` EXACTLY: the resolve reads a mask
//   value >= 0.999 as "HUD pixel" and passes it through (no history, no
//   spreading to neighbours). Everything else stays <= 0.99.
// - Opaque pixels whose material is flagged `temporal: reactive` (UV flow,
//   flipbooks, animated emissive, custom shaders reading time: colour
//   changes on a still surface) get `reactive = k_material` (camera
//   dof_params.w under TAA).
// Every other pixel is discarded. The transparent draws that follow
// MAX-blend over it.
//
// - (`ssr` variant) reflective pixels get `reflectivity * k_ssr` (03 §5.3:
//   reflections move with a different parallax than their surface).
//
// The draw's first instance carries the per-draw constants (see
// `reactive_seed.rs::seed_instance`): bits 0-1 say whether `hud_depth` holds
// this frame's HUD: 0 = no HUD (the binding is a placeholder), 1 = HUD with
// clear depth 1.0 (forward Z), 2 = HUD with clear depth 0.0 (reverse Z);
// bits 2-12 are k_ssr in 1/1024 steps (`ssr` variant only); bit 16 marks
// non-rigid meshes (TAA's prep-pass fallback, 03 §2.4: their vectors are the
// node's rigid motion only, so they get `NON_RIGID_REACTIVE`); bits 17-26
// are k_new in 1/1023 steps: meshes in their first rendered frame (meta word
// 22 bit 2, `TaaTuning::reactive_new_objects`) get it, so a new object
// starts from a history of itself, not of what was behind it.
//
// Colour target 0 is attached by the pass; the pipeline blends it to keep
// the destination.
// TAA excludes MSAA, so the visibility buffer is single-sampled.

{% include "shared_wgsl/camera.wgsl" %}
{% include "shared_wgsl/math.wgsl" %}

@group(0) @binding(0) var<uniform> camera_raw: CameraRaw;
@group(0) @binding(1) var visibility_data_tex: texture_2d<u32>;
// The per-mesh material meta, read as words (256-byte entries).
@group(0) @binding(2) var<storage, read> material_mesh_meta_words: array<u32>;
// The HUD geometry pass's depth (render size; a placeholder without a HUD).
@group(0) @binding(3) var hud_depth_tex: texture_depth_2d;
{% if ssr %}
// SSR reflection descriptor (render size): rgb = reflectivity tint.
@group(0) @binding(4) var reflection_descriptor_tex: texture_2d<f32>;
{% endif %}

// Word indices inside one `MaterialMeshMeta` (material_mesh_meta.wgsl).
const META_IS_HUD: u32 = 16u;
const META_TEMPORAL_REACTIVE: u32 = 22u;
const META_NON_RIGID: u32 = 23u;
const NON_RIGID_BIT: u32 = 65536u;
const NON_RIGID_REACTIVE: f32 = 0.5;
const NEW_OBJECT_BIT: u32 = 4u;

struct SeedVertex {
    @builtin(position) position: vec4<f32>,
    @location(0) @interpolate(flat) hud_mode: u32,
};

@vertex
fn vert_main(
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) instance_index: u32,
) -> SeedVertex {
    // One triangle covering the viewport.
    let uv = vec2<f32>(f32((vertex_index << 1u) & 2u), f32(vertex_index & 2u));
    return SeedVertex(vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0), instance_index);
}

struct SeedOutput {
    @location(0) color: vec4<f32>,
    @location(1) reactive: f32,
};

@fragment
fn frag_main(input: SeedVertex) -> SeedOutput {
    let coords = vec2<i32>(input.position.xy);
    let hud_mode = input.hud_mode & 3u;
    if (hud_mode != 0u) {
        let clear_depth = select(1.0, 0.0, hud_mode == 2u);
        if (textureLoad(hud_depth_tex, coords, 0) != clear_depth) {
            return SeedOutput(vec4<f32>(0.0), 1.0);
        }
    }
    let vis = textureLoad(visibility_data_tex, coords, 0);
    let triangle_index = join32(vis.x, vis.y);
    if (triangle_index == U32_MAX) {
        discard;
    }
    let base = join32(vis.z, vis.w) / 4u;
    let temporal = (material_mesh_meta_words[base + META_TEMPORAL_REACTIVE] & 1u) != 0u
        && material_mesh_meta_words[base + META_IS_HUD] == 0u;
    var reactive = select(0.0, camera_raw.dof_params.w, temporal);
    if ((input.hud_mode & NON_RIGID_BIT) != 0u
        && material_mesh_meta_words[base + META_NON_RIGID] != 0u) {
        reactive = max(reactive, NON_RIGID_REACTIVE);
    }
    let k_new = f32((input.hud_mode >> 17u) & 1023u) / 1023.0;
    if (k_new > 0.0
        && (material_mesh_meta_words[base + META_TEMPORAL_REACTIVE] & NEW_OBJECT_BIT) != 0u
        && material_mesh_meta_words[base + META_IS_HUD] == 0u) {
        reactive = max(reactive, k_new);
    }
{% if ssr %}
    let k_ssr = f32((input.hud_mode >> 2u) & 2047u) / 1024.0;
    let desc = textureLoad(reflection_descriptor_tex, coords, 0).rgb;
    reactive = max(reactive, max(desc.r, max(desc.g, desc.b)) * k_ssr);
{% endif %}
    if (reactive <= 0.0) {
        discard;
    }
    // Below 1.0: exactly 1.0 is reserved for HUD pixels (pass-through).
    return SeedOutput(vec4<f32>(0.0), min(reactive, 0.99));
}
