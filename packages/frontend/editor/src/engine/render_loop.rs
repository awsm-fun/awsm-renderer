//! The render loop. Each frame pushes the editor camera and renders. The
//! per-frame scene→GPU sync (lights / decals / gizmo / particles / colliders)
//! is layered in via the renderer bridge.

use awsm_editor_protocol::{CameraProjection, NodeKind};
use awsm_renderer::camera::CameraMatrices;
use awsm_renderer::cameras::CameraProjectionParams;
use awsm_renderer::AwsmRenderer;
use glam::{Mat4, Vec3};

use super::context;
use crate::controller::controller;
use crate::engine::scene::NodeId;

/// Begin the `requestAnimationFrame` loop. Idempotent-ish — call once after the
/// renderer context is ready.
pub fn start() {
    request_frame();
}

thread_local! {
    /// The previous rAF timestamp (ms), for computing the per-frame delta the
    /// animation transport advances by while playing.
    static LAST_TS: std::cell::Cell<Option<f64>> = const { std::cell::Cell::new(None) };

    /// Transport state for the playing clock: `(phase, last_emitted)`. `phase` is
    /// the **unbounded** elapsed clip time (seconds) along the clip's base
    /// direction — folding it into `[0, dur]` per loop style is what gives
    /// ping-pong its bounce without tracking a separate direction. `last_emitted`
    /// is the playhead we last wrote, so a value that differs at the next tick
    /// means an external scrub (`SetPlayhead`) landed and the phase must re-seed.
    static TRANSPORT: std::cell::Cell<(f64, f64)> =
        const { std::cell::Cell::new((0.0, f64::NAN)) };

    /// Backstop latch for the "starts black until you resize the window" bug:
    /// whether we've run the thorough surface re-sync ([`context::sync_canvas_size`])
    /// since the canvas last had a real (nonzero) client size. The mount-time
    /// call to that function only polls ~480ms before giving up, and the
    /// `ResizeObserver` doesn't reliably fire on the reparent into the viewport
    /// slot — so on a slow/late layout the surface can stay at its stale default
    /// (black) until a manual resize finally triggers the observer. We reset this
    /// to `false` whenever the canvas is zero-sized and re-run the full re-sync on
    /// the next nonzero frame, so every 0→nonzero transition (first mount, tab
    /// show, reparent) auto-heals — exactly what the manual resize did by hand.
    static DID_REAL_SIZE_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn request_frame() {
    let raf = gloo_render::request_animation_frame(move |ts| {
        let dt_ms = LAST_TS.with(|c| {
            let prev = c.replace(Some(ts));
            prev.map(|p| (ts - p).max(0.0)).unwrap_or(0.0)
        });
        tick_animation_clock(dt_ms);
        render_one_frame();
        request_frame();
    });
    context::set_raf(raf);
}

/// Advance the animation transport while playing. The editor owns the clock:
/// when playing we advance the controller `playhead` with a direct local
/// `set_neq` — the ruler binds to the `playhead` signal, so it follows without a
/// command. Each tab runs its own rAF clock, kept in agreement by the one-shot
/// `SetPlaying`/`SetPlayhead` broadcasts (play/pause + discrete scrubs), so there
/// is no per-frame dispatch to broadcast 60×/sec. The pose is *pinned* into the
/// renderer in `render_one_frame` (under the held guard, before
/// `update_transforms`) via [`super::bridge::animation_sync::pin_pose`].
///
/// Playback honors the clip's authored `direction` (Forward/Reverse) and
/// `loop_style`: **Loop** wraps, **Once** clamps at the far end, and **PingPong**
/// bounces. Since `pin_pose` only seeks to the playhead value (the clip group's
/// own loop/direction don't apply when pinning a one-shot pose), that trajectory
/// has to be produced here — see [`TRANSPORT`].
fn tick_animation_clock(dt_ms: f64) {
    use crate::controller::animation::ClipDirection;
    let ctrl = controller();
    if !ctrl.playing.get() {
        return;
    }
    let Some(clip) = ctrl
        .current_clip
        .get()
        .and_then(|id| crate::controller::animation::find_clip(&ctrl.custom_animations, id))
    else {
        return;
    };
    let dur = clip.duration.get();
    if dur <= 0.0 {
        ctrl.playhead.set_neq(0.0);
        return;
    }
    let dt_s = dt_ms / 1000.0;
    let speed = clip.speed.get();
    let base_sign = match clip.direction.get() {
        ClipDirection::Forward => 1.0,
        ClipDirection::Reverse => -1.0,
    };
    let cur = ctrl.playhead.get();

    let next = TRANSPORT.with(|t| {
        let (mut phase, last_emitted) = t.get();
        // Re-seed from the playhead on the first tick or whenever it moved
        // externally (a scrub) since we last emitted — otherwise the unbounded
        // phase would fight the scrubbed value.
        if last_emitted.is_nan() || (cur - last_emitted).abs() > 1e-9 {
            phase = cur;
        }
        // Advance along the base direction, then fold into [0, dur] per loop
        // style.
        phase += dt_s * speed * base_sign;
        let next = playhead_from_phase(phase, dur, clip.loop_style.get());
        t.set((phase, next));
        next
    });
    // Advance the clock locally — no command, no broadcast. The ruler binds to
    // the `playhead` signal; cross-tab agreement comes from the one-shot
    // play/pause + scrub broadcasts, not this per-frame tick.
    ctrl.playhead.set_neq(next);
}

/// Fold an unbounded transport `phase` (seconds) into a playhead in `[0, dur]`
/// per loop style: **Once** clamps at the ends, **Loop** wraps, **PingPong**
/// bounces (a triangle wave). `rem_euclid` keeps a negative — i.e. reverse —
/// phase correct for both Loop and PingPong. Caller guarantees `dur > 0`.
fn playhead_from_phase(
    phase: f64,
    dur: f64,
    loop_style: crate::controller::animation::ClipLoop,
) -> f64 {
    use crate::controller::animation::ClipLoop;
    match loop_style {
        ClipLoop::Once => phase.clamp(0.0, dur),
        ClipLoop::Loop => phase.rem_euclid(dur),
        ClipLoop::PingPong => {
            let m = phase.rem_euclid(2.0 * dur);
            if m <= dur {
                m
            } else {
                2.0 * dur - m
            }
        }
    }
}

fn render_one_frame() {
    // Black-on-start backstop: drive the full surface re-sync once per
    // 0→nonzero canvas-size transition. Reads only the canvas client box (no
    // renderer lock); `sync_canvas_size` is `IN_FLIGHT`-coalesced + idempotent
    // and a no-op while the size is zero, so this is safe to poll every frame.
    // It's what finally heals the stale-default surface that otherwise stays
    // black until the user resizes the window (see `DID_REAL_SIZE_SYNC`).
    {
        let (cw, ch) = context::with_canvas(|c| (c.client_width(), c.client_height()));
        if cw <= 0 || ch <= 0 {
            DID_REAL_SIZE_SYNC.with(|done| done.set(false));
        } else if !DID_REAL_SIZE_SYNC.with(|done| done.replace(true)) {
            context::sync_canvas_size();
        }
    }

    // Which camera drives the view this frame: the free built-in camera (None),
    // or a scene Camera node (Some) — see `EditorController::active_camera`.
    let active = controller().active_camera.get();

    let handle = context::renderer_handle();
    // Non-blocking: a single miss (async asset work holding the lock) just skips
    // a frame rather than stalling the RAF callback. Bind the guard to a named
    // local (declared after `handle`) so it drops before `handle`.
    let mut guard = handle.try_lock();
    if let Some(renderer) = guard.as_mut() {
        // Self-heal the surface size every frame. The canvas is reparented into the
        // viewport slot *after* layout, so the initial `ResizeObserver` / `sync_canvas_size`
        // can miss the first real size — leaving the surface at the stale default and every
        // RAF render BLACK until the user resizes the window (which is what finally
        // reconfigures it). Here we reconfigure whenever the backing store doesn't match the
        // CSS box: a cheap int compare that no-ops once they agree, so it fixes first mount
        // (and any resize the observer drops) without a manual resize. Done under the guard,
        // before any render, so there's no in-flight-submit race against texture recreation.
        let canvas = renderer.gpu.canvas().clone();
        let cw = canvas.client_width();
        let ch = canvas.client_height();
        if cw > 0 && ch > 0 && (canvas.width() != cw as u32 || canvas.height() != ch as u32) {
            canvas.set_width(cw as u32);
            canvas.set_height(ch as u32);
            renderer.gpu.sync_canvas_buffer_with_css();
            context::try_with_camera_mut(|c| c.set_aspect(cw as f32 / ch as f32));
        }

        // A scene camera reads from the renderer's transform graph, so refresh
        // world matrices before sampling it.
        if active.is_some() {
            renderer.update_transforms();
        }
        // Reading the free camera each tick reflects orbit/pan/zoom immediately;
        // a scene camera locks the view to its node's transform + config (and if
        // that node has gone away, we fall back to the free camera).
        let scene_matrices = active.and_then(|id| scene_camera_matrices(renderer, id));
        let matrices =
            match scene_matrices.or_else(|| context::try_with_camera_mut(|c| c.matrices())) {
                Some(m) => m,
                None => return, // context not ready yet
            };
        if let Err(err) = renderer.update_camera(matrices.clone()) {
            tracing::error!("update_camera failed: {err}");
        }
        // Keep the gizmo screen-constant + anchored under the selection, and
        // enforce its visibility against the selection + toggle.
        super::gizmo::per_frame_update(renderer);
        // Keep curve control-point handles screen-constant + anchored.
        super::curve_handles::per_frame_update(renderer);
        // Advance any particle emitters + push their live particles to the GPU.
        super::bridge::particles::tick_all(renderer);
        // Pin the animation pose at the current playhead BEFORE world transforms
        // are derived (animation writes locals; `update_transforms` derives world).
        super::bridge::animation_sync::pin_pose(renderer, controller().playhead.get());
        // Skin bridge: copy animated/posed mirror-bone locals onto the baked
        // joint keys the skin reads, BEFORE world matrices are derived — otherwise
        // a skinned glTF's joint data animates but the mesh stays frozen.
        super::bridge::skin_bridge::sync_bones_to_skin(renderer);
        renderer.update_transforms();
        let hooks = context::render_hooks_handle();
        let hooks = hooks.read().unwrap();
        if let Err(err) = renderer.render(hooks.as_ref()) {
            tracing::error!("render failed: {err}");
        }
        // Fulfill any pending scene screenshots/pixel reads now — the swapchain
        // texture still holds this frame's render and is the current texture
        // (a WebGPU canvas isn't `toDataURL`-readable, so we GPU-copy it).
        super::query::poll_scene_capture(renderer);
        // `render()` drains the pipeline scheduler in its pre-amble, so these
        // counts are fresh. Surface them in the activity indicator — this is
        // what makes post-import shader/pipeline compiles (and any first-start
        // editor-pipeline warmup that spills past mount) actually visible: the
        // import command's own RAII guard drops long before the GPU finishes
        // compiling, so without this the pill flashes and vanishes.
        let progress = renderer.compile_progress();
        super::activity::set_compile_progress(
            progress.materials_pending,
            progress.in_flight_subcompiles,
        );
        // Renderables are now collected — update the screen-space selection box.
        super::selection_box::update(renderer, &matrices);
    }
}

/// Build `CameraMatrices` from a scene `Camera` node's world transform + its
/// `CameraConfig`. Returns `None` if the node is gone, isn't a camera, or has no
/// renderer transform yet — the caller then falls back to the free camera.
fn scene_camera_matrices(renderer: &AwsmRenderer, node_id: NodeId) -> Option<CameraMatrices> {
    let node = crate::engine::scene::mutate::find_by_id(&controller().scene, node_id)?;
    let cfg = match node.kind.get_cloned() {
        NodeKind::Camera(c) => c,
        _ => return None,
    };
    let (transform_key, camera_key) = {
        let b = super::bridge::bridge();
        let nodes = b.nodes.lock().unwrap();
        let entry = nodes.get(&node_id)?;
        let camera_key = *entry.camera_key.lock().unwrap();
        (entry.transform_key, camera_key)
    };
    let world = *renderer.transforms.get_world(transform_key).ok()?;

    // The camera looks down its local -Z, with +Y up (glTF convention).
    let pos = world.w_axis.truncate();
    let mut forward = (-world.z_axis.truncate()).normalize_or_zero();
    let mut up = world.y_axis.truncate().normalize_or_zero();
    if forward == Vec3::ZERO {
        forward = Vec3::NEG_Z;
    }
    if up == Vec3::ZERO {
        up = Vec3::Y;
    }
    let view = Mat4::look_at_rh(pos, pos + forward, up);

    let (w, h) = renderer.gpu.current_context_texture_size().ok()?;
    let aspect = if h > 0 { w as f32 / h as f32 } else { 1.0 };

    // Read the *animatable* params from the renderer cameras store when this node
    // has a materialized slot — that's what an `AnimationTarget::Camera` channel
    // mutates, so an animated camera is live. The slot mirrors the node config for
    // a static camera (node_sync keeps it synced), so the matrices are identical
    // to reading the config directly. Fall back to the node config if the slot
    // hasn't materialized yet (e.g. the very first frame after insert).
    let (projection_params, near, far, focus_distance, aperture) =
        match camera_key.and_then(|key| renderer.cameras.get(key)) {
            Some(p) => (p.projection, p.near, p.far, p.focus_distance, p.aperture),
            None => {
                let projection = match cfg.projection {
                    CameraProjection::Perspective { fov_y_rad } => {
                        CameraProjectionParams::Perspective { fov_y_rad }
                    }
                    CameraProjection::Orthographic { half_height } => {
                        CameraProjectionParams::Orthographic { half_height }
                    }
                };
                (projection, cfg.near, cfg.far, 10.0, 5.6)
            }
        };

    let projection = match projection_params {
        CameraProjectionParams::Perspective { fov_y_rad } => {
            Mat4::perspective_rh(fov_y_rad, aspect, near, far)
        }
        CameraProjectionParams::Orthographic { half_height } => {
            let half_width = half_height * aspect;
            Mat4::orthographic_rh(
                -half_width,
                half_width,
                -half_height,
                half_height,
                near,
                far,
            )
        }
    };

    Some(CameraMatrices {
        view,
        projection,
        position_world: pos,
        focus_distance,
        aperture,
    })
}

#[cfg(test)]
mod tests {
    use super::playhead_from_phase;
    use crate::controller::animation::ClipLoop;

    const DUR: f64 = 4.0;

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    #[test]
    fn once_clamps_at_both_ends() {
        approx(playhead_from_phase(2.0, DUR, ClipLoop::Once), 2.0);
        approx(playhead_from_phase(6.0, DUR, ClipLoop::Once), DUR); // past end → clamp
        approx(playhead_from_phase(-1.0, DUR, ClipLoop::Once), 0.0); // reverse past start
    }

    #[test]
    fn loop_wraps_forward_and_reverse() {
        approx(playhead_from_phase(5.0, DUR, ClipLoop::Loop), 1.0); // 5 mod 4
        approx(playhead_from_phase(-1.0, DUR, ClipLoop::Loop), 3.0); // reverse wraps to near end
    }

    #[test]
    fn pingpong_bounces() {
        approx(playhead_from_phase(1.0, DUR, ClipLoop::PingPong), 1.0); // ascending
        approx(playhead_from_phase(DUR, DUR, ClipLoop::PingPong), DUR); // at the turn
        approx(playhead_from_phase(5.0, DUR, ClipLoop::PingPong), 3.0); // descending: 2*4-5
        approx(playhead_from_phase(8.0, DUR, ClipLoop::PingPong), 0.0); // full cycle back to 0
        approx(playhead_from_phase(-1.0, DUR, ClipLoop::PingPong), 1.0); // reverse bounce
    }
}
