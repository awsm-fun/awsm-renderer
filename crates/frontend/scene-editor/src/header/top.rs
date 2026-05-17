//! Top-strip right-hand cluster — project actions (`New` / `Save` /
//! `Load` / `Undo` / `Redo`) plus the `⋯` overflow menu with its
//! assorted modals (About, Clear All, Missing assets).

use super::menu::{
    render_menu_button, render_menu_button_owned, render_menu_checkbox, render_menu_separator,
    render_popup_backdrop,
};
use crate::{actions, prelude::*, state};

pub(super) fn divider() -> Dom {
    html!("div", {
        .style("width", "1px")
        .style("height", "1.5rem")
        .style("background-color", ColorBackground::UnderlineSecondary.value())
        .style("margin", "0 0.15rem")
    })
}

pub(super) fn render_new() -> Dom {
    Button::new()
        .with_text("New")
        .with_style(ButtonStyle::Outline)
        .with_size(ButtonSize::Sm)
        .with_on_click(actions::project::new_project)
        .render()
}

pub(super) fn render_save() -> Dom {
    // Disabled when there's nothing to save; filled accent when there are
    // unsaved changes, so it pulls the eye exactly when you want it to.
    let dirty = state::app_state().dirty.clone();
    Button::new()
        .with_text("Save")
        .with_size(ButtonSize::Sm)
        .with_disabled_signal(dirty.signal().map(|d| !d))
        .with_on_click(actions::project::save)
        .render()
}

pub(super) fn render_load() -> Dom {
    Button::new()
        .with_text("Load")
        .with_style(ButtonStyle::Outline)
        .with_size(ButtonSize::Sm)
        .with_on_click(actions::project::load)
        .render()
}

pub(super) fn render_undo() -> Dom {
    let can_undo = state::app_state().can_undo.clone();
    Button::new()
        .with_text("Undo")
        .with_style(ButtonStyle::Outline)
        .with_size(ButtonSize::Sm)
        .with_disabled_signal(can_undo.signal().map(|can| !can))
        .with_on_click(actions::history::undo)
        .render()
}

pub(super) fn render_redo() -> Dom {
    let can_redo = state::app_state().can_redo.clone();
    Button::new()
        .with_text("Redo")
        .with_style(ButtonStyle::Outline)
        .with_size(ButtonSize::Sm)
        .with_disabled_signal(can_redo.signal().map(|can| !can))
        .with_on_click(actions::history::redo)
        .render()
}

pub(super) fn render_overflow_popup(
    open: Mutable<bool>,
    scene_stats_visible: Mutable<bool>,
) -> Dom {
    static POPUP: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("position", "absolute")
            .style("top", "100%")
            .style("right", "0")
            .style("margin-top", "0.3rem")
            .style("min-width", "12rem")
            .style("background-color", ColorBackground::Sidebar.value())
            .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
            .style("border-radius", "0.4rem")
            .style("box-shadow", "0 6px 24px rgba(0, 0, 0, 0.35)")
            .style("padding", "0.35rem 0")
            .style("z-index", "50")
        }
    });

    let missing_assets = state::app_state().missing_assets.clone();
    let scene = state::app_state().scene.clone();

    html!("div", {
        .child(render_popup_backdrop(open.clone()))
        .child(html!("div", {
            .class(&*POPUP)
            .child(render_menu_checkbox("Show scene stats", scene_stats_visible.clone()))
            // Only surface the "Show missing assets" item when there's
            // something to show, so the menu stays compact for the common
            // case where every asset loaded fine.
            .child_signal(missing_assets.signal_cloned().map(clone!(open => move |paths| {
                if paths.is_empty() {
                    None
                } else {
                    let count = paths.len();
                    let label = format!("Show missing assets ({count})");
                    Some(render_missing_assets_menu_item(open.clone(), paths, label))
                }
            })))
            // Only surface "Clean unused assets" when there are any. The
            // count is derived from `scene.revision` so insert / undo /
            // delete tick it live.
            .child_signal(scene.revision.signal().map(clone!(scene, open => move |_| {
                let unused = actions::project::unused_asset_count(&scene);
                if unused == 0 {
                    None
                } else {
                    let label = format!("Clean unused assets ({unused})");
                    Some(render_menu_button_owned(
                        label,
                        false,
                        clone!(open => move || {
                            open.set(false);
                            actions::project::cleanup_unused_assets();
                        }),
                    ))
                }
            })))
            .child(render_menu_separator())
            .child(render_menu_button(
                "About",
                false,
                clone!(open => move || {
                    open.set(false);
                    open_about_modal();
                }),
            ))
            .child(render_menu_separator())
            .child(render_menu_button(
                "Clear All",
                true,
                clone!(open => move || {
                    open.set(false);
                    open_clear_all_confirm();
                }),
            ))
        }))
    })
}

