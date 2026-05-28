# More optimizations — the master plan

**Status**: design only; no implementation yet.

The renderer is structurally desktop-class: visibility buffer with 4
MRT outputs, MSAA 4× by default, ~15 render/compute passes per frame,
~11 full-screen render targets. That architecture is a deliberate
product decision and this plan does **not** try to undo it.

What this plan **does** do:

1. Define a single mechanism — a **mobile profile** selected by URL
   param (`?mobile=true`) or builder API — that flips a coordinated
   set of conservative defaults at construction time.
2. Catalogue **every per-frame optimization opportunity** found in
   the renderer crate, grouped by category, scored by effort × impact.
3. Sequence the work into shippable chunks so the next pass picks up
   where this one left off.

The numbers in this doc come from the May 2026 mobile trace analysis
(see commit `58c2398` and the trace files in the perf-tracing
session). The model-tests page renders one animated character at 5–6
ms of main-thread work but presents at ~15 fps because the GPU swap
takes 65 ms median on a 60 Hz mobile panel. Main thread is not the
bottleneck; the GPU swap is.

For ergonomics around *measuring* this (tracing tiers, `?trace=…`),
see [`docs/perf-tracing.md`](../perf-tracing.md). This doc is about
the work that creates the cost the tracing measures.

---

## Part 1 — The mobile profile

### 1.1 The shape: one profile, many knobs

Most engines expose a "quality preset" that flips a coordinated set
of defaults. We already have
[`ShadowQualityTier`](../../crates/renderer/src/shadows/quality_tier.rs)
(Low / Medium / High / Ultra / Custom) doing this for shadows. The
plan generalises that pattern to a renderer-wide
**`RendererProfile`** enum that resolves a coherent default set
across:

