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

- [x] Previous-state buffers (03 §2.3): prev world matrices, instance
      transforms, joint matrices, morph weights, `prev_time`; end-of-frame
      copies; prev = current on insert / slot reuse; `reset_history`.
- [x] Geometry pass, TAA variant: second `apply_vertex` with the previous
      inputs, `clip_now` / `clip_prev` varyings, fifth target
      `velocity: rg16float`. The masked and custom-vertex variants too. HUD
      writes zero.
- [x] Device check: `maxColorAttachmentBytesPerSample ≥ 36` picks this path.
      Otherwise TAA is unavailable until phase 5.
- [x] Sky vector (rotation only) as a helper the resolve and the debug view
      share.
- [x] Debug view `Velocity` (05 §2).
- [x] `wgsl_validation`: the TAA geometry variants validate; the velocity
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

- [x] `AntiAliasing::taa`, validation, pipeline recompiles on switch.
- [x] `render_passes/taa/`: history ping-pong (`rgba16float` × 2, post size),
      `prev_depth`, bind groups by parity (copy the SSR temporal structure),
      the resolve shader exactly as 03 §4, every tuning value a uniform.
- [x] Placement after SSR (03 §6); bloom / effects / display read `taa_out`.
- [x] Mip bias plumbing (03 §7) wired with `s = 1`. At native resolution
      `log2(s) = 0`; only `extra` applies.
- [x] Resets (03 §8), including the camera-jump guard.
- [x] Debug views: history length, rejection reason, clip amount, freeze
      jitter, history only.
- [x] `wgsl_validation`: the resolve validates; it contains the variance
      clip (pinned, like SSR's clamp); it has no `switch` with `return`
      inside, and no per-invocation array indexed by a loop counter.

Gate: 05 §4 ghosting, convergence and flicker tests pass at native
resolution; frame-time numbers recorded (05 §5).

## Phase 3: reactive pixels and the HUD

- [x] `reactive: r8unorm` (render size); transparent pass writes it with
      max-blend; lines too.
- [x] Material `temporal` flag (`stable` / `reactive`) in material data and
      the scene schema; opaque shading writes it; UV-flow materials default
      to `reactive`.
- [x] HUD drawn after TAA at post size with the unjittered camera.
- [x] `k_ssr` tuned on the mirror scene (default stays 0 unless it is needed).

Gate: 05 §4 transparent / particle / UV-flow / HUD tests pass.

## Phase 4: upscaling

- [x] `TaaConfig::render_scale < 1`: scene at render size, TAA writes post
      size, the reconstruction kernel and confidence (03 §4.2, §4.8),
      `N_max` 24, `N = ceil(8 / s²)`.
- [x] `load_depth_post` for haze and DoF; DoF's CoC at post size.
- [x] Mip bias `log2(s) + extra`; settle `extra` with the moiré scene.
- [x] Optional RCAS pass.
- [x] Presets: Quality 0.67, Balanced 0.58, Performance 0.5.

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

Phase 1:

- Previous transforms are mirrored at the full 112 B stride (same offsets as
  the current buffer), not 64 B: one contiguous copy and no change to the
  mesh meta. About 1 MB at 10k transforms, TAA only.
- Previous transforms, joint matrices and morph weights share **one** storage
  buffer (`prev_state`, three regions). Chrome on the M1 grants 10 storage
  buffers per shader stage and separate buffers needed 12 in the masked +
  custom-vertex variant. The region bases and `prev_time` ride in
  `frame_globals`' spare words (zero with TAA off). The device check is
  `maxColorAttachmentBytesPerSample >= 36` and 10 storage buffers per stage.
- The current transform / instance / joint / morph-weight buffers carry
  `COPY_SRC` in every mode. A usage flag, no memory and no copies.
- Copies: `prev = current` for new, reused and relocated ranges is recorded
  before any pass of the frame (G10); the whole-buffer `current → prev` copy
  after the last pass, only when that buffer was uploaded since the last copy.
  A static scene copies nothing. A relocated instancer block gets one frame
  of zero motion rather than its old vectors carried over.
- `AwsmRenderer::reset_motion_history(TransformKey)` (subtree, instances,
  skins) exists beside `Transforms::reset_history(key)`. A teleported parent
  moves its children.
- HUD meshes write their true motion (about zero when attached to the
  camera), not a forced zero: the exact evaluation gives it for free.
- Geometry drawn through `begin_world_geometry_extension_pass` writes no
  velocity (documented on the method).
- A custom-vertex material whose pipeline variant is missing after an AA
  change is now recompiled at the next commit, in every mode. In 0.45 such a
  mesh rendered undisplaced after an MSAA flip.
- `rg16float` resolves a vector to about 1/2048 of its length: 0.24 px on a
  272 px teleport. Every non-teleport frame is within the ±0.02 px bound.
- The material-transparent and shadow shader variants are untouched; the 16
  TAA-off geometry-family shader sources are byte-identical to 0.45.

Phase 2:

- The history has its own parity, flipped only after a frame whose resolve
  was submitted, and resets are consumed the same way. A failed frame, or a
  frame without a resolve, neither flips the history nor eats a reset.
- Resets are decided before the frame's jitter is chosen (camera jump, a
  perspective / orthographic flip, a post-size change), so the jitter cycle
  restarts on the reset frame itself.
