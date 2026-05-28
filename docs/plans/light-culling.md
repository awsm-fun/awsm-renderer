# GPU light culling — handoff to finish Phase 2 + 1D

**Branch**: `light-culling`. **PR**: [#104](https://github.com/dakom/awsm-renderer/pull/104).
**Status**: Phase 1A + 1C landed and verified. Phase 2 + 1D **stashed**,
not pushed — three bugs found, two fixed, one unidentified. This file
is the operational handoff for the next session. When the work is
done, **the last commit deletes this file.**

If you are not the next-session implementer, you can skip the rest.

---

## Self-test: does the current PR still work?

Before touching anything, confirm the pushed PR baseline still
renders. From the repo root:

```sh
trunk serve --port 9090 --address 127.0.0.1 \
  --watch crates/frontend/scene-editor \
  --watch crates/renderer \
  --watch crates/renderer-core \
  --watch crates/editor \
  --watch crates/scene-schema \
  --watch crates/web-shared
```

Open `http://localhost:9090/`. In the browser DevTools console:

```js
window.wasmBindings.load_scene_by_path('tuning-1k-meshes')
```

Expected: the 32 × 32 grid of coloured boxes renders with shadow cast
from the directional light. If that doesn't work, **stop and
investigate before doing anything else** — the baseline regressed and
nothing else in this document is valid until you understand why.

Repeat with `tuning-64-lights` — should show 10 spheres on a floor
surrounded by 64 coloured punctual lights.

---

## What's already done (in the PR)

- **3D froxel cull compute pass.** One workgroup per
  `(tile_x, tile_y, z_slice)`. Side-plane sphere/frustum test, near/far
  from the exponential Z-slice mapping (`z(s) = near · (far/near)^s`),
  atomic-append into a per-froxel slice. WGSL in
  [`render_passes/light_culling/shader/light_culling_wgsl/`](../../crates/renderer/src/render_passes/light_culling/shader/light_culling_wgsl/).
- **Transparent shader consumes per-froxel lights** via
  `apply_lighting_per_froxel*` in
  [`shared_wgsl/lighting/lights.wgsl`](../../crates/renderer/src/render_passes/shared/shared_wgsl/lighting/lights.wgsl).
  Replaces the flat 1024-light loop on every transparent fragment with
  a per-pixel froxel walk (steady-state ≤ 32 lights).
- **`tuning-1024-lights` test fixture** authored via
  [`generate_tuning_scenes.rs`](../../crates/scene-schema/examples/generate_tuning_scenes.rs) —
  ~1000 point lights + an oversized floor + a Blend-mode glass pane +
  a 40-light corner cluster meant to trigger Phase 1D auto-grow.
- **Buffer scaffolding for Phase 2 + 1D** that didn't ship:
  `LightCullingBuffers` already exposes `set_max_per_froxel_capacity`,
  `params_buffer`, and the `overflow_buffer` has `copy_src` for the
  readback path.

The **opaque** pass still uses the per-mesh CPU bucket path
([`light_buckets/buckets.rs`](../../crates/renderer/src/light_buckets/buckets.rs))
and doesn't consume the cull output yet. That's what Phase 2 lands.

---

## What's stashed

Apply with:

```sh
git stash apply
```

The stash contains a working Phase 2 + Phase 1D implementation EXCEPT
for one remaining bug — **rendering produces a black canvas on every
scene, including empty ones with no lights.** Read this whole document
before diving in; the bug is subtle.

### What the stash architecturally does

The big move is **merging `mesh_light_indices` and the cull pass's
`froxel_storage` into a single storage buffer** owned by
`LightCullingBuffers`. Required because the opaque pass was already at
its `maxStorageBuffersPerShaderStage` ceiling — adding a second
storage binding pushed it over (Dawn counts `opaque_tex`
storage-texture inside the same per-stage budget; the renderer was at
9 storage buffers + 1 storage texture = 10/10 pre-Phase-2).

Layout of the merged `LightCullingBuffers::storage_buffer`:

```
[ 0 .. mesh_indices_capacity_u32 )            ← CPU-written per-mesh slice
[ mesh_indices_capacity_u32 .. end )          ← GPU cull pass per-froxel data
                                                stride = (max_per_froxel_capacity + 1) u32
                                                slot 0   = atomic count
                                                slots 1+ = light indices
```

`MeshLightIndicesGpu` no longer owns `indices_buffer`; it routes its
per-frame upload through `light_culling_buffers.storage_buffer` head
region. The cull WGSL adds `cull_params.mesh_indices_capacity_u32` to
every froxel offset so its writes land in the tail. Both opaque and
transparent bind the same physical buffer and use the same offset
arithmetic via `cull_params`.

`max_per_froxel_capacity` is **runtime, not compile-time** — it lives
on `cull_params` so the Phase 1D auto-grow path can bump it without
recompiling the cull or consumer shaders.

### What's also in the stash

- **Phase 1D auto-grow readback.** `FroxelOverflowReadbackState`
  mirrors `EdgeOverflowReadbackState` byte-for-byte —
  `copy_buffer_to_buffer` recorded into the per-frame encoder right
  after the cull dispatch, `spawn_local` `mapAsync` after submit,
  ingest at next frame's top + `set_max_per_froxel_capacity(current * 2)`
  on observed overflow.
- **Phase 2 sentinel routing.** `MeshLightIndicesGpu` writes
  `light_slice_count = 0xFFFFFFFFu` into `MaterialMeshMeta` for meshes
  in `LightMeshBuckets::oversized_meshes()`. Opaque WGSL branches on
  the sentinel and takes `apply_lighting_per_froxel` instead of the
  per-mesh walk.
- **Temporary cull verification.** `debug_cull_heatmap` field on the
  opaque template, gated by `option_env!("AWSM_DEBUG_CULL_HEATMAP")`.
  When set, the opaque shader overrides the final colour with a
  per-pixel green→red gradient over the froxel light count.

---

## Bugs found and fixed (already in the stash)

### 1. Mapped staging ring oversized

`MeshLightIndicesGpu::write_gpu` was passing `total_storage_bytes` (~30 MB
on a 1080p viewport) as the `MappedUploader::write_dirty_ranges`
`dest_size` parameter. `MappedStagingRing` interprets that as the
backing-buffer size and allocates `ring_depth × dest_size` slots with
`mappedAtCreation: true`. Three × 30 MB = 90 MB of mapped staging
buffers, which exhausted Chrome's device-wide mapped pool and broke
unrelated mapped uploads (Shadow Descriptors started warning with
`createBuffer ... is too large for the implementation when
mappedAtCreation == true` once per frame).

**Fix in stash**: pass `head_region_bytes = mesh_indices_capacity_u32 × 4`
instead. The CPU only writes the head region; the cull pass writes
the tail via shader-side atomics with no host upload, so the staging
ring never needs to back it.

### 2. `ensure_viewport` ordering

`render.rs` preamble was calling `mesh_light_indices_gpu.write_gpu`
BEFORE `light_culling_buffers.ensure_viewport`. On the first frame
(where the placeholder 16 × 16 viewport gets resized to the real
swap-chain size), the viewport resize calls `Self::new(...)` and
replaces the whole `storage_buffer`, wiping the freshly-uploaded mesh
indices.

**Fix in stash**: moved the cull-pass per-frame setup (`ensure_viewport`,
`write_params`, `reset_overflow`) above `mesh_light_indices_gpu.write_gpu`
so the buffer is sized first, then the mesh upload lands in the final
buffer for the frame.

### 3. ⚠️ Black canvas — root cause unidentified

After both fixes above, every scene renders fully black — including
empty scenes that should show just the procedural skybox. **No GPU
validation errors. No WGSL compile errors. No uncaptured error queue
entries.** The renderer is running, frames are submitted, the cull
pass dispatches, the opaque empty pipeline (which is the skybox path
on an empty scene) compiles cleanly.

This is the one you have to fix.

---

## Debugging recipe for bug #3

Work in this order. Don't skip steps.

### Step 1 — Confirm both fixes from the stash actually applied

```sh
git stash apply
cargo check -p awsm-renderer    # must pass clean
grep -n "head_region_bytes" crates/renderer/src/light_buckets/gpu.rs   # fix #1
grep -n "viewport_w_for_cull" crates/renderer/src/render.rs            # fix #2
```

If either grep returns nothing, the stash didn't apply cleanly —
investigate before continuing.

### Step 2 — Reproduce the black canvas on the simplest case

Launch trunk (see "Self-test" above). Navigate to the scene editor
WITHOUT loading any scene. You should see the procedural skybox.
**You will see black.** That's bug #3.

Take a screenshot. Then in the DevTools console:

```js
// dump all queued GPU device errors
const adapter = await navigator.gpu.requestAdapter();
const device = await adapter.requestDevice();
device.pushErrorScope('validation');
// trigger a frame somehow (resize the canvas slightly)
const err = await device.popErrorScope();
console.log('error:', err);
```

Document what `err` contains. The bug may surface only there — the
production renderer doesn't print uncaptured validation errors when
they fire mid-frame on a different device handle.

### Step 3 — Hypothesis ranking (most-to-least likely)

1. **Opaque-empty pipeline silently fails compile** because the new
   bindings on the `lights` BGL (slot 2 lights_storage, slot 3
   cull_params) don't match the empty pipeline's pipeline-layout
   expectations. Check by adding a `tracing::info!` at every pipeline
   compile site for `OpaqueEmpty` and confirming `Ready` fires.
2. **The merged storage buffer's `min_binding_size`** computed from the
   WGSL struct doesn't match what the host binds. The
   `lights_storage: array<u32>` shader binding has no minimum size,
   but `cull_params: CullParams` has a fixed 48-byte struct. Verify
   `LightCullingBuffers::params_buffer` is created at ≥ 48 bytes from
   frame 0 (it is, but double-check the bytes are actually written
   before the first opaque dispatch).
3. **Bind-group recreation order** still wrong. The fix in step 2
   ensured `light_culling_buffers` is sized before
   `mesh_light_indices_gpu.write_gpu`, but `bind_groups.recreate(...)`
   runs even later (around `render.rs:460`). Between the cull-buffer
   resize (which marks `LightCullingFroxelsResize`) and the recreate
   call, no rendering happens — but ALSO between the recreate and the
   first render pass, the bind-group routing for
   `LightCullingFroxelsResize` may not be hitting OpaqueLights
   correctly. Re-check the fan-out in `bind_groups.rs:363` and confirm
   `recreate_lights` actually rebuilds against the new buffer.
4. **The `MaterialMeshMeta.light_slice_count` sentinel collides with
   "no lights" semantics.** The stash sets count = `0xFFFFFFFFu` for
   oversized meshes. The opaque shader's per-mesh path interprets that
   as a huge count and tries to loop 4 billion times — but only if the
   sentinel branch isn't taken first. Verify that on an empty scene,
   no mesh has the sentinel (no meshes = no sentinel writes), so
   `light_slice_count = 0` everywhere. The bug should not be visible
   on an empty scene if this hypothesis is correct.
5. **The shared `lights.wgsl` template substitution.** Phase 2's stash
   renames `mesh_light_indices` → `lights_storage` everywhere and adds
   the per-froxel walk under `{% if use_froxel_lights %}`. The empty
   template sets `use_froxel_lights = false`, so the per-froxel walk
   isn't emitted — but the binding declarations (`@binding(2)
   lights_storage`, `@binding(3) cull_params`) ARE emitted
   unconditionally from `material_opaque_wgsl/bind_groups.wgsl`. If
   the empty pipeline doesn't reference those bindings in any function
   body, WGSL drops them as dead — but the pipeline LAYOUT still
   requires them. Pipeline-layout vs shader-binding mismatch would
   throw a validation error, but it might be swallowed.

### Step 4 — Targeted instrumentation

Add a one-shot log at the top of the opaque pass's `render(...)` that
prints which bucket(s) have non-zero workgroup counts. If everything
is 0, the classify pass didn't write any tiles — the bug is upstream
of opaque entirely. If the opaque dispatch is correct but the output
is black, the bug is in opaque shading.

`crates/renderer/src/render_passes/material_opaque/render_pass.rs`
has the dispatch. Add a `tracing::info_span` around it with the
indirect-args buffer contents read back via `mapAsync` (one-shot,
discarded after the first hit).

### Step 5 — Bisect against the PR baseline

If steps 1–4 don't isolate it, copy `crates/renderer/src/` to a
scratch dir and progressively revert pieces of the stash until the
canvas un-blacks. The order to revert in (most → least likely to
matter):

1. Opaque WGSL sentinel branch (`compute.wgsl`) — revert to PR
   baseline's plain `apply_lighting_per_mesh` call.
2. Opaque BGL changes in `bind_group.rs` — go back to single
   `mesh_light_indices` binding.
3. `MeshLightIndicesGpu` refactor — go back to owning its own buffer.

Whichever step un-blacks the canvas is the bug surface.

---

## Acceptance criteria for shipping the stash

1. `tuning-1k-meshes` renders identically to the PR baseline (same
   shadow falloff on the boxes; visually compare to the screenshot in
   the PR description).
2. `tuning-64-lights` renders the 10 spheres with multi-coloured
   punctual lighting matching the PR baseline.
3. `tuning-1024-lights` renders a 100m floor with ~1000 small point
   lights visible as coloured pools on the floor, plus the back wall,
   plus the glass pane in front of the back wall. The camera will
   need to be positioned with `window.wasmBindings.set_camera_…` or
   manual orbit-cam — the default load position is too distant for
   the small props to be visible without adjustment.
4. Run with `AWSM_DEBUG_CULL_HEATMAP=1` env var set at trunk launch;
   reload; load `tuning-1024-lights`. The opaque floor should show a
   smooth green-to-red gradient sampling the per-froxel light count —
   greenish where there's 1 light, yellow-orange under the corner
   cluster, red where overflow happened.
5. Watch the dev log; after the corner cluster (which exceeds the
   default `max_per_froxel_capacity = 32`) renders for one frame,
   you should see `light-culling overflow auto-grow: doubled
   max_per_froxel_capacity (32 -> 64) to absorb N dropped light
   indices from the prior frame` (the Phase 1D readback firing).
   Steady-state from the next frame onwards: heatmap settles into
   green/yellow (no more red), overflow auto-grow doesn't fire again.

When all five pass:

- Remove the temporary `debug_cull_heatmap` field, the
  `AWSM_DEBUG_CULL_HEATMAP` env-var toggle, and the heatmap override
  block in `material_opaque_wgsl/compute.wgsl`. Cache-key churn from
  the removal: this field is only on `ShaderTemplateMaterialOpaqueCompute`,
  not on the cache key, so removing it costs nothing.
- Update the PR description to mention Phase 2 + 1D landed.
- Squash or rebase your work onto the existing PR commits (your
  call — the PR is small enough either way).
- **Delete this file** (`git rm docs/plans/light-culling.md`) in the
  final commit. The plan is done; the architecture is documented in
  the code (`crates/renderer/src/render_passes/light_culling/` module
  docs + `LightCullingBuffers` doc comments).

---

## Notes on what NOT to redo

- **Don't try to keep `mesh_light_indices` as a separate buffer.**
  That path is what blew up the opaque storage cap. The merge is the
  right architecture — the bug isn't with merging, it's with one of
  the bind-group / pipeline interactions documented above.
- **Don't reintroduce `MAX_PER_FROXEL_CAPACITY` as a WGSL `const`.**
  The auto-grow path needs it as a runtime `cull_params` field. The
  cache key field `froxel_max_per_froxel_capacity` is still useful for
  the consumer (transparent/opaque) shaders' compile-time clamp where
  one exists, but the cull pass itself reads the runtime value.
- **Don't fold `cull_params` into the camera buffer.** Tempting (saves
  one binding) but binds the cull-pass lifecycle to camera-buffer
  recreation, which is a separate concern. Camera updates happen
  per-frame at sub-µs cost; cull params change at viewport-resize
  cadence. Keep them separate.
- **Don't try to ship Phase 2 without Phase 1D.** They share the
  `cull_params.max_per_froxel_capacity` runtime field. Half of one
  without the other is more work than just landing both together.
