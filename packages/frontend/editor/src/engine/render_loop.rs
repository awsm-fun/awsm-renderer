//! The render loop. M3 keeps it slim: each frame pushes the editor camera and
//! renders. The per-frame scene→GPU sync (lights / decals / gizmo / particles /
//! colliders) layers in via the renderer bridge as those features land (M4+).

use super::context;

/// Begin the `requestAnimationFrame` loop. Idempotent-ish — call once after the
/// renderer context is ready.
pub fn start() {
    request_frame();
}

fn request_frame() {
    let raf = gloo_render::request_animation_frame(move |_ts| {
        render_one_frame();
        request_frame();
    });
    context::set_raf(raf);
}

fn render_one_frame() {
    // Reading the camera each tick reflects orbit/pan/zoom input immediately.
    let matrices = match context::try_with_camera_mut(|c| c.matrices()) {
        Some(m) => m,
        None => return, // context not ready yet
    };

    let handle = context::renderer_handle();
    // Non-blocking: a single miss (async asset work holding the lock) just skips
    // a frame rather than stalling the RAF callback. Bind the guard to a named
    // local (declared after `handle`) so it drops before `handle`.
    let mut guard = handle.try_lock();
    if let Some(renderer) = guard.as_mut() {
        if let Err(err) = renderer.update_camera(matrices) {
            tracing::error!("update_camera failed: {err}");
        }
        // Keep the gizmo screen-constant + anchored under the selection, and
        // enforce its visibility against the selection + toggle.
        super::gizmo::per_frame_update(renderer);
        renderer.update_transforms();
        let hooks = context::render_hooks_handle();
        let hooks = hooks.read().unwrap();
        if let Err(err) = renderer.render(hooks.as_ref()) {
            tracing::error!("render failed: {err}");
        }
    }
}
