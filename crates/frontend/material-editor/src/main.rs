//! Material-editor — standalone authoring tool for runtime-registered
//! custom materials.
//!
//! See `docs/plans/dynamic-materials.md` for the architectural rationale.
//! UI is built with dominator + futures-signals, mirroring scene-editor.
//!
//! ## Phase 8 status
//!
//! Phase 8 ships the crate scaffolding: minimal main + state + a hard-
//! coded "scanline" example material loaded as the initial edit state.
//! No live preview or recompile yet (Phase 9). Definition pane is
//! Phase 10. Errors + contract pane Phase 11.

mod app;
mod panes;
mod recompile;
mod state;

use wasm_bindgen_futures::spawn_local;

fn main() {
    awsm_web_shared::logger::init_logger();
    spawn_local(async {
        if let Err(err) = run().await {
            tracing::error!("[material-editor] init failed: {err:?}");
        }
    });
}

async fn run() -> anyhow::Result<()> {
    let body = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.body())
        .ok_or_else(|| anyhow::anyhow!("no <body>"))?;
    dominator::append_dom(&body, app::root());
    Ok(())
}
