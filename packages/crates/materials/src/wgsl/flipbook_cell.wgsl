// FlipBook CELL MATH — the single source of the sprite-sheet frame/UV
// selection, shared verbatim by BOTH shader families that need it:
//
//   1. the material fragment (`flipbook_material.wgsl`, opaque + transparent
//      shading templates via `build_materials_wgsl()`), and
//   2. the masked (alpha-tested) geometry/shadow variants, which inject this
//      snippet through the template's `flipbook_cell_wgsl` field — they can't
//      include the full material blob (it references opaque-only contract
//      types) but still must agree EXACTLY on which cell is visible, or the
//      cutout/shadow would lag the shaded image.
//
// Scalar API only — no material structs, no texture helpers — so it compiles
// in any template context. Plain WGSL (no askama syntax).

// Mirrors `FlipBookMode` on the Rust side; keep the numbering in sync.
const FLIPBOOK_MODE_LOOP: u32 = 0u;
const FLIPBOOK_MODE_PINGPONG: u32 = 1u;
const FLIPBOOK_MODE_CLAMP: u32 = 2u;
const FLIPBOOK_MODE_ONCE: u32 = 3u;

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

// Map an in-cell UV (the quad's authored UV0) into the atlas-space UV that
// samples the current cell — SCALAR form (no material struct), so masked
// variants can call it after loading the raw uniform words themselves.
//
// `flip_y` controls the row-indexing direction of the atlas (cell 0 at the
// top row vs the bottom row). It does NOT flip the in-cell V — the caller's
// sprite-quad UVs already track the texture's V axis, so each cell renders
// right-side-up either way; only the *row selection* flips.
fn flipbook_cell_uv(
    in_uv: vec2<f32>,
    current_time: f32,
    cols: u32,
    rows: u32,
    frame_count: u32,
    fps: f32,
    time_offset: f32,
    mode: u32,
    flip_y: u32,
) -> vec2<f32> {
    let t = current_time + time_offset;
    let frame_f = t * fps;
    let frame = flipbook_apply_mode(frame_f, frame_count, mode);
    let col = frame % cols;
    let row_raw = frame / cols;
    let row = select(row_raw, rows - 1u - row_raw, flip_y != 0u);
    let cell_size = vec2<f32>(1.0 / f32(cols), 1.0 / f32(rows));
    let cell_origin = vec2<f32>(f32(col), f32(row)) * cell_size;
    return cell_origin + in_uv * cell_size;
}
