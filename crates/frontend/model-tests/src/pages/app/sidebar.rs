mod camera;
mod editor;
mod gltf;
mod lighting;
pub mod material;
mod processing;
mod textures;

use camera::SidebarCamera;
use gltf::SidebarGltf;
use material::SidebarMaterial;

use crate::{
    models::collections::GltfId,
    pages::app::sidebar::{
        editor::SidebarEditor, lighting::SidebarLighting, processing::SidebarProcessing,
        textures::SidebarTextures,
    },
    prelude::*,
};

use super::context::AppContext;

pub struct AppSidebar {
    section: Mutable<Option<SidebarSection>>,
    ctx: AppContext,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SidebarSection {
    Gltf,
    Material,
    Animation,
    Lighting,
    Processing,
    Camera,
    Textures,
    Editor,
}

impl AppSidebar {
    pub fn new(ctx: AppContext) -> Arc<Self> {
        Arc::new(Self {
            ctx,
            section: Mutable::new(None),
        })
    }

    pub fn render(self: &Arc<Self>) -> Dom {
        static SIDEBAR: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("display", "flex")
                .style("flex-direction", "column")
                .style("margin-top", "1rem")
                .style("flex", "1")
            }
        });

        html!("div", {
            .class([&*SIDEBAR, &*USER_SELECT_NONE])
            .children([
                self.render_section(SidebarSection::Gltf),
                self.render_section(SidebarSection::Material),
                self.render_section(SidebarSection::Lighting),
                self.render_section(SidebarSection::Processing),
                self.render_section(SidebarSection::Camera),
                self.render_section(SidebarSection::Textures),
                self.render_section(SidebarSection::Editor),
                self.render_section(SidebarSection::Animation),
                Self::render_footer(),
            ])
        })
    }

    fn render_footer() -> Dom {
        static FOOTER: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("margin-top", "auto")
                .style("padding", "1rem")
                .style("display", "flex")
                .style("justify-content", "center")
            }
        });

        static GITHUB_LINK: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("display", "flex")
                .style("align-items", "center")
                .style("gap", "0.5rem")
                .style("color", ColorText::SidebarHeader.value())
                .style("text-decoration", "none")
                .style("opacity", "0.7")
                .style("transition", "opacity 0.3s")
                .style("font-size", FontSize::Md.value())
                .pseudo!(":hover", {
                    .style("opacity", "1")
                })
            }
        });

        static GITHUB_SVG: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("width", "20px")
                .style("height", "20px")
                .style("fill", "currentColor")
            }
        });

        html!("div", {
            .class(&*FOOTER)
            .child(html!("a", {
                .class(&*GITHUB_LINK)
                .attr("href", CONFIG.repo_url)
                .attr("rel", "noopener noreferrer")
                .child(svg!("svg", {
                    .class(&*GITHUB_SVG)
                    .attrs!{
                        "xmlns": "http://www.w3.org/2000/svg",
                        "viewBox": "0 0 24 24",
                    }
                    .child(svg!("path", {
                        .attr("d", "M12 0c-6.626 0-12 5.373-12 12 0 5.302 3.438 9.8 8.207 11.387.599.111.793-.261.793-.577v-2.234c-3.338.726-4.033-1.416-4.033-1.416-.546-1.387-1.333-1.756-1.333-1.756-1.089-.745.083-.729.083-.729 1.205.084 1.839 1.237 1.839 1.237 1.07 1.834 2.807 1.304 3.492.997.107-.775.418-1.305.762-1.604-2.665-.305-5.467-1.334-5.467-5.931 0-1.311.469-2.381 1.236-3.221-.124-.303-.535-1.524.117-3.176 0 0 1.008-.322 3.301 1.23.957-.266 1.983-.399 3.003-.404 1.02.005 2.047.138 3.006.404 2.291-1.552 3.297-1.23 3.297-1.23.653 1.653.242 2.874.118 3.176.77.84 1.235 1.911 1.235 3.221 0 4.609-2.807 5.624-5.479 5.921.43.372.823 1.102.823 2.222v3.293c0 .319.192.694.801.576 4.765-1.589 8.199-6.086 8.199-11.386 0-6.627-5.373-12-12-12z")
                    }))
                }))
                .child(html!("span", {
                    .text("Repository on GitHub")
                }))
            }))
        })
    }

    fn render_section(self: &Arc<Self>, section: SidebarSection) -> Dom {
        let state = self;
        html!("div", {
            .child(state.render_section_header(section))
            .child_signal(state.section.signal().map(clone!(state => move |current| {
                match current {
                    None => None,
                    Some(current) => {
                        if current == section {
                            Some(html!("div", {
                                .style("margin-left", "1rem")
                                .style("margin-bottom", "1rem")
                                .child(match section {
                                    SidebarSection::Gltf => SidebarGltf::new().render(),
                                    SidebarSection::Material => SidebarMaterial::new(state.ctx.clone()).render(),
                                    SidebarSection::Animation => render_coming_soon(),
                                    SidebarSection::Lighting =>  SidebarLighting::new(state.ctx.clone()).render(),
                                    SidebarSection::Processing => SidebarProcessing::new(state.ctx.clone()).render(),
                                    SidebarSection::Camera => SidebarCamera::new(state.ctx.clone()).render(),
                                    SidebarSection::Editor => SidebarEditor::new(state.ctx.clone()).render(),
                                    SidebarSection::Textures => SidebarTextures::new(state.ctx.clone()).render(),
                                })
                            }))
                        } else {
                            None
                        }
                    }
                }
            })))
        })
    }

    fn render_section_header(self: &Arc<Self>, section: SidebarSection) -> Dom {
        let state = self;

        static MENU_ITEM: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("font-size", FontSize::Lg.value())
                .style("color", ColorText::SidebarHeader.value())
                .style("display", "flex")
                .style("align-items", "center")
                .style("padding", "0.75rem 1rem")
                .style("gap", "0.6rem")
                .style("cursor", "pointer")
                .style("transition", "background-color 0.3s")
                .pseudo!(":hover", {
                    .style("background-color", "#636e72")
                    .style("color", "#ffffff")
                })
            }
        });

        static MENU_ITEM_SVG: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("width", FontSize::Lg.value())
                .style("height", FontSize::Lg.value())
                .style("fill", ColorText::SidebarHeader.value())
                .style("flex-shrink", "0")
            }
        });

        html!("div", {
            .apply(handle_on_click(clone!(state => move || {
                let current = state.section.get();
                if current == Some(section) {
                    state.section.set(None);
                } else {
                    state.section.set_neq(Some(section));
                }
            })))
            .class(&*MENU_ITEM)
            .child(
                svg!("svg", {
                    .class(&*MENU_ITEM_SVG)
                    .attrs!{
                        "xmlns": "http://www.w3.org/2000/svg",
                        "viewBox": "0 0 24 24",
                    }
                    .children(
                        match section {
                            SidebarSection::Gltf => {
                                vec![svg!("path", {
                                    .attr("d", "M12 2l8 4.5v9L12 22l-8-6.5v-9L12 2zm0 2.2L6 7v8l6 4.8 6-4.8V7l-6-2.8z")
                                })]
                            },
                            SidebarSection::Material => {
                                vec![svg!("path", {
                                    .attr("d", "M12 2 L4 7 L12 12 L20 7 Z M4 7 L4 17 L12 22 L12 12 Z M12 12 L12 22 L20 17 L20 7 Z")
                                })]
                            },
                            SidebarSection::Animation => {
                                vec![svg!("path", {
                                    .attr("d", "M8 5v14l11-7-11-7z")
                                })]
                            },
                            SidebarSection::Lighting => {
                                vec![svg!("path", {
                                    .attr("d", "M9 21h6v-1H9v1zm3-19a7 7 0 00-4 12.6V17a1 1 0 001 1h6a1 1 0 001-1v-2.4A7 7 0 0012 2zm3 12.7V16h-6v-1.3a5 5 0 116 0z")
                                })]
                            },
                            SidebarSection::Processing=> {
                                vec![svg!("path", {
                                    .attr("d", "M12 2a10 10 0 100 20 10 10 0 000-20zm0 18a8 8 0 110-16 8 8 0 010 16zm0-14a6 6 0 100 12 6 6 0 000-12z")
                                })]
                            },
                            SidebarSection::Camera => {
                                vec![
                                    svg!("circle", {
                                        .attrs!{ "cx": "7", "cy": "7", "r": "3",}
                                    }),
                                    svg!("circle", {
                                        .attrs!{ "cx": "15", "cy": "7", "r": "3",}
                                    }),
                                    svg!("rect", {
                                        .attrs!{ "x": "3", "y": "10", "width": "12", "height": "8", "rx": "1.2",}
                                    }),
                                    svg!("polygon", {
                                        .attr("points", "15 12 22 9 22 19 15 16")
                                    }),
                                ]
                            },
                            SidebarSection::Editor => {
                                vec![
                                    svg!("path", {
                                        .attr("d", "M3 17.25V21h3.75L17.81 9.94l-3.75-3.75L3 17.25zM20.71 7.04a.996.996 0 000-1.41l-2.34-2.34a.996.996 0 00-1.41 0l-1.83 1.83 3.75 3.75 1.83-1.83z")
                                    }),
                                ]
                            },
                            SidebarSection::Textures => {
                                vec![
                                    svg!("rect", {
                                        .attrs!{
                                            "x": "2", "y": "2", "width": "8", "height": "8",
                                            "fill": "currentColor"
                                        }
                                    }),
                                    svg!("rect", {
                                        .attrs!{
                                            "x": "10", "y": "2", "width": "8", "height": "8",
                                            "fill": "none", "stroke": "currentColor", "stroke-width": "1"
                                        }
                                    }),
                                    svg!("rect", {
                                        .attrs!{
                                            "x": "2", "y": "10", "width": "8", "height": "8",
                                            "fill": "none", "stroke": "currentColor", "stroke-width": "1"
                                        }
                                    }),
                                    svg!("rect", {
                                        .attrs!{
                                            "x": "10", "y": "10", "width": "8", "height": "8",
                                            "fill": "currentColor"
                                        }
                                    }),
                                ]
                            },
                        }
                    )
                })
            )
            .child(
                html!("span", {
                    .text(match section {
                        SidebarSection::Gltf => "Pick GLTF Model",
                        SidebarSection::Material => "Material",
                        SidebarSection::Editor => "Editor",
                        SidebarSection::Animation => "Animation",
                        SidebarSection::Lighting => "Lighting",
                        SidebarSection::Processing => "Processing",
                        SidebarSection::Camera => "Camera",
                        SidebarSection::Textures => "Textures",
                    })
                })
            )
        })
    }
}

