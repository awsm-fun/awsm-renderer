// TAA: depth of field BEFORE the resolve (`TaaDofOrder::BeforeResolve`,
// docs/TAA.md §3.12).
//
// The effects pass's own DoF (`effects_wgsl/helpers/dof.wgsl`, the same
// code) run on this frame's HDR render, `composite`: render size, jittered,
// before the resolve has averaged anything. Render pixel p and depth texel p
// are the same jittered sample, so depth is read directly (`load_depth`, not
// the effects pass's `load_depth_post`), and the circle of confusion is in
// render pixels (`camera.viewport_size` is the render size), so a disc keeps
// its angular size at any render scale. `max_blur` caps it at the effects
// pass's 20 OUTPUT pixels, scaled to render pixels.
//
// The resolve then reads `dof_out` instead of `composite` (its `.rgb`).
// Blurred pixels keep the motion, depth and id of their own surface; where a
// defocused foreground spreads over a farther one, no disocclusion test sees
// it. Measured (docs/TAA.md §3.12): adding the foreground's coverage to the
// current frame's weight changed nothing that mattered, so it is not done.
//
// Two dispatches of this module, as in the effects pass: `dof_prepare` (from
// dof.wgsl) fills `dof_data`, then `main`.

@group(0) @binding(0) var composite_tex: texture_2d<f32>;
@group(0) @binding(1) var<uniform> camera_raw: CameraRaw;
@group(0) @binding(2) var depth_tex: texture_depth_2d;
@group(0) @binding(3) var dof_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(4) var<storage, read_write> dof_data: array<u32>;

struct DofPreParams {
    // The widest disc, render pixels: 20 output pixels * render / output.
    max_blur: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};
@group(0) @binding(5) var<uniform> dof_pre_params: DofPreParams;

/*************** START camera.wgsl ******************/
{% include "shared_wgsl/camera.wgsl" %}
/*************** END camera.wgsl ******************/

/*************** START f16_round.wgsl ******************/
{% include "shared_wgsl/taa/f16_round.wgsl" %}
/*************** END f16_round.wgsl ******************/

/*************** START depth.wgsl ******************/
{% include "effects_wgsl/helpers/depth.wgsl" %}
/*************** END depth.wgsl ******************/

/*************** START dof.wgsl ******************/
{% include "effects_wgsl/helpers/dof.wgsl" %}
/*************** END dof.wgsl ******************/

@compute @workgroup_size(8, 8)
fn main(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(workgroup_id) wid: vec3<u32>,
    @builtin(local_invocation_index) lidx: u32
) {
    // Holds a workgroup barrier, so it stays ahead of every return.
    let dof_reach = dof_tile_reach(wid.xy, lidx);
    let coords = vec2<i32>(gid.xy);
    let dims = vec2<i32>(textureDimensions(composite_tex));
    if (coords.x >= dims.x || coords.y >= dims.y) {
        return;
    }
    var rgb = textureLoad(composite_tex, coords, 0).rgb;
    // `apply_dof`'s cull, without its SMAA and effects terms (neither exists
    // under TAA).
    let tile_reach = max(dof_reach.front, dof_reach.widest);
    if (tile_reach * tile_reach > DOF_CENTER_AREA) {
        let center_coc = dof_load_coc(coords, dims);
        let blurred = dof_blur(coords, dims, center_coc, dof_reach.front);
        if (blurred.touched) {
            rgb = blurred.color;
        }
    }
    // Rounded, not truncated (Apple GPUs truncate a compute store to
    // binary16): this is one more rgba16float store than with no AA, and
    // truncating it left 3% of a sky gradient one level dark.
    textureStore(dof_out, coords, vec4<f32>(taa_round_f16(rgb), 1.0));
}
