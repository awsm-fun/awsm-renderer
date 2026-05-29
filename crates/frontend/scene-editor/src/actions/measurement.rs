//! Dev-only programmatic scene loading + measurement helpers.
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
use web_sys::js_sys;

use crate::scene::SceneSnapshot;
use crate::state::app_state;

/// Programmatic scene loader. Fetches
/// `assets/world/<scene_name>/project.json` from the dev server,
/// drops prior renderer caches, applies the snapshot, and waits
/// for every Model node to materialise on the GPU.
///
/// Tuning scenes ship with empty `assets` + only `nodes`, so this
/// is the minimal subset of `crate::actions::project::load_inner`'s
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

    crate::scene::snapshot::apply_to(&snapshot, &state.scene, &state.custom_materials);
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

    // Pre-hydrate `pending_assets` with the bytes for every
    // `TextureDef::Raster` entry — mirrors `load_inner`'s same step
    // but pulls bytes via `fetch` from the dev server (no
    // `ProjectDir`). Tuning scenes have zero assets so this loop is
    // typically empty; the flipbook test scene relies on this path
    // to surface its sprite-sheet PNG to the texture cache.
    let raster_targets: Vec<(awsm_scene_schema::AssetId, String, String)> = {
        let table = state.scene.assets.lock().unwrap();
        table
            .entries
            .iter()
            .filter_map(|(id, entry)| match &entry.source {
                awsm_scene_schema::AssetSource::Texture(
                    awsm_scene_schema::TextureDef::Raster { display_name },
                ) => awsm_scene_schema::asset_disk_path(*id, entry)
                    .map(|p| (*id, display_name.clone(), p)),
                _ => None,
            })
            .collect()
    };
    for (texture_id, display_name, disk_subpath) in raster_targets {
        let asset_url = format!("assets/world/{scene_name}/{disk_subpath}");
        let resp_val = JsFuture::from(window.fetch_with_str(&asset_url)).await?;
        let resp: web_sys::Response = resp_val.dyn_into()?;
        if !resp.ok() {
            tracing::warn!(
                "load_scene_by_path: raster asset {texture_id} ({display_name}) — \
                 fetch {asset_url} returned status {}",
                resp.status()
            );
            continue;
        }
        let buf_val = JsFuture::from(resp.array_buffer()?).await?;
        let buf: js_sys::ArrayBuffer = buf_val.dyn_into()?;
        let bytes = js_sys::Uint8Array::new(&buf).to_vec();
        state
            .pending_assets
            .lock()
            .unwrap()
            .insert(texture_id, bytes);
    }

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

