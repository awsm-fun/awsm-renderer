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

> **Phase 4.4 status**: the runtime-global helpers
> (`crates/renderer/src/web_global.rs`) are in place. The
> codebase-wide audit-and-replace of `web_sys::window()` is **not
> yet complete** — many renderer subsystems still reach for the
> main-thread global directly. Until the audit lands, worker-mode
> consumers will hit `web_sys::window().unwrap()` panics. The
> `AwsmRendererWebGpuBuilder::new_with_offscreen_canvas` builder is
> also deferred. See the sprint's `BLOCKER.md` (if present) or the
> [`more-optimizations.md`](./plans/more-optimizations.md) plan's
> "Won't do (this sprint)" notes for the picked-up next.

## Worker-mode wiring (target shape, post-audit)

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
against the `WorkerInputEvent` enum (also deferred — lands with the
example crate).

## Browser support

- **`OffscreenCanvas` + WebGPU**: Chromium, Firefox 110+, Safari TP.
  Safari stable WebGPU is still flagged behind a pref in 26.0 —
  worker mode works there too, but the test surface is narrower.
- **`DedicatedWorkerGlobalScope::requestAnimationFrame`**:
  universally supported as of 2023, so no polyfill needed.

Chrome via the Claude Preview MCP is the required smoke target for
this sprint; Safari is nice-to-have.
