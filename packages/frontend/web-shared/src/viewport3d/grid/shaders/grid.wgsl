@group(0) @binding(0) var<uniform> camera_raw: CameraRaw;

// Raw camera uniform structure (matches GPU buffer layout with padding).
//
// Mirrors `shared_wgsl/camera.wgsl`'s `CameraRaw` minus the trailing
// fields this shader doesn't need. `frame_count_and_padding` was
// removed when the monotonic frame counter migrated to the
// `frame_globals` uniform (see `crates/renderer/src/frame_globals`).
// Total layout is now 496 bytes; the `_padding_end` array sizes
// the locally-declared struct out to that figure.
struct CameraRaw {
    view: mat4x4<f32>,
    proj: mat4x4<f32>,
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    inv_proj: mat4x4<f32>,
    inv_view: mat4x4<f32>,
    position: vec4<f32>,  // .xyz = position, .w = unused
    frustum_rays: array<vec4<f32>, 4>,
    _padding_end: array<vec4<f32>, 2>,  // viewport + dof_params (this shader doesn't use them)
};

// Friendly camera structure (no padding, easier to work with)
struct Camera {
    view: mat4x4<f32>,
    proj: mat4x4<f32>,
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    inv_proj: mat4x4<f32>,
    inv_view: mat4x4<f32>,
    position: vec3<f32>,
    frustum_rays: array<vec4<f32>, 4>,
};

// Convert from raw uniform to friendly structure
fn camera_from_raw(raw: CameraRaw) -> Camera {
    var camera: Camera;
    camera.view = raw.view;
    camera.proj = raw.proj;
    camera.view_proj = raw.view_proj;
    camera.inv_view_proj = raw.inv_view_proj;
    camera.inv_proj = raw.inv_proj;
    camera.inv_view = raw.inv_view;
    camera.position = raw.position.xyz;
    camera.frustum_rays = raw.frustum_rays;
    return camera;
}


@vertex
fn vert_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var out: VertexOutput;

    // Generate oversized triangle vertices using bit manipulation
    // Goal: vertex 0→(-1,-1), vertex 1→(3,-1), vertex 2→(-1,3)

    // X coordinate generation:
    // vertex_index: 0 → 0<<1 = 0 → 0&2 = 0 → 0*2-1 = -1 ✓
    // vertex_index: 1 → 1<<1 = 2 → 2&2 = 2 → 2*2-1 = 3  ✓
    // vertex_index: 2 → 2<<1 = 4 → 4&2 = 0 → 0*2-1 = -1 ✓
    let x = f32((vertex_index << 1u) & 2u) * 2.0 - 1.0;

    // Y coordinate generation:
    // vertex_index: 0 → 0&2 = 0 → 0*2-1 = -1 ✓
    // vertex_index: 1 → 1&2 = 0 → 0*2-1 = -1 ✓
    // vertex_index: 2 → 2&2 = 2 → 2*2-1 = 3  ✓
    let y = f32(vertex_index & 2u) * 2.0 - 1.0;

    out.clip_position = vec4<f32>(x, y, 0.0, 1.0);
    out.ndc = vec2<f32>(x, y);

    return out;
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) ndc: vec2<f32>,
}

struct FragmentInput {
    @location(0) ndc: vec2<f32>,
}

struct FragmentOutput {
    @location(0) color: vec4<f32>,
    @builtin(frag_depth) depth: f32,
}

// ===== GRID CONFIGURATION =====
// Blender-like, lines-only on a transparent floor. ONE grid hue: the minor↔major
// hierarchy comes from per-line OPACITY (the continuous LOD weights below), never a
// hue swap — a hue swap on a relative-decade grid would flip every 10th line's colour
// at each LOD step (a visible jump). Axes are coloured.
const GRID_COLOR_LINE: vec3<f32>  = vec3<f32>(0.55, 0.55, 0.55);      // grid lines (gray)
const GRID_COLOR_X_AXIS: vec3<f32> = vec3<f32>(0.90, 0.32, 0.34);     // Red X axis
const GRID_COLOR_Z_AXIS: vec3<f32> = vec3<f32>(0.32, 0.52, 0.92);     // Blue Z axis

