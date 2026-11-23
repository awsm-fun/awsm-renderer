# 04 — Implementation plan

Each phase ends with its gate from [05-verification.md](05-verification.md)
met, plus `task lint` and `cargo test --all-features` green. A phase that
can't meet its gate doesn't move on by loosening the gate. Record the numbers
in this file as you go, as the other plans do.

## Public API

```rust
// anti_alias.rs
pub struct AntiAliasing {
    pub msaa_sample_count: Option<u32>,
    pub smaa: bool,
    pub mipmap: bool,
    pub taa: Option<TaaConfig>,          // new; Some ⇒ msaa None and smaa false, else an error
}

pub struct TaaConfig {
    /// Render size / output size, 0.5..=1.0. 1.0 = TAA at native resolution.
    pub render_scale: f32,
    /// RCAS strength after the resolve; 0 = off.
    pub sharpen: f32,
    /// Tuning knobs (03 §10), with Default = the start values.
    pub tuning: TaaTuning,
}

impl AwsmRenderer {
    /// Forget all temporal history (cuts, teleports). Also called internally on
    /// scene load, AA change and post-size change.
    pub fn reset_temporal_history(&mut self);
    pub fn set_taa_debug_view(&mut self, view: TaaDebugView);   // 05 §2
}

impl Transforms {
    /// This transform jumped: its previous value becomes its current one.
    pub fn reset_history(&mut self, key: TransformKey);
}
```

`set_render_scale` keeps its [1, 2] supersampling meaning when TAA is off.
With TAA on, the scale comes from `TaaConfig::render_scale` and may be changed
every frame without a reset (03 §8). Scene files carry the TAA choice in
`PostProcessConfig::anti_aliasing`, as MSAA / SMAA are carried today.

## Phase 0: groundwork, no visible change

- [x] Fix the camera jitter (03 §1): NDC scale `2·j/R`, constant amplitude,
      index from `(frame % N) + 1`, `N` from the scale. Keep `camera::halton`.
      Add `view_projection_unjittered` and `jitter` (current, previous) to the
      camera uniform, end-appended. `APPLY_JITTER` becomes "TAA is on".
- [x] Unit tests: the Halton phases; the NDC offset moves a known point by
      exactly `j` render pixels (perspective and orthographic, both depth
      conventions); the unjittered matrix round-trips.
- [x] Introduce the post size: `render_size()` (scene) and `post_size()`
      (surface). Bloom, effects and display take `post_size()`. At scale 1 the
      two are equal, so nothing changes yet. Add a test that every
      render-texture consumer reads one of the two explicitly.
- [ ] Test scenes and debug-view plumbing from 05 §1–2 (the views can show
      "n/a" until their data exists).
- [ ] Reference capture tool (05 §3): N-frame jittered accumulation of a
      frozen scene, as ground truth.

Gate: with TAA off, every existing golden (`examples/test-scenes`, the
bundle-player and `player-tests` checks) still passes at its current
tolerance.

## Phase 1: motion vectors

- [ ] Previous-state buffers (03 §2.3): prev world matrices, instance
      transforms, joint matrices, morph weights, `prev_time`; end-of-frame
      copies; prev = current on insert / slot reuse; `reset_history`.
- [ ] Geometry pass, TAA variant: second `apply_vertex` with the previous
      inputs, `clip_now` / `clip_prev` varyings, fifth target
      `velocity: rg16float`. The masked and custom-vertex variants too. HUD
      writes zero.
- [ ] Device check: `maxColorAttachmentBytesPerSample ≥ 36` picks this path.
      Otherwise TAA is unavailable until phase 5.
- [ ] Sky vector (rotation only) as a helper the resolve and the debug view
      share.
- [ ] Debug view `Velocity` (05 §2).
- [ ] `wgsl_validation`: the TAA geometry variants validate; the velocity
      output divides per fragment (pin the `clip.xy / clip.w` in the fragment
      stage, so nobody moves it into the vertex stage).

Gate: 05 §4 "motion vectors" passes in every test scene. Do **not** start
phase 2 with a vector that is wrong anywhere. Every ghost in phase 2 would
then have two possible causes.

Files: `camera/buffer.rs`, `transforms.rs`, the instancer, skin / morph
buffer owners, `frame_globals`, `render_passes/geometry/**` (vertex, fragment,
masked, custom_vertex, pipeline + cache keys), `render_textures.rs`,
`render.rs` (end-of-frame copies), `wgsl_validation.rs`.

## Phase 2: TAA at native resolution

- [ ] `AntiAliasing::taa`, validation, pipeline recompiles on switch.
- [ ] `render_passes/taa/`: history ping-pong (`rgba16float` × 2, post size),
      `prev_depth`, bind groups by parity (copy the SSR temporal structure),
      the resolve shader exactly as 03 §4, every tuning value a uniform.
