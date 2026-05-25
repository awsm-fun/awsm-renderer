# Temporal Shaders Implementation Plan

## Instructions for the Implementor

This plan bundles **two related features** that share a common cause (shaders that need access to renderer-wide time):

1. **`FrameGlobals` uniform** — a new renderer-wide uniform exposing `time`, `delta_time` (and a couple of related conveniences) to every shader. Infrastructure.
2. **`FlipBook` material** — a new first-party material implementing grid-uniform sprite-sheet animation, built on top of `FrameGlobals`. The first consumer of the new uniform.

They are bundled because the flipbook is the load-bearing first consumer of the time uniform — building both together forces the uniform's surface to be exercised by a real material before it's documented as a stable public surface. The plan is meant to be followed **start to finish** in a single sustained effort.

- **Commit frequently** at every natural checkpoint. Small commits make `git bisect` cheap when something regresses. Don't squash as you go.
- **Breaking changes are fine** mid-plan. The frame-globals binding will get a new slot in shared bind groups; if that collides with anything, just rework the binding layout.
- **Update the tracking section at the bottom** as you go.
- **Only after EVERYTHING below has landed and visually verified**, run:
  ```
  cargo fmt
  cargo clippy --workspace --all-targets
  ```
  Fix everything clippy turns up. Then the branch is ready to push.

### How to test

The primary verification surface is the **scene-editor** in a browser. Start it with:

```
task scene-editor:dev
# served at http://localhost:9081
```

Use the `preview_start` / `preview_screenshot` / `preview_snapshot` tools to drive the page in a Chromium preview.

The test scene lives at `/Users/dakom/Documents/DAKOM/awsm-renderer-assets/world/project.json`. Extend it with:

- A quad with a `FlipBook` material referencing a sprite-sheet asset (an explosion / fire / smoke sheet — 4×4 grid is a good canonical test case, 16 frames at 24 fps).
- A second quad with the same material but a different `time_offset` to confirm per-instance phasing works.
- A third quad with `mode = PingPong` to confirm playback modes.

When testing, focus on:

1. **Golden path**: the flipbook plays smoothly, loops cleanly, no GPU validation errors.
2. **Time monotonicity**: `time` increases continuously from app start; no jumps, no resets across frames.
3. **Delta-time correctness**: at 60 Hz, `delta_time` ≈ 0.0167. At 30 Hz (force a slow render), it ≈ 0.033. Verify with a debug uniform readout or a logged value.
4. **Modes**: loop / pingpong / clamp / play-once each behave correctly. Pause the renderer mid-play-once and verify it sits on the final frame.
5. **Time offset**: two flipbook instances on the same asset with `time_offset` 0 and 0.5 second show different cells on the same frame.
6. **`alpha_mode = Blend`** with a sprite sheet that has transparent channels — verify a smoke sheet feathers correctly against the background.
7. **`alpha_mode = Mask`** with a cutoff sheet — verify hard edges.

### Acceptance: shader-side time visibility

After Phase 1, every shader the renderer compiles (PBR, Unlit, Toon, transmissive pass, transparent pass, particles, debug visualizers) has `frame_globals` in scope and can read `frame_globals.time` if it wants. No shader is required to use it — but if any first-party shader wants to add a time-driven effect later (animated dissolve, scrolling UV, etc.), the binding is already there.

### Acceptance: CPU-side time visibility

