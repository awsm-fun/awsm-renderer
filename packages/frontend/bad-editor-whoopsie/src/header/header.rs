use crate::{prelude::*, state, state::EditorMode};

use super::{
    assets, camera, environment, insert, object, project_label, settings_drawer, stats, top,
};

/// Which tab is showing in the action row (the second header row).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Insert,
    Object,
    Assets,
    Environment,
    Camera,
}

pub struct Header {
    active_section: Mutable<Section>,
    scene_stats_visible: Mutable<bool>,
    overflow_menu_open: Mutable<bool>,
}

impl Header {
    pub fn new() -> Self {
        Self {
            active_section: Mutable::new(Section::Insert),
            scene_stats_visible: Mutable::new(false),
            overflow_menu_open: Mutable::new(false),
        }
    }

    pub fn render(&self) -> Dom {
        static CONTAINER: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("flex", "0 0 auto")
                .style("display", "flex")
                .style("flex-direction", "column")
                .style("background-color", ColorBackground::Sidebar.value())
                .style("border-bottom", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
            }
        });

        let scene_stats_visible = self.scene_stats_visible.clone();
        let active_section = self.active_section.clone();
        let has_selection = state::app_state().has_selection.clone();

        let mode = state::app_state().mode.clone();

        html!("div", {
            .class([&*CONTAINER, ColorText::SidebarHeader.class(), &*USER_SELECT_NONE])
            .child(self.render_top_strip())
            // Scene-mode ribbon (Insert/Object/Environment/Camera action rows).
            // Hidden in Material mode — that workspace carries its own ribbon (M6).
            .child_signal(mode.signal().map(clone!(active_section => move |m| {
                if m == EditorMode::Scene {
                    Some(Self::render_action_row_for(active_section.clone()))
                } else {
                    None
                }
            })))
            .child_signal(scene_stats_visible.signal().map(clone!(scene_stats_visible => move |visible| {
                if visible {
                    Some(stats::render_stats_panel(scene_stats_visible.clone()))
                } else {
                    None
                }
            })))
            // If the Object tab is active and selection goes away, fall back
            // to Insert so users aren't stranded on a row of disabled buttons.
            .future(clone!(active_section, has_selection => async move {
                has_selection.signal().for_each(move |has| {
                    if !has && active_section.get() == Section::Object {
                        active_section.set(Section::Insert);
                    }
                    async {}
                }).await;
            }))
            // Settings drawer overlay (opened by the ⚙ button); renders
            // nothing until open.
            .child(settings_drawer::render())
        })
    }

    fn render_top_strip(&self) -> Dom {
        static STRIP: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("height", "3.15rem")
                .style("display", "flex")
                .style("align-items", "stretch")
                .style("background-color", ColorRaw::Darkest.value())
                .style("border-bottom", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
            }
        });

        let mode = state::app_state().mode.clone();

        html!("div", {
            .class(&*STRIP)
            .child(Self::render_brand_and_mode())
            // Scene-mode section tabs — hidden in Material mode.
            .child(html!("div", {
                .style("display", "flex")
                .style("align-items", "stretch")
                .style("height", "100%")
                .style_signal("display", mode.signal().map(|m| {
                    if m == EditorMode::Scene { "flex" } else { "none" }
                }))
                .child(self.render_section_tabs())
            }))
            .child(self.render_project_label())
            .child(self.render_right_cluster())
        })
    }

    /// Brand mark + the top-level Scene ⇄ Material segmented switch.
    fn render_brand_and_mode() -> Dom {
        let mode = state::app_state().mode.clone();
        html!("div", {
            .style("display", "flex")
            .style("align-items", "center")
            .style("gap", "12px")
            .style("padding", "0 14px")
            .child(html!("div", {
                .style("display", "flex")
                .style("align-items", "center")
                .style("gap", "9px")
                .child(html!("div", {
                    .style("width", "26px")
                    .style("height", "26px")
                    .style("border-radius", "7px")
                    .style("flex", "0 0 auto")
                    .style("background", "linear-gradient(145deg, var(--accent-bright), var(--accent-dim))")
                    .style("box-shadow", "inset 0 1px 0 oklch(1 0 0 / .25), var(--shadow-1)")
                }))
                .child(html!("span", {
                    .style("font-size", "13px")
                    .style("font-weight", "680")
                    .style("letter-spacing", "-0.01em")
                    .style("color", "var(--text-0)")
                    .text("Awsm")
                    .child(html!("span", {
                        .style("color", "var(--text-2)")
                        .style("font-weight", "500")
                        .text("Renderer")
                    }))
                }))
            }))
            .child(html!("div", {
                .style("width", "1px")
                .style("height", "22px")
                .style("background", "var(--line)")
            }))
            .child(
                Segmented::new(mode)
                    .option(EditorMode::Scene, "Scene")
                    .option(EditorMode::Material, "Material")
                    .render()
            )
            .child(Self::render_settings_button())
            .child(Self::render_cmdk_button())
        })
    }

    /// ⌘K search button — opens the command palette.
    fn render_cmdk_button() -> Dom {
        let cmdk_open = state::app_state().cmdk_open.clone();
        html!("button", {
            .class("t")
            .style("display", "flex")
            .style("align-items", "center")
            .style("gap", "8px")
            .style("height", "28px")
            .style("padding", "0 9px 0 11px")
            .style("cursor", "pointer")
            .style("border", "1px solid var(--line-soft)")
            .style("border-radius", "var(--r2)")
            .style("background", "var(--bg-3)")
            .style("color", "var(--text-2)")
            .style("font-size", "12px")
            .child(html!("span", { .style("color", "var(--text-3)").text("⌕") }))
            .child(html!("span", {
                .style("min-width", "60px")
                .style("text-align", "left")
                .text("Search…")
            }))
            .child(html!("span", {
                .class("mono")
                .style("font-size", "10px")
                .style("color", "var(--text-3)")
                .style("border", "1px solid var(--line)")
                .style("border-radius", "4px")
                .style("padding", "1px 5px")
                .text("⌘K")
            }))
            .event(clone!(cmdk_open => move |_: events::Click| cmdk_open.set_neq(true)))
        })
    }

    /// ⚙ button next to the mode switch — toggles the Settings drawer.
    fn render_settings_button() -> Dom {
        let settings_open = state::app_state().settings_open.clone();
        html!("button", {
            .class("t")
            .style("display", "inline-flex")
            .style("align-items", "center")
            .style("justify-content", "center")
            .style("width", "28px")
            .style("height", "28px")
            .style("border-radius", "var(--r2)")
            .style("cursor", "pointer")
            .style("font-size", "15px")
            .style("line-height", "1")
            .style_signal("color", settings_open.signal().map(|o| {
                if o { "var(--text-0)" } else { "var(--text-2)" }
            }))
            .style_signal("background", settings_open.signal().map(|o| {
                if o { "var(--bg-active)" } else { "transparent" }
            }))
            .attr("title", "Settings")
            .text("⚙")
            .event(clone!(settings_open => move |_: events::Click| {
                settings_open.set_neq(!settings_open.get());
            }))
        })
    }

    fn render_project_label(&self) -> Dom {
        project_label::render()
    }

    fn render_section_tabs(&self) -> Dom {
        static TABS: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("display", "flex")
                .style("align-items", "stretch")
                .style("height", "100%")
                .style("background-color", ColorBackground::Sidebar.value())
            }
        });

        html!("div", {
            .class(&*TABS)
            .child(self.render_section_tab(Section::Insert, "Insert"))
            .child(self.render_section_tab(Section::Object, "Object"))
            .child(self.render_section_tab(Section::Assets, "Assets"))
            .child(self.render_section_tab(Section::Environment, "Environment"))
            .child(self.render_section_tab(Section::Camera, "Camera"))
        })
    }

    fn render_section_tab(&self, section: Section, label: &'static str) -> Dom {
        static TAB: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("display", "inline-flex")
                .style("align-items", "center")
                .style("justify-content", "center")
                .style("height", "100%")
                .style("min-width", "10rem")
                .style("padding", "0 1.35rem")
                .style("margin", "0")
                .style("border", "0")
                .style("border-radius", "0")
                .style("border-right", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
                .style("font-size", "1rem")
                .style("font-weight", "700")
                .style("letter-spacing", "0.0125em")
            }
        });

        let active_section = self.active_section.clone();
        let has_selection = state::app_state().has_selection.clone();
        let is_object = section == Section::Object;

        html!("button", {
            .class(&*TAB)
            .style_signal("background-color", active_section.signal().map(move |selected| {
                if selected == section {
                    ColorBackground::ButtonPrimary.value()
                } else {
                    ColorBackground::Sidebar.value()
                }
            }))
            .style_signal("color", active_section.signal().map(move |selected| {
                if selected == section {
                    ColorText::ButtonPrimary.value()
                } else {
                    ColorText::SidebarHeader.value()
                }
            }))
            .style_signal("box-shadow", active_section.signal().map(move |selected| {
                if selected == section {
                    format!("inset 0 -3px 0 {}", ColorBackground::UnderlinePrimary.value())
                } else {
                    format!("inset 0 -1px 0 {}", ColorBackground::UnderlineSecondary.value())
                }
            }))
            .style_signal("opacity", has_selection.signal().map(move |has| {
                if is_object && !has { "0.5" } else { "1.0" }
            }))
            .style_signal("cursor", has_selection.signal().map(move |has| {
                if is_object && !has { "not-allowed" } else { "pointer" }
            }))
            .text(label)
            .event(clone!(active_section, has_selection => move |_: events::Click| {
                if is_object && !has_selection.get() {
                    return;
                }
                active_section.set(section);
            }))
        })
    }

    fn render_right_cluster(&self) -> Dom {
        static CLUSTER: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("display", "flex")
                .style("gap", "0.5rem")
                .style("align-items", "center")
                .style("margin-left", "auto")
                .style("padding", "0 0.55rem")
            }
        });

        html!("div", {
            .class(&*CLUSTER)
            .child(top::render_new())
            .child(top::render_save())
            .child(top::render_load())
            .child(top::divider())
            .child(top::render_undo())
            .child(top::render_redo())
            .child(self.render_overflow_menu())
        })
    }

    fn render_overflow_menu(&self) -> Dom {
        static TRIGGER: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("display", "inline-flex")
                .style("align-items", "center")
                .style("justify-content", "center")
                .style("width", "2.2rem")
                .style("height", "2.2rem")
                .style("border-radius", "0.5rem")
                .style("border", "0")
                .style("background", "transparent")
                .style("color", ColorText::SidebarHeader.value())
                .style("cursor", "pointer")
                .style("font-size", "1.25rem")
                .style("line-height", "1")
                .pseudo!(":hover", {
                    .style("background", ColorBackground::SidebarSelected.value())
                })
            }
        });

        let overflow_menu_open = self.overflow_menu_open.clone();
        let scene_stats_visible = self.scene_stats_visible.clone();
        let missing_assets = state::app_state().missing_assets.clone();

        html!("div", {
            .style("position", "relative")
            .child(html!("button", {
                .class(&*TRIGGER)
                .style("position", "relative")
                .text("⋯")
                .event(clone!(overflow_menu_open => move |_: events::Click| {
                    overflow_menu_open.set(!overflow_menu_open.get());
                }))
                // Tiny red dot in the upper-right when at least one asset
                // failed to load — surfaces the issue without forcing a
                // modal back open.
                .child_signal(missing_assets.signal_ref(|m| !m.is_empty()).dedupe().map(|has_missing| {
                    if has_missing {
                        Some(html!("span", {
                            .style("position", "absolute")
                            .style("top", "0.35rem")
                            .style("right", "0.35rem")
                            .style("width", "0.5rem")
                            .style("height", "0.5rem")
                            .style("border-radius", "50%")
                            .style("background", ColorRaw::Red.value())
                            .style("box-shadow", "0 0 0 1.5px rgba(0,0,0,0.45)")
                            .style("pointer-events", "none")
                        }))
                    } else {
                        None
                    }
                }))
            }))
            .child_signal(overflow_menu_open.signal().map(clone!(overflow_menu_open, scene_stats_visible => move |open| {
                if open {
                    Some(top::render_overflow_popup(overflow_menu_open.clone(), scene_stats_visible.clone()))
                } else {
                    None
                }
            })))
        })
    }

    fn render_action_row_for(active_section: Mutable<Section>) -> Dom {
        static ROW: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("display", "flex")
                .style("align-items", "center")
                .style("gap", "0.75rem")
                .style("padding", "0.55rem 0.6rem 0.65rem 0.6rem")
                .style("background-color", ColorBackground::Sidebar.value())
                .style("min-height", "2.75rem")
                .style("box-sizing", "border-box")
            }
        });

        html!("div", {
            .class(&*ROW)
            .child_signal(active_section.signal().map(move |section| {
                Some(match section {
                    Section::Insert => insert::render_insert_row(),
                    Section::Object => object::render_object_row(),
                    Section::Assets => assets::render_assets_row(),
                    Section::Environment => environment::render_environment_row(),
                    Section::Camera => camera::render_camera_row(),
                })
            }))
        })
    }
}
