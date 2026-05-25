# Temporal shaders — `FrameGlobals` + `FlipBook`

Renderer-wide per-frame state (`time`, `delta_time`, `frame_count`,
`resolution`) exposed to every shading pass via a dedicated uniform,
plus a first-party sprite-sheet material as its load-bearing consumer.

This doc covers the shipped surface — what every shader sees, what
CPU consumers read, and the conventions the bundle commits to.

---

## `FrameGlobals` uniform

A 32-byte uniform updated once per `render()` call and bound at the
next free slot of every pass's `camera` bind group (rationale: the
two have identical lifetimes — one upload per frame, read everywhere
— so co-locating them saves a bind-group switch per pass).

```wgsl
// shared_wgsl/frame_globals.wgsl — raw GPU layout
struct FrameGlobalsRaw {
    time: f32,
    delta_time: f32,
    frame_count: u32,
    _pad: u32,
    resolution: vec2<u32>,
    _pad2: vec2<u32>,
};

// Friendly view (drops `_pad` and `_pad2` — the alignment-only words)
struct FrameGlobals {
    time: f32,
    delta_time: f32,
    frame_count: u32,
    resolution: vec2<u32>,
};
```

### Where it's bound

| Pass | Group | Binding |
|------|-------|---------|
| `geometry` | 0 | 1 |
| `material_opaque` | 0 | 22 |
| `material_transparent` | 0 | 18 |
| `effects` | 0 | 5 |

Other camera-bound passes (`occlusion`, `material_decal`, `lines`,
`grid`) intentionally don't carry `frame_globals` — they don't shade
material colour and have no current consumer. Add the binding the
same way (next free slot, layout entry + recreate entry on the Rust
side) when one shows up.

### Shader call site

Mirrors the `Camera` convention exactly:

```wgsl
let camera = camera_from_raw(camera_raw);
let frame_globals = frame_globals_from_raw(frame_globals_raw);

// then anywhere downstream:
let pulse = sin(frame_globals.time * 6.28);
```

### Time semantics

- `time` is seconds since `AwsmRenderer::new()` returned (not since
  page load — distinguishing those matters for renderers that boot
  lazily). Monotonic in the default wall-clock mode.
- `delta_time` is seconds since the previous `render()` call.
  - First frame after construction: `0.0` (no prior frame to
    subtract from). Shaders that divide by `delta_time` must guard
    against zero.
  - Subsequent frames: `(time - last_time).clamp(0.0, 0.25)`. Upper-
    clamped at 0.25 s to keep per-frame integrators stable after the
    tab has been backgrounded for minutes. **No lower clamp.** If a
    consumer calls `set_time_source(t)` with the same `t` twice
    (paused gameplay), `delta_time` is exactly `0.0` — simulations
    correctly stop advancing.
- `time` vs `delta_time` on tab resume: `time` is wall-clock, so it
  jumps forward by the backgrounded duration on the next frame even
  though `delta_time` reports 0.25 s. Animations driven by
  `sin(time * f)` reflect real elapsed seconds (correct for
  time-of-day, day-night cycles); integrators driven by
  `position += velocity * delta_time` get the stable clamped rate.
