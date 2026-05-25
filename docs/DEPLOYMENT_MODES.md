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

## Browser support

- **`OffscreenCanvas` + WebGPU**: Chromium, Firefox 110+, Safari TP.
  Safari stable WebGPU is still flagged behind a pref in 26.0 —
  worker mode works there too, but the test surface is narrower.
- **`DedicatedWorkerGlobalScope::requestAnimationFrame`**:
  universally supported as of 2023, so no polyfill needed.

Chrome via the Claude Preview MCP is the required smoke target for
this sprint; Safari is nice-to-have.
