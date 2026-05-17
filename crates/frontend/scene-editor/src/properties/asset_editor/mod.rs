//! Right-sidebar inspector for the *currently-selected asset(s)*.
//!
//! When `AppState::selected_assets` is non-empty, the right sidebar
//! swaps from "edit the selected node" to "edit the selected asset(s)".
//! Single-select routes to the per-source editor; multi-select routes
//! to a batch summary + Delete-selected action.
//!
//! Per-source editors (each in its own submodule):
//! - Material — `kind_editor::material::render_asset_material`. Lives
//!   alongside the inline-material editor in `kind_editor` because the
//!   same controls drive both the asset and inline paths.
//! - Mesh — [`mesh`]. Editable label, vertex/triangle stats from
//!   `mesh_cache`, plus the source-picker + Re-capture action.
//! - Texture — [`texture`]. Procedural Checker / Gradient / Noise
//!   editor with live cascade through `texture_cache::update_existing`.
//!   File-backed `Raster` falls through to a placeholder (raster
//!   textures are imported via the Environment tab).

mod mesh;
mod texture;

use crate::prelude::*;
use crate::scene::AssetId;
use crate::state::app_state;
use awsm_scene_schema::{AssetSource, MaterialRef};

use super::kind_editor;

/// Render the inspector for `asset_id`. Picks the right per-source
/// editor or falls back to a "no editor yet" message if the asset
/// source isn't authorable from the panel today.
pub fn render(asset_id: AssetId) -> Dom {
    let scene = app_state().scene.clone();
    let source_kind = {
        let table = scene.assets.lock().unwrap();
        table.get(asset_id).map(|e| match &e.source {
            AssetSource::Material(_) => SourceTag::Material,
            AssetSource::Texture(_) => SourceTag::Texture,
            AssetSource::Mesh(_) => SourceTag::Mesh,
            AssetSource::Filename(_) | AssetSource::Url(_) => SourceTag::FileBacked,
        })
    };

    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.5rem")
        .child(render_header(asset_id))
        .child(match source_kind {
            Some(SourceTag::Material) => {
                kind_editor::material::render_asset_material(MaterialRef(asset_id))
            }
            Some(SourceTag::Texture) => texture::render(asset_id),
            Some(SourceTag::Mesh) => mesh::render(asset_id),
            Some(SourceTag::FileBacked) => {
                render_placeholder("File-backed asset — read-only; edit the file directly.")
            }
            None => render_placeholder("Asset not found in this project."),
        })
    })
}

#[derive(Clone, Copy)]
enum SourceTag {
    Material,
    Texture,
    Mesh,
    FileBacked,
}

/// Multi-select summary panel. Shown when `selected_assets.len() > 1`.
/// Lists the selected entries by label + offers a "Delete selected"
/// batch action (records one history entry for the whole batch).
pub fn render_batch(selected: &indexmap::IndexSet<AssetId>) -> Dom {
    use awsm_web_shared::atoms::buttons::{Button, ButtonColor, ButtonSize, ButtonStyle};
    let ids: Vec<AssetId> = selected.iter().copied().collect();
    let scene = app_state().scene.clone();
    let labels: Vec<String> = {
        let table = scene.assets.lock().unwrap();
        ids.iter()
            .map(|id| match table.get(*id).map(|e| &e.source) {
                Some(AssetSource::Material(def)) => {
                    if def.label.is_empty() {
                        format!("Material {}", &id.0.to_string()[..8])
                    } else {
                        def.label.clone()
                    }
                }
                Some(AssetSource::Texture(_)) => format!("Texture {}", &id.0.to_string()[..8]),
                Some(AssetSource::Mesh(def)) => {
                    if def.label.is_empty() {
                        format!("Mesh {}", &id.0.to_string()[..8])
                    } else {
                        def.label.clone()
                    }
                }
                _ => format!("Asset {}", &id.0.to_string()[..8]),
            })
            .collect()
    };

    let count = ids.len();
    let ids_for_delete = ids.clone();
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.6rem")
        .child(html!("div", {
            .style("display", "flex")
            .style("justify-content", "space-between")
            .style("align-items", "center")
            .child(html!("div", {
                .style("font-weight", "600")
                .style("font-size", "0.95rem")
                .text(&format!("{count} assets selected"))
            }))
            .child(html!("button" => web_sys::HtmlElement, {
                .style("font-size", "0.75rem")
                .style("padding", "0.25rem 0.5rem")
                .style("background-color", ColorRaw::Darkest.value())
                .style("color", ColorText::SidebarHeader.value())
                .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
                .style("border-radius", "0.25rem")
                .style("cursor", "pointer")
                .text("Clear selection")
                .event(|_: events::Click| {
                    app_state().selected_assets.set(indexmap::IndexSet::new());
                })
            }))
        }))
        .child(html!("div", {
            .style("display", "flex")
            .style("flex-direction", "column")
            .style("gap", "0.2rem")
            .style("font-size", "0.8rem")
            .style("color", ColorText::Byline.value())
            .children(labels.into_iter().map(|l| html!("div", { .text(&l) })))
        }))
        .child(Button::new()
            .with_text("Delete selected")
            .with_style(ButtonStyle::Outline)
            .with_size(ButtonSize::Sm)
            .with_color(ButtonColor::Red)
            .with_on_click(move || {
                crate::actions::project::delete_asset_entries(&ids_for_delete);
            })
            .render())
    })
}

fn render_header(asset_id: AssetId) -> Dom {
    let label = format!("Asset {}", &asset_id.0.to_string()[..8]);
    html!("div", {
        .style("display", "flex")
        .style("align-items", "center")
        .style("justify-content", "space-between")
        .style("gap", "0.5rem")
        .child(html!("div", {
            .style("font-size", "0.95rem")
            .style("font-weight", "600")
            .text(&label)
        }))
        .child(html!("button" => web_sys::HtmlElement, {
            .style("font-size", "0.75rem")
            .style("padding", "0.25rem 0.5rem")
            .style("background-color", ColorRaw::Darkest.value())
            .style("color", ColorText::SidebarHeader.value())
            .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
            .style("border-radius", "0.25rem")
            .style("cursor", "pointer")
            .text("Back to nodes")
            .event(|_: events::Click| {
                app_state().selected_assets.set(indexmap::IndexSet::new());
            })
        }))
    })
}

fn render_placeholder(text: &str) -> Dom {
    html!("div", {
        .style("color", ColorText::Byline.value())
        .style("font-size", "0.85rem")
        .style("line-height", "1.4")
        .text(text)
    })
}