- [ ] Placement after SSR (03 §6); bloom / effects / display read `taa_out`.
- [ ] Mip bias plumbing (03 §7) wired with `s = 1`. At native resolution
      `log2(s) = 0`; only `extra` applies.
- [ ] Resets (03 §8), including the camera-jump guard.
- [ ] Debug views: history length, rejection reason, clip amount, freeze
      jitter, history only.
- [ ] `wgsl_validation`: the resolve validates; it contains the variance
      clip (pinned, like SSR's clamp); it has no `switch` with `return`
      inside, and no per-invocation array indexed by a loop counter.

Gate: 05 §4 ghosting, convergence and flicker tests pass at native
resolution; frame-time numbers recorded (05 §5).

## Phase 3: reactive pixels and the HUD

- [ ] `reactive: r8unorm` (render size); transparent pass writes it with
      max-blend; lines too.
- [ ] Material `temporal` flag (`stable` / `reactive`) in material data and
      the scene schema; opaque shading writes it; UV-flow materials default
      to `reactive`.
- [ ] HUD drawn after TAA at post size with the unjittered camera.
- [ ] `k_ssr` tuned on the mirror scene (default stays 0 unless it is needed).

Gate: 05 §4 transparent / particle / UV-flow / HUD tests pass.

## Phase 4: upscaling

- [ ] `TaaConfig::render_scale < 1`: scene at render size, TAA writes post
      size, the reconstruction kernel and confidence (03 §4.2, §4.8),
      `N_max` 24, `N = ceil(8 / s²)`.
- [ ] `load_depth_post` for haze and DoF; DoF's CoC at post size.
- [ ] Mip bias `log2(s) + extra`; settle `extra` with the moiré scene.
- [ ] Optional RCAS pass.
- [ ] Presets: Quality 0.67, Balanced 0.58, Performance 0.5.

Gate: 05 §4 tests at 0.67 and 0.5; 05 §5 frame-time targets.

## Phase 5: everyone else

- [ ] Fallback velocity in the prep pass for 32 B/sample devices (03 §2.4),
      non-rigid meshes reactive at 0.5, and the `inverse(M)` decision measured.
- [ ] Verified on at least one 32 B/sample device (Android Chrome).
- [ ] Resolution-governor integration: an app changes
      `TaaConfig::render_scale` instead of the canvas size; no resets; the
      jitter cycle follows.
- [ ] Editor: TAA in the post-processing panel, the debug views, the tuning
      knobs.
- [ ] Release notes; delete this plan.

## Deviations and notes (recorded as the work goes)

Phase 0:

- The camera uniform grew by three end-appended fields, not two:
  `view_projection_unjittered`, `jitter` (xy = this frame, zw = previous,
  render pixels) and `prev_position` (billboards need the previous camera
  position to rebuild their previous facing). `R` is not stored separately:
  `viewport.zw` already is the render size. 560 → 656 bytes, for every mode.
- `prev_view_projection` under TAA is the last **rendered** frame's matrix
  (`CameraBuffer::prepare_frame`, once per `render()`), not the last camera
  that was set. An app that sets the camera only when it changes, or twice a
  frame, would otherwise get wrong camera vectors. With TAA off the upload is
  what 0.45 packed, so SSR's temporal pass keeps its 0.45 behaviour there.
- Post size is `FrameSizes { surface, render, post }` (`size.rs`). Without TAA
  `post == render`, which keeps supersampling exactly as in 0.45 (bloom and
  effects at the supersampled size, display downsamples). With TAA
  `post == surface`.
- The jitter override (`AwsmRenderer::set_jitter_override`) exists for the
  reference capture: it jitters the camera with TAA off.
- `prev_depth` is a depth-format copy bound as `texture_depth_2d`, not
  `r32float`: WebGPU cannot copy `depth32float` into `r32float`.
- The disocclusion test compares two previous-frame distances (this surface
  point through `prev_view_projection` against last frame's depth there),
  not this frame's distance against last frame's: the latter trips on any
  fast dolly. Same direction and same τ.
- The clip-box half-width has a floor of 1.5e-3 (a few steps of the
  `rgba16float` history's quantisation), or flat areas read their own
  rounding as a clipped history.
- The resolve's WGSL and plumbing were written while phase 1 was in progress.
  It was not turned on or judged until the phase-1 vector gate passed.

## Things to keep out of v1

- Velocity for transparent surfaces (reactive is enough to stop ghosting).
- Exploiting TAA to cut effect sample counts (PCSS taps, SSR rays, DoF taps,
  rotated per frame). Worth it later, but each one makes history validity
  harder to judge while the resolve is new.
- Supersampling (`s > 1`) together with TAA.
- Frame generation / extrapolation.
- Neural approaches: see [06-neural-aa.md](06-neural-aa.md), a separate
  investigation.
