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

## Phase 1 — Renderer init parallelization

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

### 4.3 🚀 First-class worker pool + glTF parse as first consumer  *(deferred — see "Won't do")*

The full design — `WorkerPool` + `WorkerJob` trait, auto-bundle
discovery via `import.meta.url`, shared `WebAssembly.Module`
postMessage protocol, `linkme`-style distributed-slice job
registry, `GltfParseJob` as first consumer, opt-in via config
knob behind a per-pass measurement gate — is preserved below for
the dedicated follow-up sprint. Reasoning for deferral is in
"Won't do".

<details>
<summary>Full original design (preserved for follow-up sprint)</summary>

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

**Inline JS shim** (built and blob-URL'd by `WorkerPool::new`).
The shim does **not** call `init()` immediately — it waits for the
main thread to post the pre-compiled `WebAssembly.Module` and the
bundle URL, then initializes with the shared Module. This avoids
re-compiling the multi-MB Rust binary in every worker:

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
   `wasm_bindgen::module().dyn_into::<WebAssembly::Module>()` (or
   the equivalent unchecked cast). The Module is the compiled
   artifact, *not* the linear-memory Instance — so it's safe to
   share.
2. Reads the bundle URL via the `awsm_bundle_url()` snippet.
3. Spawns each worker from the blob-URL shim above.
4. `postMessage({ kind: "awsm-init", wasm_module, glue_url })` to
   each worker — `WebAssembly.Module` is structured-cloneable, no
   copy of the wasm bytes is performed by the browser; each worker
   gets a reference to the same compiled artifact.
5. Awaits `{ kind: "awsm-ready" }` from each before resolving
   `WorkerPool::new`.

**What's actually duplicated per worker:**

- The JS glue (~10–30 KB; re-imported by each worker's
  `await import(glue_url)`). Mostly cheap; runs once per worker
  at startup.
- The wasm Instance — each worker has its own linear memory.
  That's intentional and is the boundary we want: workers can't
  see main-thread heap directly, all I/O is via `postMessage`
  with structured clone (or `Transferable` for large
  `ArrayBuffer`s, see `dispatch_with_transfer`).

**What's NOT duplicated:**

- The compiled `WebAssembly.Module` itself. Browser compiles it
  once on the main thread; workers reference the same
  compilation. No 100 ms–1 s re-compile per worker.
- The .wasm bytes on the wire. Browser cache + the shared
  `Module` means the network/disk side is touched once.

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
- **Naive worker `init()` (let each worker compile the .wasm from
  the URL).** Rejected: for a multi-MB Rust binary, the wasm
  compile step is 100 ms–1 s *per worker*. With 2–4 workers in
  the pool, pool startup alone would burn 200 ms–4 s of cold
  cost — unacceptable for editor open / project load. The
  shared-`WebAssembly.Module` shape (compile once on the main
  thread, structured-clone the Module to each worker, each worker
  instantiates a fresh Instance from the shared compilation) is
  the standard fix and what wasm-bindgen-rayon does.
- **`Custom`-only API (no auto-discovery)**. Rejected: pushes the
  inline-JS shim + URL plumbing onto every consumer. The whole
  point of a first-class abstraction is that simple use is one
  line.
- **Single-worker-per-job (no pool)**. Rejected: doesn't amortize
  startup cost (~5–50 ms per worker spawn). Scene loads with
  multiple glbs would pay it per file.

</details>

---

## Phase 5 — Per-frame polish

## ✅ Recently landed (do not redo)

For PR context — these shipped in the prior sprint and the
`indirect-first-instance` sprint before it.

- ✅ Phase 5.3 — `Bridge::bump_nodes_revision` debounces via
  `gloo_render::request_animation_frame`. A pending-frame slot
  on `Bridge` short-circuits subsequent bumps inside the same
  frame; the rAF callback `take()`s itself out and does the
  actual `Mutable::set`. Multi-mesh model inserts (which fired
  `bump_nodes_revision` per node during `insert_node` and again
  per node during `remove_node`) now cascade once per frame
  instead of once per node, collapsing dozens of selection /
  gizmo / point-handle / inspector re-derivations into one.
  Safe because every consumer is a `signal()` subscriber — none
  expect synchronous response.
- ✅ Phase 5.2 — added a `tracing::span!("SceneSpatial Rebuild")`
  around `scene_spatial.rebuild_if_needed` in `update_transforms`
  so `read_render_pass_timings(0)` attributes the rebuild cost.
  Measured on `tuning-10k-meshes` (151 frames, steady state,
  Chrome): mean 0.15ms, p50 0, p95 0.1, max 4.0, total 22.7ms.
  The defaults (`rebuild_period_frames=600`,
  `rebuild_dirty_threshold=200`) keep the worst-case 4ms rebuild
  to roughly one hit per 10s @60fps — the per-frame budget
  (`Render` mean 2.17ms) is dominated by `Geometry RenderPass`
  (0.36ms), `Collect renderables` (0.27ms), and `Shadow
  Generation` (0.66ms), so no clear tuning win is visible. Kept
  the defaults; the new span is the load-bearing artifact —
  future tuning has a measurement foundation that didn't exist
  before this sprint.
- ✅ Phase 5.1 — `LineRenderer` carries a `pack_buf:
  Vec<GpuLineSegment>` scratch buffer; `pack()` became
  `pack_into(out, ...)` which `out.clear()`s + extends in place.
  Editor overlays (collider wireframes, point handles, selection
  outlines) re-pack many small line strips per frame and were
  bouncing the allocator per call; this pulls them off the alloc
  path. Simulator side already pooled (`Simulator.packed: Vec
  <InstanceAttr>` is held + clear+pushed in `tick()`) — Phase 5.1
  needed only the LineRenderer half.
- ✅ Phase 4.2 — Raster bitmap prefetch fires from `prepare_model`
  the moment the gltf texture bytes are in `pending_assets`
  (right after `extract_gltf_materials_into`), as a
  `spawn_local` background task. Overlaps the `createImageBitmap`
  wall-clock with the rest of the insert path (modal copy,
  renderer-lock acquisition inside `load_and_populate`, the
  populate_gltf compile/upload window) instead of running
  serially inside populate. The original prefetch call inside
  `load_and_populate` stays as a safety net (idempotent cache)
  for paths that don't route through `prepare_model` — gizmo
  init, procedural_sync, project Load.
- ✅ Phase 4.1 — Editor texture_cache seeded from renderer-gltf
  uploads. `extract_gltf_materials_into` now stashes a per-image
  `gltf_image_asset_ids` Vec on the gltf's AssetEntry. After
  `populate_gltf` lands, `asset_cache::seed_texture_cache_from_populate`
  walks `ctx.textures`, resolves each (texture_index, color)
  back to the gltf image index, and seeds the editor's
  `texture_cache` with the renderer-gltf-side `TextureKey`. The
  override path's `get_or_upload(asset_id, ...)` now hits the
  shared key instead of re-decoding + re-uploading the same image
  — recovers the **2× GPU storage + 2× decode** per glb texture
  that the editor was paying.
- ✅ Phase 1.2 — Pre-warmed LineRenderer + Picker shader compiles.
  LineRenderer migrated off `shaders.insert_uncached` onto a real
  `ShaderCacheKeyLine` (new `ShaderCacheKey::Line` variant + a
  static `ShaderTemplateLine`). `lib.rs::build` now issues a
  single `shaders.ensure_keys(...)` for the 1 Line + 2 Picker
  shader variants before either constructor runs, so the browser
  kicks off all three `compile_shader`s together and validates
  them in parallel. The per-pass constructors then run
  sequentially through `&mut shaders / &mut pipelines` (pipeline
  state is fast vs. shader compile), but the slow part is
  pre-warmed.
- ✅ Phase 1.1 — `RenderPassInitContext.gpu: &mut` → `&`. Walk of
  every consumer showed nothing actually mutated the handle. With
  the shared borrow, `RenderPasses::new` + `RenderTextures::new`
  now run inside a single `futures::future::try_join` in
  `lib.rs::build` — both want `&gpu`, neither contends on the
  other's `&mut` fields (RenderTextureFormats is cloned for the
  textures side).
