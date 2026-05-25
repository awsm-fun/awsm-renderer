// FlipBook material — grid-uniform sprite-sheet animation.
//
// Cell selection is hard (`floor((t + offset) * fps)`); no bilinear
// blending between adjacent cells in v1. Mode selects how the running
// frame index wraps:
//
//   Loop      → `frame % count`
//   PingPong  → `0,1,...,N-1,N-2,...,1,0,1,...` (period `2N - 2`)
//   Clamp     → `min(frame, count - 1)`
//   Once      → like Clamp, but past the end alpha = 0 (lets a
//               transparent-mode flipbook disappear cleanly)
//
// CPU layout — see `FlipBookMaterial::write_uniform_buffer`. Field
// offsets are measured in u32 words, counted from the byte_offset
// argument (which points at the material's shader_id word):
//
//   word 0    shader_id  (skipped via `+ 1u` below)
//   word 1    alpha_mode
//   word 2    alpha_cutoff
//   word 3..7  atlas_tex_info (TextureInfoRaw, 5 u32s)
//   word 8..11 tint (rgba)
//   word 12   cols
//   word 13   rows
//   word 14   frame_count
//   word 15   fps (f32)
//   word 16   time_offset (f32)
//   word 17   mode (u32)
//   word 18   flip_y (u32, 0 or 1)
//
// This file is included verbatim into both the opaque and transparent
// material templates via `build_materials_wgsl()`. It can NOT use
// askama template syntax — only the parent shader's template is
// rendered. Per-pass texture sampling (mip-mode branching, fragment-
// vs-compute-side helpers) happens at the call site.

struct FlipBookMaterialRaw {
    alpha_mode: u32,
    alpha_cutoff: f32,
    atlas_tex_info: TextureInfoRaw,
    tint_r: f32,
    tint_g: f32,
    tint_b: f32,
    tint_a: f32,
    cols: u32,
    rows: u32,
    frame_count: u32,
    fps: f32,
    time_offset: f32,
    mode: u32,
    flip_y: u32,
};

struct FlipBookMaterial {
    alpha_mode: u32,
    alpha_cutoff: f32,
    atlas_tex_info: TextureInfo,
    tint: vec4<f32>,
    cols: u32,
    rows: u32,
    frame_count: u32,
    fps: f32,
    time_offset: f32,
    mode: u32,
    flip_y: u32,
};

// Mirrors `FlipBookMode` on the Rust side; keep these in lockstep.
const FLIPBOOK_MODE_LOOP: u32 = 0u;
const FLIPBOOK_MODE_PINGPONG: u32 = 1u;
const FLIPBOOK_MODE_CLAMP: u32 = 2u;
const FLIPBOOK_MODE_ONCE: u32 = 3u;

fn flipbook_get_material(byte_offset: u32) -> FlipBookMaterial {
    let base_index = (byte_offset / 4u) + 1u; // skip shader id word

    let alpha_mode = material_load_u32(base_index + 0u);
    let alpha_cutoff = material_load_f32(base_index + 1u);

    let atlas_tex_raw = material_load_texture_info_raw(base_index + 2u);
    let tint_r = material_load_f32(base_index + 7u);
    let tint_g = material_load_f32(base_index + 8u);
    let tint_b = material_load_f32(base_index + 9u);
    let tint_a = material_load_f32(base_index + 10u);

    let cols = material_load_u32(base_index + 11u);
    let rows = material_load_u32(base_index + 12u);
    let frame_count = material_load_u32(base_index + 13u);
    let fps = material_load_f32(base_index + 14u);
    let time_offset = material_load_f32(base_index + 15u);
    let mode = material_load_u32(base_index + 16u);
    let flip_y = material_load_u32(base_index + 17u);

    return FlipBookMaterial(
        alpha_mode,
        alpha_cutoff,
        convert_texture_info(atlas_tex_raw),
        vec4<f32>(tint_r, tint_g, tint_b, tint_a),
        cols,
        rows,
        frame_count,
        fps,
        time_offset,
        mode,
        flip_y,
    );
}

// Pick the current frame index for `mode` given the raw running frame
// `frame_f = (time + offset) * fps`. `count` must be >= 1; the
// CPU-side writer clamps so this is safe.
fn flipbook_apply_mode(frame_f: f32, count: u32, mode: u32) -> u32 {
    let safe_frame_f = max(frame_f, 0.0);
    switch mode {
        case 0u: { // Loop
            return u32(safe_frame_f) % count;
        }
        case 1u: { // PingPong
            if count <= 1u {
                return 0u;
            }
            let count_f = f32(count);
            let period = 2.0 * count_f - 2.0;
            let phase = safe_frame_f - floor(safe_frame_f / period) * period;
            // First half is forward; second half mirrors back. `min`
            // clamps the very last step (phase ≈ period) so we never
            // escape [0, count-1].
            let mirrored = select(phase, period - phase, phase >= count_f);
            return min(u32(mirrored), count - 1u);
        }
        case 2u: { // Clamp
            return min(u32(safe_frame_f), count - 1u);
        }
        default: { // Once (alpha=0 past end handled by caller)
            return min(u32(safe_frame_f), count - 1u);
        }
    }
}

// `true` iff `mode == Once` and the running frame has advanced past
// the last cell. Callers use this to force alpha = 0 so a
// transparent-mode flipbook disappears cleanly.
fn flipbook_is_past_end(frame_f: f32, count: u32, mode: u32) -> bool {
    return mode == FLIPBOOK_MODE_ONCE && frame_f >= f32(count);
}

// Map an in-cell UV (the quad's authored UV0) into the atlas-space UV
// that samples the current cell.
//
// `flip_y` controls the row-indexing direction of the atlas (cell 0 at
// the top row vs the bottom row). It does NOT flip the in-cell V — the
// caller's sprite-quad UVs already track the texture's V axis, so each
// cell renders right-side-up either way; only the *row selection*
// flips.
fn flipbook_compute_cell_uv(material: FlipBookMaterial, in_uv: vec2<f32>, current_time: f32) -> vec2<f32> {
    let t = current_time + material.time_offset;
    let frame_f = t * material.fps;
    let frame = flipbook_apply_mode(frame_f, material.frame_count, material.mode);
    let col = frame % material.cols;
    let row_raw = frame / material.cols;
    let row = select(row_raw, material.rows - 1u - row_raw, material.flip_y != 0u);
    let cell_size = vec2<f32>(1.0 / f32(material.cols), 1.0 / f32(material.rows));
    let cell_origin = vec2<f32>(f32(col), f32(row)) * cell_size;
    return cell_origin + in_uv * cell_size;
}

// Convenience: combine sampled-atlas color with tint + alpha-mode
// handling. The call site (templated, mip-mode-aware) sampled the
// atlas at the cell UV and passes the result in here; this helper
// applies the tint, the `Once`-past-end alpha cut, and the
// alpha-mask cutoff (for the opaque path — the transparent path
// `discard`s on cutoff so it handles that itself).

struct FlipBookColor {
    rgb: vec3<f32>,
    a: f32,
};

fn flipbook_finalize_color(
    material: FlipBookMaterial,
    sampled: vec4<f32>,
    current_time: f32,
) -> FlipBookColor {
    var rgb = sampled.rgb * material.tint.rgb;
    var a = sampled.a * material.tint.a;

    let frame_f = (current_time + material.time_offset) * material.fps;
    if flipbook_is_past_end(frame_f, material.frame_count, material.mode) {
        a = 0.0;
    }

    if material.alpha_mode == ALPHA_MODE_MASK && a < material.alpha_cutoff {
        a = 0.0;
    }

    return FlipBookColor(rgb, a);
}
