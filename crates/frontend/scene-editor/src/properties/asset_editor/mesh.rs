//! Mesh-asset inspector: editable label, vertex / triangle stats from
//! `mesh_cache`, plus a source-node picker + "Re-capture from source"
//! action that overwrites the asset's bytes without changing the
//! AssetId. The stable id is what makes "every Mesh node referencing
//! this asset picks up the new geometry on the next frame" work.

use crate::prelude::*;
use crate::properties::{history_input, kind_editor};
use crate::scene::AssetId;
use crate::state::app_state;
use awsm_scene_schema::{AssetSource, CapturedSource, MeshRef};

pub(super) fn render(asset_id: AssetId) -> Dom {
    let scene = app_state().scene.clone();
    let revision = scene.revision.clone();
    let read_label = clone!(scene => move || -> String {
        let table = scene.assets.lock().unwrap();
        match table.get(asset_id).map(|e| &e.source) {
            Some(AssetSource::Mesh(def)) => def.label.clone(),
            _ => String::new(),
        }
    });
    // `write_label` just mutates the table — history is coalesced at
    // the input widget level via FocusIn/FocusOut (H3). One drag-edit
    // gesture commits one history entry, not one per keystroke.
    let write_label = clone!(scene => move |new_label: String| {
        {
            let mut table = scene.assets.lock().unwrap();
            if let Some(entry) = table.entries.get_mut(&asset_id) {
                if let AssetSource::Mesh(def) = &mut entry.source {
                    def.label = new_label;
                }
            }
        }
        scene.bump_revision();
    });
    let stats = crate::renderer_bridge::mesh_cache::stats(MeshRef(asset_id));

    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.5rem")
        .child(kind_editor::section_header("Captured Mesh"))
        .child(kind_editor::field_row("Label", history_input::text_input(read_label, write_label, revision.signal())))
        .child(kind_editor::field_row("Stats", html!("div", {
            .style("font-size", "0.8rem")
            .style("color", ColorText::SidebarHeader.value())
            .text(&match stats {
                Some(s) => format!("{} vert, {} tri", s.vertex_count, s.triangle_count),
                None => "(loading…)".to_string(),
            })
        })))
        .child(html!("div", {
            .style("font-size", "0.75rem")
            .style("color", ColorText::Byline.value())
            .style("line-height", "1.4")
            .text(
                "Geometry is bitcode-encoded as a side file \
                 (assets/<asset-id>.mesh.bin) alongside project.json. \
                 Edit the captured source params below to re-evaluate \
                 the bytes in place — the asset id stays stable, so \
                 every NodeKind::Mesh referencing this asset picks up \
                 the new geometry on the next frame. For legacy meshes \
                 without a recorded source, use the source-picker."
            )
        }))
        .child(render_source_section(asset_id))
        .child(render_recapture_section(asset_id))
    })
}

/// If the MeshDef recorded its source kind on capture (H1), render
/// editable param rows for the captured-source params. Mutating those
/// triggers `recapture_from_source_def` which re-evaluates the
/// geometry in place. For Primitive sources we reuse the same
/// shape editor the Primitive node inspector uses; for Sweep (and
/// for assets captured before H1 landed) the source-picker dropdown
/// below remains the path.
fn render_source_section(asset_id: AssetId) -> Dom {
    let scene = app_state().scene.clone();
    let revision = scene.revision.clone();
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.5rem")
        .child(kind_editor::section_header("Captured source"))
        .child_signal(revision.signal_cloned().map(clone!(scene, revision => move |_rev| {
            let source = {
                let table = scene.assets.lock().unwrap();
                match table.get(asset_id).map(|e| &e.source) {
                    Some(AssetSource::Mesh(def)) => def.source.clone(),
                    _ => None,
                }
            };
            Some(match source {
                Some(CapturedSource::Primitive(_)) => {
                    let read_shape = clone!(scene => move || {
                        let table = scene.assets.lock().unwrap();
                        match table.get(asset_id).map(|e| &e.source) {
                            Some(AssetSource::Mesh(def)) => match &def.source {
                                Some(CapturedSource::Primitive(s)) => s.clone(),
                                _ => awsm_scene_schema::PrimitiveShape::default_box(),
                            },
                            _ => awsm_scene_schema::PrimitiveShape::default_box(),
                        }
                    });
                    let write_shape = move |new_shape: awsm_scene_schema::PrimitiveShape| {
                        crate::actions::object::recapture_from_source_def(
                            asset_id,
                            &CapturedSource::Primitive(new_shape),
                        );
                    };
                    kind_editor::primitive::render_shape_editor(read_shape, write_shape, revision.clone())
                }
                Some(CapturedSource::Sweep(_)) => {
                    let read_def = clone!(scene => move || {
                        let table = scene.assets.lock().unwrap();
                        match table.get(asset_id).map(|e| &e.source) {
                            Some(AssetSource::Mesh(def)) => match &def.source {
                                Some(CapturedSource::Sweep(d)) => d.clone(),
                                _ => awsm_scene_schema::SweepAlongCurveDef::default(),
                            },
                            _ => awsm_scene_schema::SweepAlongCurveDef::default(),
                        }
                    });
                    let write_def = move |new_def: awsm_scene_schema::SweepAlongCurveDef| {
                        crate::actions::object::recapture_from_source_def(
                            asset_id,
                            &CapturedSource::Sweep(new_def),
                        );
                    };
                    kind_editor::sweep::render_sweep_editor(read_def, write_def, revision.clone())
                }
                None => html!("div", {
                    .style("font-size", "0.8rem")
                    .style("color", ColorText::Byline.value())
                    .text(
                        "This mesh was captured before source-kind \
                         tracking landed. Use the source-picker below \
                         to re-capture; future captures will store \
                         their source."
                    )
                }),
            })
        })))
    })
}

