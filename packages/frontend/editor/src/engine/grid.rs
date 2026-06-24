//! Viewport ground grid (the reference Scene's floor grid). Compiles the
//! web-shared `viewport3d` grid pipelines once after boot and installs a
//! `before_transparent_pass` render hook that draws the grid — toggled live by
//! the Settings → Show grid switch. The transform gizmo (the other half of the
//! viewport3d editor overlay) is the larger follow-on.

use std::sync::Arc;

use awsm_renderer::render::RenderHooks;
use awsm_renderer_web_shared::viewport3d::grid::{pipelines::EditorPipelines, render::render_grid};

use crate::engine::context::{render_hooks_handle, renderer_handle};
use crate::prelude::*;

/// Compile the grid pipelines + bind the grid render hook to `settings.grid`.
/// Call once after the renderer + render loop are running.
pub fn init() {
    spawn_local(async move {
        let pipelines = {
            let handle = renderer_handle();
            let mut r = handle.lock().await;
            match EditorPipelines::load(&mut r).await {
                Ok(p) => Arc::new(p),
                Err(e) => {
                    tracing::warn!("grid pipelines: {e}");
                    return;
                }
            }
        };

        // Install / clear the grid hook as the Show-grid setting toggles.
        controller()
            .settings
            .grid
            .signal()
            .for_each(move |on| {
                let hooks = if on {
                    let bind_group = pipelines.grid_bind_group.clone();
                    let msaa4 = pipelines.grid_pipeline_msaa_4_key;
                    let single = pipelines.grid_pipeline_singlesampled_key;
                    Some(RenderHooks {
                        before_transparent_pass: Some(Box::new(move |ctx| {
                            let key = match ctx.anti_aliasing.msaa_sample_count {
                                Some(4) => msaa4,
                                None => single,
                                _ => msaa4,
                            };
                            render_grid(ctx, &bind_group, key)
                        })),
                        ..Default::default()
                    })
                } else {
                    None
                };
                *render_hooks_handle().write().unwrap() = hooks;
                // The grid pipelines compile asynchronously after boot, so the hook
                // is installed well after the initial canvas sync. Re-sync now to
                // draw one correct-aspect frame with the hook present — otherwise the
                // grid stays invisible until the user resizes the window.
                if on {
                    crate::engine::context::sync_canvas_size();
                }
                async {}
            })
            .await;
    });
}
