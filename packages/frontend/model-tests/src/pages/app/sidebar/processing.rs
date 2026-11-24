use awsm_renderer::post_process::ToneMapping;
use wasm_bindgen_futures::spawn_local;

use crate::{
    atoms::{
        checkbox::{Checkbox, CheckboxStyle},
        text_input::{TextInput, TextInputKind},
    },
    config::AntiAliasMode,
    pages::app::{
        context::AppContext,
        sidebar::{render_dropdown_label, render_input_label},
    },
    prelude::*,
};

pub struct SidebarProcessing {
    ctx: AppContext,
}

impl SidebarProcessing {
    pub fn new(ctx: AppContext) -> Arc<Self> {
        Arc::new(Self { ctx })
    }

    pub fn render(self: &Arc<Self>) -> Dom {
        let state = self;

        static CONTAINER: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("display", "flex")
                .style("flex-direction", "column")
                .style("align-items", "flex-start")
                .style("gap", "1rem")
            }
        });

        html!("div", {
            .class(&*CONTAINER)
            .child(state.render_tonemapping_selector())
            .child(state.render_exposure_input())
            .child(state.render_bloom_selector())
            .child(state.render_dof_selector())
            .child(state.render_anti_alias_selector())
        })
    }

    fn render_exposure_input(self: &Arc<Self>) -> Dom {
        let state = self;
        render_input_label(
            "Exposure (EV)",
            TextInput::new()
                .with_intial_value(state.ctx.post_processing.get_cloned().exposure.to_string())
                .with_kind(TextInputKind::Number)
                .with_on_input(clone!(state => move |value| {
                    if let Some(ev) = value.and_then(|value| value.parse::<f32>().ok()) {
                        let mut pp = state.ctx.post_processing.get_cloned();
                        pp.exposure = ev;
                        state.ctx.post_processing.set_neq(pp);
                        spawn_local(clone!(state => async move {
                            if let Some(scene) = state.ctx.scene.get_cloned() {
                                if let Err(err) = scene.reset_post_processing().await {
                                    tracing::error!("Error resetting post_processing: {}", err);
                                }
                            }
                        }));
                    }
                }))
                .render(),
        )
    }

    fn render_anti_alias_selector(self: &Arc<Self>) -> Dom {
        let state = self;
        render_dropdown_label(
            "Anti-aliasing",
            Dropdown::new()
                .with_intial_selected(AntiAliasMode::from_anti_aliasing(
                    &state.ctx.anti_alias.get_cloned(),
                ))
                .with_bg_color(ColorBackground::Dropdown)
                .with_on_change(clone!(state => move |mode: &AntiAliasMode| {
                    state.ctx.anti_alias.set_neq(mode.anti_aliasing());

                    spawn_local(clone!(state => async move {
                        if let Some(scene) = state.ctx.scene.get_cloned() {
                            if let Err(err) = scene.reset_anti_aliasing().await {
                                tracing::error!("Error resetting anti_aliasing: {}", err);
                            }
                        }
                    }));
                }))
                .with_options(AntiAliasMode::ALL.map(|mode| (mode.label().to_string(), mode)))
                .render(),
        )
    }

    fn render_bloom_selector(self: &Arc<Self>) -> Dom {
        let state = self;

        Checkbox::new(CheckboxStyle::Dark)
            .with_content_after(html!("span", {
                .text("Bloom effect")
            }))
            .with_selected_signal(
                state
                    .ctx
                    .post_processing
                    .signal_ref(|post_processing| post_processing.bloom),
            )
            .with_on_click(clone!(state => move || {
                {
                    let mut lock = state.ctx.post_processing.lock_mut();
                    lock.bloom = !lock.bloom;
                }

                spawn_local(clone!(state => async move {
                    if let Some(scene) = state.ctx.scene.get_cloned() {
                        if let Err(err) = scene.reset_post_processing().await {
                            tracing::error!("Error resetting post processing: {}", err);
                        }
                    }
                }));
            }))
            .render()
    }

    fn render_dof_selector(self: &Arc<Self>) -> Dom {
        let state = self;

        Checkbox::new(CheckboxStyle::Dark)
            .with_content_after(html!("span", {
                .text("DoF effect")
            }))
            .with_selected_signal(
                state
                    .ctx
                    .post_processing
                    .signal_ref(|post_processing| post_processing.dof),
            )
            .with_on_click(clone!(state => move || {
                {
                    let mut lock = state.ctx.post_processing.lock_mut();
                    lock.dof = !lock.dof;
                }

                spawn_local(clone!(state => async move {
                    if let Some(scene) = state.ctx.scene.get_cloned() {
                        if let Err(err) = scene.reset_post_processing().await {
                            tracing::error!("Error resetting post processing: {}", err);
                        }
                    }
                }));
            }))
            .render()
    }

    fn render_tonemapping_selector(self: &Arc<Self>) -> Dom {
        let state = self;

        render_dropdown_label(
            "Tonemapping",
            Dropdown::new()
                .with_intial_selected(Some(state.ctx.post_processing.get_cloned().tonemapping))
                .with_bg_color(ColorBackground::Dropdown)
                .with_on_change(clone!(state => move |tonemapping| {
                    let mut post_procesing = state.ctx.post_processing.get_cloned();
                    post_procesing.tonemapping = *tonemapping;
                    state.ctx.post_processing.set_neq(post_procesing);

                    spawn_local(clone!(state => async move {
                        if let Some(scene) = state.ctx.scene.get_cloned() {
                            if let Err(err) = scene.reset_post_processing().await {
                                tracing::error!("Error resetting post_processing: {}", err);
                            }
                        }
                    }));
                }))
                .with_options([
                    ("None".to_string(), ToneMapping::None),
                    (
                        "Khronos PBR Neutral".to_string(),
                        ToneMapping::KhronosNeutralPbr,
                    ),
                    ("Aces".to_string(), ToneMapping::Aces),
                ])
                .render(),
        )
    }
}
