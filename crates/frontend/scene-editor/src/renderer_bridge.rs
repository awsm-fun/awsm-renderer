//! Scene ↔ renderer bridge.
//!
//! The editor's scene graph (`Mutable`-driven, reactive) feeds the
//! `awsm_renderer` side via explicit bookkeeping. Each scene `Node` owns
//! a `RendererNode` that holds the corresponding `TransformKey` plus a
//! bag of observer tasks; dropping the `RendererNode` cancels the tasks
//! and releases the renderer resources.

#![allow(clippy::arc_with_non_send_sync)]

pub mod asset_cache;
pub mod camera_driver;
pub mod dynamic_material_bridge;
pub mod env_sync;
pub mod gizmo;
pub mod instance_batcher;
pub mod material_cache;
pub mod mesh_cache;
pub mod node_sync;
pub mod particles_sync;
pub mod point_handle_sync;
pub mod procedural_sync;
pub mod shadows_sync;
pub mod texture_cache;

use crate::collider_wireframe::sync_editor_wireframes;
use crate::context::{
    camera_handle, compile_last_error_handle, compile_pending_handle, render_hooks_handle,
    renderer_handle, set_raf, set_render_hooks,
};
use crate::prelude::*;
use awsm_renderer::render::RenderHooks;
use awsm_renderer_editor::grid::{pipelines::EditorPipelines, render::render_grid};
use std::sync::atomic::{AtomicUsize, Ordering};
use wasm_bindgen_futures::spawn_local;

pub use node_sync::{bridge, Bridge, RendererNode};

/// Called once after `context::create_context` succeeds. Starts:
/// 1. The top-level scene→renderer children observer.
/// 2. The scene.environment → renderer observer (Skybox / IBL).
/// 3. Grid + collision wireframe pipelines, wired into `RenderHooks`.
/// 4. The per-frame render loop (requestAnimationFrame).
pub fn init() {
    node_sync::start_top_level_observer();
    env_sync::start();
    shadows_sync::start();
    gizmo::init();
    point_handle_sync::start();
    spawn_local(async {
        if let Err(err) = setup_render_hooks().await {
            tracing::error!("setup_render_hooks failed: {err:?}");
        }
    });
    start_render_loop();
}

async fn setup_render_hooks() -> anyhow::Result<()> {
    let handle = renderer_handle();
    let mut renderer = handle.lock().await;
    let editor_pipelines = EditorPipelines::load(&mut renderer)
        .await
        .map_err(|e| anyhow::anyhow!("grid pipelines: {e}"))?;
    drop(renderer);

    let editor_pipelines = Arc::new(editor_pipelines);

    let grid_enabled = crate::state::app_state().grid_enabled.clone();

    let hook_editor = editor_pipelines.clone();
    let hook_grid_enabled = grid_enabled.clone();

    let hooks = RenderHooks {
        before_transparent_pass: Some(Box::new(move |ctx| {
            // The editor's collider / camera / selection wireframes now
            // live in the renderer's built-in `LineRenderer` pass; this
            // hook just renders the grid on top of the world-space
            // transparent target so the floor is visible through alpha
            // geometry.
            if hook_grid_enabled.get() {
                let grid_key = match ctx.anti_aliasing.msaa_sample_count {
                    Some(4) => hook_editor.grid_pipeline_msaa_4_key,
                    None => hook_editor.grid_pipeline_singlesampled_key,
                    _ => hook_editor.grid_pipeline_singlesampled_key,
                };
                render_grid(ctx, &hook_editor.grid_bind_group, grid_key)?;
            }

            Ok(())
        })),
        ..Default::default()
    };

    set_render_hooks(hooks);
    Ok(())
}

fn start_render_loop() {
    set_raf(gloo_render::request_animation_frame(move |_ts| {
        render_one_frame();
    }));
}

