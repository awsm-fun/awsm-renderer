//! Content Browser — a collapsible drawer under the Scene viewport that lists
//! the project's assets (materials / textures / meshes) as a searchable grid,
//! replacing the old header "Assets" dropdowns. Category tabs carry live
//! counts; clicking a card selects the asset (routing the right-sidebar
//! inspector to the asset editor); double-clicking a material opens it in
//! Material mode.

use crate::header::assets::{collect_assets_of_kind, AssetKind};
use crate::prelude::*;
use crate::scene::AssetId;
use crate::state::{self, app_state, EditorMode};

/// Category filter for the grid.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    All,
    Materials,
    Textures,
    Meshes,
}

/// The collapsible drawer. Mounted at the bottom of the Scene workspace.
pub fn render() -> Dom {
    let open = app_state().content_browser_open.clone();
    let tab = Mutable::new(Tab::All);
    let query = Mutable::new(String::new());

    html!("div", {
        .style("flex", "0 0 auto")
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("border-top", "1px solid var(--line)")
        .style("background", "var(--bg-1)")
        .child(render_header(open.clone(), tab.clone(), query.clone()))
        .child_signal(open.signal().map(clone!(tab, query => move |is_open| {
            if is_open { Some(render_body(tab.clone(), query.clone())) } else { None }
        })))
    })
}

fn render_header(open: Mutable<bool>, tab: Mutable<Tab>, query: Mutable<String>) -> Dom {
    let scene = app_state().scene.clone();
    html!("div", {
        .style("display", "flex")
        .style("align-items", "center")
        .style("gap", "10px")
        .style("height", "34px")
        .style("padding", "0 10px")
        .style("background", "var(--bg-2)")
        // Collapse/expand toggle.
        .child(html!("button", {
            .class("t")
            .style("display", "inline-flex")
            .style("align-items", "center")
            .style("gap", "7px")
            .style("cursor", "pointer")
            .style("color", "var(--text-1)")
            .style("font-size", "12px")
            .style("font-weight", "560")
            .child(html!("span", {
                .style_signal("transform", open.signal().map(|o| if o { "rotate(90deg)" } else { "rotate(0deg)" }))
                .style("font-size", "9px")
                .style("color", "var(--text-3)")
                .text("▶")
            }))
            .text("Content Browser")
            .event(clone!(open => move |_: events::Click| open.set_neq(!open.get())))
        }))
        // Category tabs with live counts (re-counted on scene revision).
        .child_signal(scene.revision.signal().map(clone!(tab => move |_| {
            let counts = (
                collect_assets_of_kind(AssetKind::Material).len(),
                collect_assets_of_kind(AssetKind::Texture).len(),
                collect_assets_of_kind(AssetKind::Mesh).len(),
            );
            Some(html!("div", {
                .style("display", "flex")
                .style("gap", "2px")
                .child(tab_button(tab.clone(), Tab::All, "All", None))
                .child(tab_button(tab.clone(), Tab::Materials, "Materials", Some(counts.0)))
                .child(tab_button(tab.clone(), Tab::Textures, "Textures", Some(counts.1)))
                .child(tab_button(tab.clone(), Tab::Meshes, "Meshes", Some(counts.2)))
            }))
        })))
        .child(html!("div", { .style("flex", "1 1 0") }))
        .child(html!("input" => web_sys::HtmlInputElement, {
            .style("width", "180px")
            .style("height", "24px")
            .style("padding", "0 9px")
            .style("background", "var(--bg-3)")
            .style("border", "1px solid var(--line-soft)")
            .style("border-radius", "var(--r2)")
            .style("outline", "none")
            .style("color", "var(--text-0)")
            .style("font-size", "12px")
            .attr("placeholder", "Search assets…")
            .with_node!(input => {
                .event(clone!(query => move |_: events::Input| query.set_neq(input.value())))
            })
        }))
        .child(add_button("+ Material", || { let _ = crate::actions::insert::material_asset(); }))
        .child(add_button("+ Texture", || { let _ = crate::actions::insert::texture_asset(); }))
    })
}

fn tab_button(tab: Mutable<Tab>, this: Tab, label: &str, count: Option<usize>) -> Dom {
    let text = match count {
        Some(n) => format!("{label} {n}"),
        None => label.to_string(),
    };
    html!("button", {
        .class("t")
        .style("cursor", "pointer")
        .style("font-size", "11.5px")
        .style("padding", "3px 9px")
        .style("border-radius", "var(--r1)")
        .style_signal("background", tab.signal().map(move |t| if t == this { "var(--bg-active)" } else { "transparent" }))
        .style_signal("color", tab.signal().map(move |t| if t == this { "var(--text-0)" } else { "var(--text-2)" }))
        .text(&text)
        .event(clone!(tab => move |_: events::Click| tab.set_neq(this)))
    })
}

