//! Assets action-row — pill list of every Material / Texture / Mesh
//! `AssetEntry` in the scene's asset table. Clicking a pill drives
//! `AppState::selected_assets` so the right-sidebar inspector switches
//! to the asset editor. Ctrl/Cmd-click + Shift-click extend the
//! selection; plain click replaces it.
//!
//! Pills + the `+ Material/Texture Asset` buttons rebuild on every
//! `scene.revision` tick so insert / undo / delete flow back into the
//! list without a manual refresh.

use crate::{actions, prelude::*, state};

pub(super) fn render_assets_row() -> Dom {
    use awsm_web_shared::prelude::SignalExt;
    let revision = state::app_state().scene.revision.clone();
    html!("div", {
        .style("display", "flex")
        .style("gap", "1.25rem")
        .style("align-items", "center")
        .style("flex-wrap", "wrap")
        .child(Button::new()
            .with_text("+ Material Asset")
            .with_style(ButtonStyle::Outline)
            .with_size(ButtonSize::Sm)
            .with_on_click(|| { let _ = actions::insert::material_asset(); })
            .render())
        .child(Button::new()
            .with_text("+ Texture Asset")
            .with_style(ButtonStyle::Outline)
            .with_size(ButtonSize::Sm)
            .with_on_click(|| { let _ = actions::insert::texture_asset(); })
            .render())
        // Materials list.
        .child_signal(revision.signal().map(|_rev| {
            Some(render_asset_group("Materials", "(none yet — click \"+ Material Asset\")", collect_assets_of_kind(AssetKind::Material)))
        }))
        // Procedural-texture list.
        .child_signal(revision.signal().map(|_rev| {
            Some(render_asset_group("Textures", "(none yet — click \"+ Texture Asset\")", collect_assets_of_kind(AssetKind::Texture)))
        }))
        // Captured-mesh list. Authored via the per-kind inspector's
        // "Capture as Mesh asset" button — not a header action since
        // the source is always a specific node.
        .child_signal(revision.signal().map(|_rev| {
            Some(render_asset_group("Meshes", "(capture a Primitive or Sweep to add one)", collect_assets_of_kind(AssetKind::Mesh)))
        }))
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AssetKind {
    Material,
    Texture,
    Mesh,
}

fn render_asset_group(
    label: &'static str,
    empty_hint: &'static str,
    items: Vec<(crate::scene::AssetId, String)>,
) -> Dom {
    use awsm_web_shared::prelude::SignalExt;
    let selected_assets = state::app_state().selected_assets.clone();
    html!("div", {
        .style("display", "flex")
        .style("align-items", "center")
        .style("gap", "0.5rem")
        .child(html!("div", {
            .style("font-size", "0.85rem")
            .style("color", ColorText::Byline.value())
            .text(label)
        }))
        .child(if items.is_empty() {
            html!("div", {
                .style("font-size", "0.8rem")
                .style("color", ColorText::Byline.value())
                .text(empty_hint)
            })
        } else {
            html!("div", {
                .style("display", "flex")
                .style("gap", "0.4rem")
                .style("flex-wrap", "wrap")
                .children(items.into_iter().map(move |(id, label)| {
                    let selected_assets = selected_assets.clone();
                    html!("button" => web_sys::HtmlElement, {
                        .style("font-size", "0.8rem")
                        .style("padding", "0.25rem 0.55rem")
                        .style("border-radius", "0.3rem")
                        .style("cursor", "pointer")
                        .style_signal("background-color", selected_assets.signal_cloned().map(move |set| {
                            if set.contains(&id) {
                                ColorBackground::UnderlineSecondary.value()
                            } else {
                                ColorRaw::Darkest.value()
                            }
                        }))
                        .style("color", ColorText::SidebarHeader.value())
                        .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
                        .text(&label)
                        .event(clone!(selected_assets => move |e: events::Click| {
                            // Ctrl/Cmd-click or Shift-click toggles
                            // membership; plain click replaces the
                            // selection with just this one. The dominator
                            // fork's `events::Click::ctrl_key()` ORs in
                            // the meta key on macOS — see the matching
                            // pattern in tree/rows.rs.
                            let additive = e.ctrl_key() || e.shift_key();
                            let mut set = selected_assets.get_cloned();
                            if additive {
                                if !set.insert(id) {
                                    // `shift_remove` preserves the
                                    // click order of the remaining
                                    // entries (vs `swap_remove` which
                                    // would reorder the tail).
                                    set.shift_remove(&id);
                                }
                            } else {
                                set.clear();
                                set.insert(id);
                            }
                            selected_assets.set(set);
                        }))
                    })
                }))
            })
        })
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
                    TextureDef::Raster { .. } => "raster",
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
