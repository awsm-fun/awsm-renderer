//! Editor view-settings → renderer sync.
//!
//! The Settings drawer toggles live on `controller().settings`; these observers
//! push the renderer-affecting ones (MSAA, light-culling debug heatmap) into the
//! renderer. (Grid is handled by `engine::grid`; gizmo visibility by
//! `engine::gizmo`.) Started once at boot.

use crate::controller::controller;
use crate::engine::context::{renderer_handle, with_renderer_mut};
use crate::prelude::*;

pub fn start() {
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

    // Shadow denoise blur (cheap, synchronous — no pipeline recompile, just a
    // per-frame dispatch gate in ShadowsConfig).
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
                    if !skip {
                        with_renderer_mut(move |r| {
                            let mut cfg = r.shadows_config().clone();
                            if cfg.denoise != on {
                                cfg.denoise = on;
                                r.set_shadows_config(cfg);
                            }
                        })
                        .await;
                    }
                }
            })
            .await;
    });

    // MSAA on/off — recompiles the affected pipelines, so it's async + guarded
    // against redundant re-applies.
    spawn_local(async {
        let mut first = true;
        controller()
            .settings
            .msaa
            .signal()
            .for_each(move |on| {
                let skip = first;
                first = false;
                async move {
                    if skip {
                        return;
                    }
                    let want = if on { Some(4) } else { None };
                    let handle = renderer_handle();
                    let mut r = handle.lock().await;
                    if r.anti_aliasing.msaa_sample_count != want {
                        let mut aa = r.anti_aliasing.clone();
                        aa.msaa_sample_count = want;
                        if let Err(e) = r.set_anti_aliasing(aa).await {
                            tracing::warn!("set_anti_aliasing: {e}");
                        }
                        // An AA flip needs the MSAA edge-resolve set rebuilt;
                        // that routes through the one compile path now, not the
                        // deleted render preamble. Live (no begin_load).
                        if let Err(e) = r.commit_load(|_| {}).await {
                            tracing::warn!("commit_load after set_anti_aliasing: {e}");
                        }
                    }
                }
            })
            .await;
    });

    // Global SSCS (screen-space contact shadows): the authored, persisted
    // `scene.shadows` SSCS fields → renderer. `enabled` / `step_count` are
    // compile-time template constants, so they can recompile the shadow-consuming
    // pipelines — route through `commit_load` (a no-op when nothing got flagged);
    // the scalar params are live uniforms and just re-upload. Guarded against
    // redundant applies so unrelated `scene.shadows` edits don't churn.
    spawn_local(async {
        let mut first = true;
        controller()
            .scene
            .shadows
            .signal_cloned()
            .for_each(move |sh| {
                let skip = first;
                first = false;
                async move {
                    if skip {
                        return;
                    }
                    let handle = renderer_handle();
                    let mut r = handle.lock().await;
                    let mut cfg = r.shadows_config().clone();
                    let changed = cfg.sscs_enabled != sh.sscs_enabled
                        || cfg.sscs_step_count != sh.sscs_step_count
                        || cfg.sscs_step_world != sh.sscs_step_world
                        || cfg.sscs_thickness != sh.sscs_thickness
                        || cfg.sscs_directional_darkening != sh.sscs_directional_darkening
                        || cfg.sscs_punctual_darkening != sh.sscs_punctual_darkening;
                    if !changed {
                        return;
                    }
                    cfg.sscs_enabled = sh.sscs_enabled;
                    cfg.sscs_step_count = sh.sscs_step_count;
                    cfg.sscs_step_world = sh.sscs_step_world;
                    cfg.sscs_thickness = sh.sscs_thickness;
                    cfg.sscs_directional_darkening = sh.sscs_directional_darkening;
                    cfg.sscs_punctual_darkening = sh.sscs_punctual_darkening;
                    r.set_shadows_config(cfg);
                    if let Err(e) = r.commit_load(|_| {}).await {
                        tracing::warn!("commit_load after SSCS change: {e}");
                    }
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