pub fn current_model_signal() -> impl Signal<Item = Option<GltfId>> {
    Route::signal().map(|route| match route {
        Route::App(AppRoute::Model(model_id)) => Some(model_id),
        _ => None,
    })
}

pub fn render_dropdown_label(label: &str, dropdown: Dom) -> Dom {
    static CONTAINER: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("display", "flex")
            .style("flex-direction", "column")
            .style("margin", "0.75rem 1rem")
            .style("gap", "0.5rem")
        }
    });

    static LABEL: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("font-size", FontSize::Sm.value())
            .style("color", ColorText::SidebarHeader.value())
            .style("text-transform", "uppercase")
            .style("letter-spacing", "0.05em")
            .style("opacity", "0.8")
        }
    });

    html!("div", {
        .class(&*CONTAINER)
        .child(html!("div", {
            .class(&*LABEL)
            .text(label)
        }))
        .child(dropdown)
    })
}

pub fn render_checkbox_label(label: &str, checkbox: Dom) -> Dom {
    static CONTAINER: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("display", "flex")
            .style("flex-direction", "column")
            .style("margin", "0.75rem 1rem")
            .style("gap", "0.5rem")
        }
    });

    static LABEL: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("font-size", FontSize::Sm.value())
            .style("color", ColorText::SidebarHeader.value())
            .style("text-transform", "uppercase")
            .style("letter-spacing", "0.05em")
            .style("opacity", "0.8")
        }
    });

    html!("div", {
        .class(&*CONTAINER)
        .child(html!("div", {
            .class(&*LABEL)
            .text(label)
        }))
        .child(checkbox)
    })
}