- The camera-jump guard measures translation against the distance to the
  scene bounds (floored by their radius), not the focus distance. Inside a
  world larger than that distance only the 30° rotation test can fire, so
  apps call `reset_temporal_history` on the cuts they know about.
- A change inside `TaaConfig` (tuning, sharpen, render scale) takes a light
  path in `set_anti_aliasing`: no pipeline or texture rebuild, no reset.
- The previous depth is copied right after the resolve (its only reader),
  not at the very end of the frame.
- Two blend rules were added to 03 §4.8 after the first gate run. The history
  cap shortens with speed (`max_history_moving` at `max_history_moving_speed`):
  history resampled every frame under motion softens, and with a 16-frame
  memory 60% of that softness was still there 8 frames after stopping (the
  `taa-orbit` stop test read 1.40 against < 1). And a motion boundary (the
  nearest and farthest surface of the 3×3 moving differently) raises the
  current weight (`motion_edge_alpha`): a moving silhouette's coverage
  changes every frame and the clip cannot reject the blend it leaves.
- Volumetrics and SSR offset their own jitter index by 7 under TAA so they do
  not run in step with the camera jitter (03 §1.4). Unchanged with TAA off.
- The disocclusion distances are both linearised with this frame's
  projection. Exact unless the projection itself changes between frames.

Phase 3:

- Opaque reactive values are written by a "seed" draw (a full-screen triangle
  at the start of the world transparent pass, TAA only, drawn only when a
  reactive material or a HUD exists), not by opaque shading: `r8unorm` is not
  a storage format, so a compute pass cannot write the mask.
- The `temporal` flag is `Auto | Stable | Reactive`. Auto is reactive for
  flipbooks, UV flow, and custom materials whose WGSL reads `frame_globals`.
  It rides in a reserved word of the mesh's material meta, packed in every
  mode (4 bytes in an upload that happens anyway).
- The two reactive factors travel in the camera uniform's `dof_params.zw`
  (zero with TAA off).
- HUD: meshes with a transparent pipeline are drawn after the resolve into
  `taa_out` with the unjittered view-projection (`hud_post` pipelines and a
  post-size HUD depth). HUD meshes shaded by the opaque pass cannot move
  there without an opaque pass at post size, so under TAA their geometry is
  rasterised with an unjittered twin of the camera uniform, the seed marks
  their pixels with exactly 1.0, and the resolve passes those pixels through
  (no history, no reconstruction, no spread to neighbours). Every other
  reactive write is capped at 0.99.
- Content drawn through the public transparent-pass hook writes no reactive
  value (documented on the method).

Phase 2, after the gates were measured:

