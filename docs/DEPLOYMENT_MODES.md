# Deployment modes — main-thread vs worker

`awsm-renderer` is designed to run in two deployment modes — both
first-class, the library doesn't favour one over the other:

| Mode | Canvas | Renderer | Main thread does | Worker does |
|---|---|---|---|---|
| **Main-thread (editor)** | `HtmlCanvasElement` in DOM | Main thread | Everything: DOM, UI, input, render | (Phase 4.3 worker jobs only) |
| **Worker (game)** | `OffscreenCanvas` transferred to worker | Worker | DOM/UI overlays, input capture, postMessage forwarding | Renderer + game logic + render loop |

The editor is **main-thread mode** by design — DOM-overlay UX
(header tabs, the right-hand inspector, the asset picker) is the
shape the editor expects. The renderer competes with editor UI for
main-thread time, but the editor's CPU budget headroom is large
enough that this isn't a problem in practice.

For **shipped games**, worker mode is the right choice: the main
thread is shared with game logic, physics, audio scheduling,
network code, etc. — isolating the renderer in a worker means it
cannot be starved by main-thread CPU contention.

> **Phase 4.4 status**: complete. Runtime-global helpers in both
> [`crates/renderer/src/web_global.rs`](../crates/renderer/src/web_global.rs)
> and [`crates/renderer-core/src/web_global.rs`](../crates/renderer-core/src/web_global.rs).
> Audit-and-replace pass closed both functional non-worker-safe
> sites (`compatibility::check`, `image/bitmap.rs::WINDOW`).
> `AwsmRendererWebGpuBuilder::new_with_offscreen_canvas(gpu, canvas)`
> is the worker-mode constructor; internally the builder stores a
> `CanvasKind { Html, Offscreen }` enum and dispatches the context
> acquisition + resize handling accordingly. A reference consumer
> is [`crates/examples/render-worker/`](../crates/examples/render-worker/)
> — single wasm-bindgen target that boots into either
> `main_thread_boot()` or `worker_thread_boot()` based on the
> active global, transfers an `OffscreenCanvas` to the worker, and
> drives the renderer's rAF loop from inside the worker.

## Worker-mode wiring

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

The library does **not** ship an input forwarder. Forwarding is
consumer-specific (different games want different event shapes,
different latency trade-offs, different filtering). The shape above
is the documented pattern; consumers DIY the actual implementation
against the `WorkerInputEvent` enum exposed by the example crate
([`crates/examples/render-worker/src/lib.rs`](../crates/examples/render-worker/src/lib.rs)).

## Worker pools vs the renderer worker

`awsm-renderer` ships **two unrelated worker concepts** — easy to
conflate, important to keep separate:

1. **`WorkerPool` for CPU jobs** (`crates/renderer/src/workers/`).
   N background workers sharing the consumer's compiled
   `WebAssembly.Module`, dispatched by `WorkerJob::NAME`. The
   first concrete consumer is `GltfParseJob` — fetch + parse +
   in-worker `createImageBitmap` of every embedded texture, with
   the resulting handles + byte payloads transferred (not
   structured-cloned) back to the dispatching thread. The
   scene-editor pre-warms a 2-worker pool at `create_context`
   time and routes `asset_cache::load_and_populate` through it
   by default ([PERFORMANCE.md §5c](PERFORMANCE.md)).
2. **The OffscreenCanvas renderer worker** (Phase 4.4, the
   subject of the rest of this doc). A *single* worker hosts
   the entire renderer + game loop, with the `OffscreenCanvas`
   transferred from the main thread.

The two are independent: a worker-mode (OffscreenCanvas)
consumer still benefits from a `WorkerPool` for glTF parse —
those pool workers offload CPU work off the *renderer worker*,
not just off the main thread. A main-thread (editor-style)
consumer benefits from the pool for the same reason: keep the
asset-load CPU spike off the thread running the UI / render
loop.

Pre-warm guidance:

- **Editor / main-thread mode:** the pool comes up at editor
  init, alongside the renderer build. See
  [`scene-editor/src/context.rs::maybe_build_worker_pool`](../crates/frontend/scene-editor/src/context.rs).
- **Worker / OffscreenCanvas mode:** build the `WorkerPool`
  *from inside the renderer worker* using
  `WorkerPoolBootstrap::ModuleUrl { bundle_url: … }` (the
  worker context can't reach the DOM, so `Bootstrap::Auto`
  can't sniff the glue URL — pass it through the same
  postMessage payload that ships the OffscreenCanvas). The
  pool workers and the renderer worker are peers; they all
  share the same compiled `WebAssembly.Module` via the init
  message.

## Browser support

- **`OffscreenCanvas` + WebGPU**: Chromium, Firefox 110+, Safari TP.
  Safari stable WebGPU is still flagged behind a pref in 26.0 —
  worker mode works there too, but the test surface is narrower.
- **`DedicatedWorkerGlobalScope::requestAnimationFrame`**:
  universally supported as of 2023, so no polyfill needed.

Chrome via the Claude Preview MCP is the required smoke target for
this sprint; Safari is nice-to-have.
