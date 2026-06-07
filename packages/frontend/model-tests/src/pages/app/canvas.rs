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
                            //.with_device_request_limits(DeviceRequestLimits::typical());
                            .with_device_request_limits(DeviceRequestLimits::max_all());

                        let loading_status = state.ctx.loading_status.clone();
                        // Profile defaults — Desktop in dev and ship by
                        // default. `?mobile=true` flips the bundle to
                        // mobile-friendly defaults (MSAA off, low shadow
                        // tier, smaller atlases, BVH rebuild halved,
                        // Depth24Plus). See
                        // `awsm_renderer::profile::RendererProfile` for
                        // the full matrix.
                        let profile = awsm_web_shared::perf::resolve_renderer_profile(
                            awsm_renderer::profile::RendererProfile::Desktop,
                        );
                        let mut renderer = match AwsmRendererBuilder::new(gpu_builder)
                            .with_profile(profile)
                            .with_logging(AwsmRendererLogging {
                                // Default tier comes from build profile + `?trace=…` URL
                                // override. See `crate::logger::default_render_timings`
                                // for the policy.
                                render_timings: crate::logger::default_render_timings(),
                            })
                            .with_clear_color(Color::MID_GREY)
                            // model-tests wires .pick() to mouse-down
                            // for editor-mode click-to-select; opt in
                            // explicitly so PickResult::Disabled isn't
                            // returned on every click. The `with_features`
                            // override layers on top of the profile —
                            // profile's `features` already defaults to
                            // all-off, so this just sets `picking`.
                            .with_features(awsm_renderer::features::RendererFeatures {
                                picking: true,
                                ..Default::default()
                            })
                            .with_phase_handler(clone!(loading_status => move |phase| {
                                // Pump every builder phase transition
                                // into the loading overlay. The phase
                                // enum maps to user-facing copy in
                                // `LoadingStatus::ok_strings`.
                                loading_status.lock_mut().renderer_phase = Some(phase);
                            }))
                            .build()
                            .await {
                                Ok(renderer) => renderer,
                                Err(err) => {
                                    tracing::error!("Error initializing renderer: {:?}", err);
                                    state.ctx.loading_status.lock_mut().renderer = Err(err.to_string());
                                    return;
                                }
                            };

                        {
                            let mut status = state.ctx.loading_status.lock_mut();
                            status.renderer = Ok(false);
                            // Builder reached Ready → clear the
                            // phase row; further rows (prewarm,
                            // ibl, gltf...) drive their own status.
                            status.renderer_phase = None;
                        }

                        // Force-compile the routinely-used pipelines
                        // before the first draw. Surfaces a distinct
                        // "Compiling shaders…" line in the loading UI
                        // — most of the cold-compile cost actually
                        // lives inside `AwsmRendererBuilder::build()`
                        // above, but the prewarm hook is the
                        // documented place that will absorb extra
                        // work when the dynamic-materials sprint
                        // lands. The
                        // explicit status flag also surfaces *that
                        // the renderer is doing shader work at all*
                        // — which used to hide inside the
                        // "Initializing renderer" phase and made
                        // first-load latency feel inexplicable.
                        state.ctx.loading_status.lock_mut().shader_prewarm = Ok(true);
                        if let Err(err) = renderer.wait_for_pipelines_ready().await {
                            tracing::warn!("wait_for_pipelines_ready: {err}");
                            state.ctx.loading_status.lock_mut().shader_prewarm =
                                Err(err.to_string());
                            return;
                        }
                        state.ctx.loading_status.lock_mut().shader_prewarm = Ok(false);
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

                        // Gate the reveal on the variant + MSAA edge-resolve
                        // pipeline compiles so the first shown frame is fully
                        // specialized and anti-aliased (no black / aliased
                        // transient while pipelines warm up).
                        scene.compile_materials().await;

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
