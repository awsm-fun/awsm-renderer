#![allow(clippy::type_complexity)]
//! Reference example — the entire renderer runs in a
//! Web Worker against an `OffscreenCanvas`.
//!
//! ### Architecture
//!
//! - **Main thread** (`main_thread_boot`): creates a `<canvas>`,
//!   calls `transferControlToOffscreen()`, spawns a worker that
//!   imports this same wasm bundle, and posts the offscreen canvas +
//!   the shared `WebAssembly.Module` to it. After init, the main
//!   thread captures pointer/resize events and forwards them via
//!   `postMessage` so the worker-side renderer can react.
//!
//! - **Worker thread** (`worker_thread_boot`): receives the
//!   `OffscreenCanvas`, builds an [`awsm_renderer_core::renderer::AwsmRendererWebGpuBuilder`] via
//!   the [`new_with_offscreen_canvas`](awsm_renderer_core::renderer::AwsmRendererWebGpuBuilder::new_with_offscreen_canvas) constructor, and
//!   drives a `requestAnimationFrame`-paced render
//!   loop. Today the worker only `tracing::trace!`s any
//!   [`WorkerInputEvent`] it receives — wiring those into a free
//!   camera is intentionally left to the consumer.
//!
//! Both entry points live in the same crate, dispatched at runtime
//! by [`crate::is_worker_scope`] so a single wasm bundle serves both
//! contexts (the same pattern `wasm-bindgen-rayon` uses).
//!
//! ### What this example covers
//!
//! - The `transferControlToOffscreen()` + shared `WebAssembly.Module`
//!   handshake.
//! - The `new_with_offscreen_canvas(..)` builder path.
//! - Wire shape for input forwarding: [`WorkerInputEvent`] defines
//!   the full protocol (pointer move/down/up, wheel, key down/up,
//!   resize) but the main-thread shim only installs a `pointermove`
//!   listener today — `PointerDown`/`PointerUp`/`Wheel`/`KeyDown`/
//!   `KeyUp` and the `ResizeObserver`-driven `Resize` forwarder are
//!   left as consumer-side work (they tie into the framework that
//!   owns the DOM layout / focus). The enum is published as the
//!   contract a consumer slots into.
//! - `requestAnimationFrame` driven from the worker side via
//!   [`awsm_renderer::web_global::request_animation_frame`].
//!
//! ### What the worker actually renders
//!
//! - A single procedural box mesh ([`awsm_renderer_meshgen::primitives::box_mesh`])
//!   with a bright opaque [`awsm_renderer_materials::pbr::PbrMaterial`]
//!   (emissive factor cranked so the box self-illuminates against an
//!   empty scene — the example deliberately skips light + environment
//!   setup to keep the smoke test focused on the OffscreenCanvas +
//!   render-loop path). The box rotates around Y each frame so the
//!   browser smoke test can confirm the render loop is alive.
//!
//! ### What it does *not* cover
//!
//! - A real glTF scene — the procedural box keeps the asset-loading
//!   surface area off the example. Plug a real scene in by calling
//!   `renderer.populate_gltf(..)` after init in
//!   `start_worker_renderer`.
//! - Punctual lights, an environment map, or shadows — the emissive
//!   PBR factor stands in. Add real lighting through `renderer.lights`
//!   / `renderer.environment` once those need exercising on the
//!   worker path.
//! - DOM-overlay UI — that's a consumer choice (HTML element
//!   absolutely-positioned over the canvas).
//!
//! Browser smoke-verification of this example is follow-on
//! work — `cargo check` passes today; an
//! end-to-end `trunk serve` boot needs a tiny `index.html` shim
//! (see [`HTML_SHIM`] below) and is verified in the editor's
//! Claude Preview MCP harness.

mod worker;
pub use worker::*;
