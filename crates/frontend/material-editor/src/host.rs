//! Renderer host for the material-editor.
//!
//! Owns the live `AwsmRenderer` + the currently-registered material's
//! `MaterialShaderId`. Implements [`RecompileSink`] so the debounced
//! recompile loop in [`crate::recompile`] can swap in new
//! [`MaterialRegistration`]s as the user edits.
//!
//! The render loop itself lives in `main.rs`'s `spawn_local` — this
//! file just provides the shared state + the registration plumbing.

use std::cell::RefCell;
use std::rc::Rc;

use awsm_materials::MaterialShaderId;
use awsm_renderer::dynamic_materials::MaterialRegistration;
use awsm_renderer::AwsmRenderer;

use crate::recompile::RecompileSink;

/// Shared renderer state. The `Option` is `None` between page load
/// and the async `AwsmRendererBuilder::build` completing.
pub type RendererHandle = Rc<RefCell<Option<RendererHost>>>;

/// Owned renderer + current material registration.
pub struct RendererHost {
    /// The live renderer driving the preview canvas.
    pub renderer: AwsmRenderer,
    /// The shader_id of the most recently successfully-registered
    /// material. `None` between init and the first registration.
    pub current_material: Option<MaterialShaderId>,
}

impl RendererHost {
    /// Construct a new host wrapping an already-built renderer.
    pub fn new(renderer: AwsmRenderer) -> Self {
        Self {
            renderer,
            current_material: None,
        }
    }
}

/// Sink wrapping a [`RendererHandle`] that the recompile loop drives.
///
/// On `try_apply`:
/// 1. If a previous material was registered, attempt to unregister
///    it. (Failures here are logged but non-fatal — registration
///    will overwrite.)
/// 2. Call `register_material` with the new payload. On success,
///    record the new shader_id as `current_material`. On
///    `WgslCompile` error, leave `current_material` untouched so the
///    preview continues drawing the last-good shader.
/// 3. Call `prewarm_pipelines()` so the classify + per-shader-id
///    opaque pipelines are warm before the next render frame.
pub struct RendererRecompileSink {
    handle: RendererHandle,
}

impl RendererRecompileSink {
    /// Construct a sink wrapping the shared renderer handle.
    pub fn new(handle: RendererHandle) -> Self {
        Self { handle }
    }
}

impl RecompileSink for RendererRecompileSink {
    fn try_apply(&mut self, reg: MaterialRegistration) -> Result<(), String> {
        let mut guard = self.handle.borrow_mut();
        let host = match guard.as_mut() {
            Some(h) => h,
            None => {
                // Renderer not yet booted. Defer silently — the next
                // edit after boot will pick this up.
                return Ok(());
            }
        };

        // Best-effort unregister of the previous material. A live
        // mesh referencing the id would block; in the material-editor
        // the stub mesh re-binds after each registration so this is
        // safe.
        if let Some(prev_id) = host.current_material.take() {
            if let Err(e) = host.renderer.unregister_material(prev_id) {
                tracing::warn!(
                    "[material-editor] unregister_material({:?}) failed: {e:?}",
                    prev_id
                );
            }
        }

        // Register the new material. WgslCompile errors propagate
        // back through the recompile sink as Err strings.
        let new_id = match host.renderer.register_material(reg) {
            Ok(id) => id,
            Err(e) => {
                return Err(format!("{e}"));
            }
        };
        host.current_material = Some(new_id);

        // prewarm_pipelines is async; for the editor's preview
        // it's fine to fire-and-forget on the JS event loop. The
        // next render frame after compilation completes picks up
        // the new pipelines.
        let handle = self.handle.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let mut guard = handle.borrow_mut();
            if let Some(host) = guard.as_mut() {
                if let Err(e) = host.renderer.prewarm_pipelines().await {
                    tracing::warn!("[material-editor] prewarm_pipelines failed: {e:?}");
                }
            }
        });

        Ok(())
    }
}
