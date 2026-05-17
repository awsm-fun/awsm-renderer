use awsm_renderer::post_process::ToneMapping;
use wasm_bindgen_futures::spawn_local;

use crate::{
    atoms::checkbox::{Checkbox, CheckboxStyle},
    pages::app::{context::AppContext, sidebar::render_dropdown_label},
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
            .child(state.render_bloom_selector())
            .child(state.render_dof_selector())
            .child(state.render_msaa_selector())
            .child(state.render_smaa_selector())
        })
    }

    fn render_msaa_selector(self: &Arc<Self>) -> Dom {
        let state = self;

        Checkbox::new(CheckboxStyle::Dark)
            .with_content_after(html!("span", {
                .text("MSAA Anti-Aliasing")
            }))
            .with_selected_signal(
                state
                    .ctx
                    .anti_alias
                    .signal_ref(|anti_alias| anti_alias.msaa_sample_count.is_some()),
            )
            .with_on_click(clone!(state => move || {
                {
                    let mut lock = state.ctx.anti_alias.lock_mut();
                    lock.msaa_sample_count = if lock.msaa_sample_count.is_some() {
                        None
                    } else {
                        Some(4)
                    };
                }

                spawn_local(clone!(state => async move {
                    if let Some(scene) = state.ctx.scene.get_cloned() {
                        if let Err(err) = scene.reset_anti_aliasing().await {
                            tracing::error!("Error resetting anti_aliasing: {}", err);
                        }
                    }
                }));
            }))
            .render()
    }

    fn render_smaa_selector(self: &Arc<Self>) -> Dom {
        let state = self;

        Checkbox::new(CheckboxStyle::Dark)
            .with_content_after(html!("span", {
                .text("SMAA Anti-Aliasing")
            }))
            .with_selected_signal(
                state
                    .ctx
                    .anti_alias
                    .signal_ref(|anti_alias| anti_alias.smaa),
            )
            .with_on_click(clone!(state => move || {
                {
                    let mut lock = state.ctx.anti_alias.lock_mut();
                    lock.smaa = !lock.smaa;
                }

                spawn_local(clone!(state => async move {
                    if let Some(scene) = state.ctx.scene.get_cloned() {
                        if let Err(err) = scene.reset_anti_aliasing().await {
                            tracing::error!("Error resetting anti_aliasing: {}", err);
                        }
                    }
                }));
            }))
            .render()
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
