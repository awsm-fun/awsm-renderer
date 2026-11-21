//! The player preview: what the viewport shows after `LoadPlayerBundle`.
//!
//! `LoadPlayerBundle` bakes the open project to a player bundle and loads it
//! back through the runtime loader, so the viewport shows what a shipped
//! player would. The editor's scene tree is empty while that is up — the
//! scene lives only in the renderer — so this module holds what the editor
//! needs to behave like a player host for it:
//!
//! - the [`LoadedScene`] itself, kept whole so it can be torn down completely;
//! - a [`ParticleDriver`], because the loader builds emitters and the host
//!   ticks them (a preview that did not would show no particles);
//! - the bundle's camera nodes, so the viewport can look through an authored
//!   camera exactly as it does in the editor;
//! - the project as it was before the preview, so `ExitPlayerBundle` can put
//!   it back.
//!
//! The render loop calls [`tick`] every frame, which advances the bundle's
//! clips with `update_animations` — a player's whole playback contract.

use std::cell::RefCell;
use std::collections::HashMap;

use awsm_renderer::camera::CameraParams;
use awsm_renderer::AwsmRenderer;
use awsm_renderer_editor_protocol::{NodeId, NodeKind};
use awsm_renderer_scene_loader::{LoadedScene, ParticleDriver};
use glam::Mat4;

/// The project as serialized just before the preview replaced it:
/// `project.toml` text plus its side files.
pub type ProjectStash = (String, HashMap<String, Vec<u8>>);

struct Preview {
    loaded: LoadedScene,
    particles: ParticleDriver,
    /// `(name, id)` of every camera node in the bundle, authored order.
    cameras: Vec<(String, NodeId)>,
    stash: Option<ProjectStash>,
}

thread_local! {
    static PREVIEW: RefCell<Option<Preview>> = const { RefCell::new(None) };
}

/// `(name, id)` of every Camera node in a baked scene, authored (DFS) order.
pub fn collect_cameras(nodes: &[awsm_renderer_scene::EditorNode]) -> Vec<(String, NodeId)> {
    fn walk(nodes: &[awsm_renderer_scene::EditorNode], out: &mut Vec<(String, NodeId)>) {
        for n in nodes {
            if matches!(n.kind, NodeKind::Camera(_)) {
                out.push((n.name.clone(), n.id));
            }
            walk(&n.children, out);
        }
    }
    let mut out = Vec::new();
    walk(nodes, &mut out);
    out
}

/// Start previewing `loaded`. `stash` is the project to restore on exit.
pub fn begin(loaded: LoadedScene, cameras: Vec<(String, NodeId)>, stash: Option<ProjectStash>) {
    let particles = ParticleDriver::new(&loaded);
    PREVIEW.with(|p| {
        *p.borrow_mut() = Some(Preview {
            loaded,
            particles,
            cameras,
            stash,
        })
    });
}

/// True while a player bundle is on screen.
pub fn is_active() -> bool {
    PREVIEW.with(|p| p.borrow().is_some())
}

/// The bundle camera called `name`, or with id `name`.
pub fn find_camera(name: &str) -> Option<NodeId> {
    PREVIEW.with(|p| {
        let p = p.borrow();
        let p = p.as_ref()?;
        p.cameras
            .iter()
            .find(|(n, id)| n == name || id.to_string() == name)
            .map(|(_, id)| *id)
    })
}

/// True when `id` is a camera node of the bundle on screen.
pub fn has_camera(id: NodeId) -> bool {
    PREVIEW.with(|p| {
        p.borrow()
            .as_ref()
            .is_some_and(|p| p.cameras.iter().any(|(_, c)| *c == id))
    })
}

/// `(id, name)` of the bundle's cameras — what the viewport's camera menu
/// lists during a preview.
pub fn cameras() -> Vec<(NodeId, String)> {
    PREVIEW.with(|p| {
        p.borrow()
            .as_ref()
            .map(|p| p.cameras.iter().map(|(n, id)| (*id, n.clone())).collect())
            .unwrap_or_default()
    })
}

/// The names of the bundle's cameras (for error messages).
pub fn camera_names() -> Vec<String> {
    PREVIEW.with(|p| {
        p.borrow()
            .as_ref()
            .map(|p| p.cameras.iter().map(|(n, _)| n.clone()).collect())
            .unwrap_or_default()
    })
}

/// `(view, params)` for bundle camera `id`, read from the live node transform
/// and the renderer's camera store — the same derivation a player uses, so an
/// animated camera (focus pulls, moves) is live.
pub fn camera_view_params(renderer: &AwsmRenderer, id: NodeId) -> Option<(Mat4, CameraParams)> {
    PREVIEW.with(|p| {
        let p = p.borrow();
        let handles = p.as_ref()?.loaded.nodes.get(&id)?;
        let world = *renderer.transforms.get_world(handles.transform).ok()?;
        let view = awsm_renderer::camera::view_from_world(world);
        let params = handles
            .camera
            .and_then(|key| renderer.cameras.get(key))
            .copied()
            .or_else(|| {
                handles
                    .camera_config
                    .as_ref()
                    .map(awsm_renderer_scene_loader::camera::camera_params_from_config)
            })?;
        Some((view, params))
    })
}

/// One frame of player hosting: advance the clips (when `playing`) and the
/// particle emitters.
pub fn tick(renderer: &mut AwsmRenderer, dt_ms: f64, playing: bool) {
    if playing {
        if let Err(err) = renderer.update_animations(dt_ms) {
            tracing::warn!("player preview: update_animations: {err}");
        }
    }
    PREVIEW.with(|p| {
        if let Some(p) = p.borrow_mut().as_mut() {
            p.particles.tick(renderer, (dt_ms / 1000.0) as f32);
        }
    });
}

/// End the preview: free everything the bundle load created and hand back the
/// stashed project (if one was taken) for the caller to restore.
pub fn end(renderer: &mut AwsmRenderer) -> Option<ProjectStash> {
    let preview = PREVIEW.with(|p| p.borrow_mut().take())?;
    let had_clips = !preview.loaded.clips.is_empty();
    preview.loaded.teardown(renderer);
    // The mixer referenced the bundle's clips by key.
    if had_clips {
        renderer.animations.mixer.clear();
    }
    preview.stash
}
