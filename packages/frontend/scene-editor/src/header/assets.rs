//! Assets action-row.
//!
//! Each asset kind (Materials / Textures / Meshes) collapses into a
//! single dropdown chip showing the count; clicking opens a popup with
//! a filter input + scrollable list. This keeps the row width bounded
//! no matter how many assets the project carries — a textured glTF
//! pulls in a dozen Materials and a couple dozen Textures, which the
//! old "everything as inline pills" row couldn't render without
//! horizontal overflow.
//!
//! Selection semantics match the old pill list: plain click replaces
//! the asset-inspector selection, ctrl/cmd or shift extends it.
//! Popups rebuild on every `scene.revision` tick so insert / undo /
//! delete flow back into the list without a manual refresh.

use super::menu::render_popup_backdrop;
use crate::{actions, prelude::*, state};

pub(super) fn render_assets_row() -> Dom {
    use awsm_web_shared::prelude::SignalExt;
    let revision = state::app_state().scene.revision.clone();
    html!("div", {
        .style("display", "flex")
        .style("gap", "0.5rem")
        .style("align-items", "center")
        .child(Button::new()
            .with_text("+ Material Asset")
            .with_style(ButtonStyle::Outline)
            .with_size(ButtonSize::Sm)
            .with_on_click(|| { let _ = actions::insert::material_asset(); })
            .render())
        .child(Button::new()
            .with_text("+ Procedural Texture")
            .with_style(ButtonStyle::Outline)
            .with_size(ButtonSize::Sm)
            .with_on_click(|| { let _ = actions::insert::texture_asset(); })
            .render())
        .child(render_insert_image_texture_button())
        .child_signal(revision.signal().map(|_rev| {
            Some(render_asset_dropdown(AssetKind::Material, "Materials"))
        }))
        .child_signal(revision.signal().map(|_rev| {
            Some(render_asset_dropdown(AssetKind::Texture, "Textures"))
        }))
        .child_signal(revision.signal().map(|_rev| {
            Some(render_asset_dropdown(AssetKind::Mesh, "Meshes"))
        }))
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AssetKind {
    Material,
    Texture,
    Mesh,
}

/// One collapsible group: a compact "Label (N) ▾" chip + a popup
/// pinned below it. Each popup owns its own `open` + `filter` Mutables
/// so multiple groups never share state.
fn render_asset_dropdown(kind: AssetKind, label: &'static str) -> Dom {
    use awsm_web_shared::prelude::SignalExt;

    let items = collect_assets_of_kind(kind);
    let count = items.len();
    let open: Mutable<bool> = Mutable::new(false);
    let filter: Mutable<String> = Mutable::new(String::new());

    html!("div", {
        .style("position", "relative")
        .style("display", "inline-flex")
        .child(html!("button" => web_sys::HtmlElement, {
            .style("display", "inline-flex")
            .style("align-items", "center")
            .style("gap", "0.35rem")
            .style("padding", "0.25rem 0.6rem")
            .style("border-radius", "0.3rem")
            .style("cursor", "pointer")
            .style("font-size", "0.85rem")
            .style("color", ColorText::SidebarHeader.value())
            .style("background", ColorRaw::Darkest.value())
            .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
            .child(html!("span", { .text(label) }))
            .child(html!("span", {
                .style("font-size", "0.75rem")
                .style("color", ColorText::Byline.value())
                .text(&format!("({count})"))
            }))
            .child(html!("span", {
                .style("font-size", "0.65rem")
                .style("color", ColorText::Byline.value())
                .text("▾")
            }))
            .event(clone!(open => move |_: events::Click| {
                let now = open.get();
                open.set(!now);
            }))
        }))
        .child_signal(open.signal().map(clone!(open, filter, items => move |is_open| {
            if !is_open {
                return None;
            }
            Some(render_asset_popup(
                kind,
                open.clone(),
                filter.clone(),
                items.clone(),
            ))
        })))
    })
}

fn render_asset_popup(
    _kind: AssetKind,
    open: Mutable<bool>,
    filter: Mutable<String>,
    items: Vec<(crate::scene::AssetId, String)>,
) -> Dom {
    use awsm_web_shared::prelude::SignalExt;
    html!("div", {
        .child(render_popup_backdrop(open.clone()))
        .child(html!("div", {
            .style("position", "absolute")
            .style("top", "calc(100% + 0.3rem)")
            .style("left", "0")
            .style("min-width", "16rem")
            .style("max-width", "22rem")
            .style("max-height", "20rem")
            .style("display", "flex")
            .style("flex-direction", "column")
            .style("background", ColorBackground::Sidebar.value())
            .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
            .style("border-radius", "0.4rem")
            .style("box-shadow", "0 6px 24px rgba(0, 0, 0, 0.35)")
            .style("padding", "0.5rem")
            .style("gap", "0.5rem")
            .style("z-index", "50")
            // Block clicks bubbling to the backdrop (which would close
            // us). The shift/ctrl/cmd click handlers on each row need
            // their own propagation through fine.
            .event(|event: events::PointerDown| event.stop_propagation())
            .child(html!("input" => web_sys::HtmlInputElement, {
                .attr("placeholder", "Filter…")
                .attr("type", "text")
                .style("padding", "0.3rem 0.5rem")
                .style("font-size", "0.85rem")
                .style("background", ColorRaw::Darkest.value())
                .style("color", ColorText::SidebarHeader.value())
                .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
                .style("border-radius", "0.3rem")
                .style("outline", "none")
                .with_node!(elem => {
                    .event(clone!(filter, elem => move |_: events::Input| {
                        filter.set(elem.value());
                    }))
                })
            }))
            .child(html!("div", {
                .style("display", "flex")
                .style("flex-direction", "column")
                .style("gap", "0.2rem")
                .style("overflow-y", "auto")
                .child_signal(filter.signal_cloned().map(clone!(open, items => move |needle| {
                    Some(render_asset_list(open.clone(), items.clone(), needle))
                })))
            }))
        }))
    })
}

fn render_asset_list(
    open: Mutable<bool>,
    items: Vec<(crate::scene::AssetId, String)>,
    needle: String,
) -> Dom {
    let needle_lc = needle.trim().to_ascii_lowercase();
    let filtered: Vec<_> = items
        .into_iter()
        .filter(|(_, label)| {
            needle_lc.is_empty() || label.to_ascii_lowercase().contains(&needle_lc)
        })
        .collect();
    if filtered.is_empty() {
        return html!("div", {
            .style("padding", "0.4rem 0.55rem")
            .style("font-size", "0.8rem")
            .style("color", ColorText::Byline.value())
            .text(if needle_lc.is_empty() {
                "No assets yet."
            } else {
                "No matches."
            })
        });
    }
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.15rem")
        .children(filtered.into_iter().map(clone!(open => move |(id, label)| render_asset_row(open.clone(), id, label))))
    })
}

fn render_asset_row(open: Mutable<bool>, id: crate::scene::AssetId, label: String) -> Dom {
    use awsm_web_shared::prelude::SignalExt;
    let selected_assets = state::app_state().selected_assets.clone();
    html!("button" => web_sys::HtmlElement, {
        .style("display", "flex")
        .style("align-items", "center")
        .style("padding", "0.3rem 0.55rem")
        .style("border-radius", "0.25rem")
        .style("cursor", "pointer")
        .style("font-size", "0.85rem")
        .style("color", ColorText::SidebarHeader.value())
        .style("border", "0")
        .style("text-align", "left")
        .style_signal("background", selected_assets.signal_cloned().map(move |set| {
            if set.contains(&id) {
                ColorBackground::UnderlineSecondary.value()
            } else {
                "transparent"
            }
        }))
        .text(&label)
        .event(clone!(open, selected_assets => move |e: events::Click| {
            // Same selection semantics as the previous inline pills:
            // ctrl/cmd or shift extends, plain replaces. The dominator
            // fork ORs the meta key into ctrl_key on macOS. A plain
            // click also dismisses the popup — additive clicks keep
            // it open so the user can toggle multiple rows.
            let additive = e.ctrl_key() || e.shift_key();
            let mut set = selected_assets.get_cloned();
            if additive {
                if !set.insert(id) {
                    set.shift_remove(&id);
                }
            } else {
                set.clear();
                set.insert(id);
                open.set(false);
            }
            selected_assets.set(set);
        }))
    })
}

fn collect_assets_of_kind(kind: AssetKind) -> Vec<(crate::scene::AssetId, String)> {
    use awsm_scene_schema::AssetSource;
    let scene = state::app_state().scene.clone();
    let table = scene.assets.lock().unwrap();
    let mut out: Vec<(crate::scene::AssetId, String)> = table
        .entries
        .iter()
        .filter_map(|(id, e)| match (kind, &e.source) {
            (AssetKind::Material, AssetSource::Material(def)) => {
                let label = if def.label.is_empty() {
                    format!("Material {}", &id.0.to_string()[..8])
                } else {
                    def.label.clone()
                };
                Some((*id, label))
            }
            (AssetKind::Texture, AssetSource::Texture(t)) => {
                use awsm_scene_schema::{ProceduralTextureDef, TextureDef};
                let variant = match t {
                    TextureDef::Procedural(ProceduralTextureDef::Checker { .. }) => "checker",
                    TextureDef::Procedural(ProceduralTextureDef::Gradient { .. }) => "gradient",
                    TextureDef::Procedural(ProceduralTextureDef::Noise { .. }) => "noise",
                    TextureDef::Raster { display_name } => {
                        // For raster textures the user's chosen name is
                        // the most useful label — fall back to the
                        // id-prefix if it's empty.
                        if display_name.is_empty() {
                            "raster"
                        } else {
                            return Some((*id, display_name.clone()));
                        }
                    }
                };
                Some((
                    *id,
                    format!("Texture {} ({variant})", &id.0.to_string()[..8]),
                ))
            }
            (AssetKind::Mesh, AssetSource::Mesh(def)) => {
                let label = if def.label.is_empty() {
                    format!("Mesh {}", &id.0.to_string()[..8])
                } else {
                    def.label.clone()
                };
                Some((*id, label))
            }
            _ => None,
        })
        .collect();
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
}

/// "+ Image Texture" — hidden `<input type=file>` triggered by a
/// visible button. Mirrors the Insert Model picker pattern; the
/// selected file is fed to `actions::insert::texture_asset_from_file`
/// which creates a `Raster` asset entry + stages bytes for save.
fn render_insert_image_texture_button() -> Dom {
    let file_input: Mutable<Option<web_sys::HtmlInputElement>> = Mutable::new(None);
    html!("div", {
        .style("display", "inline-flex")
        .child(Button::new()
            .with_text("+ Image Texture")
            .with_style(ButtonStyle::Outline)
            .with_size(ButtonSize::Sm)
            .with_on_click(clone!(file_input => move || {
                if let Some(input) = file_input.get_cloned() {
                    input.click();
                }
            }))
            .render())
        .child(html!("input" => web_sys::HtmlInputElement, {
            .attr("type", "file")
            .attr("accept", "image/png,image/jpeg,image/webp")
            .style("display", "none")
            .with_node!(input => {
                .after_inserted(clone!(file_input, input => move |_| {
                    file_input.set(Some(input));
                }))
                .after_removed(clone!(file_input => move |_| {
                    file_input.set(None);
                }))
                .event(clone!(input => move |_: events::Change| {
                    let file = input.files().and_then(|files| files.get(0));
                    input.set_value("");
                    if let Some(file) = file {
                        actions::insert::texture_asset_from_file(file);
                    }
                }))
            })
        }))
    })
}