/// Red-tinted menu item that opens a modal listing every missing asset.
/// Built dynamically because the label embeds the current count.
fn render_missing_assets_menu_item(open: Mutable<bool>, paths: Vec<String>, label: String) -> Dom {
    static ITEM: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("display", "flex")
            .style("align-items", "center")
            .style("gap", "0.4rem")
            .style("width", "100%")
            .style("padding", "0.5rem 0.8rem")
            .style("border", "0")
            .style("background", "transparent")
            .style("cursor", "pointer")
            .style("font-size", "0.9rem")
            .style("text-align", "left")
            .style("color", ColorRaw::Red.value())
            .pseudo!(":hover", {
                .style("background", ColorBackground::SidebarSelected.value())
            })
        }
    });

    html!("button", {
        .class(&*ITEM)
        .child(html!("span", {
            .style("display", "inline-block")
            .style("width", "0.5rem")
            .style("height", "0.5rem")
            .style("border-radius", "50%")
            .style("background", ColorRaw::Red.value())
        }))
        .child(html!("span", { .text(&label) }))
        .event(move |_: events::Click| {
            open.set(false);
            open_missing_assets_modal(paths.clone());
        })
    })
}

fn open_missing_assets_modal(paths: Vec<String>) {
    Modal::open(move || {
        let paths = paths.clone();
        html!("div", {
            .style("display", "flex")
            .style("flex-direction", "column")
            .style("gap", "0.85rem")
            .style("color", ColorText::SidebarHeader.value())
            .style("min-width", "440px")
            .style("max-width", "640px")
            .child(html!("h2", { .style("margin", "0") .text("Missing assets") }))
            .child(html!("p", {
                .style("margin", "0")
                .style("font-size", "0.9rem")
                .style("line-height", "1.4")
                .text("These asset files are referenced by Model nodes in the scene but couldn't be loaded. Affected nodes still appear in the tree but render nothing.")
            }))
            .child(html!("div", {
                .style("display", "flex")
                .style("flex-direction", "column")
                .style("gap", "0.25rem")
                .style("max-height", "16rem")
                .style("overflow", "auto")
                .style("padding", "0.5rem 0.75rem")
                .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
                .style("border-radius", "0.35rem")
                .style("background-color", ColorRaw::Darkest.value())
                .children(paths.iter().map(|path| {
                    html!("div", {
                        .style("font-family", "monospace")
                        .style("font-size", "0.85rem")
                        .style("color", ColorRaw::Red.value())
                        .style("word-break", "break-all")
                        .text(path)
                    })
                }))
            }))
            .child(html!("div", {
                .style("display", "flex")
                .style("justify-content", "flex-end")
                .child(Button::new()
                    .with_text("Close")
                    .with_on_click(Modal::close)
                    .render())
            }))
        })
    });
}

fn open_about_modal() {
    Modal::open(|| {
        html!("div", {
            .style("display", "flex")
            .style("flex-direction", "column")
            .style("gap", "0.85rem")
            .style("color", ColorText::SidebarHeader.value())
            .style("max-width", "560px")
            .child(html!("h2", {
                .style("margin", "0")
                .text("About awsm scene editor")
            }))
            .child(html!("p", {
                .style("margin", "0")
                .style("font-size", "0.9rem")
                .style("line-height", "1.5")
                .text("This editor runs entirely in your browser and requires two features that are currently Chromium-only — so it only works in Chrome, Edge, Arc, Brave, or other Chromium-based browsers.")
            }))
            .child(html!("div", {
                .style("display", "flex")
                .style("flex-direction", "column")
                .style("gap", "0.5rem")
                .style("font-size", "0.9rem")
                .style("line-height", "1.5")
                .child(html!("div", {
                    .child(html!("strong", { .text("WebGPU") }))
                    .child(html!("span", { .text(" — used to render the 3D scene. Not yet shipped in stable Firefox or Safari.") }))
                }))
                .child(html!("div", {
                    .child(html!("strong", { .text("File System Access API") }))
                    .child(html!("span", { .text(" — used so that Load opens a project directory and Save writes the project JSON back alongside your .glb/.gltf assets, with relative paths. Not shipped in Firefox or Safari.") }))
                }))
            }))
            .child(html!("p", {
                .style("margin", "0")
                .style("font-size", "0.9rem")
                .style("line-height", "1.5")
                .text("A project is a directory on disk containing one project.json file plus the asset files it references. The editor never stores your files anywhere else — nothing is uploaded.")
            }))
            .child(html!("div", {
                .style("display", "flex")
                .style("justify-content", "flex-end")
                .child(Button::new()
                    .with_text("Close")
                    .with_on_click(Modal::close)
                    .render())
            }))
        })
    });
}

fn open_clear_all_confirm() {
    Modal::open(|| {
        html!("div", {
            .style("display", "flex")
            .style("flex-direction", "column")
            .style("gap", "0.85rem")
            .style("color", ColorText::SidebarHeader.value())
            .style("min-width", "320px")
            .child(html!("h2", {
                .style("margin", "0")
                .text("Clear scene?")
            }))
            .child(html!("div", {
                .style("font-size", "0.9rem")
                .style("line-height", "1.4")
                .text("This will remove every node in the scene. You can undo this.")
            }))
            .child(html!("div", {
                .style("display", "flex")
                .style("justify-content", "flex-end")
                .style("gap", "0.5rem")
                .child(Button::new()
                    .with_text("Cancel")
                    .with_style(ButtonStyle::Outline)
                    .with_on_click(Modal::close)
                    .render())
                .child(Button::new()
                    .with_text("Clear All")
                    .with_color(ButtonColor::Red)
                    .with_on_click(|| {
                        Modal::close();
                        actions::project::clear_all();
                    })
                    .render())
            }))
        })
    });
}