- `f32` precision on `time`: the CPU tracks elapsed milliseconds in
  `f64` (matching `Performance.now()`'s native precision) and
  converts to `f32` seconds at upload time. At 1 ms resolution, `f32`
  precision degrades visibly after ~16 hours of continuous run.
  Fine for game sessions; installation-art-grade precision needs a
  `(time_hi, time_lo)` split with double-reconstruction in shader —
  not in scope yet.

### CPU-side surface

```rust
impl AwsmRenderer {
    /// Snapshot of the values uploaded this frame — `time`,
    /// `delta_time`, `frame_count`, `resolution`. Cheap to copy.
    /// Read this rather than rolling your own `performance.now()`
    /// delta in per-frame ticks.
    pub fn frame_globals(&self) -> FrameGlobalsSnapshot;

    /// Override the time source. The renderer derives `time` from
    /// `Performance.now()` by default; consumers running their own
    /// game-time clock (paused, time-scaled, replayed) call this
    /// before `render()` to inject the value the next frame's
    /// `frame_globals.time` will see. `delta_time` is computed from
    /// successive `time` values regardless of source, so passing the
    /// same `time` twice reports `delta_time == 0.0`.
    pub fn set_time_source(&mut self, time: f32);

    /// Drop the override; subsequent frames go back to the wall clock.
    pub fn clear_time_source(&mut self);
}
```

The editor's particle bridge
([`particles_sync.rs`](../crates/frontend/scene-editor/src/renderer_bridge/particles_sync.rs))
is the canonical first non-shader consumer: `tick_all` reads
`renderer.frame_globals().delta_time`, so pause / time-scale / replay
flow through `set_time_source` automatically. Any subsystem that used
to keep its own `last_ts_ms` should follow the same pattern.

### Why not on `Camera`?

The renderer carries some per-frame data on the `Camera` uniform
today (`viewport_size`, formerly `frame_count`). Putting `time` and
`delta_time` there would be expedient but semantically wrong — these
aren't camera properties, they're renderer-wide state. A dedicated
uniform:

- **Discoverable**: a shader author looking for "renderer time" finds
  `frame_globals.time`, not `camera.time`.
- **Multi-camera-clean**: shadow passes render from a "light camera",
  post-FX may use side cameras. `FrameGlobals` rides through every
  pass unchanged; `Camera` changes per pass.
- **Room to grow**: future renderer-wide globals (ambient color, wind
  direction, simulation seed) land here without bloating `Camera`.

`Camera.frame_count` was removed during this work — no WGSL ever
read it; the monotonic counter now lives on `FrameGlobals`. Camera
is 16 bytes slimmer (512 → 496) as a result. `Camera.viewport_size`
stays because it's per-camera (split-screen / picture-in-picture).
`FrameGlobals.resolution` is the renderer's output size, relevant to
post-FX and screen-space effects that don't care which camera drew
the frame.

---

## `FlipBookMaterial` — sprite-sheet animation

Grid-uniform sprite-sheet material as the first-party consumer of
`FrameGlobals`. Covers ~80 % of real-world sprite-sheet use cases
(VFX sheets, UI element animations, simple character anims). The
irregular-atlas variant (TexturePacker-style cells of arbitrary
positions and sizes) is out of scope here and belongs with a future
dynamic-material plan.

### Authoring

```rust
use awsm_renderer::materials::{
    flipbook::{FlipBookMaterial, FlipBookMode},
    Material, MaterialAlphaMode, MaterialTexture,
};

let mut flipbook = FlipBookMaterial::new(MaterialAlphaMode::Blend, /* double_sided */ true);
flipbook.atlas_tex = Some(MaterialTexture { /* … */ });
flipbook.cols = 4;
flipbook.rows = 4;
flipbook.frame_count = 13;    // 13 of the 16 cells actually used
flipbook.fps = 24.0;
flipbook.mode = FlipBookMode::Loop;
flipbook.time_offset = 0.0;
flipbook.tint = [1.0, 0.95, 0.9, 1.0];
material_storage.insert(Material::FlipBook(Box::new(flipbook)), &textures);
```

Indistinguishable in shape from how `UnlitMaterial` is used. Same
trait (`MaterialShader`), same registration path, same Cargo feature
gate (`flipbook` — default-on).

### Modes

| `FlipBookMode` | Behaviour |
|----------------|-----------|
| `Loop` *(default)* | `frame = floor(t * fps) mod frame_count` |
| `PingPong` | `0,1,...,N-1,N-2,...,1,0,1,...` — period `2N - 2` |
| `Clamp` | Stop and hold the last cell |
| `Once` | Like `Clamp`, but past the end writes `alpha = 0` so a `Blend`-mode quad disappears cleanly. (Pairing `Once` with `Opaque` is undefined — the shader freezes on the last cell instead.) |

### Non-goals (v1)

- **Irregular cell layouts.** Grid only.
- **Bilinear blending between cells.** Cell selection is hard
  (`floor`). A future `bool blend_frames` toggle would sample two
  adjacent cells and lerp by the fractional part — easy to add, not
  in scope.
- **Multiple animation tracks per material.** One atlas per
  `FlipBookMaterial`. Anyone needing two stacks two meshes with two
  materials.
- **Event callbacks at specific frames.** Scripting concern, not a
  rendering one.
- **GPU-driven per-instance random start.** Possible via
  `time_offset` if the caller picks a random value at mesh-instance
  time; no built-in randomizer.

### Scene-schema integration

`SpriteDef` carries an optional `flipbook: Option<SpriteFlipBookDef>`
field; when set, the editor's `materialize_sprite` builds a
`Material::FlipBook` instead of the usual unlit/PBR sprite material.
`SpriteAlphaMode::Blend` routes through `add_raw_mesh_transparent`;
`Opaque` / `Mask` through the sync `add_raw_mesh`.

### Test fixture

`awsm-renderer-assets/flipbook-test/` ships a canonical 4×4 numbered
debug atlas (cells labeled 0..15 with distinct colors) + a
three-quad scene exercising Loop, PingPong, and Loop+0.5 s offset.
Load via the dev-only `load_external_test_scene("flipbook-test")`
wasm export — sibling to `load_scene_by_path` but fetches off
`MEDIA_BASE_URL_ADDITIONAL_ASSETS` (port 9083 in dev, GitHub Pages
in prod) so test fixtures with binary assets live in the assets
repo, not the editor's build tree.

---

## Renderer-internal notes for future contributors

- **Adding a new first-party shader_id** is not purely a registry
  change since material classify + indirect dispatch landed (see
  [`PERFORMANCE.md §1`](./PERFORMANCE.md)). Bump
  `BUCKET_COUNT` in
  [`material_classify/buffers.rs`](../crates/renderer/src/render_passes/material_classify/buffers.rs),
  add the matching `BUCKET_BIT_*` + dispatch branch in
  [`compute.wgsl`](../crates/renderer/src/render_passes/material_classify/shader/material_classify_wgsl/compute.wgsl),
  extend the per-bucket extract block, and append the id to
  `OPAQUE_SHADER_IDS` in
  [`material_opaque/pipeline.rs`](../crates/renderer/src/render_passes/material_opaque/pipeline.rs).
- **Adding a new pass that needs `frame_globals`**: declare
  `@group(N) @binding(M) var<uniform> frame_globals_raw: FrameGlobalsRaw;`
  in the pass's `bind_groups.wgsl`, include `shared_wgsl/frame_globals.wgsl`
  in the pass's template, append a uniform-buffer layout entry +
  `BufferBinding::new(&ctx.frame_globals.gpu_buffer)` to its bind
  group recreate, and read via
  `let frame_globals = frame_globals_from_raw(frame_globals_raw);`.
- **Texture-pool sampling from inside `materials_wgsl` fragments**:
  the `materials_wgsl` concat is included as raw text into the
  parent shader (it can NOT use askama). For mip-mode-aware
  sampling the call site (templated) needs to dispatch between
  `texture_pool_sample_grad` and `texture_pool_sample_no_mips`.
  `FlipBookMaterial` is the canonical example: the fragment
  computes the cell UV; the per-pass call site samples.