CPU-side subsystems also benefit. The editor's particle bridge
([`crates/frontend/scene-editor/src/renderer_bridge/particles_sync.rs`](../../crates/frontend/scene-editor/src/renderer_bridge/particles_sync.rs))
currently maintains its own `last_ts_ms` per emitter runtime and
clamps a per-frame `dt: f32` to `[0.0, 0.1]` before feeding
`Simulator::tick`. Once `FrameGlobalsSnapshot` is the canonical
source of `delta_time`, the bridge can read it via
`AwsmRenderer::frame_globals().delta_time` instead of computing
its own — and pause / time-scale / replay automatically flow into
particle simulation via `set_time_source`. Phase 5 ("Edge cases +
polish") includes migrating that bridge as a smoke target for the
public-API surface; any consumer that today rolls its own
`performance.now()` delta should do the same after `FrameGlobals`
lands.

---

## High-Level Direction

### Why a dedicated `FrameGlobals` uniform

The renderer carries some "per-frame" data on the `Camera` uniform today (`frame_count`, `viewport_size`). Adding `time` and `delta_time` there would be expedient but semantically wrong — these aren't camera properties, they're renderer-wide state. A dedicated `FrameGlobals` uniform:

- Makes the surface discoverable (a shader author looking for "renderer time" finds `frame_globals`, not `camera`).
- Leaves room for future renderer-wide globals (ambient color, wind direction, simulation seed) without bloating the Camera struct.
- Keeps Camera focused on view / projection / unprojection math.
- Survives multi-camera scenarios cleanly: shadow passes render from a "light camera", post-effects may use side cameras. `FrameGlobals` rides through all of them unchanged; Camera changes per pass.

**`Camera.frame_count` is removed entirely** in this plan and lives only on `FrameGlobals`. A grep at the time of writing confirms that no WGSL code actually reads `camera.frame_count` — it's declared in the friendly Camera struct but unused on the GPU side; the CPU's source of truth is already `render_textures.frame_count()`. Removing the dead field is less churn than maintaining duplication.

**`Camera.viewport_size` stays on Camera.** It's used by camera-specific math (frustum-ray reconstruction at `shared_wgsl/camera.wgsl`) and is genuinely per-camera in split-screen / picture-in-picture scenarios. `FrameGlobals.resolution` is a separate concept — the renderer's output resolution, relevant to post-FX and screen-space effects that don't care which camera produced the frame. Different consumer, different value.

### The `FrameGlobals` surface

```wgsl
struct FrameGlobals {
    time: f32,          // seconds since renderer construction (monotonic)
    delta_time: f32,    // seconds since previous render() call (clamped, see below)
    frame_count: u32,   // monotonic frame counter (canonical home; Camera no longer carries it)
    _pad: u32,          // align to 16 bytes
    resolution: vec2<u32>,
    _pad2: vec2<u32>,
}
```

Total: 32 bytes. Single uniform binding. Bound once per frame, visible to every shader that includes `shared_wgsl/frame_globals.wgsl` (which the existing shader-composition mechanism will pull into every pass that already pulls camera).

**Time origin**: seconds since `AwsmRenderer::new()` returned. Not since page load — distinguishing those matters for embedded renderers that boot lazily.

**`delta_time` semantics**:
- First frame after construction: `delta_time = 0.0` (no prior frame to subtract from). Shaders dividing by `delta_time` must guard against zero — same discipline as anywhere else.
- Subsequent frames: `delta_time = (time - last_time).min(0.25)`. Upper-clamped at 0.25s to keep per-frame integrators stable after the tab is backgrounded for minutes.
- **No lower clamp.** If a consumer calls `set_time_source(t)` with the same `t` twice (e.g., paused gameplay), `delta_time` is exactly `0.0`. Simulations correctly stop advancing. The clamp is one-sided on purpose.

**`time` vs. `delta_time` on resume**: `time` is wall-clock — if the tab was backgrounded for 60s, `time` jumps forward by ~60s on the next frame even though `delta_time` reports 0.25. This is deliberate: animations driven by `sin(time * f)` reflect real elapsed seconds (correct for time-of-day, day-night cycles), while integrators driven by `position += velocity * delta_time` get a stable rate that doesn't explode. Document this difference so authors aren't surprised when `time` discontinuities outpace `delta_time`.

**`f32` precision on `time`**: the CPU tracks elapsed milliseconds in `f64` (matching `Performance.now()`'s native precision) and converts to `f32` seconds at upload time. The uniform stays `f32` for shader simplicity. At 1ms resolution, f32 precision degrades visibly after ~16 hours of continuous run. For game sessions this is fine. For installation-art use cases, a future enhancement would split into `(time_hi: f32, time_lo: f32)` and have shaders reconstruct double-precision via the standard trick. **Not in scope here** — document the limitation, move on.

### Why a first-party `FlipBook` material

Sprite-sheet animation is one of the most-asked-for material features in any renderer. The grid-uniform variant (regular N×M cell grid, sequential playback) covers ~80% of real use cases (VFX sheets, UI element animations, simple sprite animations). It's small (~10 uniforms), self-contained, and stable — exactly the kind of material that earns first-party shipping rather than living in a games-specific extension.

The **irregular-atlas** variant (cells of arbitrary positions and sizes, like TexturePacker output) is deliberately out of scope. That's the canonical demo case for dynamic materials + structured buffers in the follow-up dynamic-materials plan, and trying to do it as a first-party material would force the schema to grow a variable-length cell-table field that doesn't fit the rest of first-party.

### What FlipBook looks like at the call site

```rust
let mut flipbook = FlipBookMaterial::new(MaterialAlphaMode::Blend, /* double_sided */ false);
flipbook.atlas_tex = Some(MaterialTexture { texture: smoke_sheet, sampler: default_sampler, .. });
flipbook.cols = 4;
flipbook.rows = 4;
flipbook.frame_count = 13;    // 13 of the 16 cells used
flipbook.fps = 24.0;
flipbook.mode = FlipBookMode::Loop;
flipbook.time_offset = 0.0;
flipbook.tint = [1.0, 0.95, 0.9, 1.0];
material_storage.insert(Material::FlipBook(Box::new(flipbook)));
```

Indistinguishable in shape from how `UnlitMaterial` is used today. Same trait, same registration path, same Cargo feature gate.

### What FlipBook does NOT do (non-goals)

- **Irregular cell layouts.** Grid only. Anyone needing irregular cells uses a dynamic material once that path lands.
- **Bilinear blending between frames.** Cell selection is hard — frame `floor(t * fps)`. Smooth blending (sampling two adjacent cells and interpolating between them by fract) is a follow-up — easy to add later as a `bool blend_frames` toggle, but not in v1.
- **Multiple animation tracks per material** (e.g., "fire body" + "fire sparkles" in one material reading two different atlases). One atlas per FlipBookMaterial. Anyone needing two stacks two meshes with two materials.
- **Event callbacks at specific frames.** This is a scripting concern, not a rendering one.
- **GPU-driven random per-instance variation** (e.g., randomize start frame per instance based on instance_id). Possible via `time_offset` if the caller picks a random value at mesh-instance time, but no built-in randomizer in v1.

---

## Schema Changes

### `crates/scene-schema/src/material.rs`

Add a variant to `MaterialRef`:

```rust
pub enum MaterialRef {
    Pbr(PbrMaterialDef),
    Unlit(UnlitMaterialDef),
    Toon(ToonMaterialDef),
    FlipBook(FlipBookMaterialDef),     // new
    // … and Custom once dynamic-materials lands
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FlipBookMaterialDef {
    #[serde(default)]
    pub alpha_mode: MaterialAlphaMode,
    #[serde(default)]
    pub double_sided: bool,
    pub atlas_tex: Option<TextureRef>,
    #[serde(default = "default_tint")]
    pub tint: [f32; 4],                // multiplier on the sampled atlas color
    pub cols: u32,
    pub rows: u32,
    pub frame_count: u32,              // actual frames used; <= cols * rows
    #[serde(default = "default_fps")]
    pub fps: f32,
    #[serde(default)]
    pub time_offset: f32,              // per-instance phase offset (seconds)
    #[serde(default)]
    pub mode: FlipBookMode,
    #[serde(default)]
    pub flip_y: bool,                  // atlas cells indexed top-to-bottom vs bottom-to-top
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FlipBookMode {
    #[default]
    Loop,        // wrap on frame_count
    PingPong,    // forward then reverse
    Clamp,       // stop on last frame
    Once,        // play once, then transparent (alpha = 0) after the last frame
}
```

All defaults use `#[serde(default)]` so existing projects load without change.

`FrameGlobals` has **no** on-disk schema representation — it's renderer-internal state derived from wall-clock time and the frame loop.

---

## Public API Surface

### `awsm_renderer::frame_globals`

```rust
/// Renderer-wide per-frame state, exposed to shaders via the
/// `frame_globals` uniform. Values are updated once per `render()` call.
///
/// Most consumers do not interact with this struct directly — read shader
/// access is the surface that matters. The CPU-side struct is exposed
/// primarily so consumers can read back the current values (e.g., to
/// synchronize gameplay logic with renderer time).
#[derive(Clone, Copy, Debug)]
pub struct FrameGlobalsSnapshot {
    pub time: f32,
    pub delta_time: f32,
    pub frame_count: u32,
    pub resolution: [u32; 2],
}

impl AwsmRenderer {
    /// Returns the current frame-globals values, valid for the duration
    /// of this frame (between `render()` calls).
    pub fn frame_globals(&self) -> FrameGlobalsSnapshot;

    /// Overrides the time source. By default the renderer uses
    /// `Performance.now()` from the browser; consumers running their
    /// own game-time clock (paused, time-scaled, replayed) call this
    /// before `render()` to inject the value the next frame's
    /// `frame_globals.time` will see.
    ///
    /// `delta_time` is computed from successive `time` values regardless
    /// of source.
    pub fn set_time_source(&mut self, time: f32);
}
```

The `set_time_source` method matters for: paused gameplay (caller passes the same `time` repeatedly → `delta_time` becomes 0), bullet-time effects (caller passes scaled time), replay systems (caller passes recorded values). Without this hook, the renderer's wall clock can't be controlled and these use cases break.

### `awsm_renderer_materials::flipbook`

```rust
/// Sprite-sheet flipbook material. Grid-uniform atlas; samples a cell
/// per frame based on `frame_globals.time`, `time_offset`, `fps`, and `mode`.
#[derive(Clone, Debug)]
pub struct FlipBookMaterial {
    pub atlas_tex: Option<MaterialTexture>,
    pub tint: [f32; 4],
    pub cols: u32,
    pub rows: u32,
    pub frame_count: u32,
    pub fps: f32,
    pub time_offset: f32,
    pub mode: FlipBookMode,
    pub flip_y: bool,
    alpha_mode: MaterialAlphaMode,
    double_sided: bool,
}

impl FlipBookMaterial {
    pub fn new(alpha_mode: MaterialAlphaMode, double_sided: bool) -> Self;
    pub fn alpha_mode(&self) -> &MaterialAlphaMode;
    pub fn double_sided(&self) -> bool;
}

impl MaterialShader for FlipBookMaterial { /* shader_id = FLIPBOOK, etc. */ }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlipBookMode { Loop, PingPong, Clamp, Once }
```

Shader-id value: **4** (next free slot after `Pbr = 1`, `Unlit = 2`, `Toon = 3`). The exact Rust form depends on whether the dynamic-materials plan has landed when temporal-shaders is implemented:
- Pre-dynamic-materials: `MaterialShaderId::FlipBook = 4` as a new enum variant in `crates/materials/src/shader_id.rs`.
- Post-dynamic-materials: `pub const FLIPBOOK: Self = Self(4);` alongside the other first-party constants (`PBR` / `UNLIT` / `TOON`); first-party occupies 1–9999 per that plan's partitioning.

Gated behind a `flipbook` Cargo feature in `awsm-renderer-materials`; added to workspace default features so it's enabled out-of-the-box.

### Documentation requirements

- Every public type, field, and method gets a rustdoc comment.
- `cargo doc --workspace --no-deps` produces no warnings.
- The contract docs (when the dynamic-materials plan lands them) include `frame_globals` as part of the always-in-scope helper list.

---

## Renderer Changes

### New: `frame_globals` subsystem

```
crates/renderer/src/frame_globals/
├── mod.rs              ← FrameGlobals struct, GPU buffer, write_gpu
└── snapshot.rs         ← FrameGlobalsSnapshot, time-source override
```

`FrameGlobals` owns:
- A `web_sys::GpuBuffer` of 32 bytes (uniform-mode, `MAP_WRITE | COPY_DST | UNIFORM`).
- A `MappedUploader` companion — required by [`PERFORMANCE.md §5b`](../PERFORMANCE.md): every per-frame `queue.writeBuffer` site in the renderer crate routes through `MappedUploader` (the camera uniform, also 64 bytes, is the precedent — see [`crates/renderer/src/camera.rs::write_gpu`](../../crates/renderer/src/camera.rs)). Default ring depth (3) is right for a 32-byte uniform.
- A `Vec<u8>` shadow buffer for the dirty-range pack (mirrors the `raw_data` slice the camera write uses).
- `construction_ms: f64` — the `Performance.now()` reading captured at `AwsmRenderer::new()`. Kept in `f64` to preserve millisecond precision over long sessions.
- `last_time: Option<f32>` for delta computation (after conversion to `f32` seconds).
- `time_override: Option<f32>` set via `set_time_source`.

`FrameGlobals::write_gpu`:
1. Compute `time: f32` — `time_override.unwrap_or_else(|| ((performance_now_ms() - self.construction_ms) / 1000.0) as f32)`.
2. Compute `delta_time: f32`:
   - If `last_time.is_none()` (first frame): `0.0`.
   - Else: `(time - last_time.unwrap()).min(0.25)`. Upper-clamp only.
3. Update `last_time = Some(time)`.
4. Pack into the 32-byte uniform layout (struct above) inside the shadow `Vec<u8>`.
5. Call `MappedUploader::write_dirty_ranges(gpu, &gpu_buffer, BYTE_SIZE, &raw_data, &[(0, BYTE_SIZE)])` — the entire payload is "dirty" every frame since `time` always advances.

Called from `render.rs::render()`, sequenced **before** anything that might read it. Anywhere in the existing CPU→GPU upload batch is fine; the existing block between `self.transforms.write_gpu(...)` and `self.materials.write_gpu(...)` is a reasonable home.

### Shared bind group + WGSL helper

`shared_wgsl/frame_globals.wgsl`:

```wgsl
struct FrameGlobalsRaw {
    time: f32,
    delta_time: f32,
    frame_count: u32,
    _pad: u32,
    resolution: vec2<u32>,
    _pad2: vec2<u32>,
}

struct FrameGlobals {
    time: f32,
    delta_time: f32,
    frame_count: u32,
    resolution: vec2<u32>,
}

fn frame_globals_from_raw(raw: FrameGlobalsRaw) -> FrameGlobals {
    return FrameGlobals(raw.time, raw.delta_time, raw.frame_count, raw.resolution);
}

@group({{ frame_globals_group }}) @binding({{ frame_globals_binding }})
var<uniform> frame_globals_raw: FrameGlobalsRaw;
```

Then in every pass that needs it, the template renders `frame_globals.wgsl` plus a one-line:
```wgsl
let frame_globals = frame_globals_from_raw(frame_globals_raw);
```
at the top of `main` (or wherever non-uniform access is wanted). Matches the existing Camera convention exactly.

The `{{ frame_globals_group }}` / `{{ frame_globals_binding }}` askama vars are resolved per pass — frame globals piggyback on whichever bind group already carries the camera uniform in each pass, since their lifetimes are identical (one per frame, written by the renderer, read everywhere).

### Binding strategy

Two reasonable options:

**Option A: Extend the existing "globals" bind group that carries Camera.** Find the bind group Camera currently lives on in each pass; add `frame_globals_raw` as the next binding. Pro: zero new bind-group plumbing. Con: each pass's bind-group layout grows by one entry, which means a bind-group recreate when this lands.

**Option B: New dedicated `FrameGlobals` bind group at a low group index.** Pro: cleaner separation, easy to add/remove. Con: every pipeline layout grows; every pass needs to bind it.

**Choose A.** Camera is already universally bound in every pass that does material shading, animation, particles, post-processing, debug viz. Riding alongside it is the path of least resistance and lowest performance cost (one less bind-group switch per pass).

### Flipbook material — render-pass integration

Same path every existing first-party material follows:

1. New variant `Material::FlipBook(Box<FlipBookMaterial>)`.
2. Feature `flipbook` in `awsm-renderer-materials/Cargo.toml`.
3. `enabled_materials()` appends a `MaterialEntry { shader_id: FLIPBOOK, wgsl_fragment: include_str!("wgsl/flipbook_material.wgsl"), name: "flipbook" }` behind `#[cfg(feature = "flipbook")]`.
4. The opaque + transparent dispatch chains gain a `FLIPBOOK` branch (the existing template machinery handles this automatically once the registry entry is added).

### Material classify — bucket extension

Adding a new first-party shader_id is **not** purely a registry
change since the material classify + indirect dispatch landed (see
[`PERFORMANCE.md §1`](../PERFORMANCE.md) frame diagram and
[`crates/renderer/src/render_passes/material_classify/`](../../crates/renderer/src/render_passes/material_classify/)).
The classify shader scans the visibility buffer per-tile and routes
each pixel into a per-shader_id bucket using a hard-coded bitmask;
the buckets feed `dispatchWorkgroupsIndirect` so each opaque
compute pipeline runs only over tiles its shader_id touches.

Today's wiring (PBR / Unlit / Toon) is hard-coded:

- [`material_classify/buffers.rs`](../../crates/renderer/src/render_passes/material_classify/buffers.rs):
  `pub const BUCKET_COUNT: u32 = 3;`.
- [`material_classify/shader/material_classify_wgsl/compute.wgsl`](../../crates/renderer/src/render_passes/material_classify/shader/material_classify_wgsl/compute.wgsl):
  hard-coded `BUCKET_BIT_PBR/UNLIT/TOON` consts, an if-else chain
  mapping each `shader_id` to its bit, and a per-bucket extract
  block that emits `dispatchWorkgroupsIndirect` args.

Adding FlipBook requires:

1. Bump `BUCKET_COUNT: 3 → 4` in `buffers.rs`.
2. Add `BUCKET_BIT_FLIPBOOK: u32 = 8u;` to `compute.wgsl`. Extend
   the `if shader_id == SHADER_ID_PBR { … } else if …` chain to
   route flipbook pixels into the new bit.
3. Extend the per-bucket extract block (around lines 89-103 of
   `compute.wgsl`) so the `(mask & BUCKET_BIT_FLIPBOOK) != 0u`
   case writes its dispatch-args slot.
4. The pipeline-launch loop in
   [`material_opaque/render_pass.rs`](../../crates/renderer/src/render_passes/material_opaque/render_pass.rs)
   iterates per shader_id today — verify it picks up the new
   pipeline from `enabled_materials()` without additional wiring
   (the `{% match shader_id %}` askama choice in the opaque
   compute template already handles per-pipeline emission once
   `MaterialShaderId::FLIPBOOK` is in the registry).

Same shape for the transparent path's per-shader_id dispatch chain
in `material_transparent` if FlipBook is registered as a Blend
material.

This intersects with the [dynamic-materials plan](./dynamic-materials.md)'s
"Storage budget watch" note (the opaque main bind group is near
its 10/10 storage-binding cap). FlipBook itself adds no new
storage bindings — it uses the existing per-material storage
pool — so the budget is unaffected by this plan; only
dynamic-materials' `extras_pool` does.

### Pipeline pre-warm — first-visible-frame stutter

Adding FlipBook to `enabled_materials()` changes the opaque +
transparent compute kernels' WGSL source, which busts the
browser's compiled-PSO cache for those pipelines (see
[`PERFORMANCE.md §5g`](../PERFORMANCE.md)). The editor's
`prewarm_pipelines()` call (already wired in
[`crates/frontend/scene-editor/src/main.rs`](../../crates/frontend/scene-editor/src/main.rs))
walks every active `(shader_id × variant)` combination to compile
each pipeline before first frame — but **only if FlipBook is in
`enabled_materials()` at warmup time**. Phase 2 (which adds
FlipBook to `enabled_materials()` behind `#[cfg(feature = "flipbook")]`)
must run with the feature enabled for `prewarm_pipelines()` to
include it; the default-feature wiring (Phase 2 step 5) ensures
this on the editor's default build. A consumer who opts out via
`default-features = false, features = []` won't see FlipBook in
their warmup walk — that's correct (they're also not paying for
its pipeline compile).

