//! Materials pane — surfaces the project's `custom_materials` list.
//!
//! Phase 12 ships a read-only listing with Open-in-editor links.
//! Folder-picker Import + Remove + per-mesh Custom picker are the
//! next-session UI work. The data path (renderer-bridge converter,
//! registration via `register_loaded_folder`) is already in place
//! (Phase 5).

use dominator::{events, html, Dom};

use awsm_scene_schema::dynamic_material::CustomMaterialRef;

/// Render the Materials sub-pane given the project's current
/// `custom_materials` list. Returns a section the sidebar can place
/// alongside its other panes (assets / lights / shadows / etc.).
pub fn render(custom_materials: Vec<CustomMaterialRef>) -> Dom {
    html!("div", {
        .style("padding", "8px 12px")
        .style("border-top", "1px solid #333")
        .child(html!("h4", { .text("Custom Materials") }))
        .apply(|b| {
            if custom_materials.is_empty() {
                b.child(html!("p", {
                    .style("font-size", "11px")
                    .style("color", "#888")
                    .text("None imported. Use Import Material… (next-session UI) to bring in a folder.")
                }))
            } else {
                b.child(html!("ul", {
                    .style("padding-left", "16px")
                    .style("font-size", "12px")
                    .children(custom_materials.into_iter().map(render_row).collect::<Vec<_>>())
                }))
            }
        })
    })
}

fn render_row(custom: CustomMaterialRef) -> Dom {
    let name = custom.name.clone();
    let folder = custom.folder.display().to_string();
    html!("li", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("margin-bottom", "6px")
        .child(html!("strong", { .text(&name) }))
        .child(html!("small", {
            .style("color", "#888")
            .text(&folder)
        }))
        .child(html!("a", {
            // The material-editor's `?folder=<path>` URL param drives
            // the initial load on boot. Phase 12 docs note this is
            // best-effort across deployment topologies; locally it
            // points at the dev server.
            .attr("href", &format!("http://localhost:9084/?folder={}", urlencode(&folder)))
            .attr("target", "_blank")
            .style("font-size", "11px")
            .style("color", "#88f")
            .text("Open in material-editor")
            .event(|_: events::Click| {
                // No-op — the anchor's href + target handle the open.
            })
        }))
    })
}

/// Minimal URL encoder — only the characters that actually break a
/// query-string value. The full `urlencoding` crate would be nicer
/// but the scene-editor already keeps deps minimal.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '~' | '/' => out.push(c),
            ' ' => out.push_str("%20"),
            _ => {
                for b in c.to_string().bytes() {
                    out.push_str(&format!("%{:02X}", b));
                }
            }
        }
    }
    out
}
