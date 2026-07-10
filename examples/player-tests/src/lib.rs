//! Player-runtime test harness (docs/plans/007-player-tests.md).
//!
//! Loads the baked test-scene bundles (`examples/test-scenes/<scene>/bundle`,
//! served by `task test-scenes` on :9084) through the **player** consumption
//! path — `awsm_renderer_scene_loader::load_scene_for_player` over an
//! [`HttpAssets`] source, exactly the shape a shipped web game uses — and runs
//! scripted checks, printing one machine-readable line per check to the
//! browser console:
//!
//! ```text
//! PLAYER-TEST <name>: PASS — <detail>
//! PLAYER-TEST <name>: FAIL — <detail>
//! …
//! PLAYER-TESTS COMPLETE: <pass>/<total>
//! ```
//!
//! Unlike `examples/multithreaded` (the threaded reference app) this harness
//! embeds the renderer **single-threaded on the main thread** — the same shape
//! as the editor / model-tests frontends, built on the stable default
//! toolchain. That is deliberate: no COOP/COEP isolation is required, so the
//! cross-origin bundle fetches from :9084 work with plain CORS.
//!
//! URL params:
//! - `?bundles=<origin>` — bundle server origin (default `http://localhost:9084`).
//! - `?scenes=a,b,c` — run only these scenes' per-scene checks.
//! - `?stream` / `?streambudget=N` — cluster-streaming flags for `lod-nanite`,
//!   mirroring the editor's flags; they feed
//!   `RendererFeatures::cluster_streaming_budget` (the loader's budget hook).

mod checks;
mod harness;
mod report;

use wasm_bindgen::prelude::*;

#[wasm_bindgen(start)]
pub fn boot() -> Result<(), JsValue> {
    install_tracing();
    std::panic::set_hook(Box::new(|info| {
        // A panic must still produce a parseable failure line + terminator so a
        // headless driver never hangs waiting for COMPLETE.
        web_sys::console::error_1(&format!("PLAYER-TEST panic: FAIL — {info}").into());
        web_sys::console::log_1(&"PLAYER-TESTS COMPLETE: aborted (panic)".into());
    }));
    wasm_bindgen_futures::spawn_local(async {
        checks::run_all().await;
    });
    Ok(())
}

/// Browser-console tracing subscriber (renderer `tracing::info!` lands in the
/// browser console — same setup as the multithreaded example).
fn install_tracing() {
    use tracing_subscriber::prelude::*;
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .without_time()
        .with_writer(tracing_web::MakeWebConsoleWriter::new())
        .with_target(false);
    let _ = tracing_subscriber::registry().with(fmt_layer).try_init();
}
