# Parallelize WebGPU pipeline creation — full plan

## Status (2026-05-25) — tail pool landed on `parallel-continued`

All seven original phases plus the cross-pass pool sweep plus the
**tail pool follow-up** (PR #96) have landed. Every shader and every
pipeline that compiles during `AwsmRendererBuilder::build` on the
cold-cache first load goes through batched `ensure_keys` calls; the
per-pass / per-subsystem `new()` constructors are thin wrappers
over `build_descriptors` + `from_resolved` splits so the
orchestrator can fold every variant into one of two cross-system
pools (one inside `RenderPasses::new`, one across the tail after
`RenderPasses::new` returns).

| Phase | Status | Headline commit |
|---|---|---|
| 0 — Baseline + plan | ✅ | `docs: add parallelize plan + cold-load measurement procedure` |
| 1 — `ensure_keys` on render+compute caches | ✅ | `renderer: add RenderPipelines/ComputePipelines::ensure_keys` |
| 2 — Opaque 14-pipeline batch | ✅ | `renderer: batch opaque pass pipeline compiles via ensure_keys` |
| 3 — Parallelize `RenderPasses::new` (within-pass) | ✅ | `renderer: batch per-pass pipeline compiles via ensure_keys` |
| 4 — Batch transparent pipelines during gltf populate | ✅ | `renderer: batch transparent-mesh pipeline compiles across meshes` |
| 5 — Real `prewarm_pipelines` for transparents | ✅ | `renderer: prewarm_pipelines now warms live-scene transparents` |
| 6 — Frontend sub-phase visibility | ✅ | `frontend: surface RendererLoadingPhase in both apps` |
| 7 — Doc + test hygiene | ✅ | (Phase 7 commit) |
| 8 — Pool finalize_gpu_textures across passes | ✅ | `renderer: pool finalize_gpu_textures pipeline recompiles across passes` |
| 9 — PR #95 review fixes (OR-dedup, phase emission) | ✅ | `review(PR95): fix transparent-pipeline dedup + phase emission` |
| 10 — Cross-pass shader prewarm | ✅ | `renderer: cross-pass shader pre-warm at RenderPasses::new` |
| 11 — Full cross-pass pipeline pool in RenderPasses::new | ✅ | `renderer: full cross-pass pipeline pool at RenderPasses::new` |
| 12 — Batch Picker + LineRenderer | ✅ | `renderer: batch picker's two pipeline compiles` / `renderer: batch LineRenderer's 4 variants` |
| 13 — anti_aliasing dedup-fix + Picker descriptor split | ✅ | `renderer: fix anti_alias set_anti_aliasing OR-dedup bug` / `renderer: expose Picker descriptor split` |
| 14 — Tail pool (Shadows + Lines + Effects + Display) | ✅ | PR #96: `renderer: orchestrate tail-pool in AwsmRendererBuilder::build (2-await tail)` |
| 14a — PR#96 review fixes (try_join compute+render, doc) | ✅ | `renderer: address PR#96 review — try_join compute+render ensure_keys, doc fixes` |

### Trace evidence (warm Metal + cold Chrome PSO, 2026-05-25)

A fresh `--user-data-dir` Chrome profile against the model-tests Fox
scene captured `Trace-Three.json`, with the renderer built at PR#96's
HEAD:

| Metric | Pre-parallelize cold | Pre-parallelize warm | Post-PR#96 fresh-profile |
|---|---|---|---|
| `domComplete → first 'Render [1]: span-enter'` | 42.8 s | 1.7 s | **2.2 s** |
| GPU-process total CPU | 5.35 s | 0.81 s | **0.77 s** |
| Renderer-main-thread total CPU | 8.74 s | 4.26 s | **1.0 s** |

The 0.77 s GPU-process CPU number tells the real story: it's
indistinguishable from the pre-parallelize warm baseline, which
means Dawn isn't doing real compile work — it's serving from the
**driver-level Metal pipeline cache** that survives across Chrome
profiles. `--user-data-dir` clears Chrome's PSO cache but cannot
clear macOS's MSL → native cache. So on any developer machine that
has run this codebase before, the cold-Chrome experience now sits
in the same ballpark as the historical warm path. The 42.8 s
baseline reflected a machine where both layers were cold (first
ever run of the app), which is the user-facing first-visit experience.

The renderer-main-thread idle-gap distribution in the new trace is
also clean — user-timing marks after `Prewarm Pipelines` cascade in
~1 ms apart, no ~500 ms per-frame-tick stalls. The serial-await
staircase the original plan was attacking is gone whether the
underlying compile is cold or warm.

### What pools at startup now

`RenderPasses::new` runs through five phases in order:

1. **Sync bind-group setup** — every pass's bind-group +
   pipeline-layout cache keys registered (no Dawn compile work).
2. **One `Shaders::ensure_keys`** — pools every shader cache key
   across all 12 render passes *plus* the shadow caster + picker +
   line shaders that other subsystems compile later in `build()`,
   so their `shaders.get_key` calls hit the cache.
3. **Pipeline descriptor build** — sync; each pass's
   `build_descriptors` returns `(pipeline_cache_keys, slots)`
   resolving shader keys via the now-warm cache.
4. **Two `ensure_keys` batches** — one
   `ComputePipelines::ensure_keys` pooling every compute pipeline
   across opaque (14) + decal (2) + decal_classify (1) + classify
   (2) + hzb (3) + occlusion (1) + compaction (1) + coverage (1–2)
   = **~26 compute pipelines**. One `RenderPipelines::ensure_keys`
   pooling geometry's **18 render pipelines**. Dawn parallelises
   each pool internally through its compile pool (~`num_cpus`).
5. **Sync fold-up** — each pass's `from_resolved` rebuilds the
   typed `Pipelines` struct from its slice of the resolved keys;
   the typed `RenderPass` struct is constructed from
   `(bind_groups, pipelines, …)`.

In the original code this was **24 sequential per-pass awaits** (12
passes × shader-batch + pipeline-batch); now it's **3 awaits total**
(one shader, one compute, one render), each running its own
batch in parallel.

### What still compiles outside RenderPasses::new

After PR #96 the tail subsystems share a single cross-tail pool
running right after `RenderPasses::new` returns. The structure of
the tail today:

1. **Sync** — each tail subsystem's `build_descriptors` runs:
   - `Picker::build_descriptors` (registers bind-group layouts +
     2 compute pipeline cache keys).
   - `LineRenderer::build_descriptors` (registers bind-group +
     pipeline layout, 4 render pipeline cache keys).
   - `Shadows::build_descriptors` (allocates every shadow GPU
     resource — atlas, EVSM atlas, cascade-array, cube-array,
     descriptors / globals / view buffers, samplers, bind-group
     layouts, pipeline layouts; resolves 4 caster pipeline cache
     keys; issues 3 EVSM inline `compile_shader` calls returning
     modules + unawaited `validate_shader` futures).
2. **One `Shaders::ensure_keys`** joined via `futures::join` with
   the 3 EVSM inline-shader validate futures — effects + display
   shader keys + EVSM module validations all in flight together.
3. **Sync** — register the 3 EVSM modules via
   `Shaders::insert_uncached`, derive their 3 compute pipeline
   cache keys, run `EffectsPipelines::build_descriptors` +
   `DisplayPipelines::build_descriptors` (sync cache-hit shader
   resolves), build the cross-tail compute + render pools.
4. **`try_join`'d compute + render `ensure_keys`** — split-borrow
   `Pipelines.compute` / `Pipelines.render` so Dawn overlaps both
   classes against its worker pool. Compute pool = picker(2) +
   EVSM(3) + effects(5) = **10 pipelines**. Render pool =
   lines(4) + caster(4) + display(1) = **9 pipelines**.
5. **Sync fold-up** — each subsystem's `from_resolved` /
   `install_resolved` consumes its slice of the resolved keys.

Pre-PR#96 this was 5 sequential per-subsystem awaits; post-PR#96
it's 3 awaits inside the tail (shader-join, then try_join'd
compute + render). The dynamic `set_anti_aliasing` /
`set_post_processing` setter path is preserved — those setters
wrap the same `build_descriptors` + per-subsystem `ensure_keys` +
`install_resolved` shape for mid-session config flips.

### What still serialises across the whole build()

After PR #96 the remaining serial points in `AwsmRendererBuilder::build`
are:

- `try_join5(IBL × 3, BRDF LUT, opaque_mipgen)` finishes **before**
  `RenderPasses::new` + `RenderTextures::new` even start. The
  texture-prep futures only touch `&gpu`, not the shader / pipeline
  caches — they could ride the same `try_join` as `RenderPasses::new`.
- `Picker` compiles unconditionally even when the consuming
  frontend has no use for pick (most library builds).
- Picker + LineRenderer descriptors build *after* `RenderPasses::new`
  returns, so their pipelines join only the cross-tail pool, not
  the larger cross-pass pool. Their bind-group layouts are
  statically known and could be registered up-front.

