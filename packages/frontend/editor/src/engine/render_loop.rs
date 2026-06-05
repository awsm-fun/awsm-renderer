//! The render loop. M3 keeps it slim: each frame pushes the editor camera and
//! renders. The per-frame scene→GPU sync (lights / decals / gizmo / particles /
//! colliders) layers in via the renderer bridge as those features land (M4+).

use awsm_renderer::camera::CameraMatrices;
use awsm_renderer::AwsmRenderer;
use awsm_scene_schema::{CameraProjection, NodeKind};
use glam::{Mat4, Vec3};

use super::context;
use crate::controller::controller;
use crate::engine::scene::NodeId;

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
    // Which camera drives the view this frame: the free built-in camera (None),
    // or a scene Camera node (Some) — see `EditorController::active_camera`.
    let active = controller().active_camera.get();

    let handle = context::renderer_handle();
    // Non-blocking: a single miss (async asset work holding the lock) just skips
    // a frame rather than stalling the RAF callback. Bind the guard to a named
    // local (declared after `handle`) so it drops before `handle`.
    let mut guard = handle.try_lock();
    if let Some(renderer) = guard.as_mut() {
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
        // Advance any particle emitters + push their live particles to the GPU.
        super::bridge::particles::tick_all(renderer);
        renderer.update_transforms();
        let hooks = context::render_hooks_handle();
        let hooks = hooks.read().unwrap();
        if let Err(err) = renderer.render(hooks.as_ref()) {
            tracing::error!("render failed: {err}");
        }
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
    let transform_key = {
        let b = super::bridge::bridge();
        let nodes = b.nodes.lock().unwrap();
        nodes.get(&node_id)?.transform_key
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
    let projection = match cfg.projection {
        CameraProjection::Perspective { fov_y_rad } => {
            Mat4::perspective_rh(fov_y_rad, aspect, cfg.near, cfg.far)
        }
        CameraProjection::Orthographic { half_height } => {
            let half_width = half_height * aspect;
            Mat4::orthographic_rh(
                -half_width,
                half_width,
                -half_height,
                half_height,
                cfg.near,
                cfg.far,
            )
        }
    };

    Some(CameraMatrices {
        view,
        projection,
        position_world: pos,
        focus_distance: 10.0,
        aperture: 5.6,
    })
}
