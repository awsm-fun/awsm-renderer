use wasm_bindgen_futures::spawn_local;

use awsm_renderer_web_shared::util::free_camera::ProjectionMode;

use crate::{pages::app::context::AppContext, prelude::*};

use super::{render_dropdown_label, render_input_label};

pub struct SidebarCamera {
    ctx: AppContext,
}

impl SidebarCamera {
    pub fn new(ctx: AppContext) -> Arc<Self> {
        Arc::new(Self { ctx })
    }

    pub fn render(self: &Arc<Self>) -> Dom {
        let state = self;
        static CONTAINER: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("display", "flex")
                .style("flex-direction", "column")
            }
        });

        html!("div", {
            .class(&*CONTAINER)
            .child(state.render_camera_selector())
            .child(state.render_camera_aperture())
            .child(state.render_camera_focus_distance())
        })
    }

    fn render_camera_selector(self: &Arc<Self>) -> Dom {
        let state = self;

        render_dropdown_label(
            "Projection",
            Dropdown::new()
                .with_intial_selected(Some(state.ctx.camera_id.get()))
                .with_bg_color(ColorBackground::Dropdown)
                .with_on_change(clone!(state => move |id| {
                    state.ctx.camera_id.set_neq(*id);
                }))
                .with_options([
                    ("Orthographic".to_string(), ProjectionMode::Orthographic),
                    ("Perspective".to_string(), ProjectionMode::Perspective),
                ])
                .render(),
        )
    }

    fn render_camera_aperture(self: &Arc<Self>) -> Dom {
        let state = self;
        render_input_label(
            "Aperture (f-stop)",
            TextInput::new()
                .with_intial_value(state.ctx.camera_aperture.get().to_string())
                .with_kind(TextInputKind::Number)
                .with_on_input(clone!(state => move |value| {
                    if let Some(aperture) = value.and_then(|value| value.parse::<f32>().ok()) {
                        state.ctx.camera_aperture.set_neq(aperture);
                        spawn_local(clone!(state => async move {
                            if let Some(scene) = state.ctx.scene.get_cloned() {
                                if let Err(err) = scene.reset_camera().await {
                                    tracing::error!("Error resetting camera: {}", err);
                                }
                            }
                        }));
                    }
                }))
                .render(),
        )
    }

    fn render_camera_focus_distance(self: &Arc<Self>) -> Dom {
        let state = self;
        render_input_label(
            "Focus Distance",
            TextInput::new()
                .with_intial_value(state.ctx.camera_focus_distance.get().to_string())
                .with_kind(TextInputKind::Number)
                .with_on_input(clone!(state => move |value| {
                    if let Some(focus_distance) = value.and_then(|value| value.parse::<f32>().ok()) {
                        state.ctx.camera_focus_distance.set_neq(focus_distance);
                        spawn_local(clone!(state => async move {
                            if let Some(scene) = state.ctx.scene.get_cloned() {
                                if let Err(err) = scene.reset_camera().await {
                                    tracing::error!("Error resetting camera: {}", err);
                                }
                            }
                        }));
                    }
                }))
                .render(),
        )
    }
}
