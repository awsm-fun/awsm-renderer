# Optimization sprint — next round

Action-oriented picklist. Each item is concrete enough to pick up
cold; the order below reflects dependencies (earlier items unblock
or enable measurement of later items).

Status legend:
- 🚀 — clear measurable win
- ⚙️ — tooling / measurement-enabler (no perf change by itself, but
  unblocks landing later items confidently)

When implementing: see [Working agreements](#working-agreements) at
the bottom of this doc before starting.

---

## Phase 0 — Measurement & toggles (do first)

These don't change perf; they let every later item be A/B'd cheaply
in both Chrome and Safari from the running editor.

### 0.1 ⚙️ MSAA toggle in the Editor section

Add an `MSAA Anti-Aliasing` checkbox to the scene-editor's Editor
header tab ([crates/frontend/scene-editor/src/header/editor.rs](../../crates/frontend/scene-editor/src/header/editor.rs)) — mirror the pattern in [model-tests' SidebarProcessing::render_msaa_selector](../../crates/frontend/model-tests/src/pages/app/sidebar/processing.rs)
(`msaa_sample_count: Option<u32>` — `Some(4)` ↔ `None`). Re-init the
renderer's anti-aliasing on change via the existing
`set_anti_aliasing(..)` flow. **Runtime toggles like this should be
editor UI, not URL switches** — the project convention is to expose
everything switch-able through the header tabs (Editor / Camera /
Environment / etc.). The existing `?ifi=on/off` was the wrong shape
and should ALSO migrate to an Editor toggle in this phase if cheap.

### 0.2 ⚙️ Per-pass `performance.measure` sub-spans

The top-level `Render` span already lands in
`performance.getEntriesByType('measure')`. Wire per-pass children so
the Chrome-vs-Safari comparison can attribute the gap to a specific
pass without manual instrumentation. Pattern: extend the
`tracing::span!(... "Geometry RenderPass")` blocks in
[render.rs](../../crates/renderer/src/render.rs) to also emit a
`performance.mark` start/end pair when `logging.render_timings` is
on. The instrumentation should be zero-cost when the flag is off.

---

## Phase 1 — Renderer init parallelization

### 1.1 🚀 Refactor `RenderPassInitContext.gpu` from `&mut` to `&`

[crates/renderer/src/render_passes.rs:119](../../crates/renderer/src/render_passes.rs) — `gpu: &'a mut AwsmRendererWebGpu`
is never actually mutated downstream; the `&mut` is the *only* thing
blocking `RenderTextures::new` from running concurrently with
`RenderPasses::new` (the previous sprint had to settle for inner
`try_join3` in `RenderTextures::new`). Walk every consumer; the
WebGPU device handle is clonable / wrapped enough that `&` should be
sufficient.

After landing, wrap `RenderPasses::new` + `RenderTextures::new` in
`futures::future::try_join` in [lib.rs](../../crates/renderer/src/lib.rs) — they share no
mutable state (one takes `&mut shaders/pipelines/...`, the other
takes owned `RenderTextureFormats` + `&gpu`).

### 1.2 🚀 Pre-warm LineRenderer shaders in parallel with Picker

Both currently serialize through `&mut shaders / pipelines /
bind_group_layouts`. Approach: collect both passes' shader cache
keys up-front, issue a single `shaders.ensure_keys` batch, then
construct both with the cache already warm. **Prerequisite:**
`LineRenderer` currently uses `shaders.insert_uncached` so its
shader bypasses the cache entirely — needs a small refactor to a
cache-keyed shader before `ensure_keys` can help it.

---

## Phase 2 — Per-frame upload consolidation

### 2.1 🚀 Consolidate per-frame `queue.writeBuffer` calls into a shared staging buffer

Every frame the renderer fires ~15–25 distinct `writeBuffer` calls
(transforms, materials, instances, meta, shadows, light indices,
occlusion instances, occlusion params, coverage reset, classify
reset, decal reset, mesh-light-indices upload, etc.). On Safari each
call is a Metal staging-buffer create + blit + sync; Chrome amortises
better but still benefits.

Approach: introduce a per-frame **upload arena** — a single
GPU-resident staging buffer that subsystems append to via a small
`UploadHandle { offset, len }`, then one `copyBufferToBuffer` per
destination (or one `writeBuffer` of the whole arena followed by
`copyBufferToBuffer` blits). The dirty-range tracking in
`DynamicUniformBuffer` / `DynamicStorageBuffer` already coalesces;
the change here is replacing N small `writeBuffer`s with one large
one.

Worth measuring before+after on both browsers — should be the
biggest Safari delta of this sprint.

---

## Phase 3 — *(retired — see "Recently landed" below)*

The original Phase 3 ("merge opaque PBR/Unlit/Toon compute pipelines
into one shader-id-branched pass") was rejected for not scaling to
many materials. On closer look the narrow-scope alternative —
running all opaque pipelines inside a single `beginComputePass` /
`endComputePass` boundary — is *already implemented* in
[`render_pass.rs`](../../crates/renderer/src/render_passes/material_opaque/render_pass.rs):
one pass, shared bind groups, a for-loop of `set_pipeline` +
`dispatch_workgroups_indirect` per `shader_id`. Per-material
pipeline specialization is preserved (each shader stays small),
intra-workgroup divergence stays zero, and adding a new material
is one entry in the dispatch loop.

No work needed here. Phase 3 retired; renumbering left as-is so
cross-references in commit messages stay valid.

---

## Phase 4 — Model insert UX

### 4.1 🚀 Deduplicate the two GPU texture-pool uploads per glb texture

The `renderer-gltf` path uploads each image via
`Textures::add_image(ImageData::Bitmap)` for the baked materials,
AND the editor's `texture_cache::get_or_upload` uploads the same
image (separate `createImageBitmap` decode + separate pool slot) so
the editor's editable material override has its own copy. That's
**2× GPU storage + 2× decode** per texture on every model insert.

Approach: plumb a mapping `AssetId → existing TextureKey` from
`renderer-gltf` into the editor's `texture_cache` so the override
path reuses the renderer-gltf-side pool slot. Touch points:
[crates/renderer-gltf/src/populate/](../../crates/renderer-gltf/src/populate) (publish the mapping) and
[crates/frontend/scene-editor/src/renderer_bridge/texture_cache.rs](../../crates/frontend/scene-editor/src/renderer_bridge/texture_cache.rs)
(consume it).

Probably the single biggest model-insert-UX win in this list.

### 4.2 🚀 Pre-decode raster bitmaps eagerly

The raster prefetch is currently hoisted into `load_and_populate`,
which runs once per glb. Move it to fire *synchronously the moment
the bytes land in `pending_assets`* so it overlaps with the
user-visible loading modal instead of running during populate.
Saves ~1 s of `createImageBitmap` wall-clock during the loading
window for large glbs.

### 4.3 🚀 First-class worker pool + glTF parse as first consumer

This phase builds a **library-wide worker-job infrastructure** (4.3a)
and uses glTF parse as its first consumer (4.3b). The infrastructure
lands first because future items (e.g., mesh tangent computation,
animation baking, environment-map filtering, large-scene BVH
rebuild) will plug into the same pool.

**Library constraint.** `awsm-renderer` ships as a Rust library;
consumers may use Trunk, webpack, Vite, or no bundler at all. The
worker abstraction **cannot** assume a separate `worker.js` file is
copied to a known path — the consumer's build pipeline might not
produce one.

#### 4.3a — `awsm-renderer::workers` module: `WorkerPool` + `WorkerJob`

Public surface (decided shape — see "Alternatives considered" at
the bottom of this section for what was rejected and why):

```rust
pub trait WorkerJob: 'static {
    /// Identifies this job in the worker's postMessage dispatch.
    /// Use a unique string per job type (e.g. "gltf-parse",
    /// "mesh-tangents").
    const NAME: &'static str;
    type Input: Serialize + DeserializeOwned;
    type Output: Serialize + DeserializeOwned;

    /// Runs on the worker thread. No `&self` — implementations are
    /// stateless and only act on `input`.
    fn execute(input: Self::Input) -> Self::Output;
}

pub struct WorkerPool { /* private */ }

#[derive(Default)]
pub enum WorkerPoolBootstrap {
    /// Default. Auto-discovers the consumer's wasm-bindgen bundle
    /// URL via `import.meta.url` from the main thread's JS glue.
    /// Works for any wasm-bindgen `--target web` consumer (Trunk,
    /// Vite ESM, etc.) — the library reads its own
    /// `import.meta.url` via a `#[wasm_bindgen(inline_js = ...)]`
    /// snippet, which the wasm-bindgen tool embeds in the
    /// consumer's bundle output, so the URL is *always* the
    /// consumer's bundle URL regardless of how that bundle is
    /// named or hashed.
    #[default]
    Auto,

    /// Explicit bundle URL — for consumers whose build setup
    /// doesn't expose `import.meta.url` in a usable form (rare —
    /// some legacy non-module-worker builds). Library tries `Auto`
    /// first and only falls back to this if explicitly asked.
    ModuleUrl { bundle_url: String },

    /// Escape hatch — consumer constructs the `Worker` themselves;
    /// the pool then drives the postMessage protocol over the
    /// caller-supplied handle.
    Custom(Box<dyn Fn() -> Result<web_sys::Worker, JsValue> + 'static>),
}

impl WorkerPool {
    /// Most common shape: `WorkerPool::with_workers(2).await?`.
    /// Uses `WorkerPoolBootstrap::Auto`, defaults `worker_count` to
    /// `min(navigator.hardwareConcurrency, 4)` if `None`.
    pub async fn with_workers(worker_count: Option<usize>) -> Result<Self, AwsmError> { /* … */ }

    pub async fn new(
        bootstrap: WorkerPoolBootstrap,
        worker_count: usize,
    ) -> Result<Self, AwsmError> { /* … */ }

    pub async fn dispatch<J: WorkerJob>(
        &self,
        input: J::Input,
    ) -> Result<J::Output, AwsmError> { /* … */ }

    /// Zero-copy path — `transfer` lists `ArrayBuffer`s the protocol
    /// should `postMessage(..., { transfer })` instead of
    /// structured-cloning. Critical for the 27 MB robot case;
    /// otherwise the cross-thread copy eats most of the saving.
    pub async fn dispatch_with_transfer<J: WorkerJob>(
        &self,
        input: J::Input,
        transfer: js_sys::Array,
    ) -> Result<J::Output, AwsmError> { /* … */ }
}

// Bundle URL auto-discovery — runs in the consumer's wasm-bindgen
// module context, so `import.meta.url` is the consumer's bundle
// regardless of name / hash / build tool.
#[wasm_bindgen(inline_js = "export function awsm_bundle_url() { return import.meta.url; }")]
extern "C" {
    fn awsm_bundle_url() -> String;
}

/// Exported entry point the worker's wasm-bindgen init calls after
/// the module is loaded. Installs the postMessage listener and
/// dispatches incoming jobs by `NAME` to registered handlers.
#[wasm_bindgen]
pub fn awsm_worker_entry();
```

**Inline JS shim** (built and blob-URL'd by `WorkerPool::new`,
`{BUNDLE_URL}` substituted from `WorkerPoolBootstrap::ModuleUrl`):

```js
import init, { awsm_worker_entry } from "{BUNDLE_URL}";
await init();
self.postMessage({ kind: "ready" });
awsm_worker_entry();
```

**Helper for the blob-URL plumbing** (drop-in from the lockstep
codebase):

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

**Job registry.** `awsm_worker_entry` maintains a static registry of
`(NAME, dispatcher)` pairs populated via a `linkme`-style distributed
slice or a one-time-init function. Each `impl WorkerJob` is registered
automatically; consumer crates that define their own `WorkerJob`
implementations register the same way.

**Lifecycle.** `WorkerPool::new` spawns `worker_count` workers, waits
for each to send its `{ kind: "ready" }` message, then resolves.
`dispatch` round-robins jobs across workers (or picks the least-busy
if instrumented; start with round-robin). Pending jobs hold a
`oneshot::Sender<JsValue>` keyed by an incrementing `JobId`; the
incoming `{ kind: "result", id, payload }` message routes back via
the keyed sender.

**Error handling.** A worker that panics or hits an uncaught
exception posts `{ kind: "error", id, message }`; the pool surfaces
this as `AwsmError::WorkerJobFailed`. The worker stays alive for the
next job — one job's failure doesn't tear down the pool.

#### 4.3b — `GltfParseJob`: glTF parse moves to the pool

```rust
// awsm-renderer-gltf
pub struct GltfParseJob;
impl WorkerJob for GltfParseJob {
    const NAME: &'static str = "gltf-parse";
    type Input = GltfParseInput;     // { url, file_type }
    type Output = GltfParseOutput;   // { doc bytes, buffers, image data }
    fn execute(input: Self::Input) -> Self::Output { /* existing GltfLoader::load body */ }
}
```

Consumer wires it up (works identically for editor and for shipped
games — no consumer-supplied bundle URL anywhere):

```rust
// scene-editor / game / any consumer
let pool = WorkerPool::with_workers(None).await?;  // Auto, default count

// Loading a project — fire concurrent glb parses, pool round-robins:
let (a, b, c) = futures::try_join!(
    pool.dispatch::<GltfParseJob>(input_a),
    pool.dispatch::<GltfParseJob>(input_b),
    pool.dispatch::<GltfParseJob>(input_c),
)?;
```

For the 27 MB robot the parsed `Vec<u8>` buffers go through
`dispatch_with_transfer` — they're consumed once (uploaded to GPU),
so transferring ownership across the thread boundary is free.

#### Why auto-discovery instead of consumer-supplied URL

The earlier draft had a consumer-supplied `bundle_url: String`.
That doesn't survive contact with reality:

- Trunk hashes the JS filename in release builds (`scene-editor-
  abc123.js`); the consumer doesn't know the hash at compile time.
- Each consumer's bundle has a different name — `scene-editor.js`
  in the editor, `my-game.js` in a shipped game, `bundle.js` in
  the third one.
- Bundlers may chunk into multiple files; the "main" file isn't
  obvious.

`import.meta.url` from inside the library's `#[wasm_bindgen(inline_js
= ...)]` snippet sidesteps all of this. The snippet gets embedded
into the consumer's wasm-bindgen JS module by the wasm-bindgen
tool at build time; when called at runtime, `import.meta.url`
resolves to whatever URL the consumer's bundle is being served
from. Renames, hashes, chunking — none of it matters.

#### Measurement gate

Before landing 4.3b, wire 0.2's per-pass sub-spans and add a
`gltf-parse` measurement so the worker version can be compared to
the inline version. If the transfer cost dominates (small glbs,
where the parse itself is fast), the pool can be opt-in via a
config knob.

#### Alternatives considered (rejected)

- **Pure-JS workers** (worker code is JS, no Rust in worker).
  Rejected: doubles the maintenance burden, duplicates parse logic.
- **wasm-bindgen-rayon-style with SharedArrayBuffer** for
  fine-grained parallelism. Rejected: requires COOP/COEP headers
  on the consumer's deployment. We don't need fine-grained
  parallelism — coarse job offload via postMessage is enough.
- **Consumer-supplied `bundle_url: String` as the primary
  bootstrap.** Rejected: bundle filenames vary across consumers
  (editor vs each game), vary across build profiles (Trunk hashes
  release builds), and vary across build tools (chunking, ESM vs
  classic, etc.). The library has no business asking consumers to
  hard-code a filename that the build system is the one source of
  truth for. Auto-discovery via `import.meta.url` from a library-
  internal inline-JS snippet is portable across all of these.
- **`Custom`-only API (no auto-discovery)**. Rejected: pushes the
  inline-JS shim + URL plumbing onto every consumer. The whole
  point of a first-class abstraction is that simple use is one
  line.
- **Single-worker-per-job (no pool)**. Rejected: doesn't amortize
  startup cost (~5–50 ms per worker spawn). Scene loads with
  multiple glbs would pay it per file.

---

## Phase 5 — Per-frame polish

### 5.1 Particle simulator + line-strip vertex-pack Vec pooling

Same pattern as the recent `RenderablePool` work — hold the scratch
Vecs on the simulator / LineRenderer and `clear_in_place` per frame
instead of fresh-allocating.

### 5.2 `scene_spatial::rebuild_if_needed` cadence tuning

Defaults are `rebuild_period_frames = 600` and
`rebuild_dirty_threshold = 200`. Both could be data-driven —
larger scenes benefit from less-frequent rebuilds (rebuild cost
scales with mesh count); smaller scenes can rebuild more eagerly
for tighter query quality. **Measure first** with 0.2's sub-spans
on the `tuning-10k-meshes` scene.

### 5.3 Coalesce reactive signal cascades

`bump_nodes_revision` in `renderer_bridge` fires when any bridge
entry changes; consumers (selection observer, gizmo, point-handle,
inspector) re-derive their own state on every fire. For a multi-
mesh model insert this can spike to dozens of cascades per frame.
Approach: debounce / batch via `request_animation_frame` so multi-
node mutations cascade once per frame instead of once per node.

---

## ✅ Recently landed (do not redo)

For PR context — these shipped in the prior sprint and the
`indirect-first-instance` sprint before it.

- ✅ `apply_visibility_to_node` identity guard
- ✅ `apply_visibility_subtree` batches into one `with_renderer_mut`
- ✅ `MeshLightIndicesGpu::write_gpu` fast path on empty scenes
- ✅ `Materials::write_gpu` dirty-range tracking (was already done)
- ✅ Coverage readback `mapAsync` skip when inflight (was already done)
- ✅ `indirect-first-instance` dual-path architecture +
  `FeatureToggle::{Auto, On, Off}` — see [`PERFORMANCE.md §2.1`](../PERFORMANCE.md#21-feature-toggles-vs-bool-fields)
- ✅ `collect_renderables` Vec pooling + lifetime-free `Renderable`
  + precomputed pipeline keys
- ✅ OpaqueMipgen folded into IBL/BRDF parallel `try_join`
- ✅ `RenderTextures::new` inner `try_join3` for 3 blit pipelines
- ✅ `material_cache::cascade_after_delete_batch` — one scene walk
  for bulk deletes
- ✅ Safari uniform-binding fix: `GeometryMeshMeta._pad` from
  `array<u32, 52>` → `array<vec4<u32>, 13>` (16-byte-aligned stride)
- ✅ Opaque material pass uses a single `beginComputePass` with
  shared bind groups and a per-`shader_id` dispatch loop (turned
  out to already be the structure when Phase 3 was investigated).
  Per-pipeline specialization preserved; future-proof for more
  material types.

---

## ❌ Won't do

For the next picker — items explicitly considered and rejected.

- ❌ `Arc<Mutex<...>>` → `Rc<RefCell<...>>`. Rejected: the
  `Send`/`Sync` shape is intentional future-proofing for
  multi-threading. On wasm32 the lock-acquire is essentially free.
- ❌ URL switches for runtime-togglable behavior. Project
  convention: editor header tabs (Editor / Camera / Environment /
  etc.). The pre-existing `?ifi=on/off` and `?features=off`
  switches predate this convention and should migrate to Editor
  toggles in Phase 0.1.
- ❌ Lazy-allocate Occlusion/Compaction/Coverage feature buffers
  when no meshes. Win is microscopic (~70 KB GPU memory + 4 buffer
  creates at builder time, dominated by shader compilation).
- ❌ Cache `transpose_per_mesh` across frames when buckets
  unchanged. Needs dirty-event plumbing across every light/mesh
  mutation path; the empty-scene fast path covers the common case.
- ❌ Defer `OpaqueMipgen::new` lazy-on-first-transmissive. Blocked
  on sync shader-compile pattern; folded into IBL parallel block
  instead in the prior sprint.
- ❌ Pre-warm gltf loader at editor startup. Already done
  implicitly — `gizmo.glb` loads at editor init.
- ❌ `Mutex<HashMap>` → `Mutex<IndexMap>` for the bridge node
  table. Speculative; no profiling evidence it would help.
- ❌ Buffer compaction for the per-mesh material meta SSBO.
  Speculative; sparse-slot pattern hasn't shown up in profiling.
- ❌ Skip per-frame `transforms.get_world(tk)` for unchanged parent
  chains. Speculative; would need a dirty-since-last-query flag on
  every transform node.
- ❌ `mesh_node_ids` index in the bridge. File under "if it
  becomes hot."
- ❌ Camera-list cache for the header dropdown. Low impact.
- ❌ Lazy-compile material shaders only for types in scene. The 12
  variant up-front compile is parallelised already.

---

## Working agreements

When picking up an item:

- **Branch**: work from `main`; create a feature branch per logical
  chunk, or work straight on a sprint branch like the prior
  `more-optimizations` if doing many items.
- **Verification**: `cargo check --workspace` (or
  `cargo clippy --workspace`) + `cargo test --workspace` must stay
  green at every commit. **Don't run `cargo build --workspace`** —
  it's wasted wall-clock; `check` / `clippy` cover compile
  validation and the trunk dev-server is the real "does it run"
  check. For full visual verification, ensure the trunk server is
  running (`task scene-editor:dev` or the
  `mcp__Claude_Preview__preview_start` helper) and exercise the
  editor in the browser. The editor must render correctly on both
  `?ifi=on` and `?ifi=off` (or the Editor toggle equivalent once
  Phase 0.1 lands).
- **Smoke test**: launch `task scene-editor:dev`, insert a Box +
  Sphere + Torus, confirm all three render. Repeat on the opposite
  ifi setting before claiming done.
- **Test in Safari + Chrome** where possible — the
  `indirect-first-instance` saga showed how easily a Chrome-only
  validation pass can mask Safari breakage. The Safari GPU process
  is also more sensitive to dev-session state churn; a fresh
  restart is the meaningful comparison.
- **Commits**: small, logical, descriptive. End each with
  `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>`.
- **Doc maintenance**: when a Phase item lands, move it to
  "Recently landed" with a one-line summary. When an item turns
  out to be unwise, move it to "Won't do" with the reason. Keep
  this doc the source of truth — don't let it drift.

Reference docs:
- [`docs/PERFORMANCE.md`](../PERFORMANCE.md) — permanent renderer
  performance reference, including the `FeatureToggle` and
  `indirect_first_instance` dual-path semantics.
- [`docs/PERFORMANCE_OPEN_WORLD_PLAN.md`](../PERFORMANCE_OPEN_WORLD_PLAN.md) — longer-arc roadmap (not this sprint).

A 27 MB skinned `robot-001.glb` loads end-to-end in ~1.5 s on the
current branch (down from a 15 s materialize-timeout error before
the optimization sprint). Use that asset as a stress-test for
Phase 4 items.

The `indirect-first-instance` WebGPU feature has narrow real-world
support (Firefox: none; Chrome desktop: Linux-Intel only as of
mid-2026), so the portable `ifi=off` path is what most player
devices will hit in shipped games. Both paths are first-class —
benchmarks should cover both before any "optimization" claim.