/// Sibling of [`load_scene_by_path`] that fetches from the
/// `awsm-renderer-assets` repo (port 9083 in dev,
/// `dakom.github.io/awsm-renderer-assets/` in prod) instead of the
/// editor's own dist. Used for test scenes that ship alongside the
/// canonical asset bundle rather than being baked into the editor.
///
/// The URL layout is `<MEDIA_BASE_URL_ADDITIONAL_ASSETS>/<scene_name>/project.json`
/// plus `<scene_name>/<disk_subpath>` for each raster texture entry.
/// `scene_name` is the top-level directory under the assets repo
/// (e.g. `"flipbook-test"`).
///
/// `#[cfg(debug_assertions)]` matches [`load_scene_by_path`] — the
/// helper is dev-only; production builds use the standard file-picker
/// `load_project` flow.
#[cfg(debug_assertions)]
#[wasm_bindgen]
pub async fn load_external_test_scene(scene_name: String) -> Result<(), JsValue> {
    let base = crate::config::CONFIG.media_base_url_additional_assets();
    // Cache-bust on every load: edits to the external project.json are
    // frequent during development and the browser HTTP cache otherwise
    // pins the editor to a stale fixture. Cheap for a dev-only helper.
    let cache_bust = web_sys::js_sys::Date::now() as u64;
    let project_url = format!("{base}/{scene_name}/project.json?_={cache_bust}");
    tracing::info!("measurement: loading external scene from {project_url}");

    let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
    let response_value = JsFuture::from(window.fetch_with_str(&project_url)).await?;
    let response: web_sys::Response = response_value.dyn_into()?;
    if !response.ok() {
        return Err(JsValue::from_str(&format!(
            "fetch {project_url} failed: status {}",
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

    crate::scene::snapshot::apply_to(&snapshot, &state.scene, &state.custom_materials);
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

    // Pre-hydrate `pending_assets` for raster textures by fetching them
    // off the same media base URL. Mirrors the local-dist path in
    // `load_scene_by_path` but resolves against the external server.
    let raster_targets: Vec<(awsm_scene_schema::AssetId, String, String)> = {
        let table = state.scene.assets.lock().unwrap();
        table
            .entries
            .iter()
            .filter_map(|(id, entry)| match &entry.source {
                awsm_scene_schema::AssetSource::Texture(
                    awsm_scene_schema::TextureDef::Raster { display_name },
                ) => awsm_scene_schema::asset_disk_path(*id, entry)
                    .map(|p| (*id, display_name.clone(), p)),
                _ => None,
            })
            .collect()
    };
    for (texture_id, display_name, disk_subpath) in raster_targets {
        let asset_url = format!("{base}/{scene_name}/{disk_subpath}?_={cache_bust}");
        let resp_val = JsFuture::from(window.fetch_with_str(&asset_url)).await?;
        let resp: web_sys::Response = resp_val.dyn_into()?;
        if !resp.ok() {
            tracing::warn!(
                "load_external_test_scene: raster asset {texture_id} ({display_name}) — \
                 fetch {asset_url} returned status {}",
                resp.status()
            );
            continue;
        }
        let buf_val = JsFuture::from(resp.array_buffer()?).await?;
        let buf: js_sys::ArrayBuffer = buf_val.dyn_into()?;
        let bytes = js_sys::Uint8Array::new(&buf).to_vec();
        state
            .pending_assets
            .lock()
            .unwrap()
            .insert(texture_id, bytes);
    }

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

    tracing::info!("measurement: external scene {scene_name} loaded + materialised");
    Ok(())
}

/// Importance-score histogram across all shadow-casting lights —
/// importance-tier cutoff tuning aid. Mirrors
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
        let matrices = r.camera.last_matrices.as_ref()?;
        // Match the importance-scoring camera_pos (translation column
        // of the inverse view). The earlier `.transpose()` here
        // mirrored a bug in shadows::importance that was fixed; this
        // measurement helper should compute the same value the
        // scoring path uses, otherwise its histogram doesn't reflect
        // what `refresh_light_importance_budgets` would actually
        // produce.
        let camera_pos = matrices.view.inverse().w_axis.truncate();
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
            // (re-tuned to 0.05 / 1.0 / 10.0 against the
            // tuning-importance-tiers scene).
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

/// Read the renderer's `MeshCoverage` state — GPU coverage producer
/// readback verification. Returns `{ "entries": N, "frame_when_populated": F,
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

/// Group every `performance.measure` entry by tracing-span base
/// name (the `[id]: span-measure` suffix that `tracing-web` appends
/// is stripped), then summarise per-pass timing as
/// `{ "count": N, "mean_ms": f, "p50_ms": f, "p95_ms": f,
///   "max_ms": f, "total_ms": f }`. Useful for the Chrome-vs-Safari
/// comparison the optimization sprint calls out: a single
/// `preview_eval` call returns a JSON map of per-pass distributions
/// without any client-side bucketing logic. Clears the entries
/// after sampling so the next call starts fresh.
///
/// Skips spans whose count is below `min_count` so rare one-shot
/// init spans (GLTF parse, texture upload) don't dominate the output
/// — pass e.g. 30 to focus on steady-state passes. The arg is
/// required (no Rust default on a `#[wasm_bindgen]` export); pass
/// `min_count = 0` to include everything.
#[wasm_bindgen]
pub fn read_render_pass_timings(min_count: u32) -> String {
    use js_sys::{Array, JsString, Reflect};
    use std::collections::BTreeMap;
    let window = match web_sys::window() {
        Some(w) => w,
        None => return "{\"error\":\"no window\"}".to_string(),
    };
    let perf = match window.performance() {
        Some(p) => p,
        None => return "{\"error\":\"no performance\"}".to_string(),
    };
    let entries: Array = perf.get_entries_by_type("measure");
    let mut buckets: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for entry in entries.iter() {
        let name = Reflect::get(&entry, &JsValue::from_str("name"))
            .ok()
            .and_then(|v| v.dyn_into::<JsString>().ok())
            .map(String::from)
            .unwrap_or_default();
        let dur = Reflect::get(&entry, &JsValue::from_str("duration"))
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        // `tracing-web` formats every span measure as
        // `"<base> [<id>]: span-measure"`. Strip the suffix so different
        // span ids for the same call site collapse into one bucket.
        // Match the full pattern (not just a bare `" ["`) so a
        // non-tracing measure whose name happens to contain `" ["` passes
        // through untouched.
        let base = strip_tracing_span_suffix(&name)
            .map(str::to_string)
            .unwrap_or(name);
        buckets.entry(base).or_default().push(dur);
    }
    let min_count = min_count as usize;
    let mut out = String::from("{");
    let mut first = true;
    for (name, mut samples) in buckets {
        if samples.len() < min_count.max(1) {
            continue;
        }
        samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let n = samples.len();
        let sum: f64 = samples.iter().sum();
        let mean = sum / n as f64;
        let p50 = samples[n / 2];
        let p95 = samples[((n as f64 * 0.95) as usize).min(n - 1)];
        let max = *samples.last().unwrap_or(&0.0);
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str(&format!(
            "{}:{{\"count\":{},\"mean_ms\":{:.4},\"p50_ms\":{:.4},\"p95_ms\":{:.4},\"max_ms\":{:.4},\"total_ms\":{:.4}}}",
            serde_json::to_string(&name).unwrap_or_else(|_| "\"\"".to_string()),
            n, mean, p50, p95, max, sum
        ));
    }
    out.push('}');
    // Clear so the next call observes a fresh window. `clear_measures`
    // also drops any `performance.mark` entries we never explicitly
    // emit, which is fine.
    perf.clear_measures();
    out
}

/// Strip the `tracing-web` span suffix from a `performance.measure`
/// name, returning the span's base name. `tracing-web` formats every
/// span measure as `"<base> [<id>]: span-measure"` (see its
/// `performance_layer`), so the suffix is matched in full: the name
/// must end with `"]: span-measure"` and contain the opening `" ["` of
/// the id group. Returns `None` for anything that doesn't match — a
/// manually-emitted measure whose name merely contains `" ["` is left
/// alone rather than truncated.
fn strip_tracing_span_suffix(name: &str) -> Option<&str> {
    // `<base> [<id>` — the numeric span id holds no `]` or `" ["`, so the
    // last `" ["` opens the id group.
    let head = name.strip_suffix("]: span-measure")?;
    let idx = head.rfind(" [")?;
    Some(&head[..idx])
}

/// Read the renderer's light-bucket telemetry.
/// Returns a JSON string `{ "last_max_bucket": N, "oversized_count": M }`
/// for the most-recently-rebuilt `LightMeshBuckets`. Drive from
/// preview_eval after loading `tuning-open-world` (or any authored
/// scene with terrain / ocean / skyboxes) to inform re-tuning of
/// `OVERSIZED_LIST_COUNT_THRESHOLD` (default 16) and
/// `OVERSIZED_AABB_DIAGONAL_METERS` (default 50.0). Returns JSON
/// (rather than a JS object) to dodge a `serde_wasm_bindgen`
/// dependency the editor doesn't otherwise carry; the caller does
/// `JSON.parse()`.
/// Phase-2.1 upload-ring telemetry. Returns a JSON object keyed by
/// renderer subsystem (`transforms`, `materials`, `instances.transforms`,
/// …) plus a `_total` rollup. Each entry exposes
/// `{ peak_ring_depth_used, fallback_count, map_async_wait_ms,
///    bytes_uploaded_via_ring, bytes_uploaded_via_fallback,
///    bytes_uploaded_via_writebuffer, resize_count }`.
///
/// Drive from `preview_eval` on `tuning-10k-meshes` to confirm
/// `_total.fallback_count == 0` in steady state — a non-zero count
/// means a buffer's ring depth is too shallow for its frame cadence
/// and should be bumped via the (not-yet-wired) `with_ring_depth`
/// constructor.
#[wasm_bindgen]
pub async fn read_upload_ring_stats() -> String {
    use std::fmt::Write;
    let buckets = crate::context::with_renderer(|r| r.upload_ring_stats()).await;

    let fmt_stats = |s: &awsm_renderer::buffer::mapped_staging_ring::UploadStats| -> String {
        format!(
            "{{\"peak_ring_depth_used\":{},\"fallback_count\":{},\
                 \"map_async_wait_ms\":{:.4},\
                 \"bytes_uploaded_via_ring\":{},\
                 \"bytes_uploaded_via_fallback\":{},\
                 \"bytes_uploaded_via_writebuffer\":{},\
                 \"resize_count\":{}}}",
            s.peak_ring_depth_used,
            s.fallback_count,
            s.map_async_wait_ms,
            s.bytes_uploaded_via_ring,
            s.bytes_uploaded_via_fallback,
            s.bytes_uploaded_via_writebuffer,
            s.resize_count,
        )
    };

    let mut total = awsm_renderer::buffer::mapped_staging_ring::UploadStats::default();
    let mut out = String::from("{");
    for (label, s) in &buckets {
        if out.len() > 1 {
            out.push(',');
        }
        let _ = write!(
            out,
            "{}:{}",
            serde_json::to_string(label).unwrap_or_else(|_| "\"\"".to_string()),
            fmt_stats(s)
        );
        // Roll up.
        total.peak_ring_depth_used = total.peak_ring_depth_used.max(s.peak_ring_depth_used);
        total.fallback_count += s.fallback_count;
        total.map_async_wait_ms += s.map_async_wait_ms;
        total.bytes_uploaded_via_ring += s.bytes_uploaded_via_ring;
        total.bytes_uploaded_via_fallback += s.bytes_uploaded_via_fallback;
        total.bytes_uploaded_via_writebuffer += s.bytes_uploaded_via_writebuffer;
        total.resize_count += s.resize_count;
    }
    if !buckets.is_empty() {
        out.push(',');
    }
    let _ = write!(out, "\"_total\":{}", fmt_stats(&total));
    out.push('}');
    out
}

/// Phase 4.3b A/B measurement — load `url` via both the inline
/// `GltfLoader::load` and the worker `GltfParseJob` (re-entered N
/// times each via the dev-only `?gltf-worker=on` URL knob's pool).
/// Returns JSON `{ "inline_ms": [...], "worker_ms": [...],
/// "inline_mean": f, "worker_mean": f, "speedup": f }` so the
/// driver can decide whether to flip
/// `asset_cache::load_and_populate`'s default.
///
/// Per the spec: a real measurement against `robot-001.glb`
/// drives the flip decision. A reasonable 12.8 MB substitute is
/// Corset.glb from the Khronos sample-assets repo —
/// `https://raw.githubusercontent.com/KhronosGroup/glTF-Sample-Assets/main/Models/Corset/glTF-Binary/Corset.glb`
/// — large enough to amortise worker spawn cost. Pass any URL
/// the browser can fetch; the harness doesn't care where the glb
/// lives, only that both load paths see the same bytes.
#[wasm_bindgen]
pub async fn measure_gltf_load_ab(url: String, iterations: u32) -> String {
    use awsm_renderer::workers::{WorkerPool, WorkerPoolBootstrap};
    use awsm_renderer_gltf::loader::{get_type_from_filename, GltfLoader};
    use awsm_renderer_gltf::worker_job::{FileTypeHint, GltfParseInput, GltfParseJob};

    // Guard zero-iteration calls upfront — the JSON-shape contract
    // promises means / speedup as numbers, and `0/0` would emit
    // `NaN` (not a valid JSON number; `serde_json` would refuse,
    // and a naive `format!("{nan}")` would emit `"NaN"` which any
    // strict consumer parser would reject). One iteration is the
    // minimum that makes the means well-defined.
    if iterations == 0 {
        return "{\"error\":\"iterations must be >= 1\"}".to_string();
    }

    let perf = match web_sys::window().and_then(|w| w.performance()) {
        Some(p) => p,
        None => return "{\"error\":\"no performance\"}".to_string(),
    };

    let file_type = get_type_from_filename(&url);

    // JSON-safe error formatter — the `{err}` string can contain
    // arbitrary characters (quotes, newlines, control bytes from a
    // network error or deserialise failure) that would corrupt a
    // naive `format!("{{\"error\":\"...{err}...\"}}")`. Route the
    // message through `serde_json::to_string` so the documented
    // `JSON.parse()` consumer always succeeds. Matches the pattern
    // already used by `debug_pick` further down this file.
    let err_json = |prefix: &str, err: &dyn std::fmt::Display| -> String {
        let msg = format!("{prefix}: {err}");
        let escaped =
            serde_json::to_string(&msg).unwrap_or_else(|_| "\"<unprintable error>\"".to_string());
        format!("{{\"error\":{escaped}}}")
    };

    // Build a dedicated pool for the measurement. Cheaper than
    // reusing the editor's pool (which the user might or might not
    // have enabled via `?gltf-worker=on`) — guarantees a clean
    // measurement either way.
    let pool = match WorkerPool::new(WorkerPoolBootstrap::Auto, 2).await {
        Ok(p) => p,
        Err(err) => return err_json("pool", &err),
    };
    pool.register::<GltfParseJob>();

    let mut inline_ms: Vec<f64> = Vec::with_capacity(iterations as usize);
    let mut worker_ms: Vec<f64> = Vec::with_capacity(iterations as usize);

    for _ in 0..iterations {
        let start = perf.now();
        match GltfLoader::load(&url, file_type.as_ref().map(file_type_clone)).await {
            Ok(_) => {}
            Err(err) => return err_json("inline", &err),
        }
        inline_ms.push(perf.now() - start);
    }

    for _ in 0..iterations {
        let start = perf.now();
        let input = GltfParseInput {
            url: url.clone(),
            file_type: file_type.as_ref().map(FileTypeHint::from),
        };
        match pool.dispatch::<GltfParseJob>(input).await {
            Ok(out) => match out.into_loader().await {
                Ok(_) => {}
                Err(err) => return err_json("worker into_loader", &err),
            },
            Err(err) => return err_json("worker dispatch", &err),
        }
        worker_ms.push(perf.now() - start);
    }

    let inline_mean: f64 = inline_ms.iter().sum::<f64>() / inline_ms.len() as f64;
    let worker_mean: f64 = worker_ms.iter().sum::<f64>() / worker_ms.len() as f64;
    // `speedup` can become non-finite when `worker_mean` is 0.0
    // (possible if `performance.now()` returns 0 ms deltas on a
    // cached / sub-millisecond run) — `NaN`/`Inf` aren't valid JSON
    // numbers and would corrupt the `JSON.parse()` consumer. Same
    // guard applied defensively to the means so any future edge case
    // (empty arrays, etc.) can't break the output shape either; the
    // `num_or_null` helper emits the JSON literal `null` for any
    // non-finite value.
    let speedup = if worker_mean > 0.0 {
        inline_mean / worker_mean
    } else {
        f64::NAN
    };

    let num_or_null = |v: f64, decimals: usize| -> String {
        if v.is_finite() {
            format!("{v:.*}", decimals)
        } else {
            "null".to_string()
        }
    };
    let fmt_arr = |arr: &[f64]| -> String {
        let parts: Vec<String> = arr.iter().map(|v| num_or_null(*v, 2)).collect();
        format!("[{}]", parts.join(","))
    };

    format!(
        "{{\"url\":{url_json},\"iterations\":{iterations},\
         \"inline_ms\":{inline_arr},\"worker_ms\":{worker_arr},\
         \"inline_mean\":{inline_mean},\"worker_mean\":{worker_mean},\
         \"speedup\":{speedup}}}",
        url_json = serde_json::to_string(&url).unwrap_or_else(|_| "\"\"".to_string()),
        inline_arr = fmt_arr(&inline_ms),
        worker_arr = fmt_arr(&worker_ms),
        inline_mean = num_or_null(inline_mean, 2),
        worker_mean = num_or_null(worker_mean, 2),
        speedup = num_or_null(speedup, 3),
    )
}

// `GltfFileType` doesn't implement Clone, so we hand-roll one for the
// measurement loop that needs to pass the same hint into both paths.
fn file_type_clone(
    t: &awsm_renderer_gltf::loader::GltfFileType,
) -> awsm_renderer_gltf::loader::GltfFileType {
    use awsm_renderer_gltf::loader::GltfFileType::*;
    match t {
        Json => Json,
        Glb => Glb,
        Draco => Draco,
    }
}

/// Dev-only programmatic Insert-Model that fetches a glb from `url`,
/// synthesises a `File` from the bytes, and drives the normal
/// `actions::insert::model` flow. Lets the smoke harness exercise
/// the texture-upload + populate path without a file picker.
#[wasm_bindgen]
pub async fn insert_model_from_url(url: String, filename: String) -> Result<(), JsValue> {
    use js_sys::Uint8Array;
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;
    let resp_promise = web_sys::window()
        .ok_or_else(|| JsValue::from_str("no window"))?
        .fetch_with_str(&url);
    let resp: web_sys::Response = JsFuture::from(resp_promise).await?.unchecked_into();
    if !resp.ok() {
        return Err(JsValue::from_str(&format!(
            "fetch {url}: HTTP {}",
            resp.status()
        )));
    }
    let buffer = JsFuture::from(resp.array_buffer()?).await?;
    let buffer: js_sys::ArrayBuffer = buffer.unchecked_into();
    let bytes_array = Uint8Array::new(&buffer);
    let parts = js_sys::Array::new();
    parts.push(&bytes_array);
    // `web_sys::File::new_with_buffer_source_sequence_and_options`
    // isn't generated by web-sys 0.3.95; synthesise the File via the
    // JS-native `new File([buf], name, { type })` constructor.
    let global = js_sys::global();
    let file_ctor = js_sys::Reflect::get(&global, &JsValue::from_str("File"))?;
    let opts = js_sys::Object::new();
    js_sys::Reflect::set(
        &opts,
        &JsValue::from_str("type"),
        &JsValue::from_str("model/gltf-binary"),
    )?;
    let args = js_sys::Array::new();
    args.push(&parts.into());
    args.push(&JsValue::from_str(&filename));
    args.push(&opts);
    let file_ctor_fn: js_sys::Function = file_ctor.unchecked_into();
    let file_val = js_sys::Reflect::construct(&file_ctor_fn, &args)?;
    let file: web_sys::File = file_val.unchecked_into();
    crate::actions::insert::model(file);
    Ok(())
}

/// Per-mesh AABB dump (world-space). Lets the smoke harness verify
/// where inserted models land, what their bounds look like, and
/// whether the editor camera is even pointed at them.
#[wasm_bindgen]
pub async fn read_mesh_aabbs_debug() -> String {
    crate::context::with_renderer(|r| {
        let mut out = String::from("[");
        let mut first = true;
        for (mk, mesh) in r.meshes.iter() {
            if mesh.hidden || mesh.hud {
                continue;
            }
            let aabb_str = match &mesh.world_aabb {
                Some(aabb) => format!(
                    "{{\"min\":[{:.2},{:.2},{:.2}],\"max\":[{:.2},{:.2},{:.2}]}}",
                    aabb.min.x, aabb.min.y, aabb.min.z, aabb.max.x, aabb.max.y, aabb.max.z
                ),
                None => "null".to_string(),
            };
            if !first {
                out.push(',');
            }
            first = false;
            out.push_str(&format!("{{\"mesh\":{mk:?},\"aabb\":{aabb_str}}}"));
        }
        // Also dump camera position + view direction so we can tell
        // whether the model sits inside the frustum.
        let cam = match r.camera.last_matrices.as_ref() {
            Some(m) => {
                let pos = m.view.inverse().w_axis.truncate();
                format!(
                    "{{\"pos\":[{:.2},{:.2},{:.2}],\"focus\":{:.2},\"aperture\":{:.2}}}",
                    pos.x, pos.y, pos.z, m.focus_distance, m.aperture
                )
            }
            None => "null".to_string(),
        };
        format!("{{\"meshes\":{out}],\"camera\":{cam}}}")
    })
    .await
}

/// Per-mesh debug dump: lists every non-HUD visible mesh with its
/// material's PBR texture bindings (or `false` when missing). Lets
/// the smoke harness verify "texture was hooked up to the material"
/// after inserting a glb.
#[wasm_bindgen]
pub async fn read_mesh_materials_debug() -> String {
    use awsm_renderer::materials::Material;
    crate::context::with_renderer(|r| {
        let mut out = String::from("[");
        let mut first = true;
        for (mk, mesh) in r.meshes.iter() {
            if mesh.hidden || mesh.hud {
                continue;
            }
            let material_key = mesh.material_key;
            let mat_info = match r.materials.get(material_key) {
                Ok(Material::Pbr(pbr)) => format!(
                    "{{\"bcf\":[{:.2},{:.2},{:.2},{:.2}],\"bc_tex\":{},\"mr_tex\":{},\"normal_tex\":{},\"occlusion_tex\":{},\"emissive_tex\":{}}}",
                    pbr.base_color_factor[0],
                    pbr.base_color_factor[1],
                    pbr.base_color_factor[2],
                    pbr.base_color_factor[3],
                    pbr.base_color_tex.is_some(),
                    pbr.metallic_roughness_tex.is_some(),
                    pbr.normal_tex.is_some(),
                    pbr.occlusion_tex.is_some(),
                    pbr.emissive_tex.is_some(),
                ),
                Ok(_) => "\"non-pbr\"".to_string(),
                Err(_) => "\"missing\"".to_string(),
            };
            if !first {
                out.push(',');
            }
            first = false;
            out.push_str(&format!(
                "{{\"mesh\":{mk:?},\"material\":{material_key:?},\"info\":{mat_info}}}"
            ));
        }
        out.push(']');
        out
    })
    .await
}

/// Dev-only renderer-state probe. Returns mesh / material / scene
/// counts as JSON so a regression like "model inserted but never
/// renders" can be triaged without a debugger session.
#[wasm_bindgen]
pub async fn read_mesh_debug_stats() -> String {
    crate::context::with_renderer(|r| {
        let mesh_count = r.meshes.len();
        let mut visible_meshes = 0usize;
        let mut hidden_meshes = 0usize;
        let mut with_aabb = 0usize;
        let mut without_aabb = 0usize;
        let mut hud_meshes = 0usize;
        for (_, m) in r.meshes.iter() {
            if m.hidden {
                hidden_meshes += 1;
            } else {
                visible_meshes += 1;
            }
            if m.world_aabb.is_some() {
                with_aabb += 1;
            } else {
                without_aabb += 1;
            }
            if m.hud {
                hud_meshes += 1;
            }
        }
        let transforms_root = r.transforms.root_node;
        let material_count = r.materials.keys().count();
        let scene_spatial_count = r.scene_spatial.len();
        format!(
            "{{\"mesh_count\":{mesh_count},\
             \"visible_meshes\":{visible_meshes},\
             \"hidden_meshes\":{hidden_meshes},\
             \"with_aabb\":{with_aabb},\
             \"without_aabb\":{without_aabb},\
             \"hud_meshes\":{hud_meshes},\
             \"material_count\":{material_count},\
             \"scene_spatial_count\":{scene_spatial_count},\
             \"transforms_root\":{:?}}}",
            transforms_root
        )
    })
    .await
}

/// Diagnose texture binding correctness. For each visible non-HUD mesh
/// whose material is PBR, dumps every texture binding's sampler-index
/// resolution. A sampler that's not in `pool_sampler_set` returns
/// `null` — that's the symptom of the "all-white" override bug.
#[wasm_bindgen]
pub async fn read_material_sampler_diag() -> String {
    use awsm_renderer::materials::Material;
    use awsm_renderer::materials::TextureContext as _;
    crate::context::with_renderer(|r| {
        let mut out = String::from("[");
        let mut first = true;
        for (mk, mesh) in r.meshes.iter() {
            if mesh.hidden || mesh.hud {
                continue;
            }
            let Ok(Material::Pbr(pbr)) = r.materials.get(mesh.material_key) else {
                continue;
            };
            let probe = |label: &str,
                         t: &Option<awsm_renderer::materials::MaterialTexture>,
                         buf: &mut String| {
                let Some(mt) = t.as_ref() else {
                    buf.push_str(&format!("\"{label}\":\"none\","));
                    return;
                };
                let key_idx = format!("{:?}", mt.key);
                let sk = mt.sampler_key;
                let sampler_index = sk.and_then(|s| r.textures.sampler_index(s));
                let entry = r.textures.get_entry(mt.key).ok().map(|e| {
                    format!(
                        "{{\"array\":{},\"layer\":{},\"srgb_to_linear\":{}}}",
                        e.array_index, e.layer_index, e.color.srgb_to_linear
                    )
                });
                buf.push_str(&format!(
                    "\"{label}\":{{\"key\":\"{key_idx}\",\"sampler_index\":{},\"uv\":{},\"entry\":{}}},",
                    sampler_index
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "null".to_string()),
                    mt.uv_index
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "null".to_string()),
                    entry.unwrap_or_else(|| "null".to_string()),
                ));
            };
            let mut row = String::new();
            probe("bc", &pbr.base_color_tex, &mut row);
            probe("mr", &pbr.metallic_roughness_tex, &mut row);
            probe("normal", &pbr.normal_tex, &mut row);
            probe("ao", &pbr.occlusion_tex, &mut row);
            probe("emissive", &pbr.emissive_tex, &mut row);
            if row.ends_with(',') {
                row.pop();
            }
            if !first {
                out.push(',');
            }
            first = false;
            out.push_str(&format!("{{\"mesh\":\"{mk:?}\",{}}}", row));
        }
        out.push(']');
        out
    })
    .await
}

/// Dev-only: dump the live light-culling inputs (camera matrices,
/// viewport/tile grid, slice count, and every punctual light's world
/// position + range) so an offline simulation can reproduce the cull
/// math against ground-truth runtime data. JSON shape:
/// `{ "proj":[16], "view":[16], "viewport":[w,h], "tiles_x":N,
///    "tiles_y":M, "slice_count":S, "max_cap":C,
///    "lights":[{"pos":[x,y,z],"range":r}, ...] }`.
#[wasm_bindgen]
pub async fn debug_dump_cull_state() -> String {
    use awsm_renderer::lights::Light;
    crate::context::with_renderer(|r| {
        let m = match r.camera.last_matrices.as_ref() {
            Some(m) => m,
            None => return "{\"error\":\"no camera matrices\"}".to_string(),
        };
        let proj = m.projection.to_cols_array();
        let view = m.view.to_cols_array();
        let b = &r.light_culling_buffers;
        let arr = |a: &[f32]| {
            a.iter()
                .map(|v| format!("{v}"))
                .collect::<Vec<_>>()
                .join(",")
        };
        let mut lights = String::from("[");
        let mut first = true;
        for (_k, light) in r.lights.iter() {
            if let Light::Point {
                position, range, ..
            }
            | Light::Spot {
                position, range, ..
            } = light
            {
                if !first {
                    lights.push(',');
                }
                first = false;
                lights.push_str(&format!(
                    "{{\"pos\":[{},{},{}],\"range\":{}}}",
                    position[0], position[1], position[2], range
                ));
            }
        }
        lights.push(']');
        format!(
            "{{\"proj\":[{}],\"view\":[{}],\"viewport\":[{},{}],\"tiles_x\":{},\"tiles_y\":{},\"slice_count\":{},\"max_cap\":{},\"lights\":{}}}",
            arr(&proj),
            arr(&view),
            b.viewport_w,
            b.viewport_h,
            b.tiles_x(),
            b.tiles_y(),
            b.slice_count,
            b.max_per_froxel_capacity,
            lights
        )
    })
    .await
}

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

/// Dev-only: drive the renderer's GPU pick at the given canvas-local
/// pixel coordinates and return what it found. Lets a JS-side test
/// harness check picker correctness without dispatching a synthetic
/// pointerdown (which spawns work asynchronously and is hard to
/// observe). Reports the picked mesh key + whether it matches one of
/// the active gizmo handles.
///
/// Result shape:
/// ```json
/// {"result":"hit","mesh_key":"MeshKey(NvN)","is_gizmo":true}
/// {"result":"miss"}
/// {"result":"initializing"}
/// {"result":"in_flight"}
/// {"result":"error","message":"..."}
/// ```
///
/// `is_gizmo` is the only gizmo-related field — the controller's
/// `is_gizmo_mesh_key` returns a `bool`, not a `GizmoKind`. If a
/// future caller needs the specific handle (TranslationX vs
/// RotationY, etc.) the controller would need a
/// `get_gizmo_mesh_kind(mesh_key) -> Option<GizmoKind>` companion;
/// add the JSON field at the same time as that helper.
#[wasm_bindgen]
pub async fn debug_pick(x: i32, y: i32) -> String {
    use awsm_renderer::picker::PickResult;
    let handle = crate::context::renderer_handle();
    let pick_result = {
        let mut renderer = handle.lock().await;
        renderer.pick(x, y).await
    };
    match pick_result {
        Ok(PickResult::Hit(mesh_key)) => {
            // Cross-check against the live gizmo handles so a JS caller
            // can immediately see if a "Hit" landed on a gizmo mesh.
            let state = app_state();
            let controller_lock = state.transform_controller.lock().unwrap();
            let is_gizmo = controller_lock
                .as_ref()
                .map(|c| c.is_gizmo_mesh_key(mesh_key))
                .unwrap_or(false);
            format!("{{\"result\":\"hit\",\"mesh_key\":\"{mesh_key:?}\",\"is_gizmo\":{is_gizmo}}}")
        }
        Ok(PickResult::Miss) => "{\"result\":\"miss\"}".to_string(),
        Ok(PickResult::Initializing) => "{\"result\":\"initializing\"}".to_string(),
        Ok(PickResult::InFlight) => "{\"result\":\"in_flight\"}".to_string(),
        Ok(PickResult::Disabled) => "{\"result\":\"disabled\"}".to_string(),
        Err(err) => format!(
            "{{\"result\":\"error\",\"message\":{}}}",
            serde_json::to_string(&err.to_string()).unwrap_or_else(|_| "\"\"".to_string())
        ),
    }
}

/// Dev-only: dump the live `TransformController`'s gizmo mesh-key
/// table so a JS caller can confirm the keys the controller is
/// comparing against in `get_gizmo_mesh_kind`. Mostly useful for
/// catching cases where the controller was built against keys that
/// no longer exist in the renderer (e.g. a re-populate that
/// invalidated the gltf load).
#[wasm_bindgen]
pub fn debug_gizmo_mesh_keys() -> String {
    let state = app_state();
    let controller_lock = state.transform_controller.lock().unwrap();
    let Some(c) = controller_lock.as_ref() else {
        return "{\"available\":false}".to_string();
    };
    let keys = c.gizmo_mesh_keys_debug();
    format!(
        "{{\"available\":true,\"selected_object\":{},\"keys\":{}}}",
        c.selected_object
            .map(|o| format!("{:?}", o.key))
            .map(|s| serde_json::to_string(&s).unwrap_or_else(|_| "\"\"".to_string()))
            .unwrap_or_else(|| "null".to_string()),
        keys
    )
}