fn render_coming_soon() -> Dom {
    static CONTAINER: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("display", "flex")
            .style("flex-direction", "column")
            .style("align-items", "center")
            .style("justify-content", "center")
            .style("padding", "2rem 1rem")
            .style("margin", "0.5rem")
            .style("border-radius", "8px")
            .style("border", "1px dashed rgba(255, 255, 255, 0.2)")
            .style("background", "rgba(255, 255, 255, 0.03)")
        }
    });

    static ICON: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("width", "32px")
            .style("height", "32px")
            .style("fill", ColorText::SidebarHeader.value())
            .style("opacity", "0.5")
            .style("margin-bottom", "0.75rem")
        }
    });

    static TEXT: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("font-size", FontSize::Sm.value())
            .style("color", ColorText::SidebarHeader.value())
            .style("opacity", "0.6")
            .style("text-transform", "uppercase")
            .style("letter-spacing", "0.1em")
        }
    });

    html!("div", {
        .class(&*CONTAINER)
        .child(svg!("svg", {
            .class(&*ICON)
            .attrs!{
                "xmlns": "http://www.w3.org/2000/svg",
                "viewBox": "0 0 24 24",
            }
            // Construction/wrench icon
            .child(svg!("path", {
                .attr("d", "M22.7 19l-9.1-9.1c.9-2.3.4-5-1.5-6.9-2-2-5-2.4-7.4-1.3L9 6 6 9 1.6 4.7C.4 7.1.9 10.1 2.9 12.1c1.9 1.9 4.6 2.4 6.9 1.5l9.1 9.1c.4.4 1 .4 1.4 0l2.3-2.3c.5-.4.5-1.1.1-1.4z")
            }))
        }))
        .child(html!("span", {
            .class(&*TEXT)
            .text("Coming Soon")
        }))
    })
}
