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
                    }
                }
            })
            .await;
    });
}
