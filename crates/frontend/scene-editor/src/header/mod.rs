//! Top header bar. Two stacked rows:
//!
//! 1. **Top strip** — section tabs (Insert / Object / Assets /
//!    Environment / Camera / Editor) on the left, project actions
//!    (`New` / `Save` / `Load` / `Build` / `Undo` / `Redo` / `⋯`) on
//!    the right.
//! 2. **Action row** — buttons for the currently-active tab.
//!
//! The header is *layout only* — every button delegates to a function in
//! `crate::actions::*`. Reactive signals (`has_selection`, `dirty`,
//! `can_undo`, `can_redo`) come from `state::app_state()` and drive
//! enabled / disabled / accent styling.
//!
//! Per-section layout lives in sibling submodules so each section
//! file stays scannable.

mod assets;
mod camera;
mod editor;
mod environment;
mod insert;
mod menu;
mod object;
mod project_label;
pub(crate) mod shadows_config;
mod stats;
mod top;

use crate::{prelude::*, state};

/// Which tab is showing in the action row (the second header row).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Insert,
    Object,
    Assets,
    Environment,
    Camera,
    Editor,
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

        html!("div", {
            .class([&*CONTAINER, ColorText::SidebarHeader.class(), &*USER_SELECT_NONE])
            .child(self.render_top_strip())
            .child(self.render_action_row())
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

        html!("div", {
            .class(&*STRIP)
            .child(self.render_section_tabs())
            .child(self.render_project_label())
            .child(self.render_right_cluster())
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
            .child(self.render_section_tab(Section::Editor, "Editor"))
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

    fn render_action_row(&self) -> Dom {
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

        let active_section = self.active_section.clone();

        html!("div", {
            .class(&*ROW)
            .child_signal(active_section.signal().map(move |section| {
                Some(match section {
                    Section::Insert => insert::render_insert_row(),
                    Section::Object => object::render_object_row(),
                    Section::Assets => assets::render_assets_row(),
                    Section::Editor => editor::render_editor_row(),
                    Section::Environment => environment::render_environment_row(),
                    Section::Camera => camera::render_camera_row(),
                })
            }))
        })
    }
}
