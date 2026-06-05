//! Recompile sink that registers the edited custom material into the unified
//! editor's **single** scene `AwsmRenderer`.
//!
//! The former standalone material-editor booted a second `AwsmRenderer` for a
//! dedicated preview ball. In the unified editor that doesn't work: renderer-
//! core caches some GPU resources (the BRDF-LUT pipeline, mipmap/blit layouts)
//! in `thread_local!`s, so a second same-thread device reuses the first
//! device's resources → cross-device GPU validation errors. So Material mode
//! shares the one scene renderer: registering here makes the material
//! immediately available to assign onto scene meshes (the Scene⇄Material
//! hand-off), and surfaces compile errors back to the Errors pane.
//!
//! The dedicated live-preview ball is deferred — it needs either device-scoped
//! renderer-core caches or single-renderer preview multiplexing.

use awsm_materials::MaterialShaderId;
use awsm_renderer::dynamic_materials::MaterialRegistration;

use crate::material::recompile::RecompileSink;

/// Registers material recompiles into the shared scene renderer, tracking the
/// previously-registered id so each apply unregisters its predecessor.
#[derive(Default)]
pub struct SceneRendererSink {
    current: Option<MaterialShaderId>,
}

impl SceneRendererSink {
    pub fn new() -> Self {
        Self::default()
    }
}

impl RecompileSink for SceneRendererSink {
    fn try_apply<'a>(
        &'a mut self,
        reg: MaterialRegistration,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + 'a>> {
        Box::pin(async move {
            crate::context::with_renderer_mut(move |renderer| {
                if let Some(prev) = self.current.take() {
                    let _ = renderer.unregister_material(prev);
                }
                match renderer.register_material(reg) {
                    Ok(id) => {
                        self.current = Some(id);
                        Ok(())
                    }
                    Err(e) => Err(format!("{e}")),
                }
            })
            .await
        })
    }
}