The flipbook WGSL fragment calls `frame_globals.time` to compute the current cell, then samples the atlas at the cell's UV. Sketch:

```wgsl
fn flipbook_compute_cell_uv(material: FlipBookMaterial, in_uv: vec2<f32>, current_time: f32) -> vec2<f32> {
    let t = current_time + material.time_offset;
    let frame_f = t * material.fps;
    let frame = flipbook_apply_mode(frame_f, material.frame_count, material.mode);
    let col = frame % material.cols;
    let row = frame / material.cols;
    let cell_size = vec2<f32>(1.0 / f32(material.cols), 1.0 / f32(material.rows));
    let cell_origin = vec2<f32>(f32(col), f32(row)) * cell_size;
    let v = select(in_uv.y, 1.0 - in_uv.y, material.flip_y);
    return cell_origin + vec2<f32>(in_uv.x, v) * cell_size;
}

fn flipbook_apply_mode(frame_f: f32, count: u32, mode: u32) -> u32 {
    let count_f = f32(count);
    switch mode {
        case 0u { return u32(frame_f) % count; }                                  // Loop
        case 1u {                                                                  // PingPong
            let period = 2.0 * count_f - 2.0;
            let phase = frame_f - floor(frame_f / period) * period;
            return u32(select(phase, period - phase, phase >= count_f));
        }
        case 2u { return min(u32(frame_f), count - 1u); }                          // Clamp
        default { return min(u32(frame_f), count - 1u); }                          // Once (alpha=0 handled elsewhere)
    }
}
```