// Overall opacity ceiling so lines read as crisp but not pure-solid.
const GRID_MAX_ALPHA: f32 = 1.0;
// The grid is defined by just two on-screen quantities — a line width and a cell size,
// both in PIXELS. Everything else (which world scale to draw, how far the grid reaches,
// where it dissolves into the horizon) is DERIVED from these and the camera, so it
// auto-scales at every zoom/angle and needs no per-scene tuning.
const GRID_LINE_WIDTH_PX: f32 = 0.6;  // line half-width (flat-top + 1px AA → ~2*half+1 px)
const GRID_AXIS_WIDTH_PX: f32 = 1.1;  // axis half-width
const GRID_CELL_PIXELS: f32 = 16.0;   // target on-screen cell size at the view centre
const GRID_BASE_CELL: f32 = 1.0;      // world units the grid is quantised to (powers of ten)
// Push grid fragments slightly farther than coplanar scene geometry to avoid z-fighting.
const GRID_DEPTH_EPSILON: f32 = 1e-4;

fn log10f(x: f32) -> f32 {
    return log(x) * 0.4342944819032518; // 1 / ln(10)
}

// Cumulative coverage at `x` of a periodic line band (half-width `hw`, period 1, bands
// centered on the integers). C(x) = floor(x+hw)*2hw + min(fract(x+hw), 2hw). Shifting by
// +hw puts each band at [k, k+2hw] in the shifted coordinate, so the running integral is
// exactly the number of whole periods times 2hw plus the partial overlap of the current one.
fn band_cumulative(x: f32, hw: f32) -> f32 {
    let xs = x + hw;
    return floor(xs) * (2.0 * hw) + min(fract(xs), 2.0 * hw);
}

// Box-filtered (analytically anti-aliased) coverage in [0,1] of one axis' line set, averaged
// over the pixel footprint. `g` is the cell coordinate, `w` the pixel footprint in cell units,
// `hw` the line half-width in cell units. This is the exact average of the line band over the
// pixel, so it is MOIRÉ-FREE at every density: when the lines resolve (w small) it is a crisp
// AA line of half-width `hw`; when they pack sub-pixel (w large) it converges smoothly to the
// duty cycle 2*hw instead of aliasing. (The decade fade in the caller then dissolves that
// would-be haze, so dense levels vanish rather than greying the floor.)
fn line_coverage_1d(g: f32, w: f32, hw: f32) -> f32 {
    let a = g + 0.5 * w;
    let b = g - 0.5 * w;
    return clamp((band_cumulative(a, hw) - band_cumulative(b, hw)) / w, 0.0, 1.0);
}

// Box-filtered line coverage for one cell size, unioning the X and Z line sets.
//
// `coord` is the world XZ position; `half_px` is the on-screen line HALF-width in pixels;
// `footprint_world` is the ANALYTIC per-pixel world-space footprint of `coord` (|dW/dx_px| +
// |dW/dy_px|, computed in closed form by the caller — see the Jacobian derivation in
// frag_main). We deliberately do NOT use `fwidth(coord)`: `coord` is a nonlinear ray→plane
// reconstruction (world_pos = origin + dir * t, t = -camY/dir.y), so a 2×2-quad finite
// difference of it is noisy at grazing/distance and contaminated by lanes that miss the plane.
// That noise feeds the log10 LOD decade pick, making it flicker pixel-to-pixel → receding rows
// break into dashes. The analytic footprint is exact and smooth, so the pick is stable.
fn grid_coverage(coord: vec2<f32>, cell: f32, half_px: f32, footprint_world: vec2<f32>) -> f32 {
    let gc = coord / cell;
    let fp = max(footprint_world / cell, vec2<f32>(1e-9)); // pixel footprint, cell-units, per axis
    // line half-width in cell units; clamp < 0.5 so neighbouring bands never overlap a period.
    let hw = min(half_px * fp, vec2<f32>(0.49));
    let cx = line_coverage_1d(gc.x, fp.x, hw.x);
    let cz = line_coverage_1d(gc.y, fp.y, hw.y);
    return max(cx, cz);                                    // union of the X and Z line sets
}

