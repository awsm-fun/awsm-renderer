//! Editor view-settings → renderer sync.
//!
//! The Settings drawer toggles live on `controller().settings`; these observers
//! push the renderer-affecting ones (anti-aliasing, light-culling debug
//! heatmap) into the renderer. (Grid is handled by `engine::grid`; gizmo visibility by
//! `engine::gizmo`.) Started once at boot.

use crate::controller::controller;
use crate::engine::context::{renderer_handle, with_renderer_mut};
use crate::prelude::*;

pub fn start() {
    // Editor-camera clip planes: manual pin ↔ auto (orbit-tracked). Applied
    // straight onto the view camera — session-only, never persisted.
    spawn_local(async {
        let s = controller().settings.clone();
        map_ref! {
            let manual = s.cam_clip_manual.signal(),
            let near = s.cam_clip_near.signal(),
            let far = s.cam_clip_far.signal() =>
            if *manual { Some((*near as f32, *far as f32)) } else { None }
        }
        .for_each(move |clip| async move {
            crate::engine::context::try_with_camera_mut(move |c| c.set_clip_override(clip));
        })
        .await;
    });

    // Light-culling debug heatmap (cheap, synchronous).
    spawn_local(async {
        let mut first = true;
        controller()
            .settings
            .heatmap
            .signal()
            .for_each(move |on| {
                let skip = first;
                first = false;
                async move {
                    if !skip {
                        with_renderer_mut(move |r| r.set_light_culling_debug_heatmap(on)).await;
                    }
                }
            })
            .await;
    });

    // Shadow denoise blur. `denoise` gates whether the prep blur pipelines are
    // COMPILED (not just dispatched), so an off→on flip must drain the config
    // pipelines to build the pair — otherwise render_blur warn-skips until an
    // unrelated commit_load. Async + guarded against redundant re-applies,
    // like the anti-aliasing observer below.
    spawn_local(async {
        let mut first = true;
        controller()
            .settings
            .shadow_denoise
            .signal()
            .for_each(move |on| {
                let skip = first;
                first = false;
                async move {
                    if skip {
                        return;
                    }
                    let handle = renderer_handle();
                    let mut r = handle.lock().await;
                    let mut cfg = r.shadows_config().clone();
                    if cfg.denoise != on {
                        cfg.denoise = on;
                        r.set_shadows_config(cfg);
                        // Compile the blur pair for the new config (no-op on
                        // off, a cache hit on a later on→off→on).
                        if let Err(e) = r.ensure_config_pipelines().await {
                            tracing::warn!("ensure_config_pipelines after denoise flip: {e}");
                        }
                    }
                }
            })
            .await;
    });

    // Anti-aliasing: the authored, persisted `scene.post_process.anti_aliasing`
    // (MSAA / SMAA / TAA and its settings) → renderer, through the loader's
    // mapping (`anti_aliasing_to_renderer`), so the viewport and a player load
    // agree. A MODE change recompiles: `set_anti_aliasing` then `commit_load`
    // (the one compile path; it also rebuilds the MSAA edge-resolve set). A
    // change of TAA settings while TAA stays on takes the renderer's light path
    // (`AntiAliasing::same_structure`): no recompile, no history reset, no
    // commit. Guarded on equality, so the initial emission (defaults vs the
    // renderer's boot config) is a no-op and a project load applies here.
    spawn_local(async {
        controller()
            .scene
            .post_process
            .signal_ref(|pp| pp.anti_aliasing)
            .dedupe()
            .for_each(|authored| async move {
                let handle = renderer_handle();
                let mut r = handle.lock().await;
                let pp = awsm_renderer_editor_protocol::PostProcessConfig {
                    anti_aliasing: authored,
                    ..Default::default()
                };
                let want =
                    awsm_renderer_scene_loader::anti_aliasing_to_renderer(&pp, &r.anti_aliasing);
                if want == r.anti_aliasing {
                    return;
                }
                let structural = !want.same_structure(&r.anti_aliasing);
                let _guard = structural.then(crate::controller::CompileGuard::new);
                // Selecting TAA also turns supersampling off (`SetAntiAliasing`
                // sets `render_scale` 1). Its own observer may not have run
                // yet, and the renderer rejects TAA while supersampling, so
                // apply it here first.
                if want.taa.is_some() && r.render_scale() > 1.0 {
                    if let Err(e) = r.set_render_scale(1.0).await {
                        tracing::warn!("set_render_scale before TAA: {e}");
                    }
                }
                if let Err(e) = r.set_anti_aliasing(want).await {
                    tracing::warn!("set_anti_aliasing: {e}");
                    return;
                }
                if structural {
                    if let Err(e) = r.commit_load(|_| {}).await {
                        tracing::warn!("commit_load after set_anti_aliasing: {e}");
                        crate::engine::context::surface_commit_error(&e);
                    }
                }
            })
            .await;
    });

    // TAA availability on this device (its motion-vector path), for the
    // Post-processing panel and the `SetAntiAliasing` handler.
    spawn_local(async {
        let reason = with_renderer_mut(|r| {
            use awsm_renderer::anti_alias::{
                TaaVelocityPath, TAA_COLOR_ATTACHMENT_BYTES_PER_SAMPLE,
                TAA_GEOMETRY_STORAGE_BUFFERS,
            };
            (r.taa_velocity_path() != TaaVelocityPath::GeometryTarget).then(|| {
                let limits = r.gpu.device.limits();
                format!(
                    "needs maxColorAttachmentBytesPerSample >= \
                     {TAA_COLOR_ATTACHMENT_BYTES_PER_SAMPLE} (device: {}) and \
                     maxStorageBuffersPerShaderStage >= {TAA_GEOMETRY_STORAGE_BUFFERS} \
                     (device: {})",
                    limits.max_color_attachment_bytes_per_sample(),
                    limits.max_storage_buffers_per_shader_stage(),
                )
            })
        })
        .await;
        TAA_UNAVAILABLE.with(|m| m.set(reason));
    });

    // TAA debug view (transient view state).
    spawn_local(async {
        controller()
            .settings
            .taa_debug_view
            .signal()
            .for_each(|view| async move {
                with_renderer_mut(move |r| r.set_taa_debug_view(taa_debug_view(view))).await;
            })
            .await;
    });

    // Anisotropic filtering — swaps pool samplers to no-aniso twins when off
    // (one TexturePool bind-group rebuild; no pipeline work).
    spawn_local(async {
        controller()
            .settings
            .anisotropy
            .signal()
            .for_each(move |on| async move {
                let handle = renderer_handle();
                let mut r = handle.lock().await;
                if r.anisotropy_enabled() != on {
                    r.set_anisotropy_enabled(on);
                }
            })
            .await;
    });

    // Supersampling render scale — internal targets scale up, display
    // downsamples (`AwsmRenderer::set_render_scale`). Transient quality
    // option like MSAA/SMAA. Cheap-ish: only the display pipeline variant
    // recompiles on the off↔on boundary; targets rebuild lazily next frame.
    spawn_local(async {
        controller()
            .settings
            .render_scale
            .signal()
            .for_each(move |scale| async move {
                let handle = renderer_handle();
                let mut r = handle.lock().await;
                if (r.render_scale() - scale).abs() > f32::EPSILON {
                    if let Err(e) = r.set_render_scale(scale).await {
                        tracing::warn!("set_render_scale: {e}");
                    }
                }
            })
            .await;
    });

    // Renderer-wide shadows: the authored, persisted `scene.shadows` block →
    // renderer, in full. `sscs_enabled` / `sscs_step_count` are compile-time
    // template constants, so they can recompile the shadow-consuming pipelines
    // — route through `commit_load` (a no-op when nothing got flagged).
    // `atlas_size` / `evsm_atlas_size` / `max_point_shadows` /
    // `point_shadow_resolution` are resource-shape: `set_shadows_config` flags
    // them and the shadow module recreates the textures + bind groups at the
    // next frame's `write_gpu`. Everything else is a live uniform re-upload.
    // Guarded on config equality so unrelated re-emissions don't churn — that
    // guard also makes the initial boot emission (defaults == defaults) a
    // no-op, so no first-skip is needed and a project load (`apply_project`
    // sets `scene.shadows`) applies through this same path. The renderer-only
    // fields the schema doesn't author (`denoise`, cascade array shape) are
    // preserved from the live config. `CompileGuard` holds the
    // `WaitRenderSettled` barrier open so an MCP edit→settle→screenshot can't
    // capture a mid-recompile frame.
    spawn_local(async {
        controller()
            .scene
            .shadows
            .signal_cloned()
            .for_each(|sh| async move {
                let _guard = crate::controller::CompileGuard::new();
                let handle = renderer_handle();
                let mut r = handle.lock().await;
                let mut cfg = r.shadows_config().clone();
                cfg.sscs_enabled = sh.sscs_enabled;
                cfg.sscs_step_count = sh.sscs_step_count;
                cfg.sscs_step_world = sh.sscs_step_world;
                cfg.sscs_thickness = sh.sscs_thickness;
                cfg.sscs_directional_darkening = sh.sscs_directional_darkening;
                cfg.sscs_punctual_darkening = sh.sscs_punctual_darkening;
                cfg.atlas_size = sh.atlas_size;
                cfg.evsm_atlas_size = sh.evsm_atlas_size;
                cfg.evsm_exponent = sh.evsm_exponent;
                cfg.evsm_blur_radius = sh.evsm_blur_radius;
                cfg.max_point_shadows = sh.max_point_shadows;
                cfg.point_shadow_resolution = sh.point_shadow_resolution;
                cfg.debug_cascade_colors = sh.debug_cascade_colors;
                if cfg == *r.shadows_config() {
                    return;
                }
                r.set_shadows_config(cfg);
                if let Err(e) = r.commit_load(|_| {}).await {
                    tracing::warn!("commit_load after shadows change: {e}");
                    crate::engine::context::surface_commit_error(&e);
                }
            })
            .await;
    });

    // Global post-processing: the authored, persisted `scene.post_process`
    // (tonemapping / bloom / DoF / exposure) → renderer. Tonemapper/bloom/DoF
    // flips recompile the effects + display pipelines inside
    // `set_post_processing` (it awaits until GPU-resident); exposure is a live
    // uniform. `set_post_processing` no-ops on an equal config, so unrelated
    // signal re-emissions don't churn. The `CompileGuard` holds the
    // `WaitRenderSettled` barrier open across the recompile so an MCP
    // edit→settle→screenshot sequence can't capture a mid-recompile frame.
    spawn_local(async {
        let mut first = true;
        controller()
            .scene
            .post_process
            .signal_cloned()
            .for_each(move |pp| {
                let skip = first;
                first = false;
                async move {
                    if skip {
                        return;
                    }
                    let _guard = crate::controller::CompileGuard::new();
                    let handle = renderer_handle();
                    let mut r = handle.lock().await;
                    // The SHARED schema→runtime mapping (scene-loader) — the
                    // player's load path uses the same fn, so the editor
                    // viewport and a bundle playback tonemap identically.
                    let rpp = awsm_renderer_scene_loader::post_process_to_renderer(&pp);
                    if let Err(e) = r.set_post_processing(rpp).await {
                        tracing::warn!("set_post_processing: {e}");
                    }
                }
            })
            .await;
    });
}

thread_local! {
    /// Why TAA is unavailable on this device (`None` = available, or not
    /// determined yet). Set once at boot.
    static TAA_UNAVAILABLE: Mutable<Option<String>> = Mutable::new(None);
}

/// Why TAA is unavailable on this device, or `None` when it is available.
pub fn taa_unavailable_reason() -> Option<String> {
    TAA_UNAVAILABLE.with(|m| m.get_cloned())
}

/// The renderer's debug view for the wire name.
fn taa_debug_view(
    view: awsm_renderer_editor_protocol::TaaDebugViewName,
) -> awsm_renderer::taa::TaaDebugView {
    use awsm_renderer::taa::TaaDebugView as R;
    use awsm_renderer_editor_protocol::TaaDebugViewName as N;
    match view {
        N::None => R::None,
        N::Velocity => R::Velocity,
        N::ReprojectionError => R::ReprojectionError,
        N::HistoryLength => R::HistoryLength,
        N::Rejection => R::Rejection,
        N::Reactive => R::Reactive,
        N::Confidence => R::Confidence,
        N::FreezeJitter => R::FreezeJitter,
        N::HistoryOnly => R::HistoryOnly,
        N::CurrentOnly => R::CurrentOnly,
    }
}