For `mode == Once`, the WGSL additionally discards (or zeros alpha) when `frame_f >= f32(count)`. That gives the "play once and disappear" behavior.

---

## Implementation Phases

Each phase is a runnable checkpoint — commit after each.

### Phase 0 — `FrameGlobals` scaffolding + Camera cleanup

**FrameGlobals (CPU-side only — no shader binding yet):**

1. Create `crates/renderer/src/frame_globals/mod.rs` with `pub struct FrameGlobals` carrying the GPU buffer, a `MappedUploader` companion (default ring depth 3), a `Vec<u8>` shadow buffer, `construction_ms: f64`, `last_time: Option<f32>`, and `time_override: Option<f32>`. Capture `construction_ms` from `Performance.now()` at `AwsmRenderer::new()`. Mirror the camera's pattern in [`crates/renderer/src/camera.rs`](../../crates/renderer/src/camera.rs) — it's the canonical "small per-frame uniform via MappedUploader" precedent.
2. Add `pub frame_globals: FrameGlobals` to `AwsmRenderer`.
3. Implement `write_gpu` per the spec in **Renderer Changes**: `time` is f32 seconds derived from the f64 ms tracker; `delta_time` is upper-clamped at 0.25, allowing 0.0; first frame returns 0.0 delta. Routes through `MappedUploader::write_dirty_ranges`, not `queue.writeBuffer` — `PERFORMANCE.md §5b` is load-bearing here.
4. Implement `FrameGlobalsSnapshot` + `AwsmRenderer::frame_globals()` + `set_time_source()`.
5. Wire `write_gpu` into `render::render()` in the existing CPU→GPU upload batch.
6. **Verify clamping behavior**: hand-test `set_time_source(0.0)` for 3 consecutive frames; first delta is 0.0 (no prior frame), second and third deltas are 0.0 (time didn't advance). Confirm in a unit test or a printf-style probe; remove the probe before commit.

**Camera cleanup (remove the dead `frame_count` passenger):**

7. Remove `frame_count_and_padding: vec4<u32>` from the `CameraRaw` struct in `shared_wgsl/camera.wgsl`. Adjust the Rust-side layout in `crates/renderer/src/camera.rs` accordingly (the comment block enumerating field offsets needs updating, the `write_gpu` step that packs `render_textures.frame_count()` is removed).
8. Remove `frame_count: u32` from the friendly `Camera` struct in `shared_wgsl/camera.wgsl`, and the corresponding `camera.frame_count = raw.frame_count_and_padding.x;` assignment in `camera_from_raw`.
9. Repeat for any shader file that redeclares the Camera layout: at the time of writing, that's `crates/editor/src/grid/shaders/grid.wgsl` and `crates/renderer/src/render_passes/lines/shader/line_wgsl/line.wgsl`. Grep `frame_count_and_padding` once at start of phase to confirm the complete set.
10. The TAA jitter computation in `camera.rs` already reads `render_textures.frame_count()` directly — no change needed; the field was redundant on the GPU.
11. **Verify**: `cargo build --workspace` succeeds; `grep -r 'camera\.frame_count\|frame_count_and_padding' crates/` returns no matches.

Expected outcome: renderer compiles, runs, the new buffer is updated each frame, Camera is slimmer by 16 bytes. No shader behavior changes (no shader read `camera.frame_count` to begin with). Commit.

### Phase 1 — `frame_globals.wgsl` + universal binding

1. Add `shared_wgsl/frame_globals.wgsl` with the Raw + friendly struct definitions and the `@group/@binding` declaration. Use askama vars (`{{ frame_globals_group }}` / `{{ frame_globals_binding }}`) so each pass can position the binding alongside its existing Camera binding.
2. Decide which existing bind group carries Camera in each pass (it may vary by pass). For each such bind group, add `frame_globals_raw` as the next binding entry. Update the corresponding bind-group layout in Rust. The bind-group recreate triggered by this layout change is a deliberate breaking change.
3. Update the WGSL templates for every pass that uses Camera to also include `frame_globals.wgsl` and bind it at the assigned slot.
4. Add `let frame_globals = frame_globals_from_raw(frame_globals_raw);` at the appropriate entry point in each pass so the friendly struct is available to all helper functions in the pass.
5. **Audit**: grep for every `@group`/`@binding` in the shared shaders and verify no collision with the slot picked. Compare against `maxBindingsPerBindGroup` (commonly 1000+) and `maxStorageBuffersPerShaderStage` / `maxUniformBuffersPerShaderStage` adapter limits — frame_globals is a uniform binding so it draws from the uniform pool, not the storage pool the shadows plan exhausted.
6. **Verify by debug-tint**: temporarily replace some constant in an existing material's WGSL (e.g., Unlit's base_color) with `* (sin(frame_globals.time) * 0.5 + 0.5)`. Confirm the tint pulses at 1 Hz in the browser preview. Remove the debug line before committing.
7. **Update related plans**: if the dynamic-materials plan ([docs/plans/dynamic-materials.md](./dynamic-materials.md)) is in progress, add `frame_globals` to its always-in-scope helpers list (both `contract-opaque.md` and `contract-transparent.md`). If that plan hasn't started yet, this is a no-op (its Phase 1 audit will pick up `frame_globals` automatically).