fn add_button(label: &'static str, on_click: impl Fn() + 'static) -> Dom {
    html!("button", {
        .class("t")
        .style("cursor", "pointer")
        .style("font-size", "11.5px")
        .style("padding", "4px 10px")
        .style("border-radius", "var(--r2)")
        .style("border", "1px solid var(--accent-line)")
        .style("background", "var(--accent-ghost)")
        .style("color", "var(--accent-bright)")
        .text(label)
        .event(move |_: events::Click| on_click())
    })
}

fn render_body(tab: Mutable<Tab>, query: Mutable<String>) -> Dom {
    let scene = app_state().scene.clone();
    html!("div", {
        .style("height", "190px")
        .style("overflow-y", "auto")
        .style("padding", "10px")
        // Rebuild the grid on tab change, query change, or scene revision.
        .child_signal(map_ref! {
            let t = tab.signal(),
            let q = query.signal_cloned(),
            let _rev = scene.revision.signal() => {
                Some(render_grid(*t, q.clone()))
            }
        })
    })
}

fn render_grid(tab: Tab, query: String) -> Dom {
    let q = query.to_ascii_lowercase();
    let mut cards: Vec<(AssetId, String, AssetKind)> = Vec::new();
    let mut push = |kind: AssetKind| {
        for (id, name) in collect_assets_of_kind(kind) {
            cards.push((id, name, kind));
        }
    };
    match tab {
        Tab::All => {
            push(AssetKind::Material);
            push(AssetKind::Texture);
            push(AssetKind::Mesh);
        }
        Tab::Materials => push(AssetKind::Material),
        Tab::Textures => push(AssetKind::Texture),
        Tab::Meshes => push(AssetKind::Mesh),
    }
    cards.retain(|(_, name, _)| q.is_empty() || name.to_ascii_lowercase().contains(&q));

    if cards.is_empty() {
        return html!("div", {
            .style("padding", "8px 2px")
            .style("font-size", "12px")
            .style("color", "var(--text-3)")
            .text("No assets. Use + Material / + Texture, or insert geometry.")
        });
    }

    html!("div", {
        .style("display", "grid")
        .style("grid-template-columns", "repeat(auto-fill, minmax(150px, 1fr))")
        .style("gap", "8px")
        .children(cards.into_iter().map(|(id, name, kind)| asset_card(id, name, kind)))
    })
}

fn asset_card(id: AssetId, name: String, kind: AssetKind) -> Dom {
    let (badge, badge_color) = match kind {
        AssetKind::Material => ("MATERIAL", "var(--accent-bright)"),
        AssetKind::Texture => ("TEXTURE", "var(--ok)"),
        AssetKind::Mesh => ("MESH", "var(--text-2)"),
    };
    let selected_assets = app_state().selected_assets.clone();
    html!("div", {
        .class("t")
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "6px")
        .style("padding", "8px")
        .style("border-radius", "var(--r2)")
        .style("border", "1px solid var(--line-soft)")
        .style("background", "var(--bg-2)")
        .style("cursor", "pointer")
        .style_signal("border-color", selected_assets.signal_ref(move |s| s.contains(&id)).map(|sel| {
            if sel { "var(--accent-line)" } else { "var(--line-soft)" }
        }))
        // Thumbnail placeholder (real preview thumbnails are a follow-up).
        .child(html!("div", {
            .style("height", "54px")
            .style("border-radius", "var(--r1)")
            .style("background", "linear-gradient(135deg, var(--bg-3), var(--bg-active))")
            .style("display", "flex")
            .style("align-items", "flex-start")
            .style("justify-content", "flex-end")
            .style("padding", "4px")
            .child(html!("span", {
                .class("kicker")
                .style("font-size", "8px")
                .style("color", badge_color)
                .text(badge)
            }))
        }))
        .child(html!("div", {
            .style("font-size", "12px")
            .style("color", "var(--text-1)")
            .style("white-space", "nowrap")
            .style("overflow", "hidden")
            .style("text-overflow", "ellipsis")
            .text(&name)
        }))
        // Single click selects the asset (right inspector → asset editor).
        .event(clone!(selected_assets => move |_: events::Click| {
            let mut set = indexmap::IndexSet::new();
            set.insert(id);
            selected_assets.set(set);
        }))
        // Double-click a material → open it in Material mode.
        .event(move |_: events::DoubleClick| {
            if kind == AssetKind::Material {
                state::app_state().mode.set_neq(EditorMode::Material);
            }
        })
    })
}