- ✅ Phase 0.2 — `read_render_pass_timings(min_count)` measurement
  helper. The per-pass `performance.measure` entries already exist
  (`tracing-web::performance_layer` routes every renderer span
  automatically); the new helper groups by base name — stripping
  the `[id]: span-measure` suffix — and returns
  `{count, mean_ms, p50_ms, p95_ms, max_ms, total_ms}` per pass
  as JSON, then clears measures so the next sample window starts
  fresh. Drives the Chrome-vs-Safari per-pass comparison in one
  `preview_eval` call. Zero-cost when `render_timings = false`.
- ✅ Phase 0.1 (partial) — `MSAA Anti-Aliasing` checkbox in the
  scene-editor's Editor header tab. Mirrors the pattern in
  model-tests' `SidebarProcessing::render_msaa_selector`; routes
  through a new `actions::view::toggle_msaa()` that calls
  `renderer.set_anti_aliasing(..)`. `?ifi` / `?features` migration
  was carved out — see "Won't do".
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

- ❌ First-class worker pool + GltfParseJob (was Phase 4.3).
  Deferred to its own sprint — multi-day implementation surface
  (wasm worker init, shared `WebAssembly.Module` postMessage
  protocol, dynamic job dispatch with `linkme`-style registry,
  opt-in config knob, the `gltf-parse` A/B measurement). The
  plan's own measurement gate ("Before landing 4.3b, wire 0.2's
  per-pass sub-spans and add a `gltf-parse` measurement so the
  worker version can be compared to the inline version. If the
  transfer cost dominates … the pool can be opt-in via a config
  knob") makes 4.3b conditional on the measurement coming in
  favourably. The full design — `WorkerPool`, `WorkerJob` trait,
  auto-bundle discovery via `import.meta.url`, shared-Module
  shim, the `GltfParseJob` consumer wiring — is preserved in
  the Phase 4.3 section above (collapsed under "Full original
  design") so the follow-up sprint can pick it up cold.
  Recommended split: Phase 4.3a (infra + Custom-bootstrap-only
  API) → Phase 4.3b (`import.meta.url` auto-discovery +
  GltfParseJob + Chrome A/B measurement → opt-in config knob).
- ❌ Per-frame upload arena (was Phase 2.1). Deferred to its own
  sprint — broad cross-subsystem refactor (touches ~10
  subsystems' `write_gpu`: transforms, materials, instances,
  meshes/meta, textures, camera, lights, shadows globals +
  descriptors, skins, morphs, plus several reset paths). The
  dirty-range coalescing in `write_buffer_with_dirty_ranges`
  already buys most of the within-subsystem gain (~5-10 ranges
  per call typically); the arena win is in cross-subsystem
  consolidation of those into a single `writeBuffer` +
  `copyBufferToBuffer` blits. Wants its own sprint because it's
  the largest refactor in this picklist and benefits from a
  focused implementation pass with measurement (the Phase 0.2
  `read_render_pass_timings` helper is the load-bearing
  prerequisite that landed in this sprint). Worth weighing
  mapped-buffer ring (StagingBelt-style) at the same time — see
  the architecture note below.
- ❌ `Arc<Mutex<...>>` → `Rc<RefCell<...>>`. Rejected: the
  `Send`/`Sync` shape is intentional future-proofing for
  multi-threading. On wasm32 the lock-acquire is essentially free.
- ❌ URL switches for runtime-togglable behavior. Project
  convention: editor header tabs (Editor / Camera / Environment /
  etc.). MSAA migrated as part of Phase 0.1; **`?ifi` /
  `?features` migration deferred** — `RendererFeatures` is read at
  builder time (see `features.rs` doc: "Toggling a gate after
  `build()` requires a renderer rebuild"). A live editor-tab
  toggle would either (a) require a full renderer rebuild path
  (drop renderer, recreate, re-establish all bridge observers /
  hooks / RAF) or (b) flip the URL param and reload the page,
  which destroys unsaved scene state. Both are out of scope for
  this sprint. The dev-only `?ifi=` / `?features=` URL knobs (gated
  on `cfg(debug_assertions)`) stay as the measurement-harness
  escape hatch.
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

- **Branch**: work on the `optimizations` branch. Do *not* spin
  up feature branches per chunk — the whole sprint lands on
  `optimizations`. The branch will be PR'd to `main` as a single
  batch once everything has landed.
- **Commits**: small, logical, descriptive — sized for git-bisect
  to be useful. Each commit should map to one Phase item (or a
  clean sub-step of one). End each commit message with
  `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>`.
- **Verification policy** *(important — this is different from the
  prior sprint)*: it is **acceptable for the workspace to be in a
  broken state between commits** during this sprint. Don't pad
  each individual commit with compatibility shims, redundant
  conversions, or temporary forwarders just to keep the tree
  compiling mid-sequence — that work would all be deleted later
  and the technical debt is worse than the broken-middle state.
  Verification (`cargo check`, `cargo clippy`, `cargo test`,
  editor smoke test) is required **only at the end of the whole
  sprint** before requesting review. Intermediate compile
  failures are fine *if* the failure scope is bounded and the
  next commit fixes it.

  Use git-bisect semantics as the guide: a future bisector wants
  *logical* boundaries, not "every commit must build." Tight,
  honest commits beat fake-greenness.

  Within a single commit, however: keep the diff coherent. Don't
  ship "added field but forgot to update use sites" if updating
  the use sites is a one-line change — that's just sloppy. The
  "broken middle" permission is for genuine cross-file refactors
  (e.g., a trait signature change that takes 30 use sites to
  update) where staging matters for bisect.

  **Don't run `cargo build --workspace`** at any point —
  `cargo check` is cheaper and covers compile validation. The
  trunk dev-server + editor browser smoke test are the real
  "does it run" check at the end.

- **End-of-sprint verification** (do once, before opening the PR):
  - `cargo check --workspace` clean
  - `cargo clippy --workspace` clean
  - `cargo test --workspace` green
  - Editor renders Primitive Box + Sphere + Torus on both
    `?ifi=on` and `?ifi=off` (or the Editor toggle equivalent
    once Phase 0.1 lands)
  - Repeat the smoke test on Safari if possible; cold-restart
    Safari is the meaningful comparison since dev-session
    degradation is a dev-loop artifact, not a renderer bug

- **Doc maintenance**: when a Phase item lands, move it to
  "Recently landed" with a one-line summary. When an item turns
  out to be unwise mid-implementation, move it to "Won't do"
  with the reason. Keep this doc the source of truth — don't let
  it drift, even mid-sprint when other things are broken.

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