- Disocclusion has a second test, on the pixel's **own** surface (its own
  depth and vector). The test of 03 §4.7 looks only at the surface whose
  vector the pixel uses (the nearest of its 3×3). A pixel beside a moving
  object takes the object's vector, so that test compares the object with
  itself (141 of 141 such pixels unflagged on one `taa-motion` frame), and an
  object resting on the surface behind it is inside the 5% slack near the
  contact line. For a surface that did not move the expected previous
  distance is exact, so the slack there is the surface's depth slope across
  a pixel. Away from nearer surfaces the test takes the **farthest** depth of
  the previous 2×2 (was the surface visible anywhere in it); beside a nearer
  surface it fires only if that surface moves relative to the pixel's own,
  and then takes the nearest. The first version took the nearest everywhere
  and flagged a one-pixel outline around every static object (`taa-thin`
  flicker 0.27 → 0.81); with the rules above the static false-flag counts
  and flicker equal the build without the test.
- The pre-existing test still fires in a burst once per jitter cycle on
  static edges, at the largest jitter step: the 3×3 and the previous 2×2 are
  point samples at different sub-pixel positions. It passes the flicker gate
  at that level and was left alone.
- Shipped tuning differs from 03 §10: history cap under motion 2 (new),
  motion-boundary weight 1.0 (new), clip weight 1.0 per unit of clip amount
  (03 §4.8 has 0.5), reactive transparents 0.3 (0.8), reactive materials 0.2
  (0.6). Each came from a sweep recorded in `docs/TAA.md`.

Phase 4:

- The mip bias reaches opaque materials by scaling the barycentric
  derivatives the geometry pass stores (a `mip_bias` geometry variant, on in
  every TAA path), not through a new opaque shader axis: those derivatives
  feed only the shading's texture gradients, so custom materials get it too.
  The bias rides in the camera uniform's `prev_position.w`. Alpha-cutoff
  sampling is at LOD 0 and needs none.
- `render_scale` is quantised to 0.01. `set_taa_render_scale` is the
  governor entry point (light path, no history reset).
- A render-scale step rebuilds every render-size target and restarts the
  volumetric froxel history and SSR's temporal history (03 §6 hoped
  volumetrics would not care: its grid is aligned to the light-culling tiles
  at render size). TAA history, bloom and the sharpening target are kept.
- Under TAA, LOD screen error and coverage counts are measured in output
  pixels, so a scale step does not pop LOD.
- RCAS runs before the HUD-after-resolve pass and skips HUD pass-through
  pixels, so no HUD is sharpened. HUD meshes shaded by the opaque pass are
  rasterised at render size and are soft below scale 1.
- TAA pipelines that fail to compile fall back to MSAA 4× once, with the
  reason readable (`taa_fallback_reason`).

Phase 5:

- The fallback's velocity texture is `rg32float`: `rg16float` is not a core
  storage format. Which path a device uses is renderer state
  (`taa_velocity_path`), not part of `TaaConfig`.
- `inverse(M)` is the transpose of the stored normal matrix (exact for any
  affine transform). Measured against a per-pixel inverse by repeat-N:
  1.118 ms against 1.142 ms for the prep pass at 1920×1435, equal within
  noise.
- On the fallback, skinned, morphed and instanced meshes are reactive (0.5)
  only while they deform (2-frame linger); custom vertex hooks always.
  Instanced meshes get no per-instance vector on this path.
- Every device now has a velocity path, so `TaaUnavailable` is not raised.

## Things to keep out of v1

- Velocity for transparent surfaces (reactive is enough to stop ghosting).
- Exploiting TAA to cut effect sample counts (PCSS taps, SSR rays, DoF taps,
  rotated per frame). Worth it later, but each one makes history validity
  harder to judge while the resolve is new.
- Supersampling (`s > 1`) together with TAA.
- Frame generation / extrapolation.
- Neural approaches: see [06-neural-aa.md](06-neural-aa.md), a separate
  investigation.
