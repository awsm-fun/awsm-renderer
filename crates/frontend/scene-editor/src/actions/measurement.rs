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

/// Importance-score histogram across all shadow-casting lights —
/// plan §15 row T3 tuning aid. Mirrors
/// `shadows::importance::light_importance_decision`'s scoring
/// (`intensity / (1 + dist²)`) so the cutoff buckets can be
/// observed against any loaded scene without depending on the
/// renderer's persisted tier state. Output JSON shape:
/// ```json
/// { "low": N, "medium": M, "high": H, "ultra": U, "total": T,
///   "min_score": f, "max_score": f, "directional_or_no_aabb": K,
///   "scores": [s0, s1, ...] }
/// ```
/// `directional_or_no_aabb` counts lights pinned to High by the
/// directional fast-path or lights with no world AABB (drop to
/// Low). `scores` is the raw per-light score array so the caller
/// can plot distributions / pick new cutoffs offline.
#[wasm_bindgen]
pub async fn read_importance_tier_histogram() -> String {
    use awsm_renderer::lights::Light;
    let result = crate::context::with_renderer_mut(|r| {
        let Some(matrices) = r.camera.last_matrices.as_ref() else {
            return None;
        };
        let camera_pos = matrices.view.inverse().transpose().w_axis.truncate();
        let frustum =
            awsm_renderer::frustum::Frustum::from_view_projection(matrices.view_projection());
        let mut low = 0u32;
        let mut medium = 0u32;
        let mut high = 0u32;
        let mut ultra = 0u32;
        let mut directional_or_no_aabb = 0u32;
        let mut scores: Vec<f32> = Vec::new();
        for (light_key, light) in r.lights.iter() {
            let casts = r
                .shadows
                .light_params(light_key)
                .map(|p| p.cast)
                .unwrap_or(false);
            if !casts {
                continue;
            }
            if matches!(light, Light::Directional { .. }) {
                directional_or_no_aabb += 1;
                continue;
            }
            let Some(aabb) = light.world_aabb() else {
                directional_or_no_aabb += 1;
                continue;
            };
            if !frustum.intersects_aabb(&aabb) {
                low += 1;
                scores.push(0.0);
                continue;
            }
            let (position, intensity) = match light {
                Light::Point {
                    position,
                    intensity,
                    ..
                }
                | Light::Spot {
                    position,
                    intensity,
                    ..
                } => (glam::Vec3::from(*position), *intensity),
                Light::Directional { .. } => unreachable!(),
            };
            let dist_sq = (position - camera_pos).length_squared().max(0.001);
            let score = intensity / (1.0 + dist_sq);
            scores.push(score);
            // Mirrors the live cutoffs in
            // `shadows::importance::light_importance_decision`
            // (re-tuned to 0.05 / 1.0 / 10.0 against this scene —
            // see §15 row T3).
            if score > 10.0 {
                ultra += 1;
            } else if score > 1.0 {
                high += 1;
            } else if score > 0.05 {
                medium += 1;
            } else {
                low += 1;
            }
        }
        Some((low, medium, high, ultra, directional_or_no_aabb, scores))
    })
    .await;
    let Some((low, medium, high, ultra, dir_or_no, scores)) = result else {
        return "{\"error\":\"no camera matrices yet\"}".to_string();
    };
    let total = low + medium + high + ultra;
    let min_score = scores.iter().copied().fold(f32::INFINITY, f32::min);
    let max_score = scores.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let scores_str = scores
        .iter()
        .map(|s| format!("{s:.4}"))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"low\":{},\"medium\":{},\"high\":{},\"ultra\":{},\"directional_or_no_aabb\":{},\
         \"total\":{},\"min_score\":{},\"max_score\":{},\"scores\":[{}]}}",
        low, medium, high, ultra, dir_or_no, total, min_score, max_score, scores_str
    )
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
    let (last_max_bucket, oversized_count) = crate::context::with_renderer_mut(|r| {
        (
            r.light_buckets.last_max_bucket(),
            r.light_buckets.oversized_meshes().len(),
        )
    })
    .await;
    format!("{{\"last_max_bucket\":{last_max_bucket},\"oversized_count\":{oversized_count}}}")
}