// Analytic AA coverage of a single axis line at coordinate 0. `c` is the world coord
// (x for the Z axis, z for the X axis); `fp` its analytic per-pixel footprint. Flat-top
// line of half-width GRID_AXIS_WIDTH_PX px with a 1px soft edge — same footprint source
// as the grid, so it stays a constant pixel width and dissolves with the same fade.
fn axis_coverage(c: f32, fp: f32) -> f32 {
    let d_px = abs(c) / max(fp, 1e-9);
    return clamp(GRID_AXIS_WIDTH_PX - d_px + 0.5, 0.0, 1.0);
}

@fragment
fn frag_main(in: FragmentInput) -> FragmentOutput {
    let ndc = in.ndc;

    // Convert raw camera uniform to friendly structure
    let camera = camera_from_raw(camera_raw);

    // ===== PERSPECTIVE & ORTHOGRAPHIC =====
    // Unproject NDC to world space
    let clip_near = vec4<f32>(ndc.x, ndc.y, 0.0, 1.0);
    var world_near_h = camera.inv_view_proj * clip_near;
    let world_near = world_near_h.xyz / world_near_h.w;

    // Detect camera type
    let is_ortho = camera.proj[3][3] > 0.9;

    var ray_origin: vec3<f32>;
    var ray_dir: vec3<f32>;
    var world_pos: vec3<f32>;
    var t: f32;

    // ===== ANALYTIC SCREEN→PLANE JACOBIAN =====
    // dW_dnx / dW_dny are the derivatives of the world-plane hit position (.xz) with
    // respect to NDC.x / NDC.y, computed in closed form below. Combined with the EXACT
    // per-pixel NDC step `fwidth(ndc)` (ndc is the rasterizer's own linear interpolant,
    // so this is noise-free and unaffected by the horizon), they give a smooth, exact
    // world-space footprint — the thing `fwidth(world_pos.xz)` only approximates badly.
    var dW_dnx: vec2<f32>;  // (d world.x / d ndc.x, d world.z / d ndc.x)
    var dW_dny: vec2<f32>;  // (d world.x / d ndc.y, d world.z / d ndc.y)

    if (is_ortho) {
        // Orthographic: unproject far plane for direction
        let clip_far = vec4<f32>(ndc.x, ndc.y, 1.0, 1.0);
        var world_far_h = camera.inv_view_proj * clip_far;
        let world_far = world_far_h.xyz / world_far_h.w;

        ray_origin = world_near;
        // DON'T normalize - preserves world-space derivative consistency
        ray_dir = world_far - world_near;

        // Intersect with y=0 plane
        t = (0.0 - ray_origin.y) / ray_dir.y;
        world_pos = ray_origin + ray_dir * t;

        // Ortho rays are parallel (ray_dir constant across the screen) and the unproject
        // is affine (w is constant), so the near-point origin is linear in NDC: its NDC
        // derivatives are just the inv_view_proj columns. W.xz = origin.xz - (dir.xz/dir.y)
        // * origin.y, so the chain rule gives:
        let k = ray_dir.xz / ray_dir.y;
        let do_dnx = camera.inv_view_proj[0].xyz; // d(near world)/d ndc.x
        let do_dny = camera.inv_view_proj[1].xyz; // d(near world)/d ndc.y
        dW_dnx = do_dnx.xz - k * do_dnx.y;
        dW_dny = do_dny.xz - k * do_dny.y;
    } else {
        // Perspective: EXACT view-space ray from the projection's tangents. For a standard
        // perspective matrix the view ray through NDC (nx,ny) is (nx/proj00, ny/proj11, -1)
        // — exact and linear in NDC. (The old bilinear lerp of the four normalised corner
        // rays is slightly wrong near the horizon; sky pixels just above it got a faintly
        // negative ray.y, passed the t<0 test, and leaked grid lines into the sky. Exactness
        // here is what makes the horizon a single clean edge.)
        let p00 = camera.proj[0][0];
        let p11 = camera.proj[1][1];
        let view_ray = vec3<f32>(ndc.x / p00, ndc.y / p11, -1.0);

        let r0 = camera.inv_view[0].xyz;  // world-space columns of the view rotation
        let r1 = camera.inv_view[1].xyz;
        let r2 = camera.inv_view[2].xyz;
        let wr = mat3x3<f32>(r0, r1, r2) * view_ray; // world-space ray direction

        ray_origin = camera.position;
        ray_dir = wr;

        // Intersect with y=0 plane
        t = (0.0 - camera.position.y) / wr.y;
        world_pos = camera.position + wr * t;

        // Exact analytic Jacobian: view_ray is linear in NDC, so d(view_ray)/dnx =
        // (1/p00,0,0) and d/dny = (0,1/p11,0). Rotating: dWR/dnx = r0/p00, dWR/dny = r1/p11.
        // Propagate through t = -camY/wr.y and W.xz = cam.xz + wr.xz*t.
        let dWR_dnx = r0 / p00;
        let dWR_dny = r1 / p11;
        let inv_wry = 1.0 / wr.y;
        let dt_dnx = -t * inv_wry * dWR_dnx.y;
        let dt_dny = -t * inv_wry * dWR_dny.y;
        dW_dnx = dWR_dnx.xz * t + wr.xz * dt_dnx;
        dW_dny = dWR_dny.xz * t + wr.xz * dt_dny;
    }

    // World XZ position and its EXACT per-pixel footprint (|dW/dx_px| + |dW/dy_px|).
    // `fwidth(ndc)` is constant and exact (= one pixel in NDC), so no nonlinear noise.
    let coord = world_pos.xz;
    let pix = fwidth(ndc);
    let derivative = max(abs(dW_dnx) * pix.x + abs(dW_dny) * pix.y, vec2<f32>(1e-9));

    // Check for invalid intersections
    let is_parallel = abs(ray_dir.y) < 0.001;

    // For perspective, reject rays pointing away from the ground plane (t < 0)
    // For orthographic, only reject if parallel (no horizon line - grid fills screen like Blender)
    let is_behind = !is_ortho && t < 0.0;

    if (is_parallel || is_behind) {
        discard;
        // // Discard pixels that don't hit the ground
        // var output: FragmentOutput;
        // output.color = vec4<f32>(0.0, 0.0, 0.0, 0.0);
        // output.depth = 1.0; // Far plane
        // return output;
    }

    // ===== ONE GRID SCALE PER FRAME (Godot/Unity style) =====
    // Picking the scale per-PIXEL (from the local footprint) is what made the cell size
    // step up ACROSS the screen toward the horizon ("only major" bands) and made major
    // lines outlive minor. Instead pick a SINGLE world cell size for the whole frame from
    // how far the camera is looking, so the entire view is one uniform grid that recedes
    // and fades by distance. The footprint of the CENTRE pixel (camera-only → constant for
    // the frame) is that reference; the per-pixel footprint is still used for AA + the fade.
    let footprint = max(derivative.x, derivative.y);
    var center_fp: f32;
    if (is_ortho) {
        center_fp = footprint;                          // ortho footprint is already uniform
    } else {
        // World footprint of the centre pixel (ndc = 0): the same analytic Jacobian as
        // above, evaluated down the camera's forward axis.
        let p00 = camera.proj[0][0];
        let p11 = camera.proj[1][1];
        let fwd = -camera.inv_view[2].xyz;
        let cr0 = camera.inv_view[0].xyz;
        let cr1 = camera.inv_view[1].xyz;
        // Clamp the centre ray's downward angle: when the view centre sits near the
        // horizon (zoomed out / grazing) the true distance explodes and would pick an
        // absurdly coarse scale, so bound it to a sane reference distance.
        let ct = abs(camera.position.y) / max(-fwd.y, 0.25);
        let inv_fy = 1.0 / fwd.y;
        let cdx = (cr0 / p00).xz * ct + fwd.xz * (-ct * inv_fy * (cr0.y / p00));
        let cdy = (cr1 / p11).xz * ct + fwd.xz * (-ct * inv_fy * (cr1.y / p11));
        let cfp = abs(cdx) * pix.x + abs(cdy) * pix.y;
        center_fp = max(cfp.x, cfp.y);
    }

    // Three power-of-ten decades bracketing the view-centre scale (~GRID_CELL_PIXELS).
    let scale_l = log10f(max(center_fp, 1e-8) * GRID_CELL_PIXELS / GRID_BASE_CELL);
    let cell0 = GRID_BASE_CELL * pow(10.0, floor(scale_l));
    let cell1 = cell0 * 10.0;
    let cell2 = cell1 * 10.0;

    let cov0 = grid_coverage(coord, cell0, GRID_LINE_WIDTH_PX, derivative);
    let cov1 = grid_coverage(coord, cell1, GRID_LINE_WIDTH_PX, derivative);
    let cov2 = grid_coverage(coord, cell2, GRID_LINE_WIDTH_PX, derivative);
    // Each decade is at FULL strength while its cells are resolvable at the view centre
    // (more than ~a couple px = Nyquist) and fades only as they crowd toward the line
    // width — which, by construction, happens exactly as the scale steps, so the crossfade
    // is pop-free. Weighting by per-decade RESOLVABILITY (not a global `1-lf`) is what keeps
    // the grid DENSE at every zoom: the old linear crossfade dimmed the whole fine decade
    // as you crossed each power-of-ten, which is what made it go sparse / "only major".
    let w0 = smoothstep(2.0, 8.0, cell0 / center_fp);
    let w1 = smoothstep(2.0, 8.0, cell1 / center_fp);
    let w2 = smoothstep(2.0, 8.0, cell2 / center_fp);
    let grid_intensity = max(cov0 * w0, max(cov1 * w1, cov2 * w2));

    var final_color = GRID_COLOR_LINE;
    var final_alpha = grid_intensity;

    // Axes, folded into the same alpha so they fade IDENTICALLY with the grid.
    let z_axis_cov = axis_coverage(coord.x, derivative.x);
    let x_axis_cov = axis_coverage(coord.y, derivative.y);
    final_color = mix(final_color, GRID_COLOR_Z_AXIS, z_axis_cov);
    final_alpha = max(final_alpha, z_axis_cov);
    final_color = mix(final_color, GRID_COLOR_X_AXIS, x_axis_cov);
    final_alpha = max(final_alpha, x_axis_cov);

    final_alpha = final_alpha * GRID_MAX_ALPHA;

    // Depth for occlusion against scene geometry (the pipeline's depth-WRITE is off, so
    // faded/zero-alpha fragments are harmless — no discard needed beyond the no-hit one).
    let view_pos_depth = camera.view * vec4<f32>(world_pos, 1.0);
    let clip_pos_depth = camera.proj * view_pos_depth;
    let ndc_depth = clip_pos_depth.z / clip_pos_depth.w;
    let depth = clamp(ndc_depth + GRID_DEPTH_EPSILON, 0.0, 1.0);

    var output_final: FragmentOutput;
    output_final.color = vec4<f32>(final_color, final_alpha);
    output_final.depth = depth;
    return output_final;
}
