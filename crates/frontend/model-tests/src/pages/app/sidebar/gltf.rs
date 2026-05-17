use crate::{
    models::collections::{GltfId, GltfSetId, GLTF_SETS},
    pages::app::sidebar::current_model_signal,
    prelude::*,
};

use super::render_dropdown_label;

pub struct SidebarGltf {}

impl SidebarGltf {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {})
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
            .child(html!("div", {
                .child_signal(current_model_signal().map(clone!(state => move |model_id| {
                    Some(html!("div", {
                        .children(
                            GltfSetId::list().into_iter().map(|set_id| {
                                state.render_gltf_selector(set_id, model_id)
                            }).collect::<Vec<_>>()
                        )
                    }))
                })))
            }))
        })
    }

    fn render_gltf_selector(
        self: &Arc<Self>,
        set_id: GltfSetId,
        initial_selected: Option<GltfId>,
    ) -> Dom {
        let options = GLTF_SETS
            .get(&set_id)
            .unwrap_throw()
            .iter()
            .map(|gltf_id| (gltf_id.label().to_string(), *gltf_id))
            .collect::<Vec<_>>();

        let initial_selected = initial_selected.and_then(|initial_selected| {
            options.iter().find_map(|(_, id)| {
                if *id == initial_selected {
                    Some(*id)
                } else {
                    None
                }
            })
        });

        render_dropdown_label(
            set_id.as_str(),
            Dropdown::new()
                .with_intial_selected(initial_selected)
                .with_bg_color(ColorBackground::Dropdown)
                .with_on_change(|id| {
                    Route::App(AppRoute::Model(*id)).go_to_url();
                })
                .with_options(options)
                .render(),
        )
    }
}