- `AntiAliasing` (MSAA samples, mipmap policy, SMAA on/off)
- `ShadowsConfig` + `ShadowQualityTier`
- `RendererFeatures` (decals / coverage_lod / picking)
- `RendererOptimizationPolicy` (gpu_culling thresholds, cooldowns)
- `PostProcessing` (bloom / dof / tonemapping)
- `DeviceRequestLimits` (`typical` vs `max_all`)
- `MAX_EDGE_BUDGET_*` (already split desktop/mobile in
  [`edge_buffers.rs:141-143`](../../crates/renderer/src/render_passes/material_opaque/edge_buffers.rs#L141))
- `scene_spatial::Rebuild` cadence (BVH rebuild period / threshold)

```rust
// proposed shape — crates/renderer/src/profile.rs (new)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RendererProfile {
    /// Conservative defaults for mobile-class GPUs.
    /// MSAA off, low shadow tier, no bloom, smaller atlases,
    /// gpu_culling auto-engaged later. Targets ~30 fps on a 2020-era
    /// Android phone in a tight indoor scene.
    Mobile,
    /// The current defaults — desktop-class GPUs.
    #[default]
    Desktop,
    /// Maximum quality. Ultra shadows, MSAA 4×, bloom + DoF on,
    /// 8K atlas. Targets discrete GPUs and content-creation use.
    Cinema,
}
```

The profile is applied at `AwsmRendererBuilder::with_profile(...)`
time and feeds every dependent default. After build, individual
knobs can still be overridden via the existing `with_*` methods —
the profile is just the starting point.

### 1.2 The URL-param wiring

In the same shape we use for `?trace=…` (see
[`crates/web-shared/src/perf.rs`](../../crates/web-shared/src/perf.rs)):

```
?mobile=true       → RendererProfile::Mobile
?mobile=false      → RendererProfile::Desktop  (explicit)
?mobile=cinema     → RendererProfile::Cinema
(no param)         → frontend default (likely Desktop)
```

Implementation in
[`crates/web-shared/src/perf.rs`](../../crates/web-shared/src/perf.rs):

```rust
pub fn renderer_profile_override() -> Option<RendererProfile> {
    let v = query_param("mobile")?;
    match v.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "mobile" => Some(RendererProfile::Mobile),
        "false" | "0" | "no" | "desktop" => Some(RendererProfile::Desktop),
        "cinema" | "max" | "ultra" => Some(RendererProfile::Cinema),
        _ => None,
    }
}

pub fn resolve_renderer_profile(default: RendererProfile) -> RendererProfile {
    renderer_profile_override().unwrap_or(default)
}
```

Frontends pick up the resolved profile when constructing the
renderer:

```rust
// crates/frontend/model-tests/src/pages/app/canvas.rs (illustrative)
let profile = awsm_web_shared::perf::resolve_renderer_profile(
    if cfg!(debug_assertions) {
        RendererProfile::Desktop  // dev default
    } else {
        RendererProfile::Desktop  // shipping default — explicit
    },
);

let mut renderer = AwsmRendererBuilder::new(gpu_builder)
    .with_profile(profile)  // sets coordinated defaults
    .with_features(...)     // optional per-feature override on top
    .build()
    .await?;
```

### 1.3 What the mobile profile actually changes

The matrix below is the **complete list** of defaults that flip
between `Mobile` and `Desktop`. Each line is the proposed value; the
"why" column is what motivates the difference.

| Knob | Desktop (current) | Mobile (proposed) | Why |
|---|---|---|---|
| `AntiAliasing.msaa_sample_count` | `Some(4)` | `None` | MSAA 4× quadruples MRT bandwidth in the geometry pass (4 × 8B × 4 samples = 128 B/pixel of color writes); on a 400×800 mobile canvas that's ~40 MB per pass, every pass. Also kills the entire edge-resolve compute chain (per-shader edge_resolve + skybox + final-blend). |
| `AntiAliasing.smaa` | `false` | `false` | Effects pass still dispatches for tonemapping; SMAA is the cheapest add when bandwidth is the constraint, so leaving it off saves another full-screen compute. |
| `AntiAliasing.mipmap` | `true` | `true` | Mipmaps reduce texture-sample bandwidth — keep on mobile. |
| `ShadowQualityTier` | `High` | `Low` | Low = 1024 atlas (4 MB vs 64 MB), 2 cascades, no SSCS, no EVSM. See [`shadows/quality_tier.rs:60-67`](../../crates/renderer/src/shadows/quality_tier.rs#L60). |
| `ShadowsConfig.cascade_resolution` | `2048` | `1024` | Each cascade slice is `4 × cascade_resolution² B` of Depth32 — 2048 = 16 MB per cascade × `cascade_count`. |
| `ShadowsConfig.point_shadow_resolution` | `1024` | `256` | Cube maps are 6× per light; `6 × 4 × res² × max_point_shadows`. The doc-comment at [`shadows/config.rs:51`](../../crates/renderer/src/shadows/config.rs#L51) literally suggests 256 for mobile. |
| `ShadowsConfig.evsm_atlas_size` | `2048` | `512` | EVSM is `Rgba16float` so per-texel cost is double the PCF atlas. |
| `ShadowsConfig.max_point_shadows` | `8` | `2` | Allocation gate on the point-shadow cube pool. |
| `PostProcessing.tonemapping` | `KhronosNeutralPbr` | `KhronosNeutralPbr` | Tonemapping is cheap; keep — but see "Effects pass coalescing" below. |
| `PostProcessing.bloom` | `false` | `false` | Same — bloom is already opt-in. |
| `PostProcessing.dof` | `false` | `false` | Same. |
| `RendererOptimizationPolicy.gpu_culling_enable_threshold` | `800` | `2000` | The HZB + cull + compaction + drawIndirect path's fixed cost is higher on mobile relative to the gain; push the engagement threshold up. |
| `RendererFeatures.coverage_lod` | `false` | `false` | Opt-in; mobile profile leaves off. |
| `RendererFeatures.decals` | `false` | `false` | Opt-in; mobile profile leaves off. |
| `DeviceRequestLimits` | `typical()` (frontend opts into `max_all()` itself) | `typical()` | `max_all()` requests buffer/binding caps the mobile adapter can't service; the request can fail or return a degraded device. |
| `MAX_EDGE_BUDGET` | `DEFAULT_MAX_EDGE_BUDGET_DESKTOP` (512k) | `DEFAULT_MAX_EDGE_BUDGET_MOBILE` (256k) | Already split — wire the profile to pick. |
| `scene_spatial::Rebuild` thresholds | `rebuild_period_frames=600, rebuild_dirty_threshold=200` | `1200 / 400` | Halve the BVH rebuild frequency on mobile. |
| `RendererTextureFormats.depth` | `Depth32float` | `Depth24Plus` (with capability check) | 33 % less depth bandwidth on every pass that touches the depth attachment, which is most of them. |
| `RendererTextureFormats.color` | `Rgba16float` | `Rgba16float` | Keep — HDR is required for bloom/tonemapping headroom, but **see "Color format alternatives" in §3.5**. |

### 1.4 What the mobile profile does *not* change

Out of scope for the profile (because they're per-frame engine
behaviour, not config):

- The visibility-buffer pass structure
- The number of render passes
- The pass execution order
- The bind-group layout shapes
- Which compute pipelines exist

Those are addressed by the per-pass optimization items in Part 2.

---

## Part 2 — Per-pass optimization catalogue

Items are organized into four buckets, in priority order:

- **Tier 1 — Quick wins**: mechanical fixes, no architectural risk.
  An afternoon each. Help both desktop and mobile.
- **Tier 2 — Pass restructuring**: rearrange existing passes for
  fewer pass-breaks and better tile-cache reuse. A day or two each.
  Help mobile substantially, desktop a little.
- **Tier 3 — Mobile profile wiring**: code to support `?mobile=true`.
  The actual work behind §1 above.
- **Tier 4 — Architectural**: bigger refactors that need design
  proposals of their own. Listed for completeness; not in this
  sprint.

Every item below has:

- **File:line** anchor into the code
- **Effort** (trivial / moderate / architectural)
- **Impact** (qualitative — Mobile S/M/L, Desktop S/M/L)
- **Risk** (correctness considerations, if any)

### Tier 1 — Quick wins

#### T1.1 — Cache exposure uniform; skip `writeBuffer` on unchanged value

- **Where**:
  [`render_passes/display/render_pass.rs:38-50`](../../crates/renderer/src/render_passes/display/render_pass.rs#L38)
- **Pattern**: A 4-byte `exposure_scale = ctx.post_processing.exposure.exp2()`
  is `writeBuffer`'d every frame, unconditionally. The exposure rarely
  changes; `camera.rs` already uses an `gpu_dirty` flag + `matrices_equal()`
  epsilon for the same kind of gate.
- **Fix**: Add a `last_exposure: Cell<f32>` to `DisplayRenderPass` (or
  promote `exposure` to a renderer-level dirty state). Skip the
  `write_buffer` when the value matches.
- **Effort**: trivial. **Impact**: Mobile S, Desktop S. **Risk**: none.

#### T1.2 — Cache decal classify header; skip `writeBuffer` on unchanged

- **Where**:
  [`render_passes/material_decal/classify/buffers.rs`](../../crates/renderer/src/render_passes/material_decal/classify/buffers.rs)
- **Pattern**: A 16-byte tile-header struct is written every frame
  when `features.decals` is on. Header rarely changes.
- **Fix**: Same as T1.1 — cache last bytes, skip if equal.
- **Effort**: trivial. **Impact**: Mobile S (only when decals on).
  **Risk**: none.

#### T1.3 — Early-exit `Morphs::write_gpu` / `Skins::write_gpu` when no morphs/skins registered

- **Where**:
  [`meshes/morphs.rs:35`](../../crates/renderer/src/meshes/morphs.rs#L35),
  [`meshes/skins.rs:247`](../../crates/renderer/src/meshes/skins.rs#L247)
- **Pattern**: Both are called unconditionally from `render.rs:198-201`.
  They internally check `weights_dirty` / `matrices_dirty` flags, but
  the function call + dirty-flag check still happens every frame
  even when zero morphs/skins are registered.
- **Fix**: Top-of-function `if self.geometry.infos.is_empty() &&
  self.material.infos.is_empty() { return Ok(()); }`.
- **Effort**: trivial. **Impact**: Mobile S, Desktop S. **Risk**: none.

#### T1.4 — Cache `current_context_texture_size()` once per frame

- **Where**: Three call sites per frame:
  [`render.rs:239`](../../crates/renderer/src/render.rs#L239),
  [`render_textures.rs:136`](../../crates/renderer/src/render_textures.rs#L136),
  [`camera.rs:18`](../../crates/renderer/src/camera.rs#L18)
- **Pattern**: Each call is a wasm↔JS boundary crossing into
  `gpu.get_current_texture().getSize()`. Three crossings per frame
  for a value that's known to be stable for the whole frame.
- **Fix**: Compute once at the top of `render()`, pass on
  `RenderContext` as `viewport_size: (u32, u32)`. Replace all three
  call sites with the cached value.
- **Effort**: trivial. **Impact**: Mobile S, Desktop S. **Risk**: none —
  the value is stable mid-frame by construction.

#### T1.5 — Skip Effects pass entirely when no effects are configured

- **Where**:
  [`render_passes/effects/render_pass.rs:37-65`](../../crates/renderer/src/render_passes/effects/render_pass.rs#L37)
- **Pattern**: When `bloom == false`, falls through to a
  `BloomPhase::None` dispatch that runs only because of historical
  SMAA/DoF wiring. If both bloom and DoF are off, the pass is a
  no-op compute dispatch covering the entire viewport.
- **Fix**: Top-of-function `if !ctx.post_processing.bloom &&
  !ctx.post_processing.dof { return Ok(()); }`. If SMAA is later
  enabled independently, it gets its own short-circuit.
- **Effort**: trivial. **Impact**: Mobile M, Desktop S. **Risk**:
  none, assuming the `Display` pass continues to do tonemapping (it
  already does).

#### T1.6 — Don't call `LightCullingRenderPass::render` while it's a no-op

- **Where**:
  [`render_passes/light_culling/render_pass.rs:30-34`](../../crates/renderer/src/render_passes/light_culling/render_pass.rs#L30),
  called from [`render.rs:649`](../../crates/renderer/src/render.rs#L649)
- **Pattern**: The pass is currently `pub fn render(&self, _ctx) ->
  Result<()> { Ok(()) }` — a TODO. The renderer still calls it,
  opens a tracing span (sub-frame tier), pays the dynamic dispatch.
  When the GPU light-culling work (per
  [`docs/plans/light-culling.md`](light-culling.md)) lands, this
  call becomes load-bearing.
- **Fix**: Either gate the call on a build-time `cfg!()` or simply
  return early inside the method until the implementation lands.
  When the real implementation arrives, remove the gate.
- **Effort**: trivial. **Impact**: Negligible (~µs). **Risk**: none.
- **Note**: Keep the span name reserved for when the real work lands;
  test harnesses look for it.

#### T1.7 — Early-exit `Shadows::write_gpu` when no shadow-casting lights are active

- **Where**:
  [`shadows/state.rs:1446`](../../crates/renderer/src/shadows/state.rs#L1446)
  called unconditionally from
  [`render.rs:254-262`](../../crates/renderer/src/render.rs#L254).
- **Pattern**: The render pass body
  ([`shadows/render_pass.rs`](../../crates/renderer/src/shadows/render_pass.rs))
  already gates on `self.shadows.any_active()` at the call site
  (render.rs:633). But the descriptor / view matrix uploads inside
  `Shadows::write_gpu` *still* fire every frame even when no shadow
  casters exist. The pack-and-upload loop iterates `0..0` and writes
  a zero-byte buffer — but the wasm↔JS `writeBuffer` calls still
  happen.
- **Fix**: Top-of-`write_gpu` short-circuit:
  ```rust
  if self.descriptor_count() == 0 {
      return Ok(());
  }
  ```
  (Subject to verifying that downstream readers tolerate
  not-this-frame-rewritten buffers — the shadow descriptor buffer
  is already initialized at construction.)
- **Effort**: trivial. **Impact**: Mobile M when no shadows.
  **Risk**: Low — needs a one-time check that the descriptor buffer
  is correctly initialized on the first frame with no shadows. The
  test should be in `shadows::state::tests`.

#### T1.8 — Coverage zero-out via GPU `clear_buffer` instead of host `writeBuffer`

- **Where**:
  [`render_passes/coverage/buffers.rs:102`](../../crates/renderer/src/render_passes/coverage/buffers.rs#L102)
- **Pattern**: Each frame when `coverage_lod` is on, the counts
  buffer is zeroed via `gpu.write_buffer(...)` with an `&[0u8; N]`
  payload. That ships N bytes across the wasm↔JS boundary every
  frame.
- **Fix**: Use `command_encoder.clear_buffer(&counts_buffer, None,
  None)` recorded into the same frame's encoder. Zero-cost on the
  CPU side; GPU does the clear inline with other work.
- **Effort**: trivial. **Impact**: Mobile M (when coverage_lod is
  on — buffer scales with mesh count). **Risk**: ordering — the
  `clear_buffer` must execute before the coverage compute reads
  the counts. Already in command order, so this is automatic.

#### T1.9 — Batch picker submit into the main frame's command encoder

- **Where**:
  [`picker/state.rs:100`](../../crates/renderer/src/picker/state.rs#L100)
- **Pattern**: `Picker::pick` runs in its own encoder + submit,
  separate from the main frame's submit at
  [`render.rs:1177`](../../crates/renderer/src/render.rs#L1177).
  Two submits per frame when picking is active.
- **Fix**: Take an `&CommandEncoder` parameter, record into it,
  return without submitting. Caller submits.
- **Effort**: moderate (API change). **Impact**: Mobile M when
  picking is engaged, Desktop S. **Risk**: timing — pick() currently
  returns synchronously after submit. Need to verify caller flow
  tolerates async-via-frame-completion semantics.

#### T1.10 — Skip the HUD geometry pass when there are no HUD renderables

- **Where**:
  [`render.rs:567-577`](../../crates/renderer/src/render.rs#L567).
- **Pattern**: The HUD geometry pass always begins a render pass on
  the same 4 MRT MSAA targets with `LoadOp::Load + StoreOp::Store`
  (see [`geometry/render_pass.rs:62-83`](../../crates/renderer/src/render_passes/geometry/render_pass.rs#L62)),
  even when `renderables.hud.is_empty()`. On a TBR mobile GPU, this
  is the worst-case anti-pattern: full-screen tile-store of the
  just-written world MRTs to off-chip RAM, then immediate tile-load
  back in, for **zero drawn pixels** at the typical default.
  Approximate cost: ~40 MB of MRT bandwidth at the default mobile
  canvas size, every frame, when HUD is empty.
- **Fix**: Wrap the call site in `if !renderables.hud.is_empty() {
  ... }`. Same treatment for the HUD transparent and HUD render
  pass call sites at
  [`render.rs:1078-1088`](../../crates/renderer/src/render.rs#L1078).
- **Effort**: trivial. **Impact**: Mobile **L** (largest single
  Tier-1 win), Desktop S. **Risk**: low — the HUD depth and
  visibility attachments will retain their world-state contents
  for the remaining passes (which is correct, since nothing was
  meant to be drawn over them anyway).

#### T1.11 — Skip the Line render pass when no lines registered

- **Where**:
  [`render_passes/lines/renderer.rs:201-204`](../../crates/renderer/src/render_passes/lines/renderer.rs#L201)
- **Pattern**: Already returns early when `entries.is_empty()` —
  this is already correct.
- **Fix**: Hoist the check to the *caller*
  ([`render.rs:1027-1033`](../../crates/renderer/src/render.rs#L1027))
  so we also skip the surrounding tracing span and the
  `begin_world_transparent_pass` setup. Actually
  `begin_world_transparent_pass` is only called inside the body,
  so the only extra cost when there are no lines is the tracing
  span — leave alone unless profiling shows otherwise.
- **Effort**: trivial (and may not even be needed). **Impact**: S.

#### T1.12 — `frame_globals` dirty tracking

- **Where**:
  [`frame_globals/mod.rs:180-186`](../../crates/renderer/src/frame_globals/mod.rs#L180).
- **Pattern**: Comment says the buffer "always changes because time
  advances every frame". True today. But the frame-globals buffer is
  only 32 bytes — and most fields (resolution, frame_index%N, etc.)
  are stable across many frames. Time is the only float that always
  moves.
- **Fix**: This one is borderline. Probably not worth changing —
  the writeBuffer of 32 bytes is genuinely cheap on the GPU. The
  cost is the wasm↔JS host call, which is 0.1–1 µs on mobile. **Skip
  this item** unless follow-up profiling shows it matters.

### Tier 2 — Pass restructuring

#### T2.1 — Coalesce HZB mip-build into a single compute pass

- **Where**:
  [`render_passes/hzb/render_pass.rs:97-108`](../../crates/renderer/src/render_passes/hzb/render_pass.rs#L97)
- **Pattern**: For each mip level, opens its own compute pass:
  `for transition in 0..(mip_count - 1) { begin_compute_pass; …;
  end; }`. For a 400×800 canvas that's ⌈log₂(800)⌉ ≈ 10 separate
  compute passes, each with a single workgroup of work.
- **Why this matters on mobile**: Each compute pass = a tile flush
  + a synchronization point. The dispatches themselves are
  microseconds; the pass overhead can be tens of microseconds each
  on mobile drivers. Ten passes × ~30 µs ≈ 300 µs per frame
  recoverable.
- **Fix**: Open one compute pass; set the per-mip bind group inside
  the loop; dispatch; repeat. WebGPU permits multiple dispatches in
  one pass with different bind groups but same pipeline (or
  different pipelines if you re-set). Need to verify the
  synchronization between writes-and-reads-of-the-same-mip is still
  correct — WebGPU's intra-pass barriers are documented at the
  storage-binding boundary. The HZB reduce shader reads mip N to
  write mip N+1, so a `storage` barrier between dispatches is
  needed. WebGPU compute passes have automatic barriers between
  dispatches that share storage bindings — should just work, but
  verify with a layered/MIP-aware test.
- **Effort**: moderate. **Impact**: Mobile M, Desktop S. **Risk**:
  storage-barrier semantics must be re-verified against the WebGPU
  spec; a small test that compares per-mip outputs against the
  per-pass version is mandatory.

#### T2.2 — Coalesce per-shader edge-resolve into a single compute pass

- **Where**:
  [`render_passes/material_opaque/render_pass.rs:238-257`](../../crates/renderer/src/render_passes/material_opaque/render_pass.rs#L238)
- **Pattern**: The comment at line 227 explicitly says *"We still
  isolate each edge dispatch into its own compute pass — cheap
  (pass begin/end overhead is microscopic) and keeps the
  synchronization-scope reasoning local per pipeline."* True on
  desktop, false on mobile. With N material buckets, that's
  N + 2 compute passes (N per-shader + skybox + final blend).
- **Why this matters on mobile**: Same as T2.1 — fewer pass
  open/close events.
- **Fix**: Open one compute pass; iterate buckets; switch pipelines
  inside the pass; dispatch each. Skybox and final blend
  follow in the same pass. WebGPU permits arbitrary pipeline
  switches within a compute pass. The synchronization-scope
  reasoning the comment defends is real — each per-shader bucket
  *atomic-adds* into shared accumulator slots, so the
  intra-bucket order doesn't matter, but the final blend reads
  what every bucket wrote and must come after all bucket
  dispatches. WebGPU's automatic storage-binding barriers between
  intra-pass dispatches that share writes-to-then-reads-from
  storage handle this correctly. Test: existing visual MSAA edge
  output should be bit-identical.
- **Effort**: moderate. **Impact**: Mobile M-L (scales with bucket
  count, so high-material-variety scenes see more). **Risk**:
  storage-barrier semantics same as T2.1. The comment's caveat
  about "synchronization-scope reasoning local per pipeline" is
  the right thing to verify; a per-bucket regression test on a
  scene with ≥ 3 distinct material shader_ids should compare the
  edge accumulator outputs.

#### T2.3 — Drop `STORAGE_BINDING` flag where it's not actually used

- **Where**:
  [`render_textures.rs`](../../crates/renderer/src/render_textures.rs)
  — five textures declare both `STORAGE_BINDING` and
  `RENDER_ATTACHMENT`:
  - `opaque` at [line 433](../../crates/renderer/src/render_textures.rs#L433) — written by opaque compute (storage) AND read in transparent pass via texture binding. The render_attachment is there to support the clear at frame start. Could the clear be done via `clear_buffer`/`textureLoad+textureStore` pattern instead? Probably yes.
  - `decal_color` at [line 488](../../crates/renderer/src/render_textures.rs#L488) — same shape.
  - `composite` at [line 523](../../crates/renderer/src/render_textures.rs#L523).
  - `effects` at [line 538](../../crates/renderer/src/render_textures.rs#L538).
  - `bloom` at [line 553](../../crates/renderer/src/render_textures.rs#L553).
- **Why this matters on mobile**: TBR GPUs gate their on-chip
  tile-cache aggressively when a texture has `STORAGE_BINDING` —
  the driver assumes you might storage-write to it from compute,
  so it can't keep the data on-chip across passes. Every "store"
  to one of these textures becomes a forced off-chip resolve.
- **Fix**: Audit each texture. If a texture is only ever written
  by compute (not as a render attachment), drop `RENDER_ATTACHMENT`.
  If it's only ever written as a render attachment, drop
  `STORAGE_BINDING`. If both are genuinely needed, accept the cost.
  The `composite` / `effects` / `bloom` textures are likely
  pure-compute outputs.
- **Effort**: moderate (per-texture audit + test that nothing
  silently regresses). **Impact**: Mobile **L** — this is probably
  the single biggest TBR-friendly change in this plan. Desktop S.
  **Risk**: WebGPU validation will flag any usage-flag mismatch at
  bind-group creation time, so regressions surface as errors not
  silent corruption.

#### T2.4 — Eliminate `LoadOp::Load` after `StoreOp::Store` antipatterns

- **Where**:
  - HUD geometry pass at [`geometry/render_pass.rs:62-84`](../../crates/renderer/src/render_passes/geometry/render_pass.rs#L62)
    (covered by T1.10 — skipping the pass entirely when HUD is
    empty handles the common case, but the load+store on
    non-empty HUD is still bandwidth-hostile).
  - Decal composite at
    [`render_passes/material_decal/composite.rs:207`](../../crates/renderer/src/render_passes/material_decal/composite.rs#L207).
- **Pattern**: Render-pass attachment chain stores to RAM at pass
  end then loads back from RAM at next pass start. On TBR mobile
  GPUs this forcibly evicts tile-cached data and reloads it.
- **Fix**: Where possible, restructure so the next pass uses
  `LoadOp::Load` only when it has to read from the previous pass's
  output AND the data is too large for tile RAM (so it was going
  to spill anyway). For passes that *would* fit in tile RAM, see
  if the pass can be merged with the previous pass's render-pass
  scope — i.e. add the work as additional draws inside the
  upstream pass.
- **Effort**: architectural for the merge case; moderate for the
  individual restructures. **Impact**: Mobile L when applicable.
  **Risk**: per-case correctness — depth ordering, attachment
  shape compatibility.

#### T2.5 — Defer the opaque mipchain allocation until first transmissive material registers

- **Where**:
  [`render_textures.rs:427-443`](../../crates/renderer/src/render_textures.rs#L427).
- **Pattern**: The opaque texture is allocated with
  `mip_level_count = mip_levels_for(width, height)` (typically
  9–10 mips at common viewport sizes), regardless of whether any
  transmissive material is actually present. The mip storage is
  `~33%` of the base texture size — a few MB on mobile, more on
  desktop. The mips are only filled by
  [`opaque_mipgen`](../../crates/renderer/src/opaque_mipgen.rs)
  when the renderer detects transmissive materials at
  [`render.rs:736-739`](../../crates/renderer/src/render.rs#L736).
- **Fix**: Allocate the opaque texture with `mip_level_count = 1`
  initially; reallocate with the full chain when the first
  transmissive material registers (and free the old texture). The
  reallocation cost is one-time per session.
- **Effort**: moderate (lifecycle / bind-group recreate
  considerations). **Impact**: Mobile S–M (~2–4 MB recovered on
  mobile, ~10–20 MB on desktop). **Risk**: ordering — bind groups
  using the opaque view must be invalidated on reallocation. The
  same `BindGroupCreate::TextureViewRecreate` event already
  handles this.

#### T2.6 — Merge HUD depth into world depth, or skip when HUD is empty

- **Where**:
  [`render_textures.rs:511-515`](../../crates/renderer/src/render_textures.rs#L511).
- **Pattern**: `hud_depth` is a separate full-screen MSAA Depth32
  texture allocated unconditionally. At 400×800 with MSAA 4× and
  Depth32, that's ~5 MB.
- **Fix**: Either:
  - Skip allocation when HUD is empty (lifecycle: defer until
    first HUD renderable registers; rebuild when registered).
  - Or use the world depth attachment for HUD with `LoadOp::Load`
    + a depth bias / new clear range. This is the more invasive
    fix and may complicate HUD-over-world depth-testing semantics.
- **Effort**: moderate (option A) / architectural (option B).
  **Impact**: Mobile M. **Risk**: HUD depth semantics — verify HUD
  always-on-top behaviour still works.

#### T2.7 — Batch the coverage / edge-overflow `mapAsync` promise resolution

- **Where**: [`render.rs:1194`](../../crates/renderer/src/render.rs#L1194)
  (coverage), [`render.rs:1238`](../../crates/renderer/src/render.rs#L1238)
  (edge-overflow).
- **Pattern**: Two `spawn_local` futures per frame (when the
  features are on). Each future ends up enqueued on the JS
  microtask queue, which can run between vsync boundaries and
  cost a wakeup / re-entry into the wasm side.
- **Fix**: Promote the readback completion to a poll-at-frame-start
  pattern: store the future in a `Mutex<Option<Future>>`, check
  it at the next frame's `render()` start, take the result if
  ready. No `spawn_local` at all on the hot path.
- **Effort**: moderate. **Impact**: Mobile M (when features are
  on). **Risk**: backpressure if mapAsync is consistently slower
  than the frame interval — the existing `inflight` flag already
  handles this case by dropping the readback for a frame.

#### T2.8 — Skip Coverage compute when no consumer is wired up

- **Where**:
  [`render.rs:601-627`](../../crates/renderer/src/render.rs#L601)
  + [`render_passes/coverage/render_pass.rs`](../../crates/renderer/src/render_passes/coverage/render_pass.rs).
- **Pattern**: When `features.coverage_lod = true`, the coverage
  compute dispatch runs at full viewport resolution every frame.
  But the consumers (skin-skip, cheap-material LOD) are described
  as "currently parked" in the feature comment
  [`features.rs:84-95`](../../crates/renderer/src/features.rs#L84).
- **Fix**: Add a runtime gate: when `coverage_consumers_active ==
  0`, skip the compute + copy + mapAsync. Consumers register
  themselves at startup; gate is checked per-frame.
- **Effort**: moderate. **Impact**: Mobile L when `coverage_lod`
  is the only thing on; Desktop M. **Risk**: low — explicit gate.

### Tier 3 — Mobile profile wiring (the §1 work)

These items are the actual code work behind §1's "RendererProfile"
mechanism.

#### T3.1 — Define `RendererProfile` enum + `with_profile()` builder method

- **New file**: `crates/renderer/src/profile.rs`.
- **Touches**: `AwsmRendererBuilder` in
  [`lib.rs:1186-…`](../../crates/renderer/src/lib.rs#L1186).
- **Behaviour**: `with_profile(RendererProfile)` sets the
  defaults table from §1.3, then existing `with_features` /
  `with_anti_aliasing` / `with_post_processing` calls can override
  on top. **Order matters**: `with_profile` should be called
  first; later `with_*` calls take precedence.
- **Tests**: a profile-application test that asserts the right
  defaults flip together (similar to the
  `named_tiers_have_strictly_growing_atlas_sizes` test in
  `quality_tier.rs:140`).
- **Effort**: moderate. **Impact**: enabler — no perf change on
  its own.

#### T3.2 — Add `?mobile=…` URL parsing to `web-shared/perf.rs`

- **Touches**:
  [`crates/web-shared/src/perf.rs`](../../crates/web-shared/src/perf.rs).
- **Shape**: `resolve_renderer_profile(default)` —  see §1.2.
- **Effort**: trivial. **Impact**: enabler.

#### T3.3 — Frontend wiring

- **Touches**:
  - [`crates/frontend/model-tests/src/pages/app/canvas.rs:75-83`](../../crates/frontend/model-tests/src/pages/app/canvas.rs#L75)
  - [`crates/frontend/scene-editor/src/context.rs:497-510`](../../crates/frontend/scene-editor/src/context.rs#L497)
  - [`crates/frontend/material-editor/src/main.rs:280-…`](../../crates/frontend/material-editor/src/main.rs#L280)
- **Behaviour**: each frontend's renderer construction calls
  `resolve_renderer_profile(default)` and passes via
  `with_profile`. Default is `Desktop` everywhere.
- **Effort**: trivial.

#### T3.4 — Capability detection for `Depth24Plus`

- **Where**: in `RendererTextureFormats::new`
  ([`render_textures.rs:52-65`](../../crates/renderer/src/render_textures.rs#L52)).
- **Pattern**: `Depth24Plus` is mandatory in the WebGPU baseline
  spec, but `Depth24Plus + Stencil` and `Depth32float` have
  different feature levels. The renderer currently uses
  `Depth32float` for "More precision for thin/close surfaces"
  (comment at line 63). On mobile, the precision difference is
  rarely visible and the bandwidth halving is significant.
- **Fix**: When `RendererProfile::Mobile`, default
  `formats.depth` to `Depth24Plus`. Make sure the depth-bias /
  shadow-cascade math doesn't lose precision (it shouldn't —
  the scene-editor model-tests scenes are < 100 m and Depth24
  has 24 bits of mantissa).
- **Effort**: moderate (verification across passes that read
  depth — opaque, shadow, occlusion, picker, decal).

#### T3.5 — Audit existing tests and benchmarks for profile interaction

- The measurement harness at
  [`crates/frontend/scene-editor/src/actions/measurement.rs`](../../crates/frontend/scene-editor/src/actions/measurement.rs)
  uses `getEntriesByType('measure')` — should be unaffected by the
  profile.
- The `tuning-1k-meshes` and `tuning-10k-meshes` scenes
  ([`crates/scene-schema/examples/generate_tuning_scenes.rs`](../../crates/scene-schema/examples/generate_tuning_scenes.rs))
  should be runnable under both Desktop and Mobile profiles and
  produce comparable per-pass output (with different absolute
  costs).

### Tier 4 — Architectural items

Listed for completeness. These are large enough to warrant their
own design docs.

#### T4.1 — Mobile-format alternative for the `color` texture chain

- **Where**: `RendererTextureFormats.color`
  ([`render_textures.rs:62`](../../crates/renderer/src/render_textures.rs#L62)).
- **Pattern**: `Rgba16float` is 8 B/pixel × 7 textures (opaque +
  transparent + composite + effects + bloom + opaque mips).
  Replacing with `Rg11B10float` (4 B/pixel) halves the bandwidth
  of every color-write/read on mobile.
- **Cost**: tonemapping headroom — Rg11B10 has reduced HDR range
  in blue. The Khronos PBR tonemapper currently used preserves
  blue saturation; switching format would mildly desaturate
  bright blue highlights.
- **Why architectural**: every blit shader, mip-gen pipeline, and
  storage-write pipeline that touches these textures is compiled
  against the format. Switching is a coordinated change across
  ~15 sites and a pipeline-cache invalidation.

#### T4.2 — Visibility-buffer MRT reduction

- **Where**: 4 MRT outputs at the geometry pass
  ([`render_textures.rs:54-61`](../../crates/renderer/src/render_textures.rs#L54)).
- **Pattern**: Mobile tile RAM (0.5–2 MB per tile, varying by
  vendor) cannot hold 4 MRTs at MSAA 4× without spilling. Even at
  MSAA off, 4 MRTs is borderline.
- **Possible directions**:
  - Pack `normal_tangent` into `barycentric_derivatives` (both are
    Rgba16float, both are uv-derived per-pixel quantities, could
    share storage if the encoding is reworked).
  - Move barycentric_derivatives to a compute-pass derivation
    instead of an attachment-output — `dpdx_fine/dpdy_fine` in
    the opaque compute pass reconstructs them on demand.
- **Why architectural**: every geometry-fragment shader template,
  every visibility-buffer reader (classify, opaque, edge_resolve,
  decal_classify), every bind group of those passes touches this
  shape.

#### T4.3 — Optional tile-friendly forward+ path for mobile

- This is what
  [`docs/plans/light-culling.md`](light-culling.md) already
  proposes for light culling. The same approach extends to
  rendering: a feature-flagged forward+ path that does
  geometry+shading in one render pass (no visibility buffer),
  using the GPU light grid from the light_culling design. Targets
  mobile devices where the visibility buffer's pass-break cost
  dominates.
- **Why architectural**: parallel pipeline path; significant
  shader / bind-group / template duplication. Tracking this is
  the same scope as the light-culling plan.

---

## Part 3 — What we don't fix

For the record, these are things that came up during investigation
but aren't worth changing:

- **`frame_globals` writeBuffer per frame** — only 32 bytes,
  genuinely needs to update every frame because `time` advances.
  Cost is dominated by the wasm↔JS boundary, which is microseconds.
- **Light Culling pass span overhead** — the no-op TODO call
  costs less than the tracing span itself; will be load-bearing
  when the [light-culling plan](light-culling.md) lands.
- **`renderable.rs::collect_renderables` allocations** — the
  pool is already reused (Vec capacity preserved across frames),
  per-frame allocation count is zero.
- **Occlusion-instance staging Vec** — already pooled at
  [`occlusion/buffers.rs:125`](../../crates/renderer/src/render_passes/occlusion/buffers.rs#L125).
- **Camera buffer writes** — already has `gpu_dirty` flag and
  epsilon check; no upload when matrices unchanged.
- **Material dispatch hash recomputation** — already cached per
  registration cycle, not per frame.
- **BVH rebuild cadence** — already throttled; mobile profile
  doubles the throttle but otherwise the logic is right.

---

## Part 4 — Sequencing

Recommended implementation order. Each chunk is a sensible commit
boundary.

**Sprint 1 — Mechanical wins (1–2 days)**

- T1.4 — Cache `current_context_texture_size`
- T1.1 — Cache exposure uniform
- T1.2 — Cache decal classify header
- T1.3 — Early-exit Morphs/Skins write_gpu
- T1.5 — Skip Effects pass when off
- T1.7 — Early-exit Shadows write_gpu
- T1.8 — Coverage zero-out via clear_buffer
- T1.10 — Skip HUD geometry/transparent when empty ← **the big one**

Expected mobile gain: ~3–5 ms / frame, driven mostly by T1.10 and
T1.7.

**Sprint 2 — Mobile profile (2–3 days)**

- T3.1 — `RendererProfile` enum + builder
- T3.2 — `?mobile=…` URL param
- T3.3 — Frontend wiring
- T3.4 — Capability detection for Depth24Plus

Expected mobile gain: another ~5–10 ms / frame on the model-tests
page, dominated by MSAA off + low shadow tier.

**Sprint 3 — Pass restructuring (3–5 days)**

- T2.3 — Drop unnecessary `STORAGE_BINDING` flags ← biggest TBR win
- T2.1 — Coalesce HZB compute passes
- T2.2 — Coalesce edge-resolve compute passes
- T2.7 — Batch mapAsync completion polling
- T2.5 — Defer opaque mipchain
- T2.8 — Gate coverage compute on consumer presence

Expected mobile gain: another ~3–8 ms / frame.

**Sprint 4 — HUD / decal restructuring (1–2 days)**

- T2.4 — `LoadOp::Load` antipattern fixes (where feasible without
  architectural change)
- T2.6 — HUD depth lifecycle

**Sprint 5 (later) — Architectural**

- T4.x items only after the above land and we re-measure on mobile.
  Most likely candidates: T4.1 (`Rg11B10` color chain) or T4.3
  (forward+ alongside light-culling plan).

---

## Part 5 — Stress-test scenes

The user mentioned future "stress-test scenes" for mobile. The
existing tuning scenes (`tuning-1k-meshes`, `tuning-10k-meshes`)
generated by
[`crates/scene-schema/examples/generate_tuning_scenes.rs`](../../crates/scene-schema/examples/generate_tuning_scenes.rs)
are mesh-count-dominated. For mobile we'll also want:

- **`tuning-mobile-single-character`** — one animated character
  (skinned mesh, one directional light, one point light, no
  bloom, no decals). The model-tests "just one character" case.
  Targets: 60 fps mobile under `?mobile=true`.
- **`tuning-mobile-cluttered-scene`** — moderate poly density
  (~50 small meshes), 4 point lights, 1 directional, no transparent.
  Mid-range mobile target.
- **`tuning-mobile-msaa-stress`** — same as above but MSAA forced
  on, to validate the MSAA edge-resolve restructuring (T2.2)
  doesn't regress visually.
- **`tuning-mobile-transparency-stress`** — a wall of transparent
  objects covering the full screen. Tests the transparent-pass
  bandwidth pinch point.

These should be authored against the same harness as the existing
tuning scenes — see
[`measurement.rs`](../../crates/frontend/scene-editor/src/actions/measurement.rs)
for the loader entry point and
[`PERFORMANCE.md §7`](../PERFORMANCE.md) for the methodology.

Each scene should be benchmarked under three profiles (Mobile,
Desktop, Cinema) and the per-pass numbers recorded in a baseline
table in `docs/PERFORMANCE.md` so we can spot regressions.

---

## Part 6 — How to validate progress

After each sprint, capture a mobile trace using the workflow from
[`docs/perf-tracing.md`](../perf-tracing.md):

1. Load the deployed site on the target device with
   `?trace=sub-frame`.
2. Capture a 3–5 second trace via Chrome DevTools remote.
3. Compare against the May 2026 baseline:
   - **Main-thread**: median `FireAnimationFrame` dur — currently
     5.4 ms, target ≤ 3 ms.
   - **GPU swap**: median compositor `Swap` phase — currently
     65 ms, target ≤ 33 ms (i.e. 30 fps presented) for the mobile
     profile.
   - **Frame interval distribution**: currently median 34 ms,
     target median ≤ 17 ms (60 fps on a mobile-class device with
     `?mobile=true`).
4. Run `read_render_pass_timings(0)` from the scene-editor
   measurement harness (under `?trace=sub-frame`) and confirm no
   individual pass crosses 3 ms on mobile.

Don't expect the model-tests page to reach 60 fps on a 2020-era
phone even after every item in this plan. The realistic target
under `?mobile=true` is **30 fps stable**, with `?mobile=false`
performing at whatever the device can sustain (probably 15–25 fps
on the same hardware). The point of the mobile profile is not
"make every device fast"; it's "give the engine a coherent
mobile-friendly default that targets the most common mobile-class
hardware".

---

## Cross-references

- Architecture and per-frame budget: [`PERFORMANCE.md`](../PERFORMANCE.md)
- Per-pass measurement workflow:
  [`docs/perf-tracing.md`](../perf-tracing.md)
- Light culling — companion plan with overlapping mobile concerns:
  [`docs/plans/light-culling.md`](light-culling.md)
- Existing shadow quality tiers:
  [`shadows/quality_tier.rs`](../../crates/renderer/src/shadows/quality_tier.rs)
- Existing edge-budget mobile/desktop split:
  [`material_opaque/edge_buffers.rs:130-150`](../../crates/renderer/src/render_passes/material_opaque/edge_buffers.rs#L130)
- Existing adaptive optimization policy:
  [`optimization_policy.rs`](../../crates/renderer/src/optimization_policy.rs)
- Tracing tier mechanism (background context for `?mobile=…` URL
  param shape):
  [`crates/web-shared/src/perf.rs`](../../crates/web-shared/src/perf.rs)