fn render_recapture_section(asset_id: AssetId) -> Dom {
    use awsm_web_shared::atoms::buttons::{Button, ButtonSize, ButtonStyle};
    use awsm_web_shared::prelude::SignalExt;
    let revision = app_state().scene.revision.clone();
    let selected_source: Mutable<Option<crate::scene::NodeId>> = Mutable::new(None);
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.4rem")
        .style("padding-top", "0.5rem")
        .child(kind_editor::section_header("Re-capture"))
        .child_signal(revision.signal().map(clone!(selected_source => move |_rev| {
            Some(render_source_picker(selected_source.clone()))
        })))
        .child(Button::new()
            .with_text("Re-capture from source")
            .with_style(ButtonStyle::Outline)
            .with_size(ButtonSize::Sm)
            .with_disabled_signal(selected_source.signal().map(|s| s.is_none()))
            .with_on_click(clone!(selected_source => move || {
                if let Some(source) = selected_source.get() {
                    let _ = crate::actions::object::recapture_into_existing(source, asset_id);
                }
            }))
            .render())
    })
}

fn render_source_picker(selected: Mutable<Option<crate::scene::NodeId>>) -> Dom {
    let sources = collect_capturable_sources();
    let mut options: Vec<Dom> = vec![html!("option", {
        .attr("value", "")
        .text("(pick a source node)")
    })];
    for (id, label) in &sources {
        options.push(html!("option", {
            .attr("value", &id.0.to_string())
            .text(label)
        }));
    }
    html!("select" => web_sys::HtmlSelectElement, {
        .style("padding", "0.3rem 0.45rem")
        .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .style("border-radius", "0.25rem")
        .style("background-color", ColorRaw::Darkest.value())
        .style("color", ColorText::SidebarHeader.value())
        .style("font-size", "0.85rem")
        .style("cursor", "pointer")
        .children(options)
        .with_node!(select => {
            .event(clone!(selected, select => move |_: events::Change| {
                let value = select.value();
                selected.set(if value.is_empty() {
                    None
                } else {
                    uuid::Uuid::parse_str(&value).ok().map(crate::scene::NodeId)
                });
            }))
        })
    })
}

fn collect_capturable_sources() -> Vec<(crate::scene::NodeId, String)> {
    fn walk(
        nodes: &[std::sync::Arc<crate::scene::Node>],
        out: &mut Vec<(crate::scene::NodeId, String)>,
    ) {
        for n in nodes.iter() {
            let kind = n.kind.lock_ref();
            let label_suffix = match &*kind {
                crate::scene::NodeKind::Primitive { .. } => Some(" (Primitive)"),
                crate::scene::NodeKind::SweepAlongCurve { .. } => Some(" (Sweep)"),
                _ => None,
            };
            drop(kind);
            if let Some(suffix) = label_suffix {
                let name = n.name.get_cloned();
                let label = if name.is_empty() {
                    format!("Node {}{suffix}", &n.id.0.to_string()[..8])
                } else {
                    format!("{name}{suffix}")
                };
                out.push((n.id, label));
            }
            let children = n.children.lock_ref();
            walk(&children, out);
        }
    }
    let scene = app_state().scene.clone();
    let nodes = scene.nodes.lock_ref();
    let mut out = Vec::new();
    walk(&nodes, &mut out);
    out
}
