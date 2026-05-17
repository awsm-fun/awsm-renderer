use awsm_renderer::{
    core::{
        command::color::Color,
        configuration::{CanvasAlphaMode, CanvasConfiguration, CanvasToneMappingMode},
        renderer::{AwsmRendererWebGpuBuilder, DeviceRequestLimits},
    },
    debug::AwsmRendererLogging,
    AwsmRendererBuilder,
};
use wasm_bindgen_futures::spawn_local;

use crate::{pages::app::sidebar::current_model_signal, prelude::*};

use super::{context::AppContext, scene::AppScene};

pub struct AppCanvas {
    pub ctx: AppContext,
}

impl AppCanvas {
    pub fn new(ctx: AppContext) -> Arc<Self> {
        Arc::new(Self { ctx })
    }

    pub fn render(self: &Arc<Self>) -> Dom {
        let state = self;

        static FULL_AREA: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("margin", "0")
                .style("padding", "0")
                .style("position", "absolute")
                .style("top", "0")
                .style("left", "0")
                .style("width", "100%")
                .style("height", "100%")
            }
        });

        let sig = map_ref! {
            let model_id = current_model_signal(),
            let scene = state.ctx.scene.signal_cloned()
            => {
                match (model_id, scene) {
                    (Some(model_id), Some(scene)) => {
                        Some((*model_id, scene.clone()))
                    }
                    _ => {
                        None
                    }
                }
            }
        };

        html!("div", {
            .style("position", "relative")
            .style("width", "100%")
            .style("height", "100%")
            .child(html!("canvas" => web_sys::HtmlCanvasElement, {
                .class(&*CURSOR_POINTER)
                .class(&*FULL_AREA)
                .after_inserted(clone!(state => move |canvas| {
                    spawn_local(clone!(state => async move {
                        state.ctx.loading_status.lock_mut().renderer = Ok(true);
                        let gpu = web_sys::window().unwrap().navigator().gpu();
                        let gpu_builder = AwsmRendererWebGpuBuilder::new(gpu, canvas)
                            .with_configuration(CanvasConfiguration::default()
                                .with_alpha_mode(CanvasAlphaMode::Opaque)
                                .with_tone_mapping(CanvasToneMappingMode::Standard)
                            )
                            .with_device_request_limits(DeviceRequestLimits::typical());
                            //.with_device_request_limits(DeviceRequestLimits::max_all());

                        let renderer = match AwsmRendererBuilder::new(gpu_builder)
                            .with_logging(AwsmRendererLogging { render_timings: true })
                            .with_clear_color(Color::MID_GREY)
                            .build()
                            .await {
                                Ok(renderer) => renderer,
                                Err(err) => {
                                    tracing::error!("Error initializing renderer: {:?}", err);
                                    state.ctx.loading_status.lock_mut().renderer = Err(err.to_string());
                                    return;
                                }
                            };

                        state.ctx.loading_status.lock_mut().renderer = Ok(false);
                        let scene = AppScene::new(state.ctx.clone(), renderer).await.unwrap();

                        state.ctx.scene.set(Some(scene));
                    }));
                }))
            }))
            .child(html!("div", {
                .class(&*FULL_AREA)
                .class_signal(&*POINTER_EVENTS_NONE, state.ctx.loading_status.signal_ref(|loading_status| {
                    !loading_status.any_error()
                }))
                .child(html!("div", {
                    .style("padding", "1rem")
                    .class([FontSize::H3.class(), ColorText::GltfContent.class(), &*USER_SELECT_NONE])
                    .child_signal(map_ref!{
                        let loading_status = state.ctx.loading_status.signal_cloned(),
                        let gltf_id = current_model_signal()
                        => {
                            Some(if loading_status.is_loading() {
                                html!("div", {
                                    .children(loading_status.ok_strings().iter().map(|loading_status| {
                                        html!("div", {
                                            .text(loading_status)
                                        })
                                    }))
                                })
                            } else if let Some(gltf_id) = gltf_id {
                                html!("div", {
                                    .text(&format!("Showing: {}", gltf_id))
                                })
                            } else {
                                html!("div", {
                                    .text("<-- Select a model from the sidebar")
                                })
                            })
                        }
                    })
                }))
                .child_signal(state.ctx.loading_status.signal_ref(|loading_status| {
                    let errors = loading_status.err_strings();
                    if errors.is_empty() {
                        None
                    } else {
                        Some(html!("div", {
                            .style("padding", "1rem")
                            .class([FontSize::H3.class(), ColorText::Error.class()])
                            .children(errors.iter().map(|error| {
                                html!("div", {
                                    .text(error)
                                })
                            }))
                        }))
                    }
                }))
            }))
            .future(sig.for_each(clone!(state => move |data| {
                clone!(state => async move {
                    if let Some((gltf_id, scene)) = data {

                        scene.clear().await;

                        scene.wait_for_ibl_loaded().await;
                        scene.wait_for_skybox_loaded().await;

                        let loader = match scene.load_gltf(gltf_id).await {
                            Some(loader) => loader,
                            None => {
                                return;
                            }
                        };


                        scene.upload_data(gltf_id, loader).await;

                        scene.populate().await;

                        if let Err(err) = scene.setup_all().await {
                            tracing::error!("{:?}", err);
                            return;
                        }

                        scene.start_animation_loop();
                    }
                })
            })))
        })
    }
}
