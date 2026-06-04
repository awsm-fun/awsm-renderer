use crate::prelude::*;
use crate::scene::AssetId;
use crate::state::app_state;
use awsm_scene_schema::{AssetSource, MaterialRef};

use super::{mesh, texture};
use crate::properties::kind_editor;

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