fn render_one_frame() {
    // Push camera matrices (the camera itself is cheap to read each tick,
    // and ensures orbit-camera input is reflected immediately).
    let free_fly_matrices = {
        let camera = camera_handle();
        let cam = camera.lock().unwrap();
        cam.matrices()
    };

    let renderer = renderer_handle();
    let mut renderer = renderer.try_lock();

    // Track consecutive frames where the renderer lock was held by some
    // async work (e.g. asset materialization). Single misses are normal
    // — a long async batch grabbing the mutex for the duration of one
    // frame is fine — but a sustained run-up is worth surfacing because
    // it means user-visible jank. Warn once per run-up at the threshold
    // and again once the lock recovers, so the timeline shows both
    // edges.
    static CONSEC_MISS: AtomicUsize = AtomicUsize::new(0);
    const WARN_THRESHOLD: usize = 30; // ~0.5s at 60fps
    if renderer.is_none() {
        let prev = CONSEC_MISS.fetch_add(1, Ordering::Relaxed);
        if prev + 1 == WARN_THRESHOLD {
            tracing::warn!(
                "render_one_frame: renderer try_lock contended for {} frames in a row — \
                 something else is holding the mutex (likely a long-running materialize)",
                WARN_THRESHOLD
            );
        }
    } else {
        let prev = CONSEC_MISS.swap(0, Ordering::Relaxed);
        if prev >= WARN_THRESHOLD {
            tracing::warn!(
                "render_one_frame: renderer try_lock recovered after {prev} contended frames"
            );
        }
    }

    if let Some(renderer) = renderer.as_mut() {
        // Block A.4: drain pipeline scheduler status events into the
        // shared compile-status state so the floating modal can read
        // it as a dominator signal. Pending → +1, Ready/Failed →
        // -1 (saturating). Failed events also publish their error
        // string to `compile_last_error`. Cleared on the leading edge
        // of a fresh compile batch (prev_pending == 0 transitioning
        // to >0) so the next batch starts clean.
        let events = renderer.drain_pipeline_status_events();
        if !events.is_empty() {
            use awsm_renderer::pipeline_scheduler::PipelineGroupStatus;
            let pending_handle = compile_pending_handle();
            let last_err_handle = compile_last_error_handle();
            let prev_pending = pending_handle.get();
            let mut pending = prev_pending;
            let mut latest_err: Option<String> = None;
            let mut opened_new_batch = false;
            for ev in events {
                match ev.status {
                    PipelineGroupStatus::Pending => {
                        if pending == 0 {
                            opened_new_batch = true;
                        }
                        pending = pending.saturating_add(1);
                    }
                    PipelineGroupStatus::Ready => {
                        pending = pending.saturating_sub(1);
                    }
                    PipelineGroupStatus::Failed { error: _ } => {
                        // The event's `error` is intentionally a
                        // placeholder (`PipelineVariantNotCompiled
                        // ("see scheduler state")`) — the real
                        // failure detail lives on the scheduler's
                        // material/pass state. Query it back via
                        // `pipeline_group_status`. Falls back to the
                        // event's placeholder only if the entry's
                        // already been dropped (shouldn't happen
                        // mid-batch, but defensive).
                        pending = pending.saturating_sub(1);
                        let err_msg = match renderer.pipeline_group_status(ev.id) {
                            Some(PipelineGroupStatus::Failed { error: real }) => {
                                format!("{real}")
                            }
                            _ => "compile failed (status no longer queryable)".to_string(),
                        };
                        latest_err = Some(err_msg);
                    }
                }
            }
            pending_handle.set(pending);
            if let Some(msg) = latest_err {
                last_err_handle.set(Some(msg));
            } else if opened_new_batch && prev_pending == 0 {
                last_err_handle.set(None);
            }
        }

        // If the user picked an authored camera from the header, drive
        // the viewport from its CameraBehavior (mirroring the player's
        // camera_driver). Otherwise stick with the free-fly matrices
        // computed above. Authored-driver returning None (broken target,
        // wrong kind) falls back to free-fly silently — the previous
        // frame's matrices are still valid then.
        let now_ms_for_camera = web_sys::window()
            .and_then(|w| w.performance())
            .map(|p| p.now())
            .unwrap_or(0.0);
        let matrices = match crate::state::app_state().editor_camera_target.get() {
            Some(node_id) => camera_driver::evaluate(node_id, renderer, now_ms_for_camera)
                .unwrap_or(free_fly_matrices),
            None => free_fly_matrices,
        };
        if let Err(err) = renderer.update_camera(matrices) {
            tracing::error!("update_camera failed: {err}");
        }

        renderer.update_transforms();

        // Re-zoom the gizmo to a fixed screen size + re-anchor on the
        // current selection.
        gizmo::per_frame_update(renderer);
        // Same idea for point-handle gizmos (Curve/Line control points).
        point_handle_sync::per_frame_update(renderer);

        // Tick every "playing" emitter's simulator before transforms get
        // pushed to GPU. `delta_time` is sourced from
        // `renderer.frame_globals()` inside `tick_all` — the per-runtime
        // `last_ts_ms` book-keeping that used to live here moved to the
        // central `FrameGlobals` clock, so `set_time_source` overrides
        // (pause / time-scale / replay) automatically flow to particles.
        particles_sync::tick_all(renderer);

        // Push world position/direction into every active light.
        // Extracted to a separate function so additional sync logic
        // can be added without touching the render loop body.
        sync_lights_pre_render(renderer);
        // Mirror: push each Decal node's world transform into the
        // matching runtime decal.
        sync_decals_pre_render(renderer);

        // B-2: rebuild every editor-only overlay wireframe (collider
        // shapes, camera frustums, selection origin gizmos, selection
        // model bboxes) into the renderer's line registry so they draw
        // through the same fat-line pipeline as `NodeKind::Line` nodes.
        sync_editor_wireframes(renderer);

        let hooks_handle = render_hooks_handle();
        let hooks = hooks_handle.read().unwrap();
        if let Err(err) = renderer.render(hooks.as_ref()) {
            tracing::error!("render failed: {err}");
        }
    }

    set_raf(gloo_render::request_animation_frame(move |_ts| {
        render_one_frame();
    }));
}