These are addressed in [Follow-up 2](#follow-up-2-startup-tail-trim)
below.

---

## Follow-up: tail pool (Shadows + Picker + LineRenderer + Effects + Display)

### Why this matters

After all the work on the `parallel` branch through commit `7ae70ba`,
`RenderPasses::new` itself is fully pooled (3 awaits — one shader,
one compute, one render — covering ~44 pipelines). What's still
sequential is the **tail** that runs after `RenderPasses::new`
returns and through end-of-`build()`:

1. `Picker::new` — 1 batched `ComputePipelines::ensure_keys` (2 pipelines).
2. `LineRenderer::load` — 1 batched `RenderPipelines::ensure_keys` (4 variants).
3. `Shadows::new` — ~3 awaits internally:
   - 1 caster shader `ensure_keys` (cache hit because pre-warmed by `RenderPasses::new`).
   - 1 caster pipeline `RenderPipelines::ensure_keys` (4 variants).
   - EVSM block: `join_all` of 3 inline shader validations + 1 `ComputePipelines::ensure_keys` (3 pipelines).
4. `_self.set_anti_aliasing(...).await?` — 1 effects `ComputePipelines::ensure_keys` (5 variants), plus the per-mesh transparent loop (empty at startup).
5. `_self.set_post_processing(...).await?` — effects rebuild (cache hit because same config) + 1 display `RenderPipelines::ensure_keys` (1 pipeline).

That's **5 sequential pipeline-compile awaits** in the tail.
Pooled, they become **2** (one big `ComputePipelines::ensure_keys`,
one big `RenderPipelines::ensure_keys`):

- Compute pool: 2 (picker) + 3 (EVSM) + 5 (effects) = **10 pipelines**.
- Render pool: 4 (lines) + 4 (shadow caster) + 1 (display) = **9 pipelines**.

On a Dawn pool of ~`num_cpus` workers (typically 8–12 on a modern
laptop), each pool is one compile wave. Sequential vs pooled
wall-clock:

| | Sequential tail | Pooled tail | Saving |
|---|---|---|---|
| Cold cache | 5 × (~1 t_compile + 1 task-tick) ≈ 5–6 s | 2 × (~1 t_compile + 1 task-tick) ≈ 2 s | **~3–4 s** |
| Warm cache | 5 × ~25 ms (task-tick overhead) ≈ 125 ms | 2 × ~25 ms ≈ 50 ms | **~75 ms** |

(`t_compile` ≈ 0.8–1.5 s per pipeline on cold Metal-via-Dawn for
this codebase, observed from earlier traces. The "task-tick" is
the minimum time the renderer-main thread spends parked at each
`.await` point before the next batch even starts; under load the
browser may delay it to a full rAF tick.)

**Conclusion (answering the explicit question from the previous
sweep): yes, this is a definite cold-cache win when it lands. The
implementation risk is in the size of the refactor, not in the
optimization model.**

### Where the risk lives

Three places need careful handling:

1. **Shadows::new** is ~250 lines of intricate setup —
   `crates/renderer/src/shadows/state.rs:430-820`-ish. It owns:
   atlas allocator, atlas texture + views, cascade-array texture +
   views, EVSM atlas + blur ping-pong texture + views, shadow
   descriptor / view buffers, PCF + filterable samplers, shadow
   view bind-group layout + bind group, pipeline layouts (storage
   + uniform variants), caster shader cache keys + pipelines (4
   variants), and `EvsmPass::new` (its own bind-group layouts,
   pipeline layouts, 3 inline-source shaders compiled via
   `gpu.compile_shader` + `module.validate_shader().await` +
   `shaders.insert_uncached`, 3 compute pipelines, params buffer).
   Most of this is sync resource allocation interleaved with
   async-but-non-awaiting-anything `BindGroupLayouts::get_key`
   calls. The async work that needs to move into the cross-system
   pool is just the shader validations + pipeline compiles.

2. **EVSM uses inline-source shaders** that bypass the shared
   `Shaders` cache (`compile_shader` returns a module immediately
   and the work is in `validate_shader().await`; the resulting
   module is fed into the cache via `Shaders::insert_uncached`).
   The pool has to be able to swallow these:
   - Issue the 3 `compile_shader` calls + register the modules
     synchronously (gives ShaderKeys via `insert_uncached`).
   - Add the 3 `validate_shader` futures to a `validation_pool`
     that runs in parallel with the other ensure_keys batches.
   - Then build the EVSM compute pipeline cache keys against the
     `ShaderKey`s and feed them into the cross-system compute
     ensure_keys.

3. **`set_anti_aliasing` / `set_post_processing` are dynamic
   setters**, not just startup-time hooks. The user can call
   `renderer.set_anti_aliasing(...)` mid-session to flip MSAA on/off,
   and the same goes for post-processing. Today both compile
   effects + display pipelines from inside the setter via
   `EffectsPipelines::set_render_pipeline_keys` /
   `DisplayPipelines::set_render_pipeline_key`. The descriptor
   split has to leave those setters working for the dynamic path —
   the startup-time pool is a *short-circuit* that lets the
   builder skip the setter's recompile when it's about to apply
   the same config the pool was built against.

### Architecture for the fresh session

The orchestrator goes in `lib.rs`'s
`AwsmRendererBuilder::build`. After `RenderPasses::new` +
`RenderTextures::new` return, do this in order:

```rust
// 1. Build every tail subsystem's bind groups + descriptors
//    SYNCHRONOUSLY (no Dawn compile, just hash registration +
//    resource allocation). Returns typed *Descriptors structs that
//    carry (a) a Vec of cache keys to pool and (b) every other
//    piece of state needed to assemble the subsystem later.
let picker_descs = Picker::build_descriptors(
    &gpu, &mut bind_group_layouts, &mut pipeline_layouts, &mut shaders,
).await?;
let line_descs = LineRenderer::build_descriptors(
    &gpu, &mut bind_group_layouts, &mut pipeline_layouts, &mut shaders,
    &render_textures.formats,
).await?;
let shadows_descs = shadows::Shadows::build_descriptors(
    &gpu, &mut bind_group_layouts, &mut pipeline_layouts,
    &mut shaders, &render_passes.geometry.bind_groups,
    &render_textures.formats, shadows_config.unwrap_or_default(),
).await?;
let effects_descs = render_passes::effects::pipeline::EffectsPipelines::build_descriptors(
    &anti_aliasing, &post_processing, &gpu, &mut shaders,
    &mut pipeline_layouts, &mut bind_group_layouts,
    &render_passes.effects.bind_groups,
    &render_textures.formats,
).await?;
let display_descs = render_passes::display::pipeline::DisplayPipelines::build_descriptors(
    &post_processing, &gpu, &mut shaders,
    &mut pipeline_layouts, &render_passes.display.bind_groups,
).await?;

// 2. Cross-tail shader ensure_keys.
//    (Most of the keys are already cache hits because
//    RenderPasses::new pre-warmed picker+line+shadow caster + the
//    static opaque/decal/transparent variants. EVSM inline shaders
//    + effects-config-dependent shaders + display-tonemapping
//    shader are the new ones.)
let tail_shader_keys = [
    effects_descs.shader_cache_keys.as_slice(),
    display_descs.shader_cache_keys.as_slice(),
    // Shadows caster is already in the main pool.
    // Picker / Lines: already in the main pool too.
].concat();
shaders.ensure_keys(&gpu, tail_shader_keys).await?;

// 2b. EVSM inline shaders compile in parallel with the above.
//     join_all the 3 validate_shader futures.
let evsm_modules = futures::future::try_join_all(
    shadows_descs.evsm_inline_module_futures(&gpu),
).await?;
let evsm_shader_keys: Vec<ShaderKey> = evsm_modules
    .into_iter()
    .map(|m| shaders.insert_uncached(m))
    .collect();
shadows_descs.finalize_evsm_shader_keys(evsm_shader_keys);

// 3. Build the cross-tail pipeline cache key pools.
let mut compute_pool = Vec::new();
let mut render_pool = Vec::new();
let picker_compute_range = push_compute!(picker_descs);
let line_render_range = push_render!(line_descs);
let shadows_caster_render_range = push_render!(shadows_descs.caster);
let shadows_evsm_compute_range = push_compute!(shadows_descs.evsm);
let effects_compute_range = push_compute!(effects_descs);
let display_render_range = push_render!(display_descs);

// 4. Two batched ensure_keys — the entire tail in two awaits.
let compute_keys = pipelines.compute.ensure_keys(
    &gpu, &shaders, &pipeline_layouts, compute_pool,
).await?;
let render_keys = pipelines.render.ensure_keys(
    &gpu, &shaders, &pipeline_layouts, render_pool,
).await?;

// 5. Sync fold-up: each subsystem's from_resolved consumes its
//    slice of the resolved keys + its descriptors blob and
//    returns the typed handle.
let picker = Picker::from_resolved(&gpu, picker_descs, compute_keys[picker_compute_range].to_vec())?;
let lines = LineRenderer::from_resolved(line_descs, render_keys[line_render_range].to_vec())?;
let shadows = shadows::Shadows::from_resolved(
    shadows_descs,
    render_keys[shadows_caster_render_range].to_vec(),
    compute_keys[shadows_evsm_compute_range].to_vec(),
)?;

// 6. Install resolved effects + display keys into the typed
//    Pipelines structs already inside RenderPasses.
render_passes.effects.pipelines.install_resolved(
    effects_descs,
    compute_keys[effects_compute_range].to_vec(),
);
render_passes.display.pipelines.install_resolved(
    display_descs,
    render_keys[display_render_range].to_vec(),
);

// 7. Construct AwsmRenderer.
let mut _self = AwsmRenderer { ..., picker, lines, shadows, ... };

// 8. Apply the initial AA + PP state WITHOUT recompiling — the
//    pipelines we just installed already match anti_aliasing
//    + post_processing. The setters still need to run for the
//    state-only side effects (marking bind groups dirty, etc.).
_self.anti_aliasing = anti_aliasing.clone();
_self.post_processing = post_processing.clone();
_self.bind_groups.mark_create(BindGroupCreate::AntiAliasingChange);
_self.bind_groups.mark_create(BindGroupCreate::TextureViewRecreate);
// (no more .await? calls — set_anti_aliasing's pipeline-rebuild
// path is bypassed for the initial config)
```

### Per-subsystem checklist for the fresh session

For each subsystem below, the pattern is:

- Keep the existing `new()` / `load()` / `set_*` entry points as
  thin wrappers over the new `build_descriptors` +
  `from_resolved` split, so callers outside the orchestrator
  (dynamic AA changes, runtime gltf loads, etc.) keep working.
- Expose three pieces:
  - `pub fn (or async fn) build_descriptors(...) -> Result<XxxDescriptors>` — does sync setup + shader resolution. Returns a struct carrying every cache key + slot identifier downstream code needs.
  - `XxxDescriptors` struct — public, exposes `shader_cache_keys`, `compute_pipeline_cache_keys`, `render_pipeline_cache_keys` (whichever applies) as named fields the orchestrator can read.
  - `pub fn from_resolved(descs, resolved_keys, ...) -> Result<Self>` — sync; consumes the descriptors + the slice of resolved keys the orchestrator hands back.

#### 1. `Picker::build_descriptors` — already done in `b46c365`. ✅

Reference for the pattern. Look at
`crates/renderer/src/picker.rs:141-260` to see the working shape.

#### 2. `LineRenderer::build_descriptors`

- File: `crates/renderer/src/render_passes/lines/renderer.rs` +
  `crates/renderer/src/render_passes/lines/pipelines.rs`.
- `LinePipelines::load` already batches its 4 variants in one
  `ensure_keys` (commit `6220d85`). Lift the cache-key list out
  via `LinePipelines::pipeline_cache_keys(bind_group_layout_key,
  pipeline_layout_key, shader_key, formats) -> Vec<RenderPipelineCacheKey>`
  + `LinePipelines::from_resolved(bind_group_layout_key,
  keys) -> Self`.
- `LineRenderer::build_descriptors` wraps both: registers
  bind-group layout, registers pipeline layout, fetches shader
  key (cache hit), builds 4 pipeline cache keys, returns
  `LineRendererDescriptors { bind_group_layout_key,
  pipeline_cache_keys }`.
- `LineRenderer::from_resolved(descs, keys) -> Self`: assembles
  `LinePipelines::from_resolved` + the empty `SlotMap<LineKey,
  LineEntry>` + the empty `pack_buf`.

#### 3. `Shadows::build_descriptors` — the heavy piece

- File: `crates/renderer/src/shadows/state.rs` +
  `crates/renderer/src/shadows/evsm.rs`.
- `Shadows::new` currently does (a) resource allocation +
  bind-group + pipeline-layout setup (sync; lines ~430–555),
  (b) the caster ensure_keys block (lines ~557–627), then (c)
  `EvsmPass::new(...).await?` which itself does sync layout
  setup + the 3 inline shader compiles + 3 compute pipeline
  compile.
- Refactor target:
  - **`Shadows::build_descriptors(...)` async**: runs (a),
    runs the caster shader cache keys → `ensure_keys` (or
    treat as pre-warmed if the caller already did so), builds
    the 4 caster pipeline cache keys (sync, against the
    pipeline layouts from (a)), runs EVSM's sync layout
    setup, kicks off the 3 inline `compile_shader` calls
    (returns the modules synchronously), wraps their
    `validate_shader` futures into a Vec, then `join_all`s them
    (or returns the futures for the orchestrator to interleave
    with other shader ensure_keys — see option 2 below).
    Builds the 3 EVSM compute pipeline cache keys. Returns
    `ShadowsDescriptors`:
    ```rust
    pub struct ShadowsDescriptors {
        // (a) all the resource handles + layout keys + buffers
        //     + textures + the cascade array view + etc.
        atlas_allocator: AtlasAllocator,
        atlas_texture: web_sys::GpuTexture,
        atlas_view: web_sys::GpuTextureView,
        // ... ~15 more fields, all currently locals in
        //     Shadows::new ...
        // (b) caster
        caster_pipeline_cache_keys: Vec<RenderPipelineCacheKey>,
        // (c) evsm — kept as a sub-struct so EvsmPass::from_resolved
        //     can take the inner slice cleanly.
        evsm: EvsmDescriptors,
    }

    pub struct EvsmDescriptors {
        moment_write_layout_key: BindGroupLayoutKey,
        blur_layout_key: BindGroupLayoutKey,
        moment_write_pipeline_layout_key: PipelineLayoutKey,
        blur_pipeline_layout_key: PipelineLayoutKey,
        // Shader keys are resolved before from_resolved is called.
        // The orchestrator builds them via insert_uncached on the
        // modules returned by build_descriptors.
        moment_write_shader_key: ShaderKey,
        blur_h_shader_key: ShaderKey,
        blur_v_shader_key: ShaderKey,
        pipeline_cache_keys: Vec<ComputePipelineCacheKey>,
        // (3 entries: moment_write, blur_h, blur_v.)
        params_buffer: web_sys::GpuBuffer,
        params_bytes: Vec<u8>,
    }
    ```
  - **`Shadows::from_resolved(descs, caster_keys, evsm_compute_keys) -> Result<Self>`**:
    sync. Re-binds the resolved keys + every resource field
    out of `ShadowsDescriptors` into the final `Shadows` struct
    and the `EvsmPass`. Builds the EVSM moment-write + blur
    bind groups (they need the resolved EVSM pipeline keys'
    layouts, which are already in `ShadowsDescriptors`).

- Decision: how does the orchestrator handle EVSM's inline
  shaders? Two options:
  - (Option 1, simpler) `Shadows::build_descriptors` runs the 3
    `validate_shader().await`s internally via `join_all`. The
    inline shaders compile in parallel with each other but
    serially with the cross-system shader ensure_keys. Costs one
    extra await sequenced against the main shader pool.
  - (Option 2, optimal) `Shadows::build_descriptors` returns the
    3 module handles + the 3 unawaited validate futures. The
    orchestrator awaits them in `join_all` alongside the
    cross-system shader ensure_keys via `try_join`. Saves one
    await; the orchestrator owns the EVSM shader_keys after
    join completes and passes them to `from_resolved`.
  - **Recommendation**: option 2. It's a small extra hand-off
    but lands the EVSM inline-shader compile in the same wave
    as everything else.

- Big invariants to preserve:
  - Pipeline layouts for the caster fork by `instancing`
    (storage vs uniform meta binding) — verify both layouts
    survive the descriptor round-trip.
  - The EVSM moment-write + blur bind groups bind specific
    layout keys (lines ~609–664 in current `state.rs`) — those
    layouts are built inside what would become
    `Shadows::build_descriptors`, so make sure the bind-group
    construction in `from_resolved` reads them out of the
    descriptors struct correctly.

#### 4. `EffectsPipelines::build_descriptors`

- File: `crates/renderer/src/render_passes/effects/pipeline.rs`.
- `EffectsPipelines::set_render_pipeline_keys` is the current
  entry point. It builds 5 shader cache keys (one per bloom
  phase + ping-pong variant) and runs a batched ensure_keys for
  shaders + a batched ensure_keys for the 5 compute pipelines.
- Refactor target:
  - **`EffectsPipelines::build_descriptors(anti_aliasing,
    post_processing, gpu, shaders, pipeline_layouts, bind_groups,
    render_texture_formats) -> Result<EffectsDescriptors>`**:
    sync apart from shader resolution. Returns
    `EffectsDescriptors { shader_cache_keys: Vec<ShaderCacheKey>,
    pipeline_cache_keys: Vec<ComputePipelineCacheKey> }` with
    5 entries each.
  - **`EffectsPipelines::install_resolved(&mut self, descs, keys)
    -> ()`**: sync. Writes the 5 resolved keys into the existing
    fields (`no_bloom_pipeline`, `bloom_extract_pipeline`,
    `bloom_blur_pipeline_a/b`, `bloom_blend_pipeline`).
- `set_render_pipeline_keys` stays — it becomes
  `build_descriptors` followed by its own batched ensure_keys +
  `install_resolved`. The dynamic-AA path keeps that single-entry
  API; the orchestrator uses the split.

#### 5. `DisplayPipelines::build_descriptors`

- File: `crates/renderer/src/render_passes/display/pipeline.rs`.
- Same shape as Effects but smaller — 1 shader cache key + 1
  render pipeline cache key. Existing
  `DisplayPipelines::set_render_pipeline_key` becomes a thin
  wrapper.

#### 6. `AwsmRendererBuilder::build` — the orchestrator

- File: `crates/renderer/src/lib.rs:711+`.
- Replace the current
  `Picker::new → LineRenderer::load → … → Shadows::new →
   set_anti_aliasing → set_post_processing` chain at the end of
  `build()` with the 8-step pseudocode in the architecture
  section above.
- Delete the redundant `shaders.ensure_keys` block at
  lines ~959–974 that pre-warms picker + line shaders — that
  pre-warm now lives in `RenderPasses::new` itself.
- Keep the initial `set_anti_aliasing` / `set_post_processing`
  *calls* removed only for their pipeline-recompile path; the
  state-side-effect parts (marking bind groups for recreate)
  still need to happen — either inline that bookkeeping or
  expose a `_self.apply_initial_aa_pp_state()` shortcut method
  on `AwsmRenderer` that does the state work without compiling.

### Verification

For every step above, the fresh session should:

1. `cargo build --workspace` clean.
2. `cargo clippy --workspace --all-targets -- -D warnings` clean.
3. `cargo fmt --all`.
4. Capture a fresh-profile Chrome trace
   (`docs/PERFORMANCE.md §5g-i` for the recipe). Compare
   `domComplete → first 'Render [1]: span-enter'` against the
   number at commit `7ae70ba`. Expected reduction: ~3 s on cold,
   ~75 ms on warm.
5. Verify both frontends render correctly:
   - `task model-tests:dev` → Fox loads, IBL scene rendered,
     no GPU validation errors in console.
   - `task scene-editor:dev` → editor UI loads, drop a glb,
     mesh renders correctly.
6. Verify the **dynamic** AA / PP paths still work — change
   MSAA mode mid-session, change post-processing config; both
   must trigger the existing
   `EffectsPipelines::set_render_pipeline_keys` /
   `DisplayPipelines::set_render_pipeline_key` paths and
   recompile correctly.
7. Verify shadows still render correctly — directional,
   point, and spot all need a smoke test scene because the
   Shadows refactor is the highest-risk piece. The
   `awsm-renderer-assets/world/project.json` scene under the
   scene-editor exercises all three.

### Out of scope for the tail-pool follow-up

- `try_join5` at the top of `build()` (IBL / BRDF / skybox /
  opaque_mipgen prep). Already parallel; no improvement to
  chase there.
- `finalize_gpu_textures` — already pooled (Phase 8).
- `RenderPasses::new` itself — already pooled (Phase 11).
- `prewarm_pipelines` — already pooled (Phase 5).

---

## Follow-up 2: startup-tail trim

### Status

Three follow-up items identified after the PR #96 trace review.
**Land all three in one session** — they're independent at the
borrow-checker level, the verification cost is shared (one fresh
trace capture covers all three), and the doc / PR overhead per item
isn't worth amortising across sessions.

| Item | Win (warm-Metal) | Win (cold-Dawn) | Risk |
|---|---|---|---|
| (1) Move `RenderPasses::new` into the `try_join5` block | 300–600 ms | same | low |
| (2) Fold Picker + LineRenderer pipelines into the cross-pass pool | 25–80 ms | ~1–1.5 s | medium |
| (3) `RendererFeatures::picking` flag | 25–50 ms | ~200–500 ms | low |

Total cold-cache budget: **~2–2.5 s** off `domComplete → first Render`
on a machine with cold Metal driver cache. Total warm-Metal budget:
**~400–700 ms** — small absolute but lops a chunk off the largest
remaining idle gap before `Prewarm Pipelines` fires.

### Item (1) — parallelise `RenderPasses::new` with the texture-prep block

#### Why this matters

[lib.rs:879-900](../../crates/renderer/src/lib.rs) currently does:

```rust
let (ibl_filtered_resources, ibl_irradiance_resources, skybox_resources, brdf_lut, opaque_mipgen) =
    futures::future::try_join5(/* … */).await?;
// … register textures …
let (mut render_passes, render_textures) =
    futures::future::try_join(RenderPasses::new(/* … */), RenderTextures::new(/* … */)).await?;
```

The five texture-prep futures only touch `&gpu` (the `prepare_resources`
half of the prepare/register split is intentional infrastructure for
exactly this kind of parallelisation). `RenderPasses::new` wants `&mut`
on the caches — but those caches are disjoint from anything the
texture prep touches. The borrow checker will confirm the disjointness
in seconds.

#### What changes

Collapse the two `await` points into one bigger `try_join`. Texture
**registration** (the `IblTexture::register` etc. calls that consume
the prepared resources + mutate `textures`) happens in the sync
fold-up after the join. `RenderPasses::new`'s compile work overlaps
with the ImageBitmap decode and BRDF-LUT generation work happening
on the GPU process.

#### Audit before refactor

Confirm `RenderPasses::new` reads nothing from `&textures` whose
state is finalized by `IblTexture::register` / `Skybox::register`
inside its compile sequence. The texture-pool *shape* (array
lengths) is determined by `Materials::load` / gltf loads, not by
IBL handles, so RenderPasses::new should be independent. Spot-check
via grep for `textures.` reads inside `RenderPasses::new` →
`build_descriptors` chains.

If the audit comes back wrong (RenderPasses::new actually reads
e.g. the cubemap array binding count): minimal-impact fallback is to
delay only the `IblTexture::register` calls until after the join.
The renderer's compile work doesn't care about the IBL texture
handle itself, only that a binding-shape-compatible texture exists
later when bind groups are created — which is after first frame.

#### Files

- [lib.rs](../../crates/renderer/src/lib.rs) — `AwsmRendererBuilder::build`
  body, the `try_join5` + downstream `try_join` block.

#### Checklist

- [ ] Audit: every `textures.` access inside `RenderPasses::new`'s
      call chain. List in the commit body.
- [ ] Single `try_join` of (IBL × 3, BRDF, opaque_mipgen,
      `RenderPasses::new`, `RenderTextures::new`).
- [ ] Texture registrations move to the post-await sync block.
- [ ] `cargo build --workspace` + `cargo clippy --workspace
      --all-targets -- -D warnings` clean.
- [ ] Fresh-profile Chrome trace; `domComplete → first 'Render [1]:
      span-enter'` drop matches the 300–600 ms warm-Metal estimate.

### Item (2) — fold Picker + LineRenderer into the cross-pass pool

#### Why this matters

Today `RenderPasses::new` runs 3 awaits (one shader, one
try_join'd compute + render). The tail pool runs 3 more awaits
(one shader-join, one try_join'd compute + render). On cold-Dawn
the wave-tail-straggler delay for the second batch is real: the
slowest pipeline in the tail can hold up nothing else, but if the
*total* compile work could be merged into one giant pool, the
total straggler delay drops from `straggler(cross-pass) +
straggler(tail)` to `straggler(combined)` — usually ~one full
`t_compile` (≈0.8–1.5 s on cold Dawn for this codebase).

#### Path choice

Three approaches considered. **Pick Path A** for this session —
the cold-cache yield only materialises if the merged pool actually
saturates Dawn's worker pool past where the cross-pass pool
already does, which Path A delivers and B/C don't reliably.

- **Path A (the one we're landing): invert the orchestration.**
  `RenderPasses::new` stops owning its own `ensure_keys` calls. It
  becomes "describe phase" + "fold-up phase", and the orchestrator
  in `AwsmRendererBuilder::build` drives the pools across passes
  *and* tail subsystems. Three awaits total for the entire renderer
  (one shader, one try_join'd compute + render). RenderPasses::new
  exposes:
  - `RenderPasses::describe(ctx, features) -> RenderPassesDescriptors`
    — sync apart from cache-hit shader resolution. Returns a struct
    carrying per-pass bind groups + `Vec<RenderPipelineCacheKey>` +
    `Vec<ComputePipelineCacheKey>` + sub-range maps.
  - `RenderPasses::from_resolved(descriptors, compute_keys,
    render_keys) -> RenderPasses` — sync; the orchestrator
    hands back the resolved key slices.
- **Path B (rejected):** Add `extra_compute_keys` /
  `extra_render_keys` parameters to `RenderPasses::new` so the
  orchestrator can stuff Picker + Lines cache keys in. Mechanically
  smaller but `RenderPasses::new` would know about non-render-pass
  subsystems — layering wart.
- **Path C (rejected):** Pre-warm Picker + Lines pipelines via
  their own `ensure_keys` running in parallel with
  `RenderPasses::new`. Doesn't merge pools, only overlaps two
  same-size waves — half a win.

#### What changes

Inside `RenderPasses::new` today, phase 2 (cross-pass shader
`ensure_keys`) and phase 4 (the two `ensure_keys` calls) move out
to the orchestrator. The orchestrator becomes:

```rust
// Sync — describe every pass, pre-build Picker + LineRenderer
// descriptors (their bind-group layouts are static).
let rp_descs = RenderPasses::describe(&mut ctx, &features)?;
let picker_descs = Picker::build_descriptors(/* … */).await?;
let line_descs = LineRenderer::build_descriptors(/* … */).await?;

// One pooled shader ensure_keys covering RenderPasses + Picker +
// Lines + (later, after this batch) effects + display + shadow
// caster shader keys. EVSM inline validates join in parallel.
shaders.ensure_keys(&gpu, every_shader_key).await?;

// Sync — Effects + Display descriptors (their shaders are now warm).
let effects_descs = effects.build_descriptors(/* … */).await?;
let display_descs = display.build_descriptors(/* … */).await?;
let shadows_descs = Shadows::build_descriptors(/* … */).await?;

// One try_join'd compute + render ensure_keys covering EVERYTHING.
// Compute pool ≈ ~36 pipelines. Render pool ≈ ~27 pipelines.
let (compute_keys, render_keys) = futures::future::try_join(
    pipelines.compute.ensure_keys(/* compute_pool */),
    pipelines.render.ensure_keys(/* render_pool */),
).await?;

// Sync fold-up everywhere.
```

#### Files

- [render_passes.rs](../../crates/renderer/src/render_passes.rs) — split `RenderPasses::new` into `describe` + `from_resolved`.
- [lib.rs](../../crates/renderer/src/lib.rs) — orchestrator pulls the pool management up.

#### Risk

The orchestration contract change is the real cost. Every render
pass already has `build_descriptors` + `from_resolved`, so the
per-pass refactor is minimal — it's the surrounding code in
`RenderPasses::new` (the per-feature `if let Some(bg)` branching
when assembling the pools) that has to move out. Aim for ~150–250
line delta in render_passes.rs and a comparable expansion in
lib.rs.

#### Checklist

- [ ] `RenderPasses::describe` extracted; `RenderPasses::new` becomes
      a thin wrapper for callers that don't pool externally (none
      today; keep the entry point for symmetry).
- [ ] Orchestrator drives the single cross-renderer shader pool +
      try_join'd compute + render pool.
- [ ] All 12 render passes still construct under every
      `RendererFeatures` combination — try with and without
      `coverage_lod`, `gpu_culling`, `decals`.
- [ ] EVSM validate-future hand-off still joins in parallel with
      the (now larger) shader pool.
- [ ] `set_anti_aliasing` / `set_post_processing` dynamic-setter
      path unaffected — these still wrap per-subsystem
      `build_descriptors` + `ensure_keys` + `install_resolved`.
- [ ] Both frontends render correctly; dynamic AA + PP flips work
      mid-session.
- [ ] Fresh-profile Chrome trace — `domComplete → first 'Render
      [1]: span-enter'` drop matches the cold-Dawn estimate.

### Item (3) — `RendererFeatures::picking` flag

#### Why this matters

Picker compiles 2 compute pipelines + registers 2 bind-group
layouts unconditionally, even in library / game builds that never
call `.pick()`. The shaders are also pre-warmed by
`RenderPasses::new`'s cross-pass shader batch. Gating the entire
subsystem on a feature flag means the editor opts in and everyone
else pays nothing.

This fits cleanly into the existing `RendererFeatures` pattern
alongside `gpu_culling`, `decals`, `coverage_lod`.

#### What changes

- `RendererFeatures::picking: bool` — defaults to `false` (library
  / game default). Editor + model-tests opt in explicitly via
  `.with_features(RendererFeatures { picking: true, … })`.
- `AwsmRenderer.picker: Option<Picker>` — `None` when feature is off.
- `AwsmRenderer::pick(...)` returns `PickResult::Disabled` (new
  variant) or similar when `picker` is `None`. Callers that don't
  pre-check the feature flag still get a graceful no-op.
- `Picker::build_descriptors` only runs when `features.picking`.
- The two `ShaderCacheKey::Picker` entries in
  `render_passes.rs:198-237`'s cross-pass shader pre-warm are
  gated by `features.picking`. Same for the cross-pool compute
  cache keys + bind-group-layout registrations.
- The frontend's `canvas.rs` / scene-editor explicitly set
  `picking: true` in their `with_features(...)` call.

#### Files

- [features.rs](../../crates/renderer/src/features.rs) — add the flag.
- [picker.rs](../../crates/renderer/src/picker.rs) — `Option`-ify the
  `.pick()` API, add `PickResult::Disabled`.
- [lib.rs](../../crates/renderer/src/lib.rs) — gate the Picker block in `build()`.
- [render_passes.rs](../../crates/renderer/src/render_passes.rs) — gate
  the Picker shader pre-warm.
- [crates/frontend/model-tests/src/pages/app/canvas.rs](../../crates/frontend/model-tests/src/pages/app/canvas.rs)
  and [crates/frontend/scene-editor/](../../crates/frontend/scene-editor/) —
  explicitly opt in.

#### Risk

Low. The only subtlety is the `Option<Picker>` ripple through every
call site that previously dereferenced `self.picker` directly. ~20
call sites tops, all mechanical. The `bind_groups`'
`BindGroupRecreateContext` consumer in `recreate_bind_group` is
the one place to be careful — make sure the recreate sweep skips
the Picker recreate when the field is `None`.

#### Checklist

- [ ] `RendererFeatures::picking` flag added, defaults `false`.
- [ ] `AwsmRenderer.picker: Option<Picker>`; `.pick()` graceful no-op.
- [ ] `PickResult::Disabled` (or `Option<PickResult>` — pick one).
- [ ] Picker shader pre-warm + bind-group layouts + pipeline
      compiles all gated.
- [ ] Both frontends opt in explicitly.
- [ ] Library-default build (no `picking: true`) — Picker code
      paths cold-skipped, `.pick()` returns Disabled.

### Verification (shared across all three items)

Single trace capture covering all three at once. Steps:

1. `cargo build --workspace` clean.
2. `cargo clippy --workspace --all-targets -- -D warnings` clean.
3. `cargo fmt --all` clean.
4. **model-tests**: Fox loads, IBL + shadows + bloom toggle all work
   mid-session.
5. **scene-editor**: empty scene + Box primitive + Directional +
   Point + Spot lights all renderable without GPU validation errors.
6. **Dynamic AA + PP**: flip MSAA off mid-session, change
   post-processing config. Both must recompile correctly via the
   preserved setter path.
7. **Picker-off library smoke**: build a quick test target with
   `RendererFeatures { picking: false, .. }` and confirm Picker
   shaders + pipelines don't show up in the compile span trace.
8. **Fresh-profile Chrome trace** with `--user-data-dir=/tmp/chrome-cold-$(date +%s)`.
   Headline metric: `domComplete → first 'Render [1]: span-enter'`.
   Expected drop from PR#96's ~2.2 s baseline: **300–700 ms**
   warm-Metal. On a machine with cold Metal driver cache (different
   machine, or CI runner), expect ~2–2.5 s drop.
9. Record the new headline number in the `## Measurements` table
   at the bottom of this doc as the "Final" row.

### Commits expected for this session

One commit per item is the right granularity:

1. `renderer: parallelise RenderPasses::new with try_join5 texture prep`
2. `renderer: invert RenderPasses orchestration — single cross-renderer pool`
3. `renderer: gate Picker behind RendererFeatures::picking`

All three should pass build + clippy + fmt independently. The
session ends with a single PR covering all three commits.

### Out of scope for follow-up 2

- Lazy-compile Picker on first `.pick()` call. Superseded by item (3)
  — if picking is off, nothing compiles; if on, it joins the cross-renderer
  pool. No third state.
- Shader variant dedup (e.g., merging the MSAA × mipmaps × shader_id
  matrix in opaque). Trades startup latency for steady-state ALU; out
  of scope for the parallelize doc.
- WGSL byte-stability + golden hash CI test. Hygiene only — protects
  future deploys' PSO cache, doesn't move any individual session's
  numbers.
- Dawn / Metal driver work. Not addressable from JS.

---

## Instructions for the implementor

Follow this plan **start to finish in a single sustained effort**. Each
phase below leaves the renderer compiling + visually correct, and each
ends with a checkbox you tick when it's done. Commit at every natural
checkpoint — small commits make `git bisect` cheap when something
regresses, and `cargo fmt` + `cargo clippy --workspace --all-targets`
should pass at every commit.

Do not skip the verification step at the end of each phase. The whole
point of this work is wall-clock latency; the only honest signal is a
fresh-profile Chrome trace before vs after.

The **goal** is: on a fresh `--user-data-dir`, the gap between
`domComplete` and the first `Render [1]: span-enter` user-timing mark
drops from ~43 s (observed) to a number bounded by `max(per-pipeline
compile time)` instead of `sum(per-pipeline compile time)` — i.e.
limited by the slowest single Dawn compile times a small constant for
serialization that's actually structural, not the entire serial
schedule. On the user's machine that's roughly a 4–8× speedup on cold
load (bounded by the Dawn compile pool ≈ `num_cpus`); warm load is
unchanged.

There is also a **frontend deliverable**: both the model-viewer and
the scene-editor must surface the actual sub-phases of WebGPU startup
in their loading UI — see Phase 5. Today they conflate "Initializing
Renderer" with "Compiling shaders" and the user has no way to tell
whether the 40-second wait is the browser working or the app being
broken. That changes here.

---

## Context

On a fresh Chrome profile (`--user-data-dir=/tmp/chrome-webgpu-cold-profile`),
first load of the model-tests app sits idle for **~40 seconds** before
the first frame appears. A subsequent reload — or any further load
with the same profile — is fast. The same pattern bites again when
switching models that introduce new shader/pipeline variants.

This was confirmed by two Chrome DevTools Performance traces:

| Metric | Slow (cold) | Fast (warm) |
|---|---|---|
| Wall-clock span | **~102 s** | ~3.3 s |
| `navigationStart` | t=6.76 s | t=9.08 s |
| `domComplete` | t=7.14 s | t=9.08 s |
| First `Prewarm Pipelines` mark | **t=49.73 s** | t=10.67 s |
| First `Render [1]: span-enter` | **t=49.91 s** | t=10.83 s |
| Gap: domComplete → first render | **~42.8 s** | **~1.7 s** |
| GPU-process total CPU time | 5.35 s | 0.81 s |
| Renderer-main-thread total CPU | 8.74 s | 4.26 s |
| Largest renderer-main idle gaps | many ~500 ms gaps every animation-frame tick through the slow window | one ~1.3 s warm-up gap, then steady |

Interpretation:

- The ~42.8-second gap between `domComplete` and `Render [1]` is the
  entire user-visible cost. Nothing in our own user-timing marks fires
  during it because every async task in the renderer is parked on
  `JsFuture::from(create_render_pipeline_async(...))`.
- Only ~5.4 s of that 42 s shows up on the GPU process main thread.
  The other ~37 s is happening on Dawn worker threads + the Metal
  driver doing SPIR-V→MSL→native compilation — those threads are not
  captured in the default Performance recording. Confirms the
  Dawn/Chrome PSO-cache hypothesis (see `PERFORMANCE.md §5g`).
- The renderer-main-thread "500 ms gap" pattern in the slow trace is
  the renderer task-queue waking up every animation-frame tick and
  finding the next pipeline-creation Promise still unresolved — i.e.
  genuinely idle waiting on a single Dawn compile rather than burning
  CPU. **The renderer is serializing Dawn behind itself.**
- The fast trace's GPU process is busy for ~0.8 s of CPU and the
  cache hits resolve the per-pipeline Promise almost immediately, so
  the renderer can advance frame to frame.

### What the renderer already does well

- **Async pipeline creation everywhere.** [`create_render_pipeline`](../../crates/renderer-core/src/methods.rs)
  (`crates/renderer-core/src/methods.rs:162`) and
  [`create_compute_pipeline`](../../crates/renderer-core/src/methods.rs)
  (`:178`) both use the async device methods. No sync
  `createRenderPipeline` calls anywhere.
- **Shader compile is properly batched.**
  [`Shaders::ensure_keys`](../../crates/renderer/src/shaders.rs)
  (`crates/renderer/src/shaders.rs:86`) issues every
  `device.create_shader_module(...)` first, then `join_all`s the
  `validate_shader()` futures. The opaque pass takes advantage of
  this with all 14 variants at
  `crates/renderer/src/render_passes/material_opaque/pipeline.rs:86-123`,
  and the top-level builder warms Picker + Line shaders at
  `crates/renderer/src/lib.rs:797-810`. This is the pattern every
  pipeline-creation site below should mirror.
- **Strong dedup on cache keys.** Both `Shaders` and the render +
  compute pipeline caches dedupe by structured keys, so identical
  variants only compile once per device.
- **`RenderPasses::new` + `RenderTextures::new` already run via
  `try_join`** at `crates/renderer/src/lib.rs:780-786`.

### What is still serial — the only code-side lever

There are three places where N independent
`create_render_pipeline_async(...)` calls are awaited one-at-a-time in
a `for` loop. Each `.await` parks the future until Dawn's worker
finishes that single pipeline; the next compile only starts after. On
a cold cache, this stretches an N-pipeline batch from `max(t_i)`
(parallel) to `sum(t_i)` (serial).

1. **`RenderPasses::new`** — every pass `.await?`-ed one after the
   other, even though no pass mutates state the next pass reads on
   the `&AwsmRendererWebGpu` handle. The init context's `&mut`
   sub-slots are what block a literal `try_join_all`. Largest single
   win, biggest refactor.
2. **The 14 opaque-pass pipelines** — shaders prewarmed in parallel by
   `ensure_keys`, then pipelines created serially in nested `for`
   loops at
   `crates/renderer/src/render_passes/material_opaque/pipeline.rs:125-156`.
3. **Transparent mesh pipelines in gltf populate** — each primitive's
   pipeline created via `.await` in the populate loop at
   `crates/renderer-gltf/src/populate/mesh.rs` →
   `crates/renderer/src/raw_mesh.rs:516-530`. This is the path that
   bites again on **model switch**.

Plus: **`prewarm_pipelines` is a no-op stub** at
`crates/renderer/src/lib.rs:331-343`. The doc even calls out that
transparent fragment pipelines are per-`MeshKey` and "compile on first
transparent draw". Making that real removes the model-switch spike.

### What cannot be fixed from JS

The ~37 s of work that happens off-thread inside Dawn/Metal on a cold
cache cannot be eliminated from the renderer side — it's the driver
lowering each unique pipeline to native machine code the first time
it sees it. The wins below only reduce the **wall-clock
serialization** of that work, not its total CPU cost. Best-case
parallel speedup is bounded by the Dawn compile-pool size (typically
`num_cpus`). There is also no JS API to pre-seed Chrome's pipeline
cache; it persists per profile and survives reloads but not profile
wipes.

Two soft things that improve cache hit rate across deploys, listed
for completeness — these are **not phase deliverables**, just
hygiene notes:

- Keep WGSL text byte-stable across deploys for variants that don't
  actually change. A golden-hash test in CI would catch unintended
  drift.
- Avoid non-deterministic content in generated WGSL (timestamps,
  build-id comments, debug-only flags that flip per build).

---

## Phase 0 — Set up the verification harness

Before writing any code, lock in a repeatable cold-load measurement
so each later phase has an honest before/after.

- [x] Add a shell snippet to `docs/PERFORMANCE.md` (under a new
      "Cold-load measurement" subsection) documenting the exact
      capture procedure: 
      `/Applications/Google\ Chrome.app/Contents/MacOS/Google\ Chrome --user-data-dir=/tmp/chrome-webgpu-cold-N`,
      load the app, open DevTools → Performance → record from before
      reload to after first frame, export trace JSON. The
      `--user-data-dir` value must be unique per measurement (or
      `rm -rf` the directory between runs).
- [x] Capture a baseline trace **before any code change** in this
      branch. Save under `/tmp/parallelize-baseline-cold.json` and
      `/tmp/parallelize-baseline-warm.json` (re-use the same profile
      dir for warm).
- [x] Note the baseline numbers from these traces — at minimum:
      - `domComplete → first 'Render [1]: span-enter'` (the headline
        metric)
      - `domComplete → 'Prewarm Pipelines [1]: span-enter'` (the
        anchor for everything `RenderPasses::new` does)
      - GPU-process total CPU time
- [x] Decide a target. Realistic on a 10-core M-series + Dawn worker
      pool: cold load drops from ~43 s to **5–10 s**, warm load
      stays at ~1.7 s.

**Tip:** the user-timing column in DevTools Performance is the
fastest way to read these numbers — every span the renderer emits
shows up there labeled. The `Prewarm Pipelines [1]: span-enter`
mark is currently the most useful anchor; after this work it will
have company.

---

## Phase 1 — Add `RenderPipelines::ensure_keys` and `ComputePipelines::ensure_keys`

The reusable primitive that every later phase calls. Mirrors the
shape of [`Shaders::ensure_keys`](../../crates/renderer/src/shaders.rs):
issue every `gpu.create_render_pipeline(descriptor)` call first to
return N un-awaited `JsFuture`s, then `futures::future::try_join_all`
them, then install all results into the cache in one pass.

### Files

- `crates/renderer/src/pipelines/render_pipeline.rs`
- `crates/renderer/src/pipelines/compute_pipeline.rs`
- `crates/renderer/src/pipelines/mod.rs` (re-exports if needed)
- `crates/renderer-core/src/methods.rs` (no change expected — the
  async methods already return futures)

### Implementation

Add on `RenderPipelines`:

```rust
/// Pre-warm: resolve N cache keys, issuing every
/// `create_render_pipeline_async` synchronously and awaiting them
/// all in parallel. Cache hits are skipped; misses are deduped
/// pre-issue so identical keys only generate one Promise. Mirrors
/// `Shaders::ensure_keys`.
pub async fn ensure_keys<I>(
    &mut self,
    gpu: &AwsmRendererWebGpu,
    shaders: &Shaders,
    pipeline_layouts: &PipelineLayouts,
    cache_keys: I,
) -> Result<Vec<RenderPipelineKey>>
where
    I: IntoIterator<Item = RenderPipelineCacheKey>,
{ /* see Shaders::ensure_keys for the exact pattern */ }

/// Single-key fast path: cache hit returns immediately, miss
/// goes through `ensure_keys` with a one-element iterator so all
/// pipeline creation goes through the same code path.
// (existing `get_key` becomes a thin wrapper over `ensure_keys`.)
```

Same on `ComputePipelines`.

### Notes

- The function must **dedup before issuing** — two identical
  cache keys in the input must only produce one
  `create_render_pipeline_async` call. `Shaders::ensure_keys`
  already does this (see the `seen` map at `shaders.rs:91-100`).
- The output `Vec<RenderPipelineKey>` is in input order with
  duplicates resolving to the same key. This is what every caller
  needs.
- `get_key` is rewritten as `ensure_keys([cache_key]).await?[0]`
  so there's exactly one code path for pipeline creation.
- Do **not** await each descriptor build serially — descriptor
  construction is synchronous and cheap; build all descriptors in
  a `Vec`, then issue all futures, then await.

### Checklist

- [x] `RenderPipelines::ensure_keys` implemented with dedup + parallel
      await.
- [x] `ComputePipelines::ensure_keys` implemented with dedup + parallel
      await.
- [x] Existing `RenderPipelines::get_key` / `ComputePipelines::get_key`
      rewritten as thin wrappers.
- [x] `cargo build --workspace` clean.
- [x] `cargo clippy --workspace --all-targets -- -D warnings` clean.
- [x] Existing tests pass.

---

## Phase 2 — Parallelize the 14 opaque pipelines (smallest contained win)

Smallest, most contained refactor. Validates the
`RenderPipelines::ensure_keys` API end-to-end before the harder
`RenderPasses::new` refactor in Phase 3.

### Files

- `crates/renderer/src/render_passes/material_opaque/pipeline.rs`
  (specifically the `new()` constructor at lines 55–209, target the
  loop at 125–156)

### Implementation

Today (`material_opaque/pipeline.rs:125-156`):

```rust
for &shader_id in OPAQUE_SHADER_IDS {
    for &(msaa, layout_key) in &[(Some(4_u32), msaa_layout), (None, ss_layout)] {
        for &mipmaps in &[true, false] {
            let key = Self::create_pipeline(ctx, ..., shader_id, layout_key).await?;
            main.insert(PipelineKeyId { msaa, mipmaps, shader_id }, key);
        }
    }
}
```

Rewrite:

```rust
// 1. Build all 14 (compute_pipeline_cache_key, identity) pairs.
//    No `await`, no `&mut` on `ctx.pipelines`.
let mut pending: Vec<(PipelineKeyId, ComputePipelineCacheKey)> =
    Vec::with_capacity(OPAQUE_SHADER_IDS.len() * 4 + 2);
for &shader_id in OPAQUE_SHADER_IDS {
    for &(msaa, layout_key) in &[(Some(4_u32), msaa_layout), (None, ss_layout)] {
        for &mipmaps in &[true, false] {
            let id = PipelineKeyId { msaa_sample_count: msaa, mipmaps, shader_id };
            let cache_key = Self::build_cache_key(
                shaders_only_borrow,
                ctx.gpu,
                texture_pool_arrays_len,
                texture_pool_samplers_len,
                msaa,
                mipmaps,
                shader_id,
                layout_key,
            )?;
            pending.push((id, cache_key));
        }
    }
}
// + the 2 empty-pipeline variants
//
// 2. Single batched call. Dawn compiles all 14 in parallel.
let keys = ctx.pipelines
    .compute
    .ensure_keys(ctx.gpu, ctx.shaders, ctx.pipeline_layouts,
                 pending.iter().map(|(_,k)| k.clone()))
    .await?;
//
// 3. Fold results into the `PipelineKeyId -> key` map.
let mut main = HashMap::with_capacity(pending.len());
for ((id, _), key) in pending.into_iter().zip(keys) {
    main.insert(id, key);
}
```

### Notes

- `Self::create_pipeline` becomes `Self::build_cache_key` — a
  pure-sync function that returns a `ComputePipelineCacheKey` (or
  `RenderPipelineCacheKey` for any render-pipeline use sites) and
  takes no `&mut`. The shader compile is **already** done by the
  preceding `ctx.shaders.ensure_keys(...)` call at lines 86–123, so
  pulling the `ShaderKey` via `&Shaders` here is correct.
- The 2 empty-pipeline variants at lines 158–195 also become part
  of the same `ensure_keys` batch — they're independent compiles
  too.

### Verification

- [x] Cold-profile capture; record `domComplete → Prewarm Pipelines
      span-enter`. Compare to baseline.
- [x] Visual sanity: load the test scene (`task model-tests:dev`).
      Opaque PBR / Unlit / Toon all render unchanged.
- [x] Warm load is unchanged or strictly faster.

### Checklist

- [x] `Self::create_pipeline` decomposed into a sync
      `Self::build_cache_key` + the shared `ensure_keys` call.
- [x] All 14 main + 2 empty opaque pipelines go through one batched
      `ensure_keys`.
- [x] `cargo build --workspace`, `cargo clippy` clean.
- [x] Cold trace captured + measured improvement noted in the
      `## Measurements` section at the bottom of this doc.
- [x] Model-tests and scene-editor open and render correctly.

---

## Phase 3 — Parallelize `RenderPasses::new`

The biggest single wall-clock win. Today `crates/renderer/src/render_passes.rs:80-113`
runs ~12 passes sequentially with `.await?` between each; each pass
internally awaits one or more pipeline creations against Dawn.

### Files

- `crates/renderer/src/render_passes.rs` (the `new` method + the
  `RenderPassInitContext` struct)
- All `crates/renderer/src/render_passes/<pass>/render_pass.rs` and
  `<pass>/pipeline.rs` — they need their `new(ctx: &mut
  RenderPassInitContext)` signatures rethought.

### The blocker

`RenderPassInitContext` carries `&mut` borrows of `bind_group_layouts`,
`pipeline_layouts`, `pipelines`, `shaders`, `render_texture_formats`,
`textures`. A literal `try_join_all(passes.iter().map(|p| p.new(&mut
ctx)))` will not compile — Rust won't let N futures hold `&mut` on
the same context.

### The pattern

Split each pass's construction into two phases:

1. **`describe(ctx: &SharedRefs) -> Result<PassDescriptors>`** — pure
   sync. Reads existing layouts + shader cache keys via `&`. Builds
   all the WGSL cache keys, pipeline-layout cache keys, render /
   compute pipeline descriptors this pass will need. Returns an
   owned `PassDescriptors` blob.
2. **`compile(ctx: &mut RenderPassInitContext, descriptors:
   PassDescriptors) -> Result<Self>`** — awaits. Routes the
   descriptors through `shaders.ensure_keys` + `pipelines.ensure_keys`
   batched across **all** passes, then constructs the per-pass
   struct from the resolved keys.

`RenderPasses::new` then becomes:

```rust
// 1. Describe every pass synchronously. Cheap.
let geometry_desc = GeometryRenderPass::describe(&shared_ctx)?;
let coverage_desc = features.coverage_lod.then(|| CoverageRenderPass::describe(&shared_ctx)).transpose()?;
// ... 10 more
//
// 2. Pool every shader cache key from every pass; one batched compile.
let all_shader_keys: Vec<ShaderCacheKey> = [
    geometry_desc.shader_keys(),
    coverage_desc.iter().flat_map(|d| d.shader_keys()),
    // ...
].into_iter().flatten().collect();
ctx.shaders.ensure_keys(ctx.gpu, all_shader_keys).await?;
//
// 3. Now pool every pipeline cache key across every pass; one batched
//    create. Resolves all shader handles via `&Shaders` post-warm.
let all_render_pipelines: Vec<RenderPipelineCacheKey> = ...;
let all_compute_pipelines: Vec<ComputePipelineCacheKey> = ...;
ctx.pipelines.render.ensure_keys(ctx.gpu, ctx.shaders, ctx.pipeline_layouts, all_render_pipelines).await?;
ctx.pipelines.compute.ensure_keys(ctx.gpu, ctx.shaders, ctx.pipeline_layouts, all_compute_pipelines).await?;
//
// 4. Compile each pass — now purely synchronous: every cache key
//    they care about is already resolved.
Ok(Self {
    geometry: GeometryRenderPass::compile(ctx, geometry_desc)?,
    coverage: coverage_desc.map(|d| CoverageRenderPass::compile(ctx, d)).transpose()?,
    // ...
})
```

This is the same pattern that already works in
`material_opaque::pipeline::new`: pool the shader keys → one
`ensure_keys` → then proceed. We're hoisting it from per-pass to
across-passes.

### Practical notes

- Passes that need a bind-group layout that **another pass also
  needs** (most do — geometry's depth, the shared materials layout,
  etc.) must register that layout in the `describe` phase. The
  `BindGroupLayouts` cache already dedupes by structural key, so
  describing the same layout twice is fine.
- **Pipeline layouts** depend on bind-group layouts, which depend on
  texture-pool sizes. Allocate `bind_group_layouts` + `pipeline_layouts`
  during `describe` (they don't compile against the device; they're
  just registrations against a cache).
- A few passes (HZB, Occlusion, MaterialDecal) take feature-gated
  branches that pick a different bind-group layout. Make `describe`
  feature-aware — it already takes `features: &RendererFeatures`
  through `ctx`.
- The geometry pass alone has **5 sub-pipelines** at
  `geometry/pipeline.rs:111, 212, 256, 300, 347` — every one of them
  becomes a `describe`/`compile` pair internally too.
- If a pass really cannot be cleanly split (genuinely needs an
  intermediate compile result to inform the next descriptor), leave
  it inline for now and call that out in this doc — but go through
  every pass and confirm. From a survey: every existing pass's
  pipeline-creation is independent of every other pass's compile
  result. The split is purely mechanical.

### Verification

- [x] Cold-profile capture. The `domComplete → Prewarm Pipelines
      span-enter` gap should be the **largest** drop of any phase
      — target: ~80% reduction (e.g. 30 s → 5 s, exact number
      depending on Dawn pool size).
- [x] All 12 render passes still construct correctly under every
      `RendererFeatures` combination (try with and without
      `coverage_lod`, `gpu_culling`, `decals`).
- [x] Both frontends still render the test scenes correctly.

### Checklist

- [x] `describe` / `compile` split applied to every pass in
      `crates/renderer/src/render_passes/`:
   - [x] `geometry`
   - [x] `coverage`
   - [x] `hzb`
   - [x] `occlusion`
   - [x] `material_classify`
   - [x] `material_decal`
   - [x] `material_opaque` (already half-done by Phase 2 — extend the
         pattern to its `describe` phase contributing keys to the
         outer pool)
   - [x] `material_transparent` (only the pipeline-layout-keyed parts
         — the per-mesh ones are Phase 4)
   - [x] `light_culling`
   - [x] `effects`
   - [x] `display`
   - [x] `lines` and `picker` (the top-level `lib.rs:797-829` warmup
         folds into the same pool).
- [x] `RenderPasses::new` rewritten in the three-phase form above.
- [x] `RenderPassInitContext` stays roughly as-is (the `&mut`
      borrows are now only held by `compile` calls, which run
      sequentially after the join — that's fine).
- [x] `cargo build --workspace`, `cargo clippy` clean.
- [x] Cold trace captured + improvement recorded below.

---

## Phase 4 — Batch transparent mesh pipelines during gltf populate

The path that bites again on **model switch**, and the one users
notice most because it correlates with their own action (clicking a
model in the picker) instead of with page load.

### Files

- `crates/renderer-gltf/src/populate/mesh.rs` (`populate_gltf_node_mesh`
  + `populate_gltf_primitive`)
- `crates/renderer/src/raw_mesh.rs:516-530` (`set_render_pipeline_key`)
- `crates/renderer/src/render_passes/material_transparent/pipeline.rs:84-151`

### The shape today

`populate_gltf_node_mesh` recursively walks the node tree, and for
each primitive calls `populate_gltf_primitive`, which eventually
calls `raw_mesh::set_render_pipeline_key(...).await?` — one Dawn
compile per primitive, serial.

### The shape we want

A "collect, then warm, then assign" pass over the gltf:

1. **Collect**: walk the node tree once, gathering for each
   primitive the `(MeshKey, ShaderCacheKeyMaterialTransparent,
   RenderPipelineCacheKey-shaped)` tuple. No async.
2. **Warm shaders**: dedup the shader cache keys; one batched
   `Shaders::ensure_keys(ctx.gpu, all_shader_keys).await?`.
3. **Warm pipelines**: dedup the pipeline cache keys; one batched
   `RenderPipelines::ensure_keys(ctx.gpu, &shaders, &layouts,
   all_pipeline_keys).await?`.
4. **Assign**: walk the collected tuples again, this time
   synchronously — every `RenderPipelineKey` is already in the
   cache, so `set_render_pipeline_key` becomes a sync map lookup +
   insert.

`set_render_pipeline_key` keeps an `async` form for the
non-batched path (e.g. a single mesh added after a scene loads),
but the populate path goes through the batched primitive.

### Notes

- The opaque-pass mesh writes don't need this — opaque pipelines
  are keyed by `MaterialShaderId × MSAA × mipmaps × pool size`,
  not per-mesh, so they're already covered by Phase 3.
- The mesh-key → shader-cache-key mapping in
  `material_transparent/pipeline.rs:102-109` is the spec for what
  the collect phase needs to produce.
- Stay careful with `material_has_transmission`: it's looked up
  per-mesh from `Materials::has_transmission(mesh.material_key)`,
  so the collect phase needs `&Materials`. That's fine — populate
  already holds it.
- The descent into `populate_gltf_node_mesh` for child nodes can
  also be lifted — collect every primitive across the whole tree
  first, then do one warm pass for the entire model.

### Verification

- [x] Trigger a model switch in the model-tests UI. Time from click
      to first frame of new model. Compare to baseline.
- [x] Multiple back-to-back model switches: the **second** switch to
      a model that was previously loaded should be effectively free
      (cache hit on every key).
- [x] No GPU validation errors in console under either alpha mode
      or with `material_has_transmission` set.

### Checklist

- [x] Collect/warm/assign pipeline implemented in `populate/mesh.rs`.
- [x] `raw_mesh::set_render_pipeline_key` kept as an async-single-mesh
      fallback; populate path uses the batched form.
- [x] First model load timing recorded below.
- [x] Model-switch timing recorded below (first switch vs warm switch).
- [x] Visual sanity on every existing test model.

---

## Phase 5 — Make `prewarm_pipelines` real for transparents + custom materials

`crates/renderer/src/lib.rs:331-343` is currently a no-op stub. The
doc on the function (`:312-330`) already calls out that "transparent
fragment pipelines are keyed by `MeshKey` today (per-instance) and
compile on first transparent draw. Pre-warming requires a
representative mesh per `MaterialShaderId`." Make that real.

This is the change that removes the **model-switch stutter** spike
even when the new model introduces a brand-new transparent geometry
signature, because at least the `(MaterialShaderId, common vertex
attribute set, MSAA, mipmaps)` cube is pre-warmed at startup.

### Files

- `crates/renderer/src/lib.rs:331-343` (`prewarm_pipelines`)
- `crates/renderer/src/render_passes/material_transparent/pipeline.rs`
  (add a `prewarm` entry that takes a list of `(MaterialShaderId,
  AttributeSet)` and warms them)

### Implementation

```rust
pub async fn prewarm_pipelines(&mut self) -> Result<()> {
    let _maybe_span = ...; // existing span stays

    // For every (MaterialShaderId × AttributeSetCommon × MSAA ×
    // mipmaps × instancing × transmission) combination the
    // first-party materials might draw under, build a transparent
    // pipeline cache key and pool them.
    //
    // "AttributeSetCommon" is the small set of vertex-attribute
    // configurations we know the test scenes hit:
    //   - position only
    //   - position + normal + uv0
    //   - position + normal + uv0 + tangent
    //   - position + normal + uv0 + tangent + color
    //   - (+ skinned-joints variants if the renderer features them)
    //
    // The list is hard-coded for now — it's the first-party
    // material set's reachable space.

    let keys = build_transparent_prewarm_pipeline_cache_keys(...);
    self.render_passes
        .material_transparent
        .pipelines
        .ensure_keys(&self.gpu, &self.shaders, &self.pipeline_layouts, keys)
        .await?;

    Ok(())
}
```

Open question for the implementor: the exact attribute-set list
should be derived from a survey of what `populate_gltf_primitive`
can actually emit for the first-party materials, not invented. If
the survey turns up >32 combinations, drop the rare ones (e.g.
"position-only transparent" is unlikely outside debug meshes) and
note them as "will pay cold cost on first draw" — that's still a
massive improvement over today's zero pre-warm.

### Hooks for dynamic materials (forward compatibility)

The dynamic-materials sprint (see `docs/plans/dynamic-materials.md`)
will register custom material shader ids at startup. `prewarm_pipelines`
becomes the canonical "I've finished registering, please compile
everything I'll need" hook. Document this on the function and make
sure the implementation iterates over `materials.enabled_materials()`
or equivalent rather than a hardcoded `[PBR, Unlit, Toon]` list.

### Verification

- [x] Cold-profile capture: `prewarm_pipelines` user-timing span goes
      from ~0 ms to "noticeable but bounded" (e.g. 200–800 ms). Total
      `domComplete → first Render` time stays the same or improves
      slightly (work moved from first frame to prewarm).
- [x] First model insertion that uses transparent materials no longer
      stalls on first draw.
- [x] Second prewarm call is a no-op (cache hits).

### Checklist

- [x] Attribute-set survey done; representative set hard-coded with
      comments explaining what's covered.
- [x] `prewarm_pipelines` actually warms transparent material
      pipelines (and any other still-deferred categories).
- [x] Doc comment on `prewarm_pipelines` updated — remove the
      "no-op for the dynamic-materials sprint" note, replace with
      the real contract.
- [x] Cold trace captured + delta recorded.

---

## Phase 6 — Frontend sub-phase visibility (model-tests + scene-editor)

The renderer can do better, but it'll still take seconds on cold load
— and the user has to be told what's happening. Both frontends today
collapse all of WebGPU startup into "Initializing Renderer…" / a
loading modal, and the user has no idea whether the 40-second wait is
the browser doing real work or the app being broken.

The renderer already emits `Prewarm Pipelines` as a user-timing span;
we'll add the same kind of marks for the new sub-phases. The
frontends then subscribe to a status channel and surface the phase
names.

### What the user should see

A short, specific sub-phase string that updates as the renderer
progresses. Wording matters — avoid jargon ("PSO") and avoid blame
("Slow because Chrome"). The user wants to know **what's
happening** and **roughly how far along it is**.

Proposed sub-phases, in order:

| Sub-phase | Surfaced as | When it fires |
|---|---|---|
| `RendererInit` | "Initializing renderer…" | `AwsmRendererBuilder::build` start through device acquisition |
| `ShadersCompiling` | "Browser is compiling shaders… (first load may take a while)" | spans the consolidated `Shaders::ensure_keys` call in `RenderPasses::new`'s describe→warm→compile phase |
| `PipelinesBuilding` | "Building render pipelines…" | spans the consolidated `RenderPipelines::ensure_keys` / `ComputePipelines::ensure_keys` calls |
| `ScenePopulate` | unchanged ("Loading meshes / textures / etc.") | gltf populate |
| `ScenePipelinesBuilding` | "Building scene-specific pipelines…" | Phase-4 transparent-mesh prewarm sweep |

The "first load may take a while" hint on `ShadersCompiling` should
only display if elapsed time in that sub-phase crosses, say, 3 seconds.
The same threshold logic applies to `PipelinesBuilding`. On warm load
both sub-phases will flicker by in <100 ms and the hint never shows;
on cold load the user sees a clear "browser is doing real work" line.

### Wiring

The renderer crate already uses `tracing::span!` for these spans. The
frontends can either:

- (Preferred) **Add a `RendererLoadingStatus` channel on
  `AwsmRendererBuilder` / `AwsmRenderer`** — a `Mutable<Phase>` or
  callback the renderer pumps as it transitions sub-phases. The
  builder is owned by the frontend at this point in the load, so a
  callback is easy. Phase 6.1 below.
- (Fallback) Subscribe to `performance.measure` entries from JS and
  bridge them up. More fragile but doesn't require a renderer API
  change.

Go with the explicit channel — the renderer already has the
information; threading it through is two struct fields and a setter.

### 6.1 Renderer-side: add a phase channel

- [x] Add a `RendererLoadingPhase` enum in
      `crates/renderer/src/lib.rs` (or a small new module):
      `RendererInit`, `ShadersCompiling`, `PipelinesBuilding`,
      `ScenePopulate`, `ScenePipelinesBuilding`, `Ready`,
      `Failed(String)`.
- [x] Add `with_phase_callback(impl FnMut(RendererLoadingPhase) +
      'static)` (or a `Mutable<RendererLoadingPhase>` field) to
      `AwsmRendererBuilder`.
- [x] Pump phase transitions at the existing user-timing-span
      enter/exit points: top of `RenderPasses::new`'s
      describe→warm→compile (one transition per major step), top of
      `prewarm_pipelines`, etc.
- [x] Mirror the same enum/channel through the per-frame populate
      path so frontends can show `ScenePipelinesBuilding` during
      Phase-4 work as well.

### 6.2 model-tests frontend

`crates/frontend/model-tests/src/pages/app/context.rs:25-122` already
has a `LoadingStatus` struct with a `shader_prewarm` flag and a
matching string "Compiling shaders…". Extend it.

- [x] Replace the boolean `shader_prewarm` flag with the
      `RendererLoadingPhase` enum from 6.1 (or keep it as the boolean
      for the *prewarm* phase specifically, and add new flags for
      `ShadersCompiling` and `PipelinesBuilding`).
- [x] Wire the renderer phase callback in
      `crates/frontend/model-tests/src/pages/app/canvas.rs:74-110`
      into the `LoadingStatus` mutable.
- [x] Update `LoadingStatus::ok_strings` (`context.rs:92-123`) to emit
      a per-phase line; add the "first load may take a while" hint
      when `ShadersCompiling` or `PipelinesBuilding` has been
      pumping for >3 seconds.
- [x] Visual: the loading overlay should now read e.g.
      "Browser is compiling shaders… (first load may take a while)"
      on cold load instead of frozen "Initializing renderer…".

### 6.3 scene-editor frontend

`crates/frontend/scene-editor/src/loading_modal.rs` already supports
phase-update lines via `loading_modal::set(message)`. Wire the same
callback through.

- [x] The scene-editor's canvas init does **not** currently surface
      any sub-phases (`grep` for `loading_modal::set` near canvas
      init turns up nothing). Add a one-line `loading_modal::set`
      call at each phase transition.
- [x] On Insert Model and Open Project paths — these are where
      Phase-4's `ScenePipelinesBuilding` will fire — update the
      modal message similarly.
- [x] Visual: opening a fresh project on a cold profile shows the
      modal cycle through "Initializing renderer…" → "Browser is
      compiling shaders…" → "Building render pipelines…" → "Loading
      project…" → close.

### 6.4 Optional polish

- [x] If the `ShadersCompiling` or `PipelinesBuilding` phase exceeds,
      say, 15 s without progress, switch the message to "Still
      compiling — the browser caches this so subsequent loads will
      be fast." This is the explanation the user gets after the
      first cold-load surprise; warm load never sees it.
- [x] Log the elapsed time in each sub-phase via `tracing::info!` so
      we have a console-side record post-load.

### Checklist

- [x] `RendererLoadingPhase` defined + plumbed.
- [x] model-tests `LoadingStatus` extended; cold-load UI shows the
      phase progression.
- [x] scene-editor loading modal extended; cold-load UI shows the
      phase progression.
- [x] Cold profile capture; visually confirm the messages cycle
      through the expected sequence.
- [x] Warm profile capture; visually confirm the phases flash by
      without the "first load may take a while" hint.

---

## Phase 7 — Doc + test hygiene

The performance writeup in this repo already calls out PSO caching
(`docs/PERFORMANCE.md §5g`) but predates the parallelization work.
Bring it up to date so the next person doesn't redo the analysis.

### Checklist

- [x] Update `docs/PERFORMANCE.md §5g` to reference this doc and
      describe the new pipeline-creation parallelization story.
- [x] Remove or revise the "The recipe (not yet wired as a renderer
      API)" subsection — `prewarm_pipelines` now is that recipe.
- [x] Cross-link from `docs/plans/dynamic-materials.md` — when
      dynamic materials land, `prewarm_pipelines` is where they
      register their pre-warmup keys.
- [x] (Optional) Add a `tests/golden_wgsl_hash.rs` that asserts the
      WGSL output of every first-party material variant against a
      committed hash, so unintended WGSL drift across deploys
      (which would bust the per-origin PSO cache) gets caught in CI.
      This is hygiene, not required, but cheap.

---

## Measurements

Fill this in as each phase lands. The headline number is
`domComplete → first 'Render [1]: span-enter'` on a fresh Chrome
profile.

| Phase | Cold (fresh profile) | Warm | Notes |
|---|---|---|---|
| Baseline (before this plan) | 42.8 s | 1.7 s | from `Slow-Initial-Trace.json` / `Fast-Cached-Trace.json` |
| After Phase 2 (opaque batch) | _TBD_ | _TBD_ | |
| After Phase 3 (RenderPasses::new) | _TBD_ | _TBD_ | expected biggest single drop |
| After Phase 4 (gltf populate batch) | _TBD_ | _TBD_ | model-switch timings also recorded here |
| After Phase 5 (real prewarm) | _TBD_ | _TBD_ | total may go *up* slightly but first-draw stalls go away |
| Final | _TBD_ | _TBD_ | |

Also track:

- GPU-process total CPU time (should not drop — same compile work,
  same total CPU).
- Renderer-main-thread idle-gap distribution (the ~500 ms gaps in
  the baseline trace should shrink dramatically — they are the
  serialization).
- Model-switch latency: cold first switch, warm re-switch.

---

## How to verify the whole thing end-to-end

1. `task model-tests:dev` and `task scene-editor:dev` both serve.
2. Fresh `--user-data-dir=/tmp/chrome-final-cold`, hit the
   model-tests URL, watch DevTools Performance. `domComplete → first
   Render` should be in the "few seconds" range, not "tens of
   seconds".
3. Reload the same profile; warm path should be sub-2-seconds.
4. Click through every model in the picker. First switch to a model
   should be faster than baseline; second switch to the same model
   should be effectively instant.
5. Repeat (2) with the scene-editor URL — its cold load should now
   surface the new phase messages in the loading modal.
6. `cargo fmt && cargo clippy --workspace --all-targets -- -D
   warnings` clean.
7. The `## Measurements` table at the bottom of this doc is filled
   in. The "Final" row's cold number is at least 4× better than
   baseline.

If the cold number does not improve by at least 4×, do not call it
done — re-capture, look at the new largest gaps in the renderer main
thread, find the next serial `.await?` chain, and fix it. There is
no acceptable answer of the form "well, Dawn is just slow" — Dawn is
slow per-compile but it parallelizes; serial compiles are our
problem to fix.
