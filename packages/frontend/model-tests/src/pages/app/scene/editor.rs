use crate::{pages::app::scene::camera::Camera, prelude::*};
use anyhow::Result;
use awsm_renderer::{render::RenderHooks, AwsmRenderer};
use dominator_helpers::futures::AsyncLoader;
use futures::StreamExt;

use awsm_web_shared::viewport3d::{
    grid::{pipelines::EditorPipelines, render::render_grid},
    transform_controller::{TransformController, TransformObject},
};

#[derive(Clone)]
pub struct AppSceneEditor {
    pub pipelines: Arc<EditorPipelines>,
    pub render_hooks: Arc<std::sync::RwLock<Option<Arc<RenderHooks>>>>,
    pub transform_controller: Arc<std::sync::Mutex<Option<TransformController>>>,
    pub selected_object: Mutable<Option<TransformObject>>,
    grid_enabled: Mutable<bool>,
    gizmo_translation_enabled: Mutable<bool>,
    gizmo_rotation_enabled: Mutable<bool>,
    gizmo_scale_enabled: Mutable<bool>,
    reactor: AsyncLoader,
}

impl AppSceneEditor {
    pub async fn new(
        renderer: Arc<futures::lock::Mutex<AwsmRenderer>>,
        camera: Arc<std::sync::Mutex<Option<Camera>>>,
        grid_enabled: Mutable<bool>,
        gizmo_translation_enabled: Mutable<bool>,
        gizmo_rotation_enabled: Mutable<bool>,
        gizmo_scale_enabled: Mutable<bool>,
    ) -> Result<Self> {
        let pipelines = Arc::new(EditorPipelines::load(&mut *renderer.lock().await).await?);

        let render_hooks = Arc::new(std::sync::RwLock::new(None));

        let transform_controller: Arc<std::sync::Mutex<Option<TransformController>>> =
            Arc::new(std::sync::Mutex::new(None));
        let selected_object = Mutable::new(None);

        let reactor = AsyncLoader::new();

        reactor.load(clone!(grid_enabled, gizmo_translation_enabled, gizmo_rotation_enabled, gizmo_scale_enabled, selected_object, render_hooks, pipelines, renderer, transform_controller => async move {

            let mut stream = map_ref! {
                let grid_enabled = grid_enabled.signal(),
                let gizmo_translation_enabled = gizmo_translation_enabled.signal(),
                let gizmo_rotation_enabled = gizmo_rotation_enabled.signal(),
                let gizmo_scale_enabled = gizmo_scale_enabled.signal(),
                let selected_object = selected_object.signal()
                => (*grid_enabled, *gizmo_translation_enabled, *gizmo_rotation_enabled, *gizmo_scale_enabled, *selected_object)
            }.to_stream();

            while let Some((grid_enabled, gizmo_translation_enabled, gizmo_rotation_enabled, gizmo_scale_enabled, selected_transform_key)) = stream.next().await {
                {
                    let mut render_hooks = render_hooks.write().unwrap();

                    *render_hooks = Some(Arc::new(RenderHooks {
                        pre_render: Some(Box::new(clone!(transform_controller, camera => move |renderer| {
                            #[allow(clippy::single_match)]
                            match (transform_controller.lock().unwrap().as_mut(), camera.lock().unwrap().as_ref()) {
                                (Some(transform_controller), Some(camera)) => {
                                    transform_controller.zoom_gizmo_transforms(renderer, &camera.matrices())?;
                                }
                                _ => {}
                            }

                            Ok(())
                        }))),
                        before_transparent_pass: if grid_enabled {
                            let grid_bind_group = pipelines.grid_bind_group.clone();
                            let grid_pipeline_msaa_4_key = pipelines.grid_pipeline_msaa_4_key;
                            let grid_pipeline_singlesampled_key = pipelines.grid_pipeline_singlesampled_key;

                            Some(Box::new(move |ctx| {
                                let grid_pipeline_key = match ctx.anti_aliasing.msaa_sample_count {
                                    Some(4) => grid_pipeline_msaa_4_key,
                                    None => grid_pipeline_singlesampled_key,
                                    _ => panic!("Unsupported MSAA sample count for grid pipeline"),
                                };

                                render_grid(
                                    ctx,
                                    &grid_bind_group,
                                    grid_pipeline_key,
                                )
                            }))
                        } else {
                            None
                        },
                        ..Default::default()
                    }));
                }

                {
                    let renderer = &mut *renderer.lock().await;
                    if let Some(transform_controller) = transform_controller.lock().unwrap().as_mut() {
                        // Anchor the gizmo to the current selection. (Object
                        // selection is set on the `selected_object` Mutable by the
                        // pointer handler; the controller's own `selected_object`
                        // is what `zoom_gizmo_transforms` reads to place + show the
                        // gizmo, so keep the two in sync here.)
                        transform_controller.selected_object = selected_transform_key;
                        if selected_transform_key.is_some() {
                            if let Err(err) = transform_controller.set_hidden(renderer, !gizmo_translation_enabled, !gizmo_rotation_enabled, !gizmo_scale_enabled) {
                                tracing::error!("Error setting transform controller enabled state: {}", err);
                            }
                        } else if let Err(err) = transform_controller.set_hidden(renderer, true, true, true) {
                            tracing::error!("Error setting transform controller enabled state: {}", err);
                        }
                    }
                }
            }
        }));

        Ok(Self {
            render_hooks,
            pipelines,
            grid_enabled,
            gizmo_translation_enabled,
            gizmo_rotation_enabled,
            gizmo_scale_enabled,
            selected_object,
            reactor,
            transform_controller,
        })
    }
}
