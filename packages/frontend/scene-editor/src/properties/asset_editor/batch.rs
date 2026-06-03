use crate::prelude::*;
use crate::scene::AssetId;
use crate::state::app_state;
use awsm_scene_schema::AssetSource;

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
