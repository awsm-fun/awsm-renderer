//! Dev-only programmatic scene loading + measurement helpers
//! (plan §16.G).
//!
//! Hidden behind `#[cfg(debug_assertions)]` — release builds never
//! see this module. Used by an external driver (Claude Preview MCP
//! + `preview_eval`) to:
//!   1. Boot the editor with `?features=off|on` to A/B the
//!      always-on infra cost.
//!   2. Call `window.wasmBindings.load_scene_by_path("tuning-1k-meshes")`
//!      to materialise a tuning scene without opening the
//!      directory picker.
//!   3. Wait ~60 frames + `setTimeout(2000)` for the scene to
//!      settle (gltf-style materialise / pipeline warmup).
//!   4. Read tracing-span timings via
//!      `performance.getEntriesByType('measure')` — the
//!      `tracing-web::performance_layer` already routes every
//!      render-pass span through the browser's Performance API,
//!      so no separate JSON harness is needed.

use std::sync::Arc;

use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::scene::SceneSnapshot;
use crate::state::app_state;

/// Programmatic scene loader. Fetches
/// `assets/world/<scene_name>/project.json` from the dev server,
/// drops prior renderer caches, applies the snapshot, and waits
/// for every Model node to materialise on the GPU.
///
/// Tuning scenes ship with empty `assets` + only `nodes`, so this
/// is the minimal subset of [`crate::actions::project::load_inner`]'s
/// behaviour — no glTF material extraction, no raster texture
/// staging. If the tuning scene set grows to include external
/// assets, mirror the relevant `load_inner` blocks here.
///
/// Resolves once the scene is on the GPU (or the materialise
/// timeout fires). Rejects with an error string on fetch / parse
/// / materialise failure.
#[wasm_bindgen]
pub async fn load_scene_by_path(scene_name: String) -> Result<(), JsValue> {
    let path = format!("assets/world/{scene_name}/project.json");
    tracing::info!("measurement: loading scene from {path}");

    let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
    let response_value = JsFuture::from(window.fetch_with_str(&path)).await?;
    let response: web_sys::Response = response_value.dyn_into()?;
    if !response.ok() {
        return Err(JsValue::from_str(&format!(
            "fetch {path} failed: status {}",
            response.status()
        )));
    }
    let text_value = JsFuture::from(response.text()?).await?;
    let text = text_value
        .as_string()
        .ok_or_else(|| JsValue::from_str("response.text() not a string"))?;

    let snapshot: SceneSnapshot = serde_json::from_str(&text)
        .map_err(|e| JsValue::from_str(&format!("parse project.json: {e}")))?;

    let state = app_state();
    crate::renderer_bridge::mesh_cache::clear();
    super::project::drop_renderer_caches().await;
    state.pending_assets.lock().unwrap().clear();

    crate::scene::snapshot::apply_to(&snapshot, &state.scene);
    state.scene.bump_revision();
    state.clear_selection();
    state.history.lock().unwrap().clear();
    state.refresh_history_signals();
    {
        let mut project = state.project.lock().unwrap();
        project.directory = None;
        project.dirty = false;
    }
    state.project_name.set(Some(scene_name.clone()));
    state.mark_clean();

    // Wait for every Model node to reach Ready. Tuning scenes use
    // only primitive nodes (no glTF) so this should resolve fast,
    // but the helper still uses the editor's standard 60s timeout
    // path so the contract matches `load_inner`'s.
    let roots: Vec<Arc<crate::scene::Node>> =
        state.scene.nodes.lock_ref().iter().cloned().collect();
    let outcome = crate::loading_modal::wait_for_models_ready(&roots).await;
    if !outcome.is_clean() {
        return Err(JsValue::from_str(&format!(
            "materialise incomplete: {} failure(s), timed_out={}",
            outcome.failures.len(),
            outcome.timed_out
        )));
    }

    tracing::info!("measurement: scene {scene_name} loaded + materialised");
    Ok(())
}

/// Read the renderer's `MeshCoverage` state — plan §8.2 readback
/// verification. Returns `{ "entries": N, "frame_when_populated": F,
/// "min": M, "max": X, "nonzero": K }` so the measurement harness
/// can confirm the GPU coverage producer actually wired its
/// counts back into the CPU table.
#[wasm_bindgen]
pub async fn read_mesh_coverage_stats() -> String {
    let stats = crate::context::with_renderer_mut(|r| {
        let entries = r.coverage.len();
        let frame = r.coverage.frame_when_populated();
        let mut min: u32 = u32::MAX;
        let mut max: u32 = 0;
        let mut nonzero: u32 = 0;
        for (key, _) in r.meshes.iter() {
            if let Some(c) = r.coverage.pixel_count(key) {
                if c < min {
                    min = c;
                }
                if c > max {
                    max = c;
                }
                if c > 0 {
                    nonzero += 1;
                }
            }
        }
        let min_out = if min == u32::MAX { 0 } else { min };
        (entries, frame, min_out, max, nonzero)
    })
    .await;
    format!(
        "{{\"entries\":{},\"frame_when_populated\":{},\"min\":{},\"max\":{},\"nonzero\":{}}}",
        stats.0, stats.1, stats.2, stats.3, stats.4
    )
}

/// Read the renderer's light-bucket telemetry — plan §15 row T6.
/// Returns a JSON string `{ "last_max_bucket": N, "oversized_count": M }`
/// for the most-recently-rebuilt `LightMeshBuckets`. Drive from
/// preview_eval after loading `tuning-open-world` (or any authored
/// scene with terrain / ocean / skyboxes) to inform re-tuning of
/// `OVERSIZED_LIST_COUNT_THRESHOLD` (default 16) and
/// `OVERSIZED_AABB_DIAGONAL_METERS` (default 50.0). Returns JSON
/// (rather than a JS object) to dodge a `serde_wasm_bindgen`
/// dependency the editor doesn't otherwise carry; the caller does
/// `JSON.parse()`.
#[wasm_bindgen]
pub async fn read_oversized_mesh_stats() -> String {
    let (last_max_bucket, oversized_count) =
        crate::context::with_renderer_mut(|r| {
            (
                r.light_buckets.last_max_bucket(),
                r.light_buckets.oversized_meshes().len(),
            )
        })
        .await;
    format!(
        "{{\"last_max_bucket\":{last_max_bucket},\"oversized_count\":{oversized_count}}}"
    )
}
