//! Scene `ShadowsConfig` → renderer sync.
//!
//! Observes `scene.shadows`; whenever it changes (Load, the global
//! Shadows… modal in the Environment toolbar, or undo/redo), pushes
//! the matching config into the renderer via
//! `AwsmRenderer::set_shadows_config`.
//!
//! Resource-shaped fields (`atlas_size`, `point_shadow_resolution`,
//! `max_point_shadows`, `evsm_atlas_size`) only resize on a renderer
//! rebuild — the underlying GPU textures are allocated in
//! `Shadows::new`. We still push the values so the next session picks
//! them up; live tweaks of the tunables (SSCS toggle, blur, exponent,
//! cascade-color debug overlay) take effect immediately on the next
//! `render()` call.

use crate::context::with_renderer_mut;
use crate::scene::ShadowsConfig as SchemaShadowsConfig;
use crate::state::app_state;
use awsm_renderer::shadows::ShadowsConfig;
use futures_signals::signal::SignalExt;
use wasm_bindgen_futures::spawn_local;

pub fn start() {
    let state = app_state();
    let signal = state.scene.shadows.signal_cloned();
    spawn_local(async move {
        signal
            .for_each(move |cfg| async move {
                apply(cfg).await;
            })
            .await;
    });
}

async fn apply(cfg: SchemaShadowsConfig) {
    let runtime = schema_to_runtime(&cfg);
    with_renderer_mut(move |r| {
        r.set_shadows_config(runtime);
    })
    .await;
}

/// Local schema → runtime conversion. The renderer crate exposes the
/// same conversion behind its `scene-schema` feature
/// (`shadows::schema_convert`) for non-editor consumers (players);
/// the editor frontend keeps this small helper for legacy reasons —
/// it avoids enabling that feature just for one bridge.
fn schema_to_runtime(s: &SchemaShadowsConfig) -> ShadowsConfig {
    ShadowsConfig {
        sscs_enabled: s.sscs_enabled,
        sscs_step_count: s.sscs_step_count,
        atlas_size: s.atlas_size,
        evsm_atlas_size: s.evsm_atlas_size,
        evsm_exponent: s.evsm_exponent,
        evsm_blur_radius: s.evsm_blur_radius,
        max_point_shadows: s.max_point_shadows,
        point_shadow_resolution: s.point_shadow_resolution,
        debug_cascade_colors: s.debug_cascade_colors,
        ..ShadowsConfig::default()
    }
}
