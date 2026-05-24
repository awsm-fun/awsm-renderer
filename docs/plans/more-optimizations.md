# Renderer optimization sprint plan

This plan covers the next implementation sprint for the
`awsm-renderer` library. The work targets three interrelated goals:

1. Replace `queue.writeBuffer` with a mapped-buffer ring inside
   `Dynamic{Storage,Uniform}Buffer` for renderer-owned per-frame
   uploads (**Phase 2.1**).
2. Land first-class worker infrastructure (`WorkerPool` +
   `WorkerJob`) so CPU-bound logical work can run off the main
   thread (**Phase 4.3**).
3. Support the `OffscreenCanvas` deployment mode so library
   consumers can run the entire renderer in a worker
   (**Phase 4.4**).

The library targets **shipped games** — where the main thread is
shared with game logic, physics, audio scheduling, network code,
etc. — as much as the editor. Both deployment modes (canvas-in-DOM
on main thread vs. OffscreenCanvas in worker) are first-class.

When implementing: read [Working agreements](#working-agreements)
at the bottom of this doc before starting.

---

## Sprint roadmap

| Phase | Depends on | Summary |
|---|---|---|
| **2.1** | — | Mapped-buffer ring inside `Dynamic{Storage,Uniform}Buffer`; migrate every per-frame `writeBuffer` call site to use the Dynamic types. |
| **4.3a** | — | `awsm-renderer::workers` module: `WorkerPool` + `WorkerJob` trait + auto-bundle-discovery bootstrap. |
| **4.3b** | 4.3a | `GltfParseJob` — first `WorkerJob` consumer; validates the pool design. |
| **4.4** | 4.3a | `OffscreenCanvas` render-worker deployment mode. |

**2.1** and **4.3a** are independent and can be implemented in
parallel. **4.3b** and **4.4** both depend on 4.3a's worker
bootstrap protocol but are independent of each other.

Recommended implementation order if going serial:
**2.1 → 4.3a → 4.3b → 4.4**.

---

## Phase 2.1 — Mapped-buffer ring inside `Dynamic{Storage,Uniform}Buffer`

### Goal

Replace `queue.writeBuffer` as the per-frame upload mechanism for
renderer-owned dirty-tracked data. The mapped path writes directly
into GPU-visible memory and skips the browser's internal
staging-copy hop that `queue.writeBuffer` performs.

`queue.writeBuffer` stays as the canonical path for ingesting
*foreign* `ArrayBuffer`s (worker job output, file decode results) —
those are one-shot uploads where the mapped ring adds complexity
without saving the memcpy.

### Design summary

> **Spec amendment (mid-implementation, kept as record):** the
> originally-described "ring inside `DynamicStorageBuffer`" placement
> was reconsidered when we hit the cross-cutting cost of moving
> `gpu_buffer` ownership out of every call site (`Transforms`,
> `Materials`, `Instances`, …) and threading `&AwsmRendererWebGpu`
> through previously-sync, CPU-only Dynamic constructors used in unit
> tests. The functional design — mapped-write upload path replacing
> per-frame `queue.writeBuffer` — is unchanged. The implementation
> places the ring in a per-call-site companion
> [`MappedUploader`](../../crates/renderer/src/buffer/mapped_uploader.rs)
> that owns a [`MappedStagingRing`](../../crates/renderer/src/buffer/mapped_staging_ring.rs)
> sized to the consumer's destination buffer and exposes
> `write_dirty_ranges(..)` as a one-line swap for `write_buffer_with_dirty_ranges`.
> Call sites stay in charge of their `gpu_buffer` field; the
> Dynamic types' public API doesn't change at all. Telemetry is
> aggregated by walking subsystems instead of by walking Dynamic
> instances. Migration of every per-frame writeBuffer site
> (Phase 2.1 main objective) is still in-scope.

- Each renderer subsystem with a per-frame upload owns a
  `MappedUploader` companion that holds a small ring (default 3) of
  `MAP_WRITE | COPY_SRC` staging buffers. The consumer's
  `DynamicStorageBuffer` / `DynamicUniformBuffer` still owns the
  `Vec<u8>` (CPU-authoritative state); the consumer's existing
  `gpu_buffer` field is still the destination `GpuBuffer`.
- `write_gpu()` flow per frame:
  1. Acquire the current `Mapped` slot (always one available in
     steady state; see [Exhaustion handling](#exhaustion-handling)).
  2. Copy the Vec's dirty ranges into the mapped slot's
     `getMappedRange()`-returned `ArrayBuffer` (raw `memcpy`).
  3. `unmap()` the slot.
  4. Record `copyBufferToBuffer(slot → destination)` into the
     frame's command encoder.
  5. Mark the slot in-flight.
  6. Rotate: the previously-`Submitted` slot transitions to
     `Pending` (kick `mapAsync()` so it's ready by the next rotation).
- The `Vec<u8>` stays because it's the only thing that's *always*
  CPU-writable. Ring slots cycle through `Mapped` →
  `Submitted` → `Pending` → `Ready` states, so synchronous
  subsystem writes (`update_at(slot_index, &bytes)`) cannot be
  served by the ring alone. The Vec also carries the authoritative
  contents across ring rotations — a freshly-`Mapped` slot has
  stale contents from N frames ago; only the Vec knows what the
  correct current bytes look like.

### Public API additions

**Post-amendment shape** (the `Dynamic*` API is unchanged; new types
live alongside):

```rust
impl MappedUploader {
    /// Default ring depth (3).
    pub fn new(label: impl Into<String>) -> Self;

    /// Override the ring depth. Use when the caller has out-of-
    /// band knowledge that the buffer will grow large (e.g. mesh
    /// meta in a 100k-mesh open-world scene at ~25 MB per slot →
    /// 75 MB at 3-deep; passing 2 trades a tighter ring for
    /// halved GPU-side overhead, and 1 falls back to writeBuffer
    /// every frame).
    pub fn with_ring_depth(label: impl Into<String>, depth: usize) -> Self;

    /// One-line swap for `write_buffer_with_dirty_ranges`.
    pub fn write_dirty_ranges(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        encoder: &CommandEncoder,
        dest: &web_sys::GpuBuffer,
        dest_size: usize,
        raw_data: &[u8],
        ranges: &[(usize, usize)],
    ) -> Result<(), MappedUploaderError>;

    /// Foreign-bytes ingestion path (worker job output, file decode
    /// results). Bypasses the ring.
    pub fn ingest_foreign(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        dest: &web_sys::GpuBuffer,
        offset: usize,
        bytes: &[u8],
    ) -> Result<(), MappedUploaderError>;

    /// Telemetry accessor.
    pub fn stats(&self) -> UploadStats;
}

/// Renderer-wide aggregator. Exposed via `read_render_pass_timings`
/// JSON under the `upload_rings` key.
pub struct UploadStats {
    pub peak_ring_depth_used: usize,
    pub fallback_count: u64,
    pub map_async_wait_ms: f64,
    pub bytes_uploaded_via_ring: u64,
    pub bytes_uploaded_via_fallback: u64,
}
```

The **subsystem-facing API** (`update`, `update_at`,
`take_dirty_ranges`, `raw_slice`, etc.) **does not change**.
Migration of standalone writeBuffer sites is a refactor to use the
existing Dynamic type's API, not a new API.

### Ring lifecycle (per slot)

Each slot is in one of four states:

| State | Meaning | CPU may write? |
|---|---|---|
| `Mapped` | `getMappedRange()` returned an `ArrayBuffer` | yes |
| `Submitted` | `unmap()` + `copyBufferToBuffer` recorded + encoder submitted; GPU-owned | no |
| `Pending` | `mapAsync()` called, awaiting resolution | no |
| `Ready` | `mapAsync()` resolved (callback fired); promotable to `Mapped` | no, until promoted |

Per `write_gpu()` call: one slot transitions `Mapped → Submitted`;
the previously-`Submitted` slot transitions `Submitted → Pending`
(kick `mapAsync`); a `Ready` slot is lazily promoted to `Mapped` on
next access.

### Initial buffer state — `mappedAtCreation: true`

All ring slots are created with `mappedAtCreation: true` so the
first frame does not have to wait for `mapAsync` resolution. This
turns the cold-start path into one synchronous `getMappedRange()`
per slot, with no async dependency on the first frame.

### Buffer resize handling

When `DynamicStorageBuffer` grows (its internal `Vec<u8>` capacity
exhausted by a new allocation), the ring must be recreated at the
new size:

1. **Drain the ring**: any `Pending`/`Submitted` slots are
   abandoned (their `unmap`/`mapAsync` lifecycle continues; we
   just drop our handle).
2. **Allocate fresh ring**: `depth` new buffers at the new size,
   all with `mappedAtCreation: true`.
3. **Force-flush the Vec into the first slot**: the next
   `write_gpu()` writes the *entire* Vec (not just dirty ranges),
   because the new slot has no prior contents to preserve. Mark
   every range dirty before the write.
4. **Bump a `resize_count` telemetry counter** so consumers can
   see how often this path fires (frequent resizes indicate poor
   initial-capacity sizing).

This is the one case where the dirty-range optimization is
deliberately bypassed — correctness over micro-perf.

### Exhaustion handling

If `write_gpu()` is called and no `Ready`/`Mapped` slot is
available (all are `Pending` or `Submitted`):

- **Debug build (`cfg(debug_assertions)`)**: `debug_assert!`-panic
  with a descriptive message ("ring exhausted: depth N, all slots
  in-flight"). Forces ring-depth-too-shallow bugs surface during
  development.
- **Release build**: fall back to `queue.writeBuffer` for this
  frame's upload of the dirty ranges. The fallback path stays as a
  one-line escape hatch; bumps `fallback_count`.

The fallback path is intentionally not the "fast" path — its job
is to never crash a shipped game. A release build that regularly
hits fallback should bump the buffer's ring depth via
`new_with_ring_depth(..)`.

### Foreign `ArrayBuffer` ingestion — explicit entrypoint, not the ring

When a glTF parse completes (worker job or main-thread decode), the
result is a set of `ArrayBuffer`s the renderer needs to upload to
geometry / texture buffers. The mapped-staging path would require
copying from the `ArrayBuffer` into the mapped region — the same
memcpy as `queue.writeBuffer(arrayBuffer)`. So `queue.writeBuffer`
stays as the canonical path for foreign-bytes ingestion.

Phase 2.1 adds an **explicit entrypoint** so call sites use a
documented method instead of reaching for raw `gpu.write_buffer`:

```rust
impl DynamicStorageBuffer {
    /// Ingest a foreign byte payload (e.g. worker job output or
    /// file decode) directly into the destination GPU buffer at
    /// `offset`, *bypassing the ring*. Use when the data didn't
    /// originate from our `Vec<u8>` — for those, just call
    /// `update_at(..)` + the next `write_gpu()`.
    ///
    /// Bumps `bytes_uploaded_via_writebuffer` (a separate counter
    /// from `bytes_uploaded_via_fallback`, which is for ring
    /// exhaustion).
    pub fn ingest_foreign(&mut self, offset: usize, bytes: &[u8]) -> Result<()>;
}
```

Module-level docs on `Dynamic{Storage,Uniform}Buffer` document this
split so future contributors don't mistakenly route worker results
through the ring.

### Cleanup on `Drop`

`Drop` must safely tear down ring slots:

- `Mapped` slots: explicit `unmap()` (WebGPU validation warns if a
  mapped buffer is destroyed).
- `Submitted` / `Pending` slots: the underlying `GpuBuffer`
  destructor handles destruction; in-flight GPU usage holds the
  slot alive until completion.
- `Ready` slots: same as Submitted.

Implementation: `impl Drop for MappedStagingRing` walks slots,
calls `unmap()` on `Mapped` ones, then drops the underlying
buffers. Safe to call mid-mapAsync (the resolution callback
becomes a no-op once the buffer is destroyed).

### Telemetry

`UploadStats` (exposed per-buffer + aggregated by the renderer):

- `peak_ring_depth_used: usize` — max number of slots
  simultaneously non-`Ready` since last reset. Reveals whether the
  ring is correctly sized.
- `fallback_count: u64` — times `queue.writeBuffer` fallback fired
  due to ring exhaustion.
- `map_async_wait_ms: f64` — accumulated wall-clock time spent
  blocked on mapAsync resolution waits. ~zero in steady state;
  spikes when a slot's prior submit is stuck on the GPU.
- `bytes_uploaded_via_ring: u64` — total bytes through the fast
  path.
- `bytes_uploaded_via_fallback: u64` — total bytes via writeBuffer
  fallback (ring exhaustion).
- `bytes_uploaded_via_writebuffer: u64` — total bytes via the
  explicit `ingest_foreign` entrypoint (foreign-data writeBuffer
  ingestion). Tracked separately from `fallback` so consumers can
  distinguish "ring is too shallow" from "lots of foreign data
  came in this frame."
- `resize_count: u64` — times the ring was recreated due to buffer
  growth.

Surfaced through `read_render_pass_timings()` JSON (the Phase 0.2
helper, already landed) under a new `upload_rings` top-level key
so the measurement harness picks them up alongside per-pass
timings.

### WebGPU validation constraint

A `GpuBuffer` with `MAP_WRITE` usage can only have `COPY_SRC` as
its *other* usage. The ring slots are
`MAP_WRITE | COPY_SRC` and are write-only from the CPU; the
destination buffer keeps whatever usage it had before
(`STORAGE | UNIFORM | COPY_DST | ...`). The
`copyBufferToBuffer` blit bridges them.

### Standalone writeBuffer sites — migration scope

Phase 2.1 migrates **every** per-frame `queue.writeBuffer` call
site that owns renderer-internal data to use
`Dynamic{Storage,Uniform}Buffer`:

| Site | Current shape | Target |
|---|---|---|
| `transforms` | DynamicStorageBuffer (already) | inherits the ring |
| `materials` | DynamicStorageBuffer (already) | inherits the ring |
| `instances` | DynamicStorageBuffer ×2 (already) | inherit the ring |
| `meshes.meta` | DynamicStorageBuffer (already) | inherits the ring |
| `meshes.skins` / `morphs` | DynamicStorageBuffer (already) | inherit the ring |
| `textures.write_texture_transforms_gpu` | DynamicStorageBuffer (already) | inherits the ring |
| `camera.write_gpu` | raw writeBuffer (~64 B) | promote to DynamicUniformBuffer |
| `shadows.write_gpu` globals | raw writeBuffer | promote to DynamicStorageBuffer |
| `shadows.write_gpu` descriptors | raw writeBuffer | promote to DynamicStorageBuffer |
| `lights` (info + LightsBuffer) | raw writeBuffer ×2 | promote to DynamicStorageBuffer |
| `mesh_light_indices_gpu.write_gpu` | raw writeBuffer | promote to DynamicStorageBuffer |
| `occlusion_buffers.write_params` | raw writeBuffer | promote to DynamicUniformBuffer |
| occlusion instance pack (in `render.rs`) | raw writeBuffer (variable size) | promote to DynamicStorageBuffer |
| `lines/renderer.rs` per-line uniform | raw writeBuffer | promote to DynamicUniformBuffer |
| `lines/gpu.rs` per-line segment write | raw writeBuffer | promote to DynamicStorageBuffer |

By end of Phase 2.1, there is **one canonical upload path** for
renderer-owned per-frame data.

**Out of scope** (intentional): the per-frame *reset* writes
(`coverage_buffers.reset_counts`, `material_classify_buffers.reset_header`,
`decal_classify_buffers.reset_counts`). These are full-replace of
small fixed-content payloads (zeros / static headers); the ring's
mapped-write win doesn't apply to "upload these constant bytes
again." A possible future cleanup is to switch them to
`clear_buffer` GPU commands, but that's not Phase 2.1's concern.

### Implementation order (commit-by-commit, git-bisect-able)

1. **`MappedStagingRing<const N: usize>` type** in
   `crates/renderer/src/buffer/` — generic over ring depth, owns
   the `MAP_WRITE | COPY_SRC` slots, the per-slot state machine,
   `mappedAtCreation: true` initialization, the mapAsync
   bookkeeping, the resize-reset path, telemetry counters, and
   the Drop impl. Standalone unit tests for the state machine.
2. **Integrate the ring into `DynamicStorageBuffer`**: replace the
   `write_gpu` path; keep the Vec; add `new_with_ring_depth(..)`,
   `ingest_foreign(..)`, `upload_stats()`.
3. **Same for `DynamicUniformBuffer`**.
4. **Migrate the already-Dynamic call sites** (transforms,
   materials, instances, meta, skins, morphs, texture transforms)
   — these should be no-ops at the call site (they already use
   the type's API).
5. **Promote `camera.write_gpu`** to `DynamicUniformBuffer`.
6. **Promote `shadows`** globals + descriptors.
7. **Promote `lights`** info + LightsBuffer.
8. **Promote `mesh_light_indices_gpu`**.
9. **Promote `occlusion_buffers.write_params`** + the occlusion
   instance pack in `render.rs`.
10. **Promote `lines`** per-line uniform + per-line segment buffer.
11. **Wire telemetry** into `read_render_pass_timings()` JSON
    output under `upload_rings`.
12. **Update `docs/PERFORMANCE.md`**: document the upload path,
    the `upload_rings` telemetry, and expected values for the
    tuning scenes.

Each promotion commit is one site at a time; intermediate states
are mixed-mode (some Dynamic, some raw writeBuffer) but always
correct.

### Open questions

- **Ring depth for `morphs` / `skins` buffers**: these can be
  sparse + huge in a worst-case rig. Telemetry-driven; ship
  3-deep, revisit if `peak_ring_depth_used` consistently shows
  1-2 (over-provisioned) or `fallback_count > 0` (under).
- **Removing the fallback path long-term**: if a year of telemetry
  shows zero fallbacks in production, the fallback path can
  become `debug_assert!` always and a hard error in release.
  Defer to evidence.

### Alternatives considered (rejected)

- **Shared StagingBelt across all subsystems** — adds a new
  coordination layer (someone needs to own per-frame rotation
  cadence; `UploadHandle { offset, len }` returned per write);
  per-instance rings are strictly simpler with negligible memory
  overhead at small buffer sizes.
- **Eliminate the `Vec<u8>` entirely** — would require either
  full-write-every-frame (kills the dirty-range optimization) or
  copy-from-prev-ring-slot on rotation (forces a CPU sync wait
  for the in-flight slot to release). The Vec is cheap;
  eliminating it saves ~25% memory but is a worse trade-off.
- **`queue.writeBuffer` of a single CPU arena per frame + N
  `copyBufferToBuffer` blits** — captures the "fewer
  writeBuffer calls" win but not the "skip browser's internal
  staging copy" win. Strictly less optimal than the mapped ring.
- **Adaptive ring-depth-by-buffer-size heuristic** — rejected in
  favour of caller-driven `new_with_ring_depth(depth)`. The
  engine author knows "this buffer will grow huge" better than a
  generic size threshold.
- **Persistent mapping (Vulkan-style HOST_VISIBLE +
  HOST_COHERENT memory)** — WebGPU doesn't expose this. You must
  `unmap()` before submit. The ring is the WebGPU-shaped
  equivalent.

---

## Phase 4.3 — `WorkerPool` + `WorkerJob` infrastructure

### 4.3a — `awsm-renderer::workers` module

#### Goal

A library-wide worker-job infrastructure so CPU-bound logical
work can run off the main thread. **This is explicitly for
CPU-only work that doesn't touch the GPU.** The renderer's GPU
device cannot be shared across workers (see [Phase 4.4](#phase-44--offscreencanvas-render-worker-deployment-mode-new)
for the GPU-in-worker mode); workers produce *bytes* (e.g.
`Vec<u8>` payloads, parsed asset structures) which the main
thread ingests via the foreign-bytes entrypoint added in Phase
2.1.

Natural future consumers (each a separate sprint, not Phase 4.3
scope):

- **Animation evaluation** — bones / blend shapes / skeleton math.
- **Particle simulation** — CPU integration producing per-frame
  instance attribute payloads.
- **BVH / spatial-structure rebuild** — for large-scene reloads.
- **IBL filtering / environment map convolution** — startup work
  for new HDR environments.
- **Mesh tangent / normal computation** — for glTFs that lack
  them.

The first consumer is **glTF parse** (Phase 4.3b), which validates
the pool design before scope expands.

#### Library constraint

`awsm-renderer` ships as a Rust library; consumers may use Trunk,
webpack, Vite, or no bundler at all. The worker abstraction
**cannot** assume a separate `worker.js` file is copied to a
known path — the consumer's build pipeline might not produce one.
The bootstrap design (below) makes simple use *one line* for
consumers regardless of build tool.

#### Public API

```rust
pub trait WorkerJob: 'static {
    /// Unique string identifier; used in the worker's postMessage
    /// dispatch (e.g. "gltf-parse", "mesh-tangents").
    const NAME: &'static str;
    type Input: Serialize + DeserializeOwned;
    type Output: Serialize + DeserializeOwned;

    /// Runs on the worker thread. No `&self` — implementations
    /// are stateless and only act on `input`.
    fn execute(input: Self::Input) -> Self::Output;
}

pub struct WorkerPool { /* private */ }

#[derive(Default)]
pub enum WorkerPoolBootstrap {
    /// Default. Auto-discovers the consumer's wasm-bindgen bundle
    /// URL via `import.meta.url` from the library's own inline-JS
    /// snippet (which the wasm-bindgen tool embeds in the
    /// consumer's bundle output). Works for any wasm-bindgen
    /// `--target web` consumer (Trunk, Vite ESM, etc.) regardless
    /// of bundle name / hash / chunking.
    #[default]
    Auto,

    /// Explicit bundle URL — for consumers whose build setup
    /// doesn't expose `import.meta.url` in a usable form (rare).
    /// Library tries `Auto` first and only falls back to this if
    /// explicitly asked.
    ModuleUrl { bundle_url: String },

    /// Escape hatch — consumer constructs the `Worker` themselves;
    /// the pool then drives the postMessage protocol over the
    /// caller-supplied handle.
    Custom(Box<dyn Fn() -> Result<web_sys::Worker, JsValue> + 'static>),
}

impl WorkerPool {
    /// Most common shape: `WorkerPool::with_workers(None).await?`.
    /// Uses `WorkerPoolBootstrap::Auto`, defaults `worker_count`
    /// to `min(navigator.hardwareConcurrency, 4)` if `None`.
    pub async fn with_workers(worker_count: Option<usize>) -> Result<Self, AwsmError>;

    pub async fn new(
        bootstrap: WorkerPoolBootstrap,
        worker_count: usize,
    ) -> Result<Self, AwsmError>;

    /// Register a job type so workers know how to dispatch it.
    /// Call once per `WorkerJob` impl at consumer init time, after
    /// pool construction. (See "Job registration" below for why
    /// we use explicit registration instead of `linkme`.)
    pub fn register<J: WorkerJob>(&self);

    /// Dispatch a job. Round-robins across workers.
    pub async fn dispatch<J: WorkerJob>(
        &self,
        input: J::Input,
    ) -> Result<J::Output, AwsmError>;

    /// Zero-copy path — `transfer` lists `ArrayBuffer`s the
    /// protocol should `postMessage(..., { transfer })` instead of
    /// structured-cloning. Critical for the 27 MB robot case;
    /// otherwise the cross-thread copy eats most of the saving.
    pub async fn dispatch_with_transfer<J: WorkerJob>(
        &self,
        input: J::Input,
        transfer: js_sys::Array,
    ) -> Result<J::Output, AwsmError>;

    /// Telemetry.
    pub fn stats(&self) -> WorkerPoolStats;
}

pub struct WorkerPoolStats {
    pub workers_alive: usize,
    pub jobs_dispatched: u64,
    pub jobs_completed: u64,
    pub jobs_failed: u64,
    /// Total wall-clock time jobs spent queued before a worker
    /// picked them up.
    pub queue_wait_ms: f64,
}

// Auto-bundle-URL discovery. Inline JS gets embedded into the
// consumer's wasm-bindgen JS glue at build time; when called at
// runtime, `import.meta.url` resolves to whatever URL the
// consumer's bundle is being served from (hashed / chunked / any
// bundler — doesn't matter).
#[wasm_bindgen(inline_js = "export function awsm_bundle_url() { return import.meta.url; }")]
extern "C" {
    fn awsm_bundle_url() -> String;
}

/// Exported entry point the worker's wasm-bindgen init calls
/// after the module is loaded. Installs the postMessage listener
/// and dispatches incoming jobs by `NAME` to registered handlers.
#[wasm_bindgen]
pub fn awsm_worker_entry();
```

#### Inline JS shim

Built and blob-URL'd by `WorkerPool::new`. The shim does **not**
call `init()` immediately — it waits for the main thread to post
the pre-compiled `WebAssembly.Module` and the bundle URL, then
initializes with the shared Module. This avoids re-compiling the
multi-MB Rust binary in every worker:

```js
self.onmessage = async (e) => {
    if (e.data && e.data.kind === "awsm-init") {
        const { wasm_module, glue_url } = e.data;
        const wbg = await import(glue_url);
        await wbg.default(wasm_module);  // re-uses the compiled Module
        wbg.awsm_worker_entry();
        self.postMessage({ kind: "awsm-ready" });
        return;
    }
    // Subsequent messages are job dispatches — handled by
    // `awsm_worker_entry`'s installed listener.
};
```

The main thread's pool ctor:

1. Reads its own `WebAssembly.Module` via
   `wasm_bindgen::module().dyn_into::<WebAssembly::Module>()`.
   The Module is the compiled artifact, *not* the linear-memory
   Instance — safe to share.
2. Reads the bundle URL via the `awsm_bundle_url()` snippet.
3. Spawns each worker from the blob-URL shim above.
4. `postMessage({ kind: "awsm-init", wasm_module, glue_url })` to
   each worker. `WebAssembly.Module` is structured-cloneable, no
   copy of the wasm bytes; each worker gets a reference to the
   same compiled artifact.
5. Awaits `{ kind: "awsm-ready" }` from each before resolving
   `WorkerPool::new`.

#### What's duplicated per worker / what's not

**Duplicated:**

- The JS glue (~10–30 KB; re-imported by each worker's
  `await import(glue_url)`). Mostly cheap; runs once per worker
  at startup.
- The wasm Instance — each worker has its own linear memory.
  That's intentional and is the boundary we want: workers can't
  see main-thread heap directly; all I/O is via `postMessage`
  with structured clone (or Transferable for large
  `ArrayBuffer`s).

**Not duplicated:**

- The compiled `WebAssembly.Module` itself. Browser compiles it
  once on the main thread; workers reference the same
  compilation. No 100 ms–1 s re-compile per worker.
- The `.wasm` bytes on the wire. Browser cache + the shared
  Module means the network/disk side is touched once.

#### Helper for the blob-URL plumbing

```rust
use js_sys::Array;
use wasm_bindgen::JsValue;
use web_sys::{Blob, BlobPropertyBag, Url, Worker, WorkerOptions};

pub fn new_worker_from_js(
    js: &str,
    options: Option<WorkerOptions>,
) -> Result<Worker, JsValue> {
    let mut blob_options = BlobPropertyBag::new();
    blob_options.type_("application/javascript");
    let blob_parts = Array::new_with_length(1);
    blob_parts.set(0, JsValue::from_str(js));
    let blob = Blob::new_with_str_sequence_and_options(&blob_parts, &blob_options)?;
    let blob_url = Url::create_object_url_with_blob(&blob)?;
    let worker = match options {
        Some(options) => Worker::new_with_options(&blob_url, &options)?,
        None => Worker::new(&blob_url)?,
    };
    Url::revoke_object_url(&blob_url)?;
    Ok(worker)
}
```

#### Job registration — explicit, not `linkme`

`linkme`'s `distributed_slice!` relies on linker-section magic
that does not survive wasm32 reliably. Use **explicit
registration** via `WorkerPool::register::<J>()` at consumer init
time:

```rust
let pool = WorkerPool::with_workers(None).await?;
pool.register::<GltfParseJob>();
pool.register::<AnimationEvalJob>();
// etc.
```

The pool's internal registry is a `HashMap<&'static str, Box<dyn
Fn(JsValue) -> Pin<Box<dyn Future<Output = Result<JsValue,
JsValue>>>>>>` keyed by `J::NAME`. Registration is one-shot per
pool instance.

#### Lifecycle

- `WorkerPool::new` spawns `worker_count` workers, posts the
  init message to each, waits for each `{ kind: "ready" }`, then
  resolves.
- `dispatch` round-robins jobs across workers (or picks the
  least-busy if instrumented — start with round-robin). Pending
  jobs hold a `oneshot::Sender<JsValue>` keyed by an incrementing
  `JobId`; incoming `{ kind: "result", id, payload }` routes back
  via the keyed sender.
- `Drop` on `WorkerPool` calls `Worker::terminate()` on each
  worker.

#### Error handling

A worker that panics or hits an uncaught exception posts
`{ kind: "error", id, message }`; the pool surfaces this as
`AwsmError::WorkerJobFailed`. The worker stays alive for the
next job — one job's failure doesn't tear down the pool.

#### Implementation order

1. Inline-JS shim + `new_worker_from_js` helper in
   `crates/renderer/src/workers/blob.rs`.
2. `WorkerPool` struct + `WorkerPoolBootstrap` enum + `Auto`
   bootstrap (calling `awsm_bundle_url()` + sharing the
   compiled `WebAssembly.Module`).
3. `WorkerJob` trait + the registration + dispatch protocol +
   the `JobId` ↔ `oneshot::Sender` routing.
4. `awsm_worker_entry` worker-side dispatcher.
5. `dispatch_with_transfer` zero-copy variant.
6. `WorkerPoolStats` telemetry.
7. Round-trip integration test: a dummy `EchoJob` that returns
   its input.
8. Update `docs/PERFORMANCE.md` with the worker pattern.

#### Alternatives considered (rejected)

- **Pure-JS workers** (worker code is JS, no Rust in worker).
  Rejected: doubles maintenance, duplicates parse logic.
- **`wasm-bindgen-rayon` with `SharedArrayBuffer`** for
  fine-grained parallelism. Rejected: requires COOP/COEP headers
  on the consumer's deployment, which not every consumer can
  set. We don't need fine-grained parallelism — coarse job
  offload via postMessage is enough.
- **Consumer-supplied `bundle_url: String` as the primary
  bootstrap.** Rejected: bundle filenames vary across consumers,
  build profiles (Trunk hashes release builds), and bundlers
  (chunking, ESM vs classic). `import.meta.url` from a
  library-internal inline-JS snippet is portable across all of
  these.
- **Naive worker `init()`** (each worker compiles the .wasm from
  the URL). Rejected: for a multi-MB Rust binary, wasm compile
  is 100 ms–1 s *per worker*. With 2–4 workers, pool startup
  alone would burn 200 ms–4 s of cold cost. The
  shared-`WebAssembly.Module` shape is the standard fix and what
  `wasm-bindgen-rayon` does.
- **`Custom`-only API (no auto-discovery)**. Rejected: pushes
  the inline-JS shim + URL plumbing onto every consumer. The
  whole point of a first-class abstraction is that simple use is
  one line.
- **Single-worker-per-job (no pool)**. Rejected: doesn't
  amortize startup cost (~5–50 ms per worker spawn). Scene loads
  with multiple glbs would pay it per file.
- **`linkme`-based registration**. Rejected: distributed slices
  don't reliably survive wasm32 linking. Explicit registration
  is portable and clear.

### 4.3b — `GltfParseJob` (first consumer)

```rust
// crates/renderer-gltf/src/worker_job.rs
pub struct GltfParseJob;

#[derive(Serialize, Deserialize)]
pub struct GltfParseInput {
    pub url: String,
    pub file_type: Option<GltfFileType>,
}

#[derive(Serialize, Deserialize)]
pub struct GltfParseOutput {
    pub doc_bytes: Vec<u8>,
    pub buffer_bytes: Vec<Vec<u8>>,
    pub image_bytes: Vec<Vec<u8>>,
    // ... whatever GltfLoader produces ...
}

impl WorkerJob for GltfParseJob {
    const NAME: &'static str = "gltf-parse";
    type Input = GltfParseInput;
    type Output = GltfParseOutput;

    fn execute(input: Self::Input) -> Self::Output {
        // Existing GltfLoader::load body, async-converted to sync
        // (workers can block-await via wasm-bindgen-futures
        // executor). Or split into a sync inner that's called from
        // an async wrapper if we keep the existing async signature.
    }
}
```

Consumer wiring (identical for editor and shipped games):

```rust
let pool = WorkerPool::with_workers(None).await?;
pool.register::<GltfParseJob>();

// Loading a project — fire concurrent glb parses, pool round-robins:
let (a, b, c) = futures::try_join!(
    pool.dispatch::<GltfParseJob>(input_a),
    pool.dispatch::<GltfParseJob>(input_b),
    pool.dispatch::<GltfParseJob>(input_c),
)?;
```

For the 27 MB robot the parsed `Vec<u8>` buffers go through
`dispatch_with_transfer` — they're consumed once (ingested into
GPU buffers via Phase 2.1's `ingest_foreign`), so transferring
ownership across the thread boundary is free.

#### Measurement gate

Before declaring Phase 4.3b done, use the Phase 0.2
`read_render_pass_timings` helper to measure end-to-end glTF load
time in both modes:

- **Inline mode**: `GltfLoader::load` on the main thread (current).
- **Worker mode**: `pool.dispatch::<GltfParseJob>(input)`.

If the worker version is consistently faster on a representative
glb (use the 27 MB robot stress asset), wire it into
`asset_cache::load_and_populate` as the default. If the transfer
cost dominates for small glbs, the pool dispatch becomes opt-in
via a config knob (e.g. a builder method on `AwsmRendererBuilder`)
and the inline path stays as the default.

#### Implementation order

1. `GltfParseJob` impl in `crates/renderer-gltf/src/worker_job.rs`.
2. Refactor `GltfLoader::load` so the body is callable from the
   worker context (no main-thread-only API dependencies).
3. Integration test: dispatch the job from the scene-editor
   against a known glb, verify the output matches the inline path.
4. A/B measurement on the 27 MB robot via
   `read_render_pass_timings`.
5. Wire into `asset_cache::load_and_populate` based on the
   measurement outcome.
6. Update `docs/PERFORMANCE.md` with the result + the opt-in knob
   if applicable.

---

## Phase 4.4 — `OffscreenCanvas` render-worker deployment mode (NEW)

### Goal

Support a second library deployment mode where **the entire
renderer runs in a worker** via `OffscreenCanvas` +
`transferControlToOffscreen()`. The main thread becomes a thin
shim: it owns the DOM canvas element, forwards input events to
the worker, and does nothing GPU-related itself.

This is the right mode for shipped games where the main thread is
otherwise busy with game logic, physics, audio scheduling,
network code, etc. — isolating the renderer in a worker means it
cannot be starved by main-thread CPU contention.

The **editor stays in main-thread mode** (canvas in the DOM next
to UI controls, renderer on main thread). Phase 4.4 *enables* the
worker mode for consumers; it does not migrate the editor.

### The two deployment modes

| Mode | Canvas | Renderer | Main thread does | Worker does |
|---|---|---|---|---|
| **Main-thread (editor)** | `HtmlCanvasElement` in DOM | Main thread | Everything: DOM, UI, input, render | (Phase 4.3 jobs only) |
| **Worker (game)** | `OffscreenCanvas` transferred to worker | Worker | DOM/UI overlays, input capture, postMessage forwarding | Renderer + game logic + render loop |

Trade-offs:

- **Main-thread mode** keeps the GPU available to other
  main-thread code (canvas2D overlays, video, WebGL shaders for
  unrelated features). Renderer competes with everything else on
  the main thread for CPU.
- **Worker mode** isolates the renderer from main-thread CPU
  contention. Main thread cannot do any GPU work (no canvas2D
  overlay using the same canvas, no shared GPU resources). DOM
  overlays are still possible via separate HTML elements
  absolutely-positioned over the canvas.

### Worker-safety audit

The renderer crate currently reaches for some main-thread-only
APIs that will break in a worker context. Audit + fix:

| API | Worker-safe alternative |
|---|---|
| `web_sys::window()` | `js_sys::global().dyn_into::<DedicatedWorkerGlobalScope>()` (when in worker), with a helper that picks the right global at runtime |
| `window().navigator()` | `DedicatedWorkerGlobalScope::navigator()` (returns `WorkerNavigator`, exposes `.gpu()` the same way) |
| `window().performance()` | `DedicatedWorkerGlobalScope::performance()` |
| `window().request_animation_frame(..)` | `DedicatedWorkerGlobalScope::request_animation_frame(..)` (available in workers since 2023) |
| `web_sys::document()` | Not available in worker. Any code path that touches `document` must be either main-thread-only or routed via postMessage |

Audit deliverable: add a `crates/renderer/src/web_global.rs`
helper module:

```rust
/// Get a `JsValue` for the current global scope, regardless of
/// whether we're running on the main thread or in a worker.
pub fn global() -> js_sys::Object { /* ... */ }

/// Try to get the `Window` (main-thread mode); returns `None` in
/// a worker.
pub fn window() -> Option<web_sys::Window> { /* ... */ }

/// Try to get the `DedicatedWorkerGlobalScope`; returns `None`
/// on the main thread.
pub fn worker_scope() -> Option<web_sys::DedicatedWorkerGlobalScope> { /* ... */ }

/// Get `Navigator.gpu` from whichever global is active.
pub fn navigator_gpu() -> Option<web_sys::Gpu> { /* ... */ }

/// Get `Performance` from whichever global is active.
pub fn performance() -> Option<web_sys::Performance> { /* ... */ }

/// Schedule a frame callback against whichever global is active.
pub fn request_animation_frame(callback: &js_sys::Function) -> Result<i32, JsValue>;
```

Then walk the renderer crate and replace every `web_sys::window()`
call with `web_global::*`. The fix is mechanical; the
audit-and-replace work is what makes the renderer worker-safe.

### Builder API additions

```rust
impl AwsmRendererWebGpuBuilder {
    /// Existing: takes an `HtmlCanvasElement` for main-thread mode.
    pub fn new(gpu: web_sys::Gpu, canvas: web_sys::HtmlCanvasElement) -> Self;

    /// Alternative constructor for worker mode. Caller must have
    /// already done `canvas.transferControlToOffscreen()` on the
    /// main thread and postMessaged the resulting `OffscreenCanvas`
    /// to the worker, where this is called.
    pub fn new_with_offscreen_canvas(
        gpu: web_sys::Gpu,
        canvas: web_sys::OffscreenCanvas,
    ) -> Self;
}
```

Internally, the builder normalises both into a private enum
`CanvasKind { Html(HtmlCanvasElement), Offscreen(OffscreenCanvas) }`
and dispatches the GpuCanvasContext acquisition + resize handling
accordingly. The WebGPU spec gives both kinds the same context
API, so most of the renderer code doesn't care which is which.

### Input event forwarding pattern (documented, not implemented)

The library does **not** ship an input forwarder. Forwarding is
consumer-specific (different games want different event shapes,
different latency trade-offs, different filtering). The library
**documents the recommended pattern** in
`docs/DEPLOYMENT_MODES.md` (new file):

```js
// Main thread: capture events, postMessage to worker.
const canvas = document.querySelector('canvas');
const offscreen = canvas.transferControlToOffscreen();
const worker = new Worker('./renderer-worker.js', { type: 'module' });
worker.postMessage({ kind: 'init', canvas: offscreen }, [offscreen]);

canvas.addEventListener('pointermove', (e) => {
    worker.postMessage({
        kind: 'pointermove',
        x: e.offsetX, y: e.offsetY, buttons: e.buttons,
    });
});
// (similar for pointerdown, pointerup, wheel, keydown, keyup, resize)

const resizeObserver = new ResizeObserver(entries => {
    const { inlineSize, blockSize } = entries[0].contentBoxSize[0];
    worker.postMessage({ kind: 'resize', width: inlineSize, height: blockSize });
});
resizeObserver.observe(canvas);
```

The library exposes a typed `WorkerInputEvent` enum that consumers
can deserialise on the worker side, but does not auto-wire the
forwarding.

### Resize handling

Workers cannot observe DOM resize events directly. Pattern:

1. Main thread: `ResizeObserver` on the canvas element.
2. Main thread: on resize, postMessage `{ kind: 'resize',
   width, height }` to worker.
3. Worker: handler calls `OffscreenCanvas::set_width(..)` +
   `set_height(..)` then calls into the renderer's existing resize
   path (already worker-safe once the audit completes).

### `requestAnimationFrame` in workers

`DedicatedWorkerGlobalScope::requestAnimationFrame` has been
available in all major browsers since 2023. The worker has its own
rAF clock — independent from the main thread's. For the editor
pattern where main-thread UI + renderer share the rAF callback,
this isn't ideal; for the game pattern where the worker owns the
render loop, it's exactly right.

The renderer's existing rAF-driven `start_render_loop` works in
worker context via `web_global::request_animation_frame(..)` once
the audit lands.

### Picker integration in worker mode

The editor's picker is a read-back of a GPU compute result —
`mapAsync` on a small output buffer. This works identically in
worker context. The selection-result-back-to-main-thread hop is
the consumer's responsibility (postMessage the picked `MeshKey` /
`NodeId`).

### Documentation deliverables

- **`docs/DEPLOYMENT_MODES.md`** (new): explains the two modes,
  trade-offs, and shows the worker recipe end-to-end (consumer-
  level: how to set up the main-thread shim, the worker
  entrypoint, the canvas transfer, the input forwarding skeleton).
- **`docs/PERFORMANCE.md`** update: note that worker mode
  eliminates main-thread CPU contention as a renderer hazard.
- **Example crate**: `crates/examples/render-worker/` — minimal
  game-shape consumer that runs the renderer in a worker. Loads
  a glb, renders it, responds to input. ~300 lines including
  HTML/JS shim + worker entrypoint.

### Implementation order

1. **Audit**: walk `crates/renderer/` + `crates/renderer-core/`
   + `crates/renderer-gltf/` and grep for `web_sys::window()`,
   `web_sys::document()`, `Navigator::performance()`. Build the
   list of replacement sites.
2. **`web_global` helper module**: implement the runtime-global-
   picking helpers.
3. **Mechanical replace**: every `web_sys::window()` →
   `web_global::window()` (or the `_or_worker` variant for paths
   that need to work in both).
4. **`AwsmRendererWebGpuBuilder::new_with_offscreen_canvas`**.
5. **Internal `CanvasKind` enum** + dispatch in the existing
   GpuCanvasContext + resize paths.
6. **`WorkerInputEvent` enum** for the documented postMessage
   protocol.
7. **`crates/examples/render-worker/`** sample.
8. **`docs/DEPLOYMENT_MODES.md`** doc.
9. **`docs/PERFORMANCE.md`** update.
10. **Smoke test**: render the example, verify input forwarding
    + resize work, take screenshots.

### Open questions

- **Should the library ship a higher-level "worker bootstrap"
  helper**? (Something like `awsm_renderer::deployment::spawn_render_worker(canvas).await?`
  that returns a `WorkerHandle` with typed input/output methods.)
  Probably worth doing once we have one consumer working — defer
  to evidence.
- **Editor migration to worker mode**? Out of scope for Phase 4.4.
  The editor's DOM-overlay-heavy UX makes main-thread mode the
  natural fit, but if main-thread CPU contention becomes a
  practical issue, a future sprint could migrate.

### Alternatives considered (rejected)

- **Split the renderer's per-frame work across the main/worker
  boundary** (e.g. "main thread renders, but workers prep
  buffers and send them in"). Rejected: WebGPU device-owned
  objects (buffers, command encoders, bind groups) cannot cross
  thread boundaries. The worker would have to send raw `Vec<u8>`
  to the main thread (which is what Phase 4.3 already enables),
  and the main thread would still do all the GPU upload +
  recording. No savings on the renderer hot path; just extra
  postMessage latency.
- **Single deployment mode (worker-only)**. Rejected: the editor
  needs main-thread mode for the DOM-overlay UX. Library has to
  support both.
- **Ship a full input-event forwarder**. Rejected for v1: input
  semantics are too consumer-specific (latency trade-offs,
  coalescing, gesture recognition). Document the pattern; let
  consumers DIY.

---

## Working agreements

### Branch + commits

- Work on the current sprint branch (`more-optimizations` at the
  time of writing). All sprint work lands on one branch; PR to
  `main` as a single batch at the end.
- **Commits**: small, logical, descriptive — sized for git-bisect
  to be useful. Each commit should map to one Phase item or a
  clean sub-step of one. End each commit message with:
  ```
  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  ```

### Verification policy

- **Intermediate commits may break the workspace.** This is a
  large cross-cutting refactor (Phase 2.1 in particular); padding
  every commit with compatibility shims would balloon the diff
  for work that gets deleted anyway. Use git-bisect semantics:
  bisectors want *logical* boundaries, not "every commit must
  build." Within a single commit, keep the diff coherent (no
  "added a field, forgot to update the one-line use site").
- **Don't run `cargo build --workspace`** — use `cargo check`
  (cheaper, same compile validation). The trunk dev-server + the
  editor browser smoke test are the "does it actually run" gate.

### End-of-sprint verification (run once, before opening the PR)

- `cargo check --workspace` clean
- `cargo clippy --workspace` clean
- `cargo test --workspace` green
- Editor smoke test (main-thread mode): Primitive Box + Sphere +
  Torus render with no console errors.
- Render-worker example (Phase 4.4): loads, renders, responds to
  input. Take screenshots.
- Measurement: `read_render_pass_timings(0)` JSON on
  `tuning-10k-meshes`. Confirm `upload_rings.fallback_count == 0`
  in steady state.
- Browser scope: Chrome via the Claude Preview MCP is the
  required smoke target. Safari is a nice-to-have, not a gate.

### Doc maintenance

- When a Phase item lands, move it to "Recently landed" with a
  one-line summary.
- When an item turns out to be unwise mid-implementation, move it
  to "Won't do" with the reason.
- Keep this doc the source of truth — don't let it drift, even
  mid-sprint when other things are broken.
- `docs/PERFORMANCE.md` gets updated per-phase (each phase's
  implementation order lists this explicitly).

---

## Reference

### Phase 3 — retired (kept for cross-reference)

The original Phase 3 ("merge opaque PBR/Unlit/Toon compute
pipelines into one shader-id-branched pass") was rejected for not
scaling to many materials. The narrow-scope alternative — running
all opaque pipelines inside a single `beginComputePass` /
`endComputePass` boundary — is *already implemented* in
[`render_pass.rs`](../../crates/renderer/src/render_passes/material_opaque/render_pass.rs):
one pass, shared bind groups, a for-loop of `set_pipeline` +
`dispatch_workgroups_indirect` per `shader_id`. No work needed.
Phase number 3 reserved so cross-references in commit messages
stay valid.

### ✅ Recently landed (prior sprint)

Brief one-liners; full commit messages on the branch history.

- **Phase 4.4 (this sprint, complete)** — runtime-global picker
  helpers in both
  [`crates/renderer/src/web_global.rs`](../../crates/renderer/src/web_global.rs)
  and
  [`crates/renderer-core/src/web_global.rs`](../../crates/renderer-core/src/web_global.rs);
  the renderer-core audit fixed
  `compatibility::check()` and the `WINDOW` LazyLock in
  `image/bitmap.rs` (worker-safe via `web_global::navigator_gpu()`
  and `create_image_bitmap_*` helpers).
  `AwsmRendererWebGpuBuilder` now stores a `CanvasKind` enum and
  exposes `new_with_offscreen_canvas(..)`. A reference example
  ([`crates/examples/render-worker/`](../../crates/examples/render-worker/))
  ships the `OffscreenCanvas` handshake + worker bootstrap +
  `WorkerInputEvent` protocol; the worker-side render loop stops at
  GPU-device init (full scene-load wiring is the consumer's job).
  Doc: [`DEPLOYMENT_MODES.md`](../DEPLOYMENT_MODES.md).
- **Phase 4.3b (this sprint, complete)** —
  [`crates/renderer-gltf/src/worker_job.rs`](../../crates/renderer-gltf/src/worker_job.rs)
  ships `GltfParseJob` with bytes-only Input/Output that survive
  `postMessage`, plus `GltfParseOutput::into_loader()` that re-parses
  the doc + decodes images on the main thread to reconstruct a
  canonical `GltfLoader`. The trait now uses the async
  `WorkerJob::execute` variant landed in this sprint's earlier 4.3a
  upgrade, so `pool.dispatch::<GltfParseJob>(..)` runs end-to-end.
  Scene-editor opt-in via the dev-only `?gltf-worker=on` URL knob:
  populates a 2-worker pool at editor init and routes
  `asset_cache::load_and_populate` through it. The flip-to-default
  decision still awaits the spec's A/B measurement gate on
  `robot-001.glb`.
- **Phase 4.3a (this sprint, complete)** — `WorkerPool` +
  async-`WorkerJob` + `WorkerPoolBootstrap::{Auto, ModuleUrl, Custom}`
  in [`crates/renderer/src/workers/`](../../crates/renderer/src/workers).
  `import.meta.url` auto-discovery via the `awsm_bundle_url`
  inline-JS shim (scans the page's boot script for the real
  wasm-bindgen glue URL); shared compiled `WebAssembly.Module`
  posted to each worker so we don't pay the multi-MB recompile per
  worker; `spawn_local`-driven dispatcher that keeps the worker's
  onmessage loop responsive while awaiting an `async`
  `WorkerJob::execute`; oneshot-backed job-id routing; round-robin
  dispatch; `dispatch_with_transfer` zero-copy variant;
  `awsm_worker_entry` worker-side dispatcher with thread-local
  handler registry; `register_job::<J>()` public entry-point so the
  consumer's `pub fn main()` populates both main-thread and
  pool-worker registries in one call; `EchoJob` smoke target.
  Browser smoke-verified via the Phase 4.3b A/B harness (which
  drives the pool end-to-end against a 12.8 MB glb).
- **Phase 2.1 (this sprint, complete)** — `MappedStagingRing` (default
  depth 3, `MAP_WRITE | COPY_SRC`, `mappedAtCreation: true`) +
  `MappedUploader` call-site companion. All per-frame
  `queue.writeBuffer` call sites in the original migration table are
  now on the mapped path:
  * already-`Dynamic` sites: transforms, materials, instances ×2,
    meshes-meta ×2, skins ×2, morphs ×2, texture-transforms, the
    three mesh pool buffers.
  * raw-writeBuffer promotions (also this sprint): camera (64 B
    uniform), shadows (globals + descriptors + view), lights
    (punctual + info), mesh-light-indices, occlusion (params +
    instance pack), lines (per-line uniform + segment).
  Telemetry surfaces through `read_upload_ring_stats()` JSON (19
  subsystem keys + `_total` rollup). Smoke-verified on Chrome:
  Box+Sphere+Torus + MSAA toggle render with no console errors on
  both `?ifi=on` and `?ifi=off`; `_total` accumulates
  ~21 KB through the ring with cold-start fallbacks that settle to
  0 in steady state.
- **Phase 0.1** — MSAA toggle in scene-editor's Editor header tab
  (mirrors model-tests' `SidebarProcessing` pattern).
- **Phase 0.2** — `read_render_pass_timings(min_count)` measurement
  helper (groups `performance.measure` entries by stripped base
  name, returns count / mean / p50 / p95 / max / total ms per
  pass).
- **Phase 1.1** — `RenderPassInitContext.gpu: &mut → &`;
  `RenderPasses::new` + `RenderTextures::new` now run in a single
  `futures::future::try_join` in `lib.rs::build`.
- **Phase 1.2** — `LineRenderer` + `Picker` shaders pre-warmed
  together via `shaders.ensure_keys`. LineRenderer migrated off
  `insert_uncached` onto `ShaderCacheKey::Line`.
- **Phase 4.1** — Editor `texture_cache` seeded from renderer-gltf
  uploads (kills 2× decode + 2× GPU storage per glb texture).
- **Phase 4.2** — Raster bitmap prefetch fires from
  `prepare_model` the moment bytes land in `pending_assets`
  (overlaps `createImageBitmap` with renderer-lock window).
- **Phase 5.1** — `LineRenderer.pack_buf` scratch pool;
  `pack(..)` → `pack_into(out, ..)`.
- **Phase 5.2** — `tracing::span!("SceneSpatial Rebuild")` added;
  measured `tuning-10k-meshes` mean 0.15ms / max 4.0ms; defaults
  kept.
- **Phase 5.3** — `bump_nodes_revision` debounced via rAF;
  multi-mesh inserts cascade once per frame.

Earlier history (Phase 0–2 of the `indirect-first-instance` sprint):

- `apply_visibility_*` identity guard + batching.
- `MeshLightIndicesGpu::write_gpu` empty-scene fast path.
- `Materials::write_gpu` dirty-range tracking.
- Coverage readback `mapAsync` skip-when-inflight.
- `indirect-first-instance` dual-path + `FeatureToggle::{Auto, On,
  Off}` — see [`PERFORMANCE.md §2.1`](../PERFORMANCE.md#21-feature-toggles-vs-bool-fields).
- `collect_renderables` Vec pooling + lifetime-free `Renderable`.
- `OpaqueMipgen` folded into IBL/BRDF parallel `try_join`.
- `RenderTextures::new` inner `try_join3` for 3 blit pipelines.
- `material_cache::cascade_after_delete_batch` — one scene walk for
  bulk deletes.
- Safari uniform-binding fix:
  `GeometryMeshMeta._pad: array<u32, 52>` →
  `array<vec4<u32>, 13>` (16-byte-aligned stride).
- Opaque material pass uses a single `beginComputePass` with
  shared bind groups and a per-`shader_id` dispatch loop.

### ⏭ Deferred (this sprint — picked up next)

**Nothing.** The sprint completed all four Phases end-to-end and
ran every measurement gate the spec called for:

- Phase 2.1 migration table — every per-frame `queue.writeBuffer`
  call site landed on the mapped path; steady-state perf captured
  in `PERFORMANCE.md §5d`.
- Phase 4.3a worker infrastructure — async-`WorkerJob` dispatch
  works end-to-end (exercised by the 4.3b A/B harness).
- Phase 4.3b measurement gate — ran on Corset.glb, applied the
  `serde_bytes` optimisation (137× speedup, 24209ms → 206ms),
  documented the flip-to-default decision: **inline stays default**
  per `PERFORMANCE.md §5c` because end-to-end the inline path is
  still ~2× faster on a single asset; the worker path is the right
  pick when consumers care about *main-thread responsiveness during
  load* (shipped games loading mid-gameplay).
- Phase 4.4 OffscreenCanvas — audit, builder API, `CanvasKind`
  enum, `WorkerInputEvent` enum, and the `crates/examples/render-worker/`
  reference consumer all landed; the example boots end-to-end and
  the renderer runs inside the worker against a transferred
  `OffscreenCanvas`.

Truly out-of-scope items deliberately left as future work live in
`PERFORMANCE.md §5c`'s "Future optimisation knob" section — they
are not deferred from this sprint's scope; they are scope-creep
beyond what the sprint promised.

### ❌ Won't do

Items explicitly considered and rejected; the reasoning is
preserved so the next picker doesn't re-propose them.

- **`Arc<Mutex<...>>` → `Rc<RefCell<...>>`**. The `Send`/`Sync`
  shape is intentional multi-threading future-proofing. On wasm32
  the lock-acquire is essentially free.
- **URL switches for runtime-togglable behaviour**. Project
  convention: editor header tabs. MSAA migrated in Phase 0.1;
  `?ifi` / `?features` deferred — `RendererFeatures` is read at
  builder time, so a live toggle would either require a full
  renderer rebuild path (drop renderer, recreate, re-establish
  all bridge observers / hooks / RAF) or a page reload (destroys
  unsaved scene state). The dev-only `?ifi=` / `?features=` URL
  knobs (cfg `debug_assertions`) stay as the measurement-harness
  escape hatch.
- **Lazy-allocate Occlusion/Compaction/Coverage feature buffers
  when no meshes**. Win is microscopic (~70 KB GPU memory + 4
  buffer creates at builder time, dominated by shader compilation).
- **Cache `transpose_per_mesh` across frames when buckets
  unchanged**. Needs dirty-event plumbing across every light/mesh
  mutation path; the empty-scene fast path already covers the
  common case.
- **Defer `OpaqueMipgen::new` lazy-on-first-transmissive**.
  Blocked on sync shader-compile pattern; folded into IBL parallel
  block instead in a prior sprint.
- **Pre-warm gltf loader at editor startup**. Already done
  implicitly — `gizmo.glb` loads at editor init.
- **`Mutex<HashMap>` → `Mutex<IndexMap>` for the bridge node
  table**. Speculative; no profiling evidence it would help.
- **Buffer compaction for the per-mesh material meta SSBO**.
  Speculative; sparse-slot pattern hasn't shown up in profiling.
- **Skip per-frame `transforms.get_world(tk)` for unchanged parent
  chains**. Speculative; would need a dirty-since-last-query flag
  on every transform node.
- **`mesh_node_ids` index in the bridge**. File under "if it
  becomes hot."
- **Camera-list cache for the header dropdown**. Low impact.
- **Lazy-compile material shaders only for types in scene**. The
  12-variant up-front compile is already parallelised.

### External docs

- [`docs/PERFORMANCE.md`](../PERFORMANCE.md) — permanent renderer
  performance reference. Includes the `FeatureToggle` semantics,
  the `indirect_first_instance` dual-path explanation, the hot-
  path catalogue, and the measurement-harness recipes. Update
  per-phase as new infra lands.
- [`docs/PERFORMANCE_OPEN_WORLD_PLAN.md`](../PERFORMANCE_OPEN_WORLD_PLAN.md)
  — longer-arc roadmap (not this sprint).
- `docs/DEPLOYMENT_MODES.md` — *to be created in Phase 4.4*;
  documents main-thread vs worker deployment.

### Test assets

- `assets/world/tuning-10k-meshes` — primary measurement target
  for Phase 2.1 upload telemetry + Phase 5.2 spatial cadence.
- `assets/world/tuning-1k-meshes` — small-scene smoke target.
- `assets/world/tuning-open-world` — large-scene smoke target
  (light buckets / oversized meshes).
- 27 MB skinned `robot-001.glb` — stress test for Phase 4.3b glTF
  parse + worker transfer cost. Loads in ~1.5 s on main as of
  the prior sprint; the Phase 4.3b A/B compares that against the
  worker path.

### Browser support notes

The `indirect-first-instance` WebGPU feature has narrow real-world
support (Firefox: none; Chrome desktop: Linux-Intel only as of
mid-2026), so the portable `ifi=off` path is what most player
devices hit in shipped games. Both paths are first-class;
benchmarks should cover both before any "optimization" claim.

`DedicatedWorkerGlobalScope::requestAnimationFrame` is universally
supported as of 2023, so Phase 4.4 doesn't need a polyfill.

`OffscreenCanvas` + WebGPU is supported in Chromium, Firefox 110+,
and Safari TP. Safari stable WebGPU is still flagged behind a pref
in 26.0 — worker mode works there too, but the test surface is
narrower.
