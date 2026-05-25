# Parallelize WebGPU pipeline creation — full plan

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

- [ ] Add a shell snippet to `docs/PERFORMANCE.md` (under a new
      "Cold-load measurement" subsection) documenting the exact
      capture procedure: 
      `/Applications/Google\ Chrome.app/Contents/MacOS/Google\ Chrome --user-data-dir=/tmp/chrome-webgpu-cold-N`,
      load the app, open DevTools → Performance → record from before
      reload to after first frame, export trace JSON. The
      `--user-data-dir` value must be unique per measurement (or
      `rm -rf` the directory between runs).
- [ ] Capture a baseline trace **before any code change** in this
      branch. Save under `/tmp/parallelize-baseline-cold.json` and
      `/tmp/parallelize-baseline-warm.json` (re-use the same profile
      dir for warm).
- [ ] Note the baseline numbers from these traces — at minimum:
      - `domComplete → first 'Render [1]: span-enter'` (the headline
        metric)
      - `domComplete → 'Prewarm Pipelines [1]: span-enter'` (the
        anchor for everything `RenderPasses::new` does)
      - GPU-process total CPU time
- [ ] Decide a target. Realistic on a 10-core M-series + Dawn worker
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

- [ ] `RenderPipelines::ensure_keys` implemented with dedup + parallel
      await.
- [ ] `ComputePipelines::ensure_keys` implemented with dedup + parallel
      await.
- [ ] Existing `RenderPipelines::get_key` / `ComputePipelines::get_key`
      rewritten as thin wrappers.
- [ ] `cargo build --workspace` clean.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean.
- [ ] Existing tests pass.

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

- [ ] Cold-profile capture; record `domComplete → Prewarm Pipelines
      span-enter`. Compare to baseline.
- [ ] Visual sanity: load the test scene (`task model-tests:dev`).
      Opaque PBR / Unlit / Toon all render unchanged.
- [ ] Warm load is unchanged or strictly faster.

### Checklist

- [ ] `Self::create_pipeline` decomposed into a sync
      `Self::build_cache_key` + the shared `ensure_keys` call.
- [ ] All 14 main + 2 empty opaque pipelines go through one batched
      `ensure_keys`.
- [ ] `cargo build --workspace`, `cargo clippy` clean.
- [ ] Cold trace captured + measured improvement noted in the
      `## Measurements` section at the bottom of this doc.
- [ ] Model-tests and scene-editor open and render correctly.

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

- [ ] Cold-profile capture. The `domComplete → Prewarm Pipelines
      span-enter` gap should be the **largest** drop of any phase
      — target: ~80% reduction (e.g. 30 s → 5 s, exact number
      depending on Dawn pool size).
- [ ] All 12 render passes still construct correctly under every
      `RendererFeatures` combination (try with and without
      `coverage_lod`, `gpu_culling`, `decals`).
- [ ] Both frontends still render the test scenes correctly.

### Checklist

- [ ] `describe` / `compile` split applied to every pass in
      `crates/renderer/src/render_passes/`:
   - [ ] `geometry`
   - [ ] `coverage`
   - [ ] `hzb`
   - [ ] `occlusion`
   - [ ] `material_classify`
   - [ ] `material_decal`
   - [ ] `material_opaque` (already half-done by Phase 2 — extend the
         pattern to its `describe` phase contributing keys to the
         outer pool)
   - [ ] `material_transparent` (only the pipeline-layout-keyed parts
         — the per-mesh ones are Phase 4)
   - [ ] `light_culling`
   - [ ] `effects`
   - [ ] `display`
   - [ ] `lines` and `picker` (the top-level `lib.rs:797-829` warmup
         folds into the same pool).
