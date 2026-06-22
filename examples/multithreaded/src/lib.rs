#![allow(clippy::type_complexity)]
//! Standalone **multithreaded reference app** for `awsm-renderer`.
//!
//! This is the copyable reference behind **`docs/PLAYER-GUIDE.md` §9**. It
//! is built with the **nightly threaded profile** — real wasm threads
//! over a shared `WebAssembly.Memory` (`+atomics,+bulk-memory` +
//! `-Z build-std`) — and served with the COOP/COEP headers that enable
//! `crossOriginIsolated` (and therefore `SharedArrayBuffer`).
//!
//! ### Roles
//!
//! A single wasm bundle serves every thread; the active role is chosen
//! at runtime (the `wasm-bindgen-rayon` pattern):
//!
//! - **Main thread** ([`main_thread_boot`]): owns the DOM, spawns the
//!   worker(s), and posts each one the shared `WebAssembly.Module` +
//!   `WebAssembly.Memory` so they attach to the *same* linear memory.
//! - **Workers**: bootstrapped by [`crate::bootstrap::WORKER_BOOTSTRAP_JS`],
//!   which initialises the wasm against the shared memory and then calls
//!   a role-specific exported entry point.
//!
//! Each capability is a selectable `?demo=` mode (see the per-module docs and
//! the README); the shared-memory sim-state primitive itself lives in the
//! renderer crate (`awsm_renderer::buffer::shared_arena`).

pub mod arena_test;
pub mod bootstrap;
pub mod churn_demo;
pub mod crowd_demo;
pub mod input_demo;
pub mod lights_demo;
pub mod motion_demo;
pub mod protocol;
pub mod remote_demo;
pub mod render_demo;
pub mod scene_demo;
pub mod skin_demo;
pub mod smoke;
pub mod viewport;

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::js_sys;

/// `true` when running inside a `DedicatedWorkerGlobalScope`.
pub fn is_worker_scope() -> bool {
    js_sys::global()
        .dyn_into::<web_sys::DedicatedWorkerGlobalScope>()
        .is_ok()
}

/// Single entry point. `wasm-bindgen` runs this automatically on every
/// `init()` (main thread *and* every worker). On the main thread it
/// boots the app; in a worker it does nothing — the worker's real work
/// is triggered explicitly by the bootstrap JS calling a role entry
/// point *after* init returns (a worker can't `postMessage` to itself).
#[wasm_bindgen(start)]
pub fn boot() -> Result<(), JsValue> {
    install_tracing();
    if is_worker_scope() {
        Ok(())
    } else {
        main_thread_boot()
    }
}

/// Main-thread bootstrap. Selects a demo via the `?demo=` query param so
/// each milestone's gate stays independently runnable:
/// - `smoke` (M0): 2-worker shared-memory smoke.
/// - `arena` (M1, default): physics worker writes a ramp into the shared
///   arena at high rate; render worker reads with torn-read detection.
fn main_thread_boot() -> Result<(), JsValue> {
    tracing::info!("multithreaded example: main-thread boot");
    let isolated = crossorigin_isolated();
    let has_sab = shared_array_buffer_available();
    tracing::info!("crossOriginIsolated = {isolated}, SharedArrayBuffer = {has_sab}");
    if !isolated || !has_sab {
        tracing::error!(
            "cross-origin isolation is OFF — shared memory is unavailable. \
             Serve with COOP: same-origin + COEP: require-corp."
        );
    }
    match demo_param().as_str() {
        "smoke" => smoke::start_main(),
        "arena" => arena_test::start_main(),
        "render" => render_demo::start_main(),
        "motion" => motion_demo::start_main(),
        "crowd" => crowd_demo::start_main(),
        "churn" => churn_demo::start_main(),
        "lights" => lights_demo::start_main(),
        "skin" => skin_demo::start_main(),
        "scene" => scene_demo::start_main(),
        "remote" => remote_demo::start_main(),
        // Default: M6 input forwarding + main-thread responsiveness.
        _ => input_demo::start_main(),
    }
}

/// Read the `?demo=` query param (defaults to empty).
fn demo_param() -> String {
    web_sys::window()
        .and_then(|w| w.location().search().ok())
        .and_then(|s| web_sys::UrlSearchParams::new_with_str(&s).ok())
        .and_then(|p| p.get("demo"))
        .unwrap_or_default()
}

/// Install the browser-console tracing subscriber (idempotent — safe to
/// call on the main thread and in every worker).
pub fn install_tracing() {
    use tracing_subscriber::prelude::*;
    // The default `fmt` time formatter calls `SystemTime::now()`, which
    // panics on wasm32; `without_time` strips it (the browser console
    // prepends its own timestamp).
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .without_time()
        .with_writer(tracing_web::MakeWebConsoleWriter::new())
        .with_target(false);
    let _ = tracing_subscriber::registry().with(fmt_layer).try_init();
}

/// `globalThis.crossOriginIsolated` from whichever scope is active.
pub fn crossorigin_isolated() -> bool {
    js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("crossOriginIsolated"))
        .ok()
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// `typeof SharedArrayBuffer !== "undefined"` in the active scope.
pub fn shared_array_buffer_available() -> bool {
    js_sys::Reflect::has(&js_sys::global(), &JsValue::from_str("SharedArrayBuffer"))
        .unwrap_or(false)
}
