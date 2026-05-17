// ─────────────────────────────────────────────────────────────────────
// Mesh (captured procedural mesh asset)
// ─────────────────────────────────────────────────────────────────────

use crate::prelude::*;
use crate::scene::{Node, NodeKind};

use super::{field_row, section_header};

pub fn render(node: Arc<Node>) -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.5rem")
        .child(section_header("Mesh"))
        .child(html!("div", {
            .style("font-size", "0.8rem")
            .style("color", ColorText::Byline.value())
            .style("line-height", "1.4")
            .text(
                "Mesh references a captured procedural mesh asset \
                 (AssetSource::Mesh). Create one via the \"Capture as \
                 Mesh asset\" button on a Primitive or SweepAlongCurve \
                 inspector — the geometry snapshot lives in \
                 assets/<asset-id>.mesh.bin alongside project.json."
            )
        }))
        .child(field_row("Asset id", html!("div", {
            .style("font-size", "0.75rem")
            .style("font-family", "monospace")
            .style("color", ColorText::SidebarHeader.value())
            .style("word-break", "break-all")
            .text_signal(node.kind.signal_cloned().map(|k| match k {
                NodeKind::Mesh { mesh, .. } => mesh.0.to_string(),
                _ => String::new(),
            }))
        })))
        .child(field_row("Capture stats", html!("div", {
            .style("font-size", "0.75rem")
            .style("color", ColorText::SidebarHeader.value())
            .text_signal(node.kind.signal_cloned().map(|k| match k {
                NodeKind::Mesh { mesh, .. } => {
                    // Snapshot vertex / triangle counts from the live
                    // mesh_cache if we have them; otherwise show a
                    // pending message (the bytes may still be loading
                    // from the side file).
                    crate::renderer_bridge::mesh_cache::stats(mesh)
                        .map(|s| format!(
                            "{} vert, {} tri", s.vertex_count, s.triangle_count
                        ))
                        .unwrap_or_else(|| "(loading…)".to_string())
                }
                _ => String::new(),
            }))
        })))
        // Optional shared-material picker (D-1c).
        .child(field_row("Material asset", super::material_ref_select(
            node.clone(),
            |k| match k {
                NodeKind::Mesh { material, .. } => *material,
                _ => None,
            },
            |k, new_ref| {
                if let NodeKind::Mesh { material, .. } = k {
                    *material = new_ref;
                }
            },
        )))
        // Material editor — see Primitive for the asset-vs-inline switch.
        .child(super::material::render_material_for_node(
            node.clone(),
            |k| match k {
                NodeKind::Mesh { material, .. } => *material,
                _ => None,
            },
            |k| match k {
                NodeKind::Mesh { inline_material, .. } => Some(inline_material),
                _ => None,
            },
            |k, new_def| {
                if let NodeKind::Mesh { inline_material, .. } = k {
                    *inline_material = new_def;
                }
            },
        ))
    })
}
