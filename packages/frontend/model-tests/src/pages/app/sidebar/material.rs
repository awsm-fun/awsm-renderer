use awsm_renderer::materials::pbr::PbrMaterialDebug;
use wasm_bindgen_futures::spawn_local;

use crate::{
    pages::app::{context::AppContext, sidebar::render_dropdown_label},
    prelude::*,
};

pub struct SidebarMaterial {
    ctx: AppContext,
}

impl SidebarMaterial {
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
            .child(state.render_debug_selector())
        })
    }
    fn render_debug_selector(self: &Arc<Self>) -> Dom {
        let state = self;

        render_dropdown_label(
            "Debug",
            Dropdown::new()
                .with_intial_selected(Some(state.ctx.material_debug.get_cloned()))
                .with_bg_color(ColorBackground::Dropdown)
                .with_on_change(clone!(state => move |debug| {
                    state.ctx.material_debug.set_neq(*debug);

                    spawn_local(clone!(state => async move {
                        if let Some(scene) = state.ctx.scene.get_cloned() {
                            if let Err(err) = scene.reset_material_debug().await {
                                tracing::error!("Error resetting material debug: {}", err);
                            }
                        }
                    }));
                }))
                .with_options([
                    ("None".to_string(), PbrMaterialDebug::None),
                    ("Normals".to_string(), PbrMaterialDebug::Normals),
                    ("Base Color".to_string(), PbrMaterialDebug::BaseColor),
                    (
                        "Metallic Roughness".to_string(),
                        PbrMaterialDebug::MetallicRoughness,
                    ),
                    ("Occlusion".to_string(), PbrMaterialDebug::Occlusion),
                    ("Emissive".to_string(), PbrMaterialDebug::Emissive),
                    ("Specular".to_string(), PbrMaterialDebug::Specular),
                ])
                .render(),
        )
    }
}

#[derive(Clone, Debug, Copy, PartialEq, Eq, Hash)]
pub enum FragmentShaderKind {
    Pbr,
    DebugNormals,
}