- [ ] `RenderPasses::new` rewritten in the three-phase form above.
- [ ] `RenderPassInitContext` stays roughly as-is (the `&mut`
      borrows are now only held by `compile` calls, which run
      sequentially after the join — that's fine).
- [ ] `cargo build --workspace`, `cargo clippy` clean.
- [ ] Cold trace captured + improvement recorded below.

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

- [ ] Trigger a model switch in the model-tests UI. Time from click
      to first frame of new model. Compare to baseline.
- [ ] Multiple back-to-back model switches: the **second** switch to
      a model that was previously loaded should be effectively free
      (cache hit on every key).
- [ ] No GPU validation errors in console under either alpha mode
      or with `material_has_transmission` set.

### Checklist

- [ ] Collect/warm/assign pipeline implemented in `populate/mesh.rs`.
- [ ] `raw_mesh::set_render_pipeline_key` kept as an async-single-mesh
      fallback; populate path uses the batched form.
- [ ] First model load timing recorded below.
- [ ] Model-switch timing recorded below (first switch vs warm switch).
- [ ] Visual sanity on every existing test model.

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

- [ ] Cold-profile capture: `prewarm_pipelines` user-timing span goes
      from ~0 ms to "noticeable but bounded" (e.g. 200–800 ms). Total
      `domComplete → first Render` time stays the same or improves
      slightly (work moved from first frame to prewarm).
- [ ] First model insertion that uses transparent materials no longer
      stalls on first draw.
- [ ] Second prewarm call is a no-op (cache hits).

### Checklist

- [ ] Attribute-set survey done; representative set hard-coded with
      comments explaining what's covered.
- [ ] `prewarm_pipelines` actually warms transparent material
      pipelines (and any other still-deferred categories).
- [ ] Doc comment on `prewarm_pipelines` updated — remove the
      "no-op for the dynamic-materials sprint" note, replace with
      the real contract.
- [ ] Cold trace captured + delta recorded.

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

- [ ] Add a `RendererLoadingPhase` enum in
      `crates/renderer/src/lib.rs` (or a small new module):
      `RendererInit`, `ShadersCompiling`, `PipelinesBuilding`,
      `ScenePopulate`, `ScenePipelinesBuilding`, `Ready`,
      `Failed(String)`.
- [ ] Add `with_phase_callback(impl FnMut(RendererLoadingPhase) +
      'static)` (or a `Mutable<RendererLoadingPhase>` field) to
      `AwsmRendererBuilder`.
- [ ] Pump phase transitions at the existing user-timing-span
      enter/exit points: top of `RenderPasses::new`'s
      describe→warm→compile (one transition per major step), top of
      `prewarm_pipelines`, etc.
- [ ] Mirror the same enum/channel through the per-frame populate
      path so frontends can show `ScenePipelinesBuilding` during
      Phase-4 work as well.

### 6.2 model-tests frontend

`crates/frontend/model-tests/src/pages/app/context.rs:25-122` already
has a `LoadingStatus` struct with a `shader_prewarm` flag and a
matching string "Compiling shaders…". Extend it.

- [ ] Replace the boolean `shader_prewarm` flag with the
      `RendererLoadingPhase` enum from 6.1 (or keep it as the boolean
      for the *prewarm* phase specifically, and add new flags for
      `ShadersCompiling` and `PipelinesBuilding`).
- [ ] Wire the renderer phase callback in
      `crates/frontend/model-tests/src/pages/app/canvas.rs:74-110`
      into the `LoadingStatus` mutable.
- [ ] Update `LoadingStatus::ok_strings` (`context.rs:92-123`) to emit
      a per-phase line; add the "first load may take a while" hint
      when `ShadersCompiling` or `PipelinesBuilding` has been
      pumping for >3 seconds.
- [ ] Visual: the loading overlay should now read e.g.
      "Browser is compiling shaders… (first load may take a while)"
      on cold load instead of frozen "Initializing renderer…".

### 6.3 scene-editor frontend

`crates/frontend/scene-editor/src/loading_modal.rs` already supports
phase-update lines via `loading_modal::set(message)`. Wire the same
callback through.

- [ ] The scene-editor's canvas init does **not** currently surface
      any sub-phases (`grep` for `loading_modal::set` near canvas
      init turns up nothing). Add a one-line `loading_modal::set`
      call at each phase transition.
- [ ] On Insert Model and Open Project paths — these are where
      Phase-4's `ScenePipelinesBuilding` will fire — update the
      modal message similarly.
- [ ] Visual: opening a fresh project on a cold profile shows the
      modal cycle through "Initializing renderer…" → "Browser is
      compiling shaders…" → "Building render pipelines…" → "Loading
      project…" → close.

### 6.4 Optional polish

- [ ] If the `ShadersCompiling` or `PipelinesBuilding` phase exceeds,
      say, 15 s without progress, switch the message to "Still
      compiling — the browser caches this so subsequent loads will
      be fast." This is the explanation the user gets after the
      first cold-load surprise; warm load never sees it.
- [ ] Log the elapsed time in each sub-phase via `tracing::info!` so
      we have a console-side record post-load.

### Checklist

- [ ] `RendererLoadingPhase` defined + plumbed.
- [ ] model-tests `LoadingStatus` extended; cold-load UI shows the
      phase progression.
- [ ] scene-editor loading modal extended; cold-load UI shows the
      phase progression.
- [ ] Cold profile capture; visually confirm the messages cycle
      through the expected sequence.
- [ ] Warm profile capture; visually confirm the phases flash by
      without the "first load may take a while" hint.

---

## Phase 7 — Doc + test hygiene

The performance writeup in this repo already calls out PSO caching
(`docs/PERFORMANCE.md §5g`) but predates the parallelization work.
Bring it up to date so the next person doesn't redo the analysis.

### Checklist

- [ ] Update `docs/PERFORMANCE.md §5g` to reference this doc and
      describe the new pipeline-creation parallelization story.
- [ ] Remove or revise the "The recipe (not yet wired as a renderer
      API)" subsection — `prewarm_pipelines` now is that recipe.
- [ ] Cross-link from `docs/plans/dynamic-materials.md` — when
      dynamic materials land, `prewarm_pipelines` is where they
      register their pre-warmup keys.
- [ ] (Optional) Add a `tests/golden_wgsl_hash.rs` that asserts the
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