/// Push every Decal node's current world transform / texture ref /
/// alpha into the runtime decal table. The runtime API (`update_decal`)
/// takes a closure over `&mut Decal`; we call `Decal::new` inside so
/// the inverse_transform + world_aabb cache is rebuilt in lockstep
/// with the new transform.
fn sync_decals_pre_render(renderer: &mut awsm_renderer::AwsmRenderer) {
    // Walk the per-frame decal index instead of the full bridge node
    // table — for scenes with many Group/Model nodes but few decals
    // this turns an O(N) scan + N decal_key mutex acquisitions into
    // an O(decal_count) walk. Kept in sync by apply_kind_decal /
    // clear_decal / remove_node.
    let bridge_handle = bridge();
    let entries: Vec<Arc<RendererNode>> = {
        let nodes = bridge_handle.nodes.lock().unwrap();
        bridge_handle
            .decal_node_ids
            .lock()
            .unwrap()
            .iter()
            .filter_map(|id| nodes.get(id).cloned())
            .collect()
    };

    for entry in entries {
        let decal_key = *entry.decal_key.lock().unwrap();
        let Some(decal_key) = decal_key else {
            continue;
        };
        let world = match renderer.transforms.get_world(entry.transform_key) {
            Ok(m) => *m,
            Err(_) => continue,
        };
        let kind = entry.node.kind.get_cloned();
        let crate::scene::NodeKind::Decal(cfg) = kind else {
            continue;
        };
        let visible = *entry.effective_visible.lock().unwrap();
        // Hidden decals contribute zero — easier than removing /
        // re-inserting on every eye-toggle flip.
        let alpha = if visible { cfg.alpha } else { 0.0 };
        let texture_index = crate::renderer_bridge::node_sync::decal_texture_index(&cfg);
        renderer.update_decal(decal_key, |decal| {
            *decal = awsm_renderer::decals::Decal::new(world, texture_index, alpha);
        });
    }
}

fn sync_lights_pre_render(renderer: &mut awsm_renderer::AwsmRenderer) {
    use awsm_renderer::lights::Light;
    use glam::Vec3;

    // Same per-frame index trick as `sync_decals_pre_render` — walk
    // only entries that own a runtime light, not the full bridge
    // node table.
    let bridge_handle = bridge();
    let entries: Vec<Arc<RendererNode>> = {
        let nodes = bridge_handle.nodes.lock().unwrap();
        bridge_handle
            .light_node_ids
            .lock()
            .unwrap()
            .iter()
            .filter_map(|id| nodes.get(id).cloned())
            .collect()
    };

    for entry in entries {
        let light_key = *entry.light_key.lock().unwrap();
        let Some(light_key) = light_key else {
            continue;
        };
        let world = match renderer.transforms.get_world(entry.transform_key) {
            Ok(m) => *m,
            Err(_) => continue,
        };
        let position = world.transform_point3(Vec3::ZERO);
        // "Forward" in gltf / blender convention is -Z in local space.
        let forward_world = world.transform_vector3(Vec3::new(0.0, 0.0, -1.0));
        let direction = if forward_world.length_squared() > 1e-10 {
            forward_world.normalize()
        } else {
            Vec3::NEG_Z
        };

        // Also pull the latest params off the scene Node (in case the
        // user just edited color/intensity/range/angles).
        let kind = entry.node.kind.get_cloned();
        if let crate::scene::NodeKind::Light(cfg) = kind {
            // Hidden lights are kept in the renderer (so their LightKey
            // stays stable) but contribute zero — easier than removing
            // and re-inserting through the eye-toggle cycle.
            let intensity_scale: f32 = if *entry.effective_visible.lock().unwrap() {
                1.0
            } else {
                0.0
            };
            let new_light = match &cfg {
                crate::scene::LightConfig::Directional {
                    color, intensity, ..
                } => Light::Directional {
                    color: *color,
                    intensity: *intensity * intensity_scale,
                    direction: direction.to_array(),
                },
                crate::scene::LightConfig::Point {
                    color,
                    intensity,
                    range,
                    ..
                } => Light::Point {
                    color: *color,
                    intensity: *intensity * intensity_scale,
                    position: position.to_array(),
                    range: *range,
                },
                crate::scene::LightConfig::Spot {
                    color,
                    intensity,
                    range,
                    inner_angle,
                    outer_angle,
                    ..
                } => Light::Spot {
                    color: *color,
                    intensity: *intensity * intensity_scale,
                    position: position.to_array(),
                    direction: direction.to_array(),
                    range: *range,
                    inner_angle: *inner_angle,
                    outer_angle: *outer_angle,
                },
            };
            renderer.lights.update(light_key, |light| {
                // Only assign if the variants match; otherwise `apply_kind`
                // will re-insert with the right variant shortly.
                if std::mem::discriminant(&*light) == std::mem::discriminant(&new_light) {
                    *light = new_light;
                }
            });
        }
    }
}