Expected outcome: every shader has `frame_globals` in scope and can read it without further plumbing. Scene renders identically (no production shader uses it yet). Commit.

### Phase 2 — `FlipBookMaterial` Rust struct

1. New file `crates/materials/src/flipbook.rs` with `FlipBookMaterial`, `FlipBookMode`, `new()`, accessors. Pattern-match the shape of `unlit.rs` closely.
2. `WGSL_FRAGMENT` const pointing at `wgsl/flipbook_material.wgsl` (created in Phase 3).
3. `impl MaterialShader for FlipBookMaterial`:
   - `shader_id()` returns the new `FLIPBOOK` id (see step 4 — exact form depends on `MaterialShaderId`'s state at implementation time).
   - `alpha_mode()`, `is_transparency_pass()` mirroring Unlit
   - `write_uniform_buffer` packs: `shader_id`, `alpha_mode`, `alpha_cutoff`, `atlas_tex_info`, `tint(4)`, `cols`, `rows`, `frame_count`, `fps`, `time_offset`, `mode` (as u32), `flip_y` (as u32). Use the existing `write` helper.
4. Add the `FLIPBOOK` shader-id in `crates/materials/src/shader_id.rs`. **Two cases depending on whether the dynamic-materials plan has landed yet:**
   - **If `MaterialShaderId` is still the closed enum** (`enum { Pbr = 1, Unlit = 2, Toon = 3 }`): add a `FlipBook = 4` variant. Pattern-match sites that exhaustively destructure `MaterialShaderId` need a `FlipBook` arm.
   - **If `MaterialShaderId` has been rewritten** to the `#[repr(transparent)] struct(u32)` form (per the dynamic-materials plan's Phase 0): add `pub const FLIPBOOK: Self = Self(4);` alongside `PBR` / `UNLIT` / `TOON`. No pattern-match sites change.
   Either form lands at the same wire-level shader_id value (4); promotion path is mechanical if the system transitions later.
5. Cargo feature `flipbook` in `awsm-renderer-materials/Cargo.toml`. Add `flipbook` to the workspace's default features so the material ships enabled by default (matches how `pbr-standard`, `unlit`, `toon` are handled today — check `Cargo.toml` to confirm convention and follow it).
6. Append to `enabled_materials()` behind `#[cfg(feature = "flipbook")]`.
7. Add `Material::FlipBook(Box<FlipBookMaterial>)` variant. Update every `Material` pattern-match to handle it.

Expected outcome: code compiles with `--features flipbook`. No shader yet. Commit.

### Phase 3 — `flipbook_material.wgsl`

1. New file `crates/materials/src/wgsl/flipbook_material.wgsl`.
2. Define `FlipBookMaterialRaw` + `FlipBookMaterial` (friendly) WGSL structs matching the byte layout defined by `write_uniform_buffer` in Phase 2.
3. Implement `flipbook_get_material(byte_offset) -> FlipBookMaterial` (mirrors `unlit_get_material`).
4. Implement `flipbook_compute_cell_uv(...)` and `flipbook_apply_mode(...)` per the sketch in **Renderer Changes**.
5. Implement the per-pass entry function the dispatch chain calls — naming mirrors how Unlit's fragment is structured. Sample the atlas at the computed cell UV, multiply by `material.tint`, return the result for the appropriate pass.
6. For `mode == Once` past the end: alpha = 0 (lets transparent-mode flipbooks disappear cleanly; opaque-mode ones freeze on the last frame which is the more useful semantic, since `Once` + Opaque is essentially undefined).

Expected outcome: a hand-built FlipBookMaterial registered against a test mesh shows the first cell of its atlas. Commit.

### Phase 4 — Test scene + visual verification

1. Add a **numbered 4×4 debug sheet** (cells labeled 0–15) to `awsm-renderer-assets/`. This is the load-bearing test asset — every visual verification step below uses it to read off cell indices directly. An artistic asset (smoke / explosion / etc.) is optional polish and not required for sign-off.
2. Three test quads in the scene:
   - Default loop, `time_offset = 0.0`
   - PingPong, `time_offset = 0.0`
   - Loop, `time_offset = 0.5` (offset by half a second relative to the first quad)
3. Verify in the browser:
   - First and third quads should be at different cells at any given moment (visible because the labeled cells make the phase explicit).
   - PingPong quad should reverse direction at the end of the sequence.
   - Loop cell sequence should be 0, 1, 2, …, 15, 0, 1, … (counting visible on the debug sheet).
4. Switch alpha modes (Opaque, Mask, Blend) on one of the quads; verify each renders correctly. For Mask, use a sheet whose cells have transparent corners to confirm the cutoff path.

Expected outcome: visible, correct flipbook animation in the scene editor. Commit.

### Phase 5 — Edge cases + polish

0. **Migrate the editor's particle bridge to `FrameGlobalsSnapshot`.**
   [`crates/frontend/scene-editor/src/renderer_bridge/particles_sync.rs`](../../crates/frontend/scene-editor/src/renderer_bridge/particles_sync.rs)
   currently keeps its own `last_ts_ms` per runtime and computes a
   clamped `dt: f32` before calling `Simulator::tick`. Replace
   that math with `renderer.frame_globals().delta_time`. Pause /
   time-scale / replay (the use cases `set_time_source` exists
   for) automatically flow to particles after this change — that
   is the load-bearing first non-shader consumer of the new
   public surface, and the smoke target for `set_time_source`
   actually controlling something visible.
1. **Zero-frame or single-frame materials**: `frame_count == 0` is invalid (assert in `new` / log a warning); `frame_count == 1` should display only cell 0 regardless of time / mode.
2. **frame_count > cols * rows**: invalid (cell index would overflow the atlas). Validate in CPU code; surface an error.
3. **`fps == 0`**: collapses `frame_f = (t + time_offset) * 0 = 0` regardless of `t` or `time_offset`, so the material freezes on cell 0. This is a useful "static cell-cropper" mode worth documenting in the `fps` field's rustdoc rather than rejecting.
4. **Very small / very large `time_offset`** values: no special handling needed; the modulo math is well-behaved.
5. **Tab backgrounding**: when the browser tab is hidden, `requestAnimationFrame` typically stops firing; on resume, the first `frame_globals.delta_time` is upper-clamped to 0.25 while `frame_globals.time` jumps to the new wall-clock value. For a flipbook this means: the animation appears to skip forward to the cell matching the new wall-clock time (correct — the world advanced even if the renderer didn't). No anti-jump-cut work needed; the cell selection is `(t + time_offset) * fps` modulo, which produces a discrete jump but not an erroneous reset.
6. **PingPong loop boundary**: verify that for a 4-frame material the cell sequence is `0,1,2,3,2,1,0,1,2,3,2,1,...` (period of `2*N - 2 = 6`). Off-by-one bugs at the inflection points are the classic ping-pong hazard.
7. **`cargo doc`** for the new public items. Every method has a rustdoc.

Expected outcome: edge cases handled, documented, clean. Commit.

### Phase 6 — Ship

1. Update `docs/ROADMAP.md`: tick "Temporal shaders" / "FrameGlobals" / "FlipBook material".
2. Update the test scene to keep the flipbook quads (becomes the visual regression baseline for both features).
3. If the dynamic-materials plan ([docs/plans/dynamic-materials.md](./dynamic-materials.md)) is in progress or has landed: update its contract docs (`docs/dynamic-materials/contract-opaque.md` and `contract-transparent.md`) to list `frame_globals` as part of the always-in-scope helper set.
4. `cargo fmt`
5. `cargo clippy --workspace --all-targets` — fix everything.
6. `cargo doc --workspace --no-deps` — fix any broken intra-doc links.

Done.

---

## Key References

- **WGSL memory layout spec** — uniform alignment rules: <https://www.w3.org/TR/WGSL/#memory-layouts>
- **`Performance.now()`** — high-resolution monotonic timer used as the default time source: <https://developer.mozilla.org/en-US/docs/Web/API/Performance/now>
- **glTF KHR_materials_unlit** — the closest sibling material; FlipBook's structure mirrors Unlit's (no lighting, color * factor): <https://github.com/KhronosGroup/glTF/tree/main/extensions/2.0/Khronos/KHR_materials_unlit>
- **Internal**: [crates/materials/src/unlit.rs](../../crates/materials/src/unlit.rs) — the prototype this plan's FlipBook structure mirrors.
- **Internal**: [crates/materials/src/wgsl/unlit_material.wgsl](../../crates/materials/src/wgsl/unlit_material.wgsl) — the WGSL prototype FlipBook's WGSL mirrors.
- **Internal**: [crates/renderer/src/render_passes/shared/shared_wgsl/camera.wgsl](../../crates/renderer/src/render_passes/shared/shared_wgsl/camera.wgsl) — the Raw-and-friendly struct pattern FrameGlobals follows exactly.
- **Internal**: [docs/plans/dynamic-materials.md](./dynamic-materials.md) — the follow-up plan whose contract docs depend on `frame_globals` being in scope.

---

## Tracking

Tick items as they land. A future session can resume by reading this list.

### Phase 0 — FrameGlobals scaffolding + Camera cleanup
- [x] `crates/renderer/src/frame_globals/` module with `construction_ms: f64` + `last_time: Option<f32>` + `time_override: Option<f32>`
- [x] `pub frame_globals: FrameGlobals` field on `AwsmRenderer`
- [x] GPU buffer (32 bytes, uniform)
- [x] `MappedUploader` companion (NOT raw `queue.writeBuffer`) — mirrors `camera.rs`
- [x] `write_gpu` with upper-clamp-only delta (allows 0.0), first-frame delta = 0.0
- [x] `FrameGlobalsSnapshot` + `frame_globals()` accessor + `set_time_source()` method
- [x] Wired into `render::render()` in the existing CPU→GPU upload batch
- [x] Clamp verified: 3 consecutive `set_time_source(0.0)` → all `delta_time == 0.0`
- [x] Rustdoc on `set_time_source` documents pause / time-scale / replay use cases
- [x] `frame_count_and_padding` removed from `CameraRaw` (shared_wgsl/camera.wgsl + camera.rs Rust layout)
- [x] `frame_count: u32` removed from friendly `Camera` struct + `camera_from_raw` assignment
- [x] Same field removed from `editor/src/grid/shaders/grid.wgsl` and `renderer/src/render_passes/lines/.../line.wgsl` (verify with grep)
- [x] `grep -r 'camera\.frame_count\|frame_count_and_padding' crates/` returns no matches

### Phase 1 — frame_globals.wgsl + universal binding
- [x] `shared_wgsl/frame_globals.wgsl` with Raw + friendly structs and binding declaration
- [x] Bind group carrying Camera in each pass extended with `frame_globals_raw`
- [x] Templates updated to include `frame_globals.wgsl` and bind it
- [x] Friendly `let frame_globals = ...` available at the right scope in each pass
- [x] Verified by debug pulse on an existing material (e.g. Unlit `* (sin(time)*0.5+0.5)`); debug code removed before commit
- [x] If dynamic-materials plan is in progress: `frame_globals` added to its contract docs as always-in-scope

### Phase 2 — FlipBookMaterial Rust struct
- [x] `crates/materials/src/flipbook.rs` with struct, `FlipBookMode`, `new`, accessors
- [x] `WGSL_FRAGMENT` const
- [x] `impl MaterialShader` with `write_uniform_buffer` matching the WGSL byte layout
- [x] `MaterialShaderId::FLIPBOOK` added — as enum variant or `const FLIPBOOK: Self = Self(4)` depending on whether dynamic-materials plan has landed
- [x] `flipbook` Cargo feature in `awsm-renderer-materials`; added to workspace default features
- [x] `enabled_materials()` appends FlipBook behind the feature
- [x] `Material::FlipBook` variant + every match arm updated
- [x] **Material classify extension**: `BUCKET_COUNT: 3 → 4` in `material_classify/buffers.rs`
- [x] **Material classify WGSL**: `BUCKET_BIT_FLIPBOOK` + if-else chain + per-bucket extract block all extended
- [x] **Pipeline pre-warm**: verify `prewarm_pipelines()` picks up FlipBook on a debug-build editor boot (cold-cache reload should compile the FlipBook pipelines during splash, not on first interactive frame)

### Phase 3 — flipbook_material.wgsl
- [x] `flipbook_material.wgsl` with Raw + friendly structs
- [x] `flipbook_get_material(byte_offset)` follows Unlit's mapping pattern
- [x] `flipbook_compute_cell_uv` + `flipbook_apply_mode` implemented
- [x] Per-pass entry function dispatching from the chain
- [x] `mode == Once` past end → alpha = 0

### Phase 4 — Test scene + visual verification
- [x] Sprite sheet asset (or numbered debug sheet) in `awsm-renderer-assets`
- [x] Three quads in test scene: default loop, PingPong, offset loop
- [x] Cell sequence visually verified with debug sheet
- [x] Three alpha modes (Opaque, Mask, Blend) verified

### Phase 5 — Edge cases + polish
- [x] **Particle bridge migrated**: `particles_sync.rs` reads `delta_time` from `frame_globals()` instead of its private `last_ts_ms` math. `set_time_source` smoke target: paused gameplay freezes particle motion; bullet-time slows it.
- [x] `frame_count == 0` handled (assert / log)
- [x] `frame_count > cols * rows` rejected
- [x] `fps == 0` documented as valid (static cell)
- [x] Tab-backgrounding behavior verified (delta clamps; cell jumps to new wall-clock)
- [x] PingPong sequence at 4 frames verified: `0,1,2,3,2,1,...` (period = 2N − 2)
- [x] `cargo doc` clean for new public items

### Phase 6 — Ship
- [x] `docs/ROADMAP.md` updated
- [x] Test scene kept as visual regression baseline
- [x] Dynamic-materials contract docs updated (if/when that plan lands)
- [x] `cargo fmt` clean
- [x] `cargo clippy --workspace --all-targets` clean
- [x] `cargo doc --workspace --no-deps` clean
