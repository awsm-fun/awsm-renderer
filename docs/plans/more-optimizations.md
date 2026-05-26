# Renderer cold-boot optimizations & Android Chrome compatibility

## Status when this plan was written

The user reported a renderer init failure on Android Chrome that didn't reproduce on OSX Chrome. After an extensive investigation (full diagnosis below), Android Chrome now initializes successfully but with an **R&D workaround in place** that needs to be replaced. Several supporting improvements landed along the way and should stay. A bigger set of lazy-pool optimizations is queued and should be picked up after the dynamic-materials work lands.

Everything in this plan is reachable from a clean working tree. The 8 currently-modified files are listed in [§ Files left modified](#files-left-modified).

## Progress since this plan was written (2026-05-26)

The dynamic-materials PR (#98, branch `dynamic-shaders`) landed work that overlaps with parts of this plan. Snapshot of what's now done:

### Lazy-pool refactors landed

The per-pass `*_for_config` + `merge_resolved` pattern that this plan called out as "the right shape" is now used by **five passes**, not just geometry:

| Pass | Cold-boot variants stripped | Lazy recompile entry point |
|---|---|---|
| Material Opaque | 14 → 5 (1 active msaa/mipmap × 4 shader_ids + 1 empty) | `AwsmRenderer::set_anti_aliasing` |
| Material Classify | 2 → 1 (active msaa only) | `AwsmRenderer::set_anti_aliasing` |
| Effects | 5 → 1 (bloom-off → only `BloomPhase::None`) | `AwsmRenderer::set_post_processing` |
| HZB | 3 → 2 (single seed variant + reduce) | `AwsmRenderer::set_anti_aliasing` |
| Picker | 2 → 1 (active msaa only) | `AwsmRenderer::set_anti_aliasing` |
| Geometry | 18 → 9 (the existing item #1 in this plan) | `AwsmRenderer::set_anti_aliasing` |

`Shaders::ensure_keys` now returns `Vec<ShaderKey>` directly (matching the shape of `RenderPipelines::ensure_keys` / `ComputePipelines::ensure_keys`), so the per-pass `*_for_config` builders can pipeline through one shader-compile batch + one pipeline-compile batch with no follow-up `get_key` round-trips.

### Boot-timing instrumentation

Three new log surfaces under `target = "awsm_renderer::boot_timing"` (filter via `RUST_LOG=awsm_renderer::boot_timing=info`):

- `AwsmRendererBuilder::build` emits per-phase wall-clock: `phase = CompilingShaders (+42ms phase, 42ms total)` etc. so the operator can attribute cold-boot time to a specific phase without ad-hoc instrumentation.
- `Shaders::ensure_keys` emits `Shaders::ensure_keys: 32 shaders compiled in 845ms` per batched call.
- `{Render,Compute}Pipelines::ensure_keys` emit the same shape plus the per-pipeline finish-order labels already documented in this plan (compute now also logs per-pipeline cumulative timing — see item #2 below).

Combined with the per-pipeline labels (item #2 below), the cold-boot waterfall is now diagnosable end-to-end without modifying the binary.

### Dynamic-materials surface

Every dynamic-material registration triggers a `prewarm_pipelines` call that now goes through **one** batched `Shaders::ensure_keys` + **one** batched `ComputePipelines::ensure_keys` covering the full union of (classify variant + per-shader-id opaque variants + per-Blend transparent stub). This used to be a doubly-nested for-loop with serial awaits — fixed before this plan was reviewed.

The dynamic-materials work also added two registry-side caches consumed by the render hot path:
- `DynamicMaterials::bucket_entries_cached() -> &[BucketEntry]` — refreshed on register/unregister; replaces a per-frame `bucket_entries(materials)` allocation + sort on the opaque pass.
- `DynamicMaterials::dispatch_hash_cached() -> u64` — same lifecycle; the classify pass's dynamic-pipeline cache is now keyed on `(dispatch_hash, msaa)` instead of `(Vec<BucketEntry>, Option<u32>)`, so the per-frame probe is alloc-free.

### What that means for the priorities below

- **Priority 1** ("Defer first-party opaque pipeline pre-warm") is now the highest-value un-landed item. The plumbing (`shader_descriptors_for_config` + `merge_resolved`) is already in place — the remaining step is wiring a trigger when a material of a new shader_id is first registered/inserted instead of compiling all 4 first-party variants at cold-boot. See updated approach below.
- **Priority 2** ("Defer EVSM / Line / Shadow render pre-warm") is unchanged.
- **Priority 3** ("Replace the `msaa_resolve_samples` workaround") is unchanged.
- **Priority 4** ("Audit the rest of the eager pre-warm set") has been narrowed — effects bloom-on, classify non-active-MSAA, opaque non-active-MSAA × non-active-mipmap, HZB non-active-seed, picker non-active-MSAA are all already lazy. Audit what's left.
- Two **new priorities** were added from the dynamic-materials work — see Priorities 5 + 6 below.

---

## Context

The original error on Android Chrome:

```
Error initializing Renderer: [compute pipeline]:
  PipelineCreation("Pipeline creation [Internal] error:
    CreateComputePipelines failed with VK_ERROR_INITIALIZATION_FAILED
    - While initializing [ComputePipeline (unlabeled)]
    at CheckVkSuccessImpl (../../third_party/dawn/src/dawn/native/vulkan/VulkanError.cpp:106)")
```

Same renderer, same scene loaded fine on OSX Chrome (Metal-via-Dawn). Failure was exclusive to Android Chrome (Vulkan-via-Dawn) and only at startup.

The user described the desired architecture clearly during investigation:

> We should NOT eagerly precompile what we're not using. We should have an architecture which is more like: 1) declare a batch of shaders/pipelines that need to be compiled/created, 2) kick them all off (FuturesUnordered / Promise.all), 3) wait for the batch to finish. And we should be able to do it at any time — so up-front, we ONLY do it for what's NECESSARY for the defaults (msaa on, bloom off, etc.). Then, when anything is changed, kick off the batch that needs to be done at that point in time.

This plan implements progress toward that architecture; the geometry pass is done, the rest is queued.

---

## Root cause (the actual one — after several wrong guesses)

**The PBR opaque compute shader was emitting SPIR-V large enough to exceed the Android Vulkan driver's pipeline-compile complexity ceiling.**

The path that triggers it: `msaa_resolve_samples` in [helpers/material_shading.wgsl:234-266](../../crates/renderer/src/render_passes/material_opaque/shader/material_opaque_wgsl/helpers/material_shading.wgsl#L234) was unrolled into 4 explicit calls to `msaa_process_sample` — one per MSAA sample. `msaa_process_sample` is a ~150-line function with the per-shader-id branch (UNLIT/TOON/PBR), `compute_material_color` (texture pool sampling + mipmapping), and `apply_lighting` / `apply_lighting_per_mesh` (the full lighting loop with IBL/BRDF/shadows). Tint inlined each of the 4 call sites, producing SPIR-V with **the entire shading pipeline duplicated 4 times** for the edge-resolve path, plus once more for the main non-edge path.

Only PBR fails because `msaa_resolve_samples` is called exclusively from the PBR-only template branch in [compute.wgsl:256-270](../../crates/renderer/src/render_passes/material_opaque/shader/material_opaque_wgsl/compute.wgsl#L256) (PBR owns skybox-edge resolution per the comment at compute.wgsl:241-247). UNLIT/TOON/FLIPBOOK don't touch the resolve path. The empty pipeline doesn't touch any shading.

### Things that LOOK related but aren't

The investigation ruled these out via direct testing (see [§ Investigation log](#investigation-log)). Don't waste time on them next session:

- **Multisampled `textureLoad` in compute**: works fine on this device. Verified by forcing MSAA off — same 4 opaque shaders still failed.
- **`maxUniformBufferBindingSize` (lights array at exactly 64 KB)**: device reports 65,536 max; binding is exactly 65,536. Reducing the array to 32 KB didn't change anything.
- **`maxStorageBuffersPerShaderStage`**: device reports 16, shader uses 9. Not close.
- **`maxBindGroups`** / `maxBindingsPerBindGroup`: we're under both.
- **rgba16float storage texture write**: the empty shader uses `textureStore(opaque_tex, ...)` on the same `texture_storage_2d<rgba16float, write>` binding and succeeds.
- **Cube texture sampling, dynamic indexing into uniforms / storage buffers, integer texture loads**: all proven to work via bisect step 2 (in [§ Investigation log](#investigation-log)).

### How the diagnosis was reached

Bisected by stubbing the body of `compute.wgsl`'s `main()` and progressively re-enabling chunks. Confirmed via two experiments:

1. Stub everything → all 5 pipelines compile in 40 ms.
2. Stub only the 4 unrolled `msaa_process_sample` calls down to 1 → all 5 pipelines compile in 2.8 s, full body intact.

---

## What's currently in the codebase

### Permanent improvements (keep)

These are real wins regardless of how the rest of the plan proceeds. Roughly ranked by leverage.

#### 1. Geometry MSAA pre-warm cut

[crates/renderer/src/render_passes/geometry/pipeline.rs](../../crates/renderer/src/render_passes/geometry/pipeline.rs) was rewritten to match the lazy-pool pattern that opaque/classify/HZB/picker already use:

- `shader_cache_keys(multisampled_geometry: bool)` and `build_descriptors(..., multisampled_geometry: bool)` now take the active MSAA explicitly and emit 3 shaders + 9 pipelines for just that branch (was: 6 shaders + 18 pipelines for both branches).
- `GeometryRenderPipelineKeys.{no_anti_alias, msaa_4_anti_alias}` are `Option<Level1>`; only the active branch is populated at cold-boot.
- New `merge_resolved(...)` mirrors `MaterialClassifyPipelines::merge_resolved` so toggling MSAA back and forth pays the compile cost only on the first transition in each direction.
- New `has_branch_for(anti_aliasing)` lets `set_anti_aliasing` skip the recompile if the now-active branch is already cached.

[crates/renderer/src/anti_alias.rs](../../crates/renderer/src/anti_alias.rs) `set_anti_aliasing` was extended to:
- Build geometry descriptors when the new MSAA branch isn't yet populated.
- Run the geometry render-pipeline batch in a `try_join`'d pair alongside the existing compute batch (matches the cold-boot pattern in `AwsmRendererBuilder::build` at lib.rs:1568-1583).

Result on Android: render-pipeline batch at init went from 27 pipelines / 5 s + watchdog kill → 18 pipelines / ~700 ms.

#### 2. Per-pipeline labels + cumulative timings in `ensure_keys`

- [crates/renderer/src/pipelines/compute_pipeline.rs](../../crates/renderer/src/pipelines/compute_pipeline.rs): label format `compute:ShaderKey(_):PipelineLayoutKey(_)`; logs per-pipeline finish-order and total wall-clock under `target = "awsm_renderer::boot_timing"`.
- [crates/renderer/src/pipelines/render_pipeline.rs](../../crates/renderer/src/pipelines/render_pipeline.rs): label embeds the shader's `debug_label()` so the log reads e.g. `render:Geometry(ShaderKey(1)):PipelineLayoutKey(12)` directly. Same per-pipeline + total format.

Before this, the Android error said `[ComputePipeline (unlabeled)]` and we had no way to know which pipeline failed. The labels also surface in any `popErrorScope` / `onuncapturederror` paths.

#### 3. Adapter + device limits log + `onuncapturederror` hook

[crates/renderer-core/src/renderer.rs](../../crates/renderer-core/src/renderer.rs) now logs (one-shot at device creation, filter `target = "awsm_renderer_core::limits"`):

```
device limits: maxStorageBuffersPerShaderStage=16 maxStorageBufferBindingSize=2147483644
  maxUniformBufferBindingSize=65536 maxBufferSize=4294967292 maxBindGroups=4
  maxBindingsPerBindGroup=1000 maxSampledTexturesPerShaderStage=48
  maxComputeWorkgroupStorageSize=16384 maxComputeInvocationsPerWorkgroup=256
  maxComputeWorkgroupSizeX=256 maxComputeWorkgroupSizeY=256
```

Saved hours of guessing — confirmed Android isn't actually capping us at 8 storage buffers, etc.

Also: a `device.onuncapturederror` hook (using `js_sys::Reflect` because the web-sys typed bindings for `GpuValidationError` / `GpuOutOfMemoryError` / `GpuInternalError` / `GpuUncapturedErrorEvent` aren't in our enabled feature set). Logs under `target = "awsm_renderer_core::uncaptured_error"`. Didn't surface anything new for the specific failure we hit (Dawn passes async pipeline failures through the Promise rejection, not this channel), but earns its keep for runtime errors.

#### 4. `PipelineVariantNotCompiled` error variant

[crates/renderer/src/error.rs](../../crates/renderer/src/error.rs):

```rust
#[error("Pipeline variant not yet compiled: {0}")]
PipelineVariantNotCompiled(&'static str),
```

Used by the geometry lookup tree when a branch is `None`. Should be reused as more passes go lazy-pool.

### R&D workaround (replace before shipping)

#### 5. `msaa_resolve_samples` loop conversion

[helpers/material_shading.wgsl:241-281](../../crates/renderer/src/render_passes/material_opaque/shader/material_opaque_wgsl/helpers/material_shading.wgsl#L241) — replaced the 4 unrolled `msaa_process_sample` calls with a single call inside a `for s in 0..msaa_sample_count` loop. Same behavior (still processes every sample, same blend), but Tint sees one call site and the SPIR-V is small enough for the Android driver to compile.

This is the change that takes Android from "init fails" → "init succeeds." It is **NOT** the right long-term shape:

- PBR compute pipeline compile is still 14.2 s on the test device — *right* at the watchdog edge. A slightly weaker device or memory pressure would push it over.
- The TODO(quality) comment at the workaround site lists two cleaner shapes:
  - **(a)** Per-sample intermediate buffer + dedicated resolve pass. Main pass writes per-sample shaded colors to a 4-layer storage texture (or equivalent); a tiny resolve pass blends them. Decouples shader complexity from sample count.
  - **(b)** Specialize the resolve for the common case. Most MSAA edges are same-material or material-vs-skybox. Cross-material edges are rare. A fast path for the common case + a slow path for cross-material would dramatically reduce the average-case SPIR-V cost.

When you come back to this: pick (a) for correctness/quality and tie it into the broader lazy-pool refactor; (b) is a smaller win that doesn't change architecture.

---

## What's queued (the real work)

Ordered by impact. Each item is independent — they can land in any order.

### Priority 1 — Defer first-party opaque pipeline pre-warm per shader_id

**Why**: today, 4 first-party opaque compute pipelines (PBR + UNLIT + TOON + FLIPBOOK) compile at init for the active MSAA × mipmap combo, even though an empty scene dispatches zero of them. The PBR one alone takes ~14 s on Android. Lazy-compiling per-shader-id at first-use:
- Drops cold-boot compute batch from 5 → 1 (just the empty pipeline) for a zero-mesh scene.
- For a scene that only uses PBR (the common case), drops it from 5 → 2.
- Spreads the per-shader compile cost over the load-the-scene flow instead of stacking it at init.
- Aligns with the user's stated architecture (compile what's needed when it's needed, in a batch).

**Plumbing**: already half-done. `MaterialOpaquePipelines::shader_descriptors_for_config` takes `&AntiAliasing` and emits descriptors for all 4 first-party shader_ids × the active MSAA/mipmap. `merge_resolved` already supports filling in a subset (it iterates the slot vec; missing slots stay as their previous Option value). The eager path today is `shader_descriptors_and_layouts` in [pipeline.rs:142](../../crates/renderer/src/render_passes/material_opaque/pipeline.rs#L142) which always emits all 4 shader_ids unconditionally.

**Concrete approach**:
- Add a `shader_ids: &[MaterialShaderId]` parameter (or a separate "for_shader_id" variant) to `shader_descriptors_for_config` so callers can request a subset.
- At cold-boot, emit only `[]` (or `[]` + the empty pipeline) — every first-party shader_id is deferred. The empty pipeline stays eager (it's tiny and runs for skybox-only frames).
- Add `AwsmRenderer::ensure_opaque_shader_compiled(shader_id) -> impl Future<Output = Result<()>>` that:
  1. Returns immediately if the pipeline for the current `(shader_id, msaa, mipmap)` is already cached on `MaterialOpaquePipelines::main`.
  2. Otherwise builds descriptors for that single shader_id via the new variant of `shader_descriptors_for_config`, runs them through one batched `Shaders::ensure_keys` + one batched `ComputePipelines::ensure_keys`, and folds via `merge_resolved`.
- Wire the trigger: in `Materials::insert` (and the gltf-loading path's equivalent), call `ensure_opaque_shader_compiled(material.shader_id()).await` for any first-party variant. Batch multiple insertions in one frame by collecting the set of shader_ids and running one async batch — the gltf loader and the scene-editor's `materialize_*` paths already operate on sets of meshes, so threading a `HashSet<MaterialShaderId>` through is natural.
- Dynamic materials already follow this pattern (the `prewarm_pipelines` call after `register_material`). Just generalize it to first-party.

**Caveat**: `Materials::insert` is sync today. The async trigger needs either a) propagating async through that path (call site in scene-editor's bridge is already async; gltf loader is async), or b) marking the material "pending-pipeline" and dispatching `ensure_opaque_shader_compiled` from the next render frame's pre-amble. (b) is simpler and avoids API churn — render frames skip materials whose pipeline isn't ready yet (the dispatch path already has `get_compute_pipeline_key(...) -> Option` for the lazy-pool path; a `None` return safely skips the bucket).

**Acceptance**: on Android, init's compute batch should compile in <1 s (down from 14 s — empty + classify only) and the PBR compile fires lazily on first PBR-mesh load with no watchdog pressure. The new `awsm_renderer::boot_timing` logs make this directly measurable.

### Priority 2 — Defer EVSM / Line / Shadow render pre-warm

**Why**: secondary contributors to cold-boot pipeline count.

- **EVSM** (3 compute pipelines): only useful when at least one shadow-casting light exists. [crates/renderer/src/shadows/evsm.rs:144-146](../../crates/renderer/src/shadows/evsm.rs#L144) currently registers them at init.
- **Line render** (2 render pipelines): only useful when the user adds a line primitive. The line pass cache key registration is in `crates/renderer/src/render_passes/lines/`.
- **Shadow Generation VS** (2 render pipelines): only useful with shadow casters. `crates/renderer/src/shadows/helpers.rs` references the pipeline layout.

**Concrete approach**: same lazy-pool pattern. Gate each on a "first use" trigger.

**Acceptance**: cold-boot render batch drops from 18 → 9 pipelines on this Android device. (Geometry stays at 9; lines/shadow-gen become lazy.)

### Priority 3 — Replace the `msaa_resolve_samples` workaround

**Why**: the loop conversion (#5 above) works but is fragile; the compute pipeline compile still takes 14 s on this device. The R&D NOTE in the shader file calls this out.

**Approach** — pick one or both:

- **(a) Per-sample intermediate buffer + separate resolve pass.** Main material pass writes per-sample shaded colors to a 4-layer storage texture. A tiny resolve pass (~30 lines, dynamic indexing into the layers) blends them. The main pass shader no longer needs `msaa_resolve_samples` at all. This is the architecturally clean answer.
- **(b) Specialize for the common edge case.** Most MSAA edges are same-material-vs-same-material or material-vs-skybox. Cross-material edges are rare. Fast path: shade once at sample 0, blend with skybox for the missing samples. Slow path (cross-material): the current loop. Probably halves average-case SPIR-V size.

**Acceptance**: PBR compute pipeline compile drops to <2 s on the Android test device. Visual output for the cross-material-edge case still looks right.

### Priority 4 — Audit the rest of the eager pre-warm set

**Why**: complete the user's "only what's necessary for defaults" architecture.

Today's eager batch (after Priority 1+2 above): probably 6 compute + 9 render pipelines. Walk the set and verify each is truly necessary for a zero-scene render. Things to scrutinize:

- `material_classify` (1 compute) — runs every frame; needs default first-party bucket entries pre-compiled. Probably keep eager.
- `HZB` (2 compute, `features.gpu_culling`) — runs every frame when on. Keep eager when feature on.
- `occlusion cull + compaction` (2 compute, `features.gpu_culling`) — same.
- `coverage` (1 compute, `features.coverage_lod`) — same.
- `picker` (1 compute, `features.picking`) — runs only on mouse events. Could be lazy.
- `decal_classify` (1 compute, present-when-decals) — currently gated by decal_bg presence. OK.
- `display` (1 render) — needed for first frame. Keep eager.
- `effects` (render) — bloom/SMAA/DoF off by default per `PostProcessing::default()`. The variants for effects-off should be eager; effects-on variants should be lazy (compiled when toggled). Audit whether this is already true.

Each "make lazy" change is a 20-50 line patch following the [`MaterialClassifyPipelines::merge_resolved`](../../crates/renderer/src/render_passes/material_classify/pipeline.rs#L158) pattern.

### Priority 5 — Defer dynamic-material pipeline pre-warm per shader_id

**Why**: after a dynamic material is registered via `AwsmRenderer::register_material`, the renderer calls `prewarm_pipelines` which compiles every per-shader-id opaque variant (4 variants per registered material: MSAA × mipmap) + the classify variant + (for Blend) a transparent stub. That's correct for the common case where the user is about to render the material, but it's eager-against-might-use-later for the material-editor's authoring path — every keystroke that changes the WGSL fires a full recompile, even though only the active (msaa, mipmap) is dispatched on the preview canvas.

For the **single-config** authoring use case, this overpays by 4×. For the **multi-config** runtime case (a real scene with multiple materials each potentially exercised at multiple MSAA states), the existing behavior is right.

**Concrete approach**:
- Split `prewarm_pipelines` into two flavours:
  - `prewarm_dynamic_pipelines_for_config(shader_id, &anti_aliasing)` — compiles only the active config for the registered material. The material-editor's recompile sink uses this.
  - `prewarm_dynamic_pipelines_full(shader_id)` — the current behaviour, all 4 variants. The scene-editor's import-material path uses this (because the scene the user is editing may have meshes at multiple MSAA states).
- `AwsmRenderer::set_anti_aliasing` already calls `prewarm_dynamic_pipelines` post-flip; that stays.

Less impact than Priority 1 (dynamic materials are a smaller class than first-party) but the material-editor's per-keystroke recompile latency is the user-facing surface that benefits most.

**Acceptance**: material-editor's debounced recompile cycle drops from 4-variant to 1-variant per dynamic material, observable via `awsm_renderer::boot_timing` logs after each edit.

### Priority 6 — Build-time bundle: warmed pipeline cache

**Why**: every cold-boot today re-does WGSL→MSL/SPIR-V lowering from scratch because Dawn's pipeline cache is cleared on browser session start. On Chrome, the disk-backed shader-cache persists across reloads of the same origin in the same session — but a fresh-tab / new-origin / new-profile boot pays the full cost. The Geometry MSAA cut + the lazy-pool work cap the worst case at "~6 pipelines / a few seconds on Android"; below that, the dominant cost is intrinsic to Dawn's per-shader compile.

Two cache-leverage paths:

- **(a) `requestAdapter` device options.** The WebGPU spec exposes pipeline-cache helpers via the `GPUDevice` extension surface (chrome flag-gated today). When promoted, threading a stable cache identifier through `AwsmRendererWebGpuBuilder` would let return-visit cold-boots skip the compile entirely. Track the spec status; wire when it ships.
- **(b) Ship a pre-warmed cache for prod builds.** At `trunk build --release` time, run a build script that boots the renderer in a headless WebGPU context, drives every default-config compile, and serializes the resulting cache to disk alongside the wasm bundle. The runtime loads it via `requestDevice({pipelineCache: ...})`. Requires (a) to be available; the build-time tooling is a separate concern.

This is a "do later when the platform catches up" item, but worth tracking — the lazy-pool work is the maximum we can do on the JS side. Beyond it, the disk-cache surface is where time goes.

### Priority 7 — Going-forward rule

Once Priority 1-6 are done, add a one-line comment at the eager-batch site in [render_passes.rs:333-365](../../crates/renderer/src/render_passes.rs#L333) saying something like: *"Every entry here must be required for a zero-scene render. If it's scene-content-driven, route it through the per-pass `*_for_config` lazy path instead."*

---

## Diagnostic tooling (in place, ready to use)

### `task debug-mobile:chrome-check`

User-provided task that reloads the renderer on the connected Android phone via Chrome and captures the JS console output back to the terminal. This is the primary feedback loop — every change can be validated within ~30 seconds.

Run it from the project root.

### What to grep for in the output

| Pattern | What it tells you |
|---|---|
| `device limits:` | Adapter/device caps (max storage buffers, uniform binding size, workgroup storage size, etc.). |
| `phase = CompilingShaders \| BuildingPipelines \| Ready (+Tms phase, Tms total)` | Per-phase wall-clock during `AwsmRendererBuilder::build`. Decomposes cold-boot into the canonical phases. |
| `Shaders::ensure_keys: N shaders compiled in Tms` | Total wall-clock for a shader-compile batch. |
| `{Render,Compute}Pipelines::ensure_keys: N pipelines compiled in Tms` | Total wall-clock for a pipeline batch. |
| `pipeline N/M render:... cum=Tms ok\|ERR` | Per-pipeline finish time + outcome. The label has the shader name embedded. |
| `pipeline N/M compute:ShaderKey(_):PipelineLayoutKey(_)` | Compute pipeline label (less informative than render — Tint shader-label not threaded through here yet, see [§ Followups](#small-followups)). |
| `[asset_cache] model loaded: asset_id=AssetId(_) (Tms)` | Scene-editor gltf asset reaching `AssetStatus::Ready` (full load + populate wall-clock). |
| `[scene] model loaded: <GltfId> (Tms)` | Model-tests gltf finishing load. |
| `VK_ERROR_` | Vulkan-layer pipeline rejection. Catch-all for driver-side issues. |
| `External Instance reference no longer exists` | Watchdog killed the GPU instance — typically follows a long compile. |
| `GPU uncaptured` | Anything Dawn fires through `onuncapturederror`. |
| `phase = Ready` | Init succeeded end-to-end. |

All boot-timing logs use the `awsm_renderer::boot_timing` target. Filter to just these lines with `RUST_LOG=awsm_renderer::boot_timing=info` (or the equivalent in the browser's `tracing-subscriber` filter — `tracing-web` exposes the standard `EnvFilter` syntax).

### When stuck

Bisect the kernel by progressively moving an early `return;` through the shader body. The investigation log below shows this technique — 4 iterations got from "no idea" to "exact failing construct."

---

## Files left modified

8 files are in the working tree, all clean. Listed in dependency order:

1. **[crates/renderer-core/src/renderer.rs](../../crates/renderer-core/src/renderer.rs)** — limits log + onuncapturederror hook. Permanent.
2. **[crates/renderer/src/error.rs](../../crates/renderer/src/error.rs)** — `PipelineVariantNotCompiled` variant. Permanent.
3. **[crates/renderer/src/pipelines/compute_pipeline.rs](../../crates/renderer/src/pipelines/compute_pipeline.rs)** — per-pipeline labels + timings in `ensure_keys`. Permanent.
4. **[crates/renderer/src/pipelines/render_pipeline.rs](../../crates/renderer/src/pipelines/render_pipeline.rs)** — same, with shader debug_label embedded. Permanent.
5. **[crates/renderer/src/render_passes/geometry/pipeline.rs](../../crates/renderer/src/render_passes/geometry/pipeline.rs)** — MSAA-aware shader_cache_keys/build_descriptors, Option<Level1> branches, merge_resolved, has_branch_for. Permanent.
6. **[crates/renderer/src/render_passes.rs](../../crates/renderer/src/render_passes.rs)** — plumbed MSAA config to geometry. Permanent (minor).
7. **[crates/renderer/src/anti_alias.rs](../../crates/renderer/src/anti_alias.rs)** — extended set_anti_aliasing for geometry's new branch, try_join compute + render batches. Permanent.
8. **[crates/renderer/src/render_passes/material_opaque/shader/material_opaque_wgsl/helpers/material_shading.wgsl](../../crates/renderer/src/render_passes/material_opaque/shader/material_opaque_wgsl/helpers/material_shading.wgsl)** — **R&D workaround**: msaa_resolve_samples loop conversion. Has an inline R&D NOTE and TODO(quality) comment. Replace per Priority 3 above.

Everything compiles; full workspace `cargo check --target wasm32-unknown-unknown` is clean. `task debug-mobile:chrome-check` reaches `phase = Ready` on the Android test device.

The stale plan file at `~/.claude/plans/any-idea-why-i-quirky-unicorn.md` (created during the investigation) is superseded by this one and can be deleted.

---

## Small followups (nice-to-have, not blocking)

- **Compute-pipeline labels could include the shader debug_label too** (like render_pipeline.rs does today). The render-pipeline path threads it through via `descriptor.label`; the compute path uses the raw `compute:ShaderKey(_):PipelineLayoutKey(_)` form. A `Shaders::get_label(ShaderKey) -> Option<String>` helper would let compute pipeline labels read `compute:MaterialOpaque(...)` instead of just `ShaderKey(5)`. ~20 lines.
- **The `onuncapturederror` hook uses `js_sys::Reflect`** because the web-sys feature flags for `GpuValidationError` etc. aren't enabled. Adding `"GpuValidationError", "GpuInternalError", "GpuOutOfMemoryError", "GpuUncapturedErrorEvent"` to the workspace web-sys feature list would let us use typed bindings. Cleanup, not functional.

---

## Investigation log (for the historical record)

In rough order, the things I tried and what each told me:

| Hypothesis | Test | Result |
|---|---|---|
| Too many storage buffers per stage on Android | Logged `device.limits()` | Device reports 16; we use 9. Ruled out. |
| Render-pipeline batch overflowing watchdog | Cut geometry MSAA pre-warm (18 → 9 pipelines) | Render batch went from 5 s + kill to 660 ms. Real win; landed permanently. Did not fix compute side. |
| Wave-based pipeline issuance | Issued compute promises in chunks of 6 | Total wall-clock went from 8 s to 12 s — Dawn was already absorbing parallelism. Reverted. |
| Multisampled textureLoad in compute is the issue | Forced MSAA off | Same 4 opaque pipelines still failed. Hypothesis was wrong. |
| `lights: array<LightPacked, 1024>` at exactly 64 KB | Shrunk to 512 | Same failure. Ruled out. |
| Body of `main()` is the issue | Stubbed body to `return;` | All 5 PipelineLayoutKey(5) pipelines compiled in 40 ms. Body confirmed as culprit. |
| Body up to `material_load_shader_id` | Early-return at that point | UNLIT/TOON/FLIPBOOK compiled; only PBR still failed. Narrowed to PBR-unique code. |
| PBR-unique `msaa_resolve_samples` is the issue | Replaced its call with a constant write | All 5 pipelines compiled in 105 ms. Confirmed. |
| 4× unrolled `msaa_process_sample` inlining is the SPIR-V bloat | Reduced to 1 call | All 5 pipelines compiled in 2.8 s with full body intact. Confirmed mechanism. |
| Loop instead of unroll | Converted to `for s in 0..N` | All 5 pipelines compile (14.2 s for PBR). Works but slow — current state. |

The two wrong hypotheses early on (multisampled-textureLoad-in-compute, uniform-binding-at-limit) cost two iterations each. Net cost was a few hours; net benefit was a thoroughly confirmed diagnosis.
