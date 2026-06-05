use crate::prelude::*;
use crate::scene::{AssetId, AssetSource, Node, NodeKind};
use crate::state::app_state;
use awsm_scene_schema::{NodeId, TextureDef, TextureRef};

// ─────────────────────────────────────────────────────────────────────
// NodeId picker (snapshot-based; rebuilds when the host kind changes)
// ─────────────────────────────────────────────────────────────────────

/// Collect `(NodeId, display_name)` pairs from the live scene tree that
/// satisfy the given `predicate` on each node's current `NodeKind`. The
/// caller renders a `<select>` from the result — reactive updates when
/// the host kind signal changes are sufficient for v1 (adding a new
/// curve node and immediately reassigning a sweep's `curve_node` will
/// require switching the sweep node first to re-snapshot).
pub(crate) fn collect_nodes_matching<F>(predicate: F) -> Vec<(NodeId, String)>
where
    F: Fn(&NodeKind) -> bool,
{
    fn walk<F>(nodes: &[Arc<crate::scene::Node>], predicate: &F, out: &mut Vec<(NodeId, String)>)
    where
        F: Fn(&NodeKind) -> bool,
    {
        for n in nodes.iter() {
            if predicate(&n.kind.lock_ref()) {
                out.push((n.id, n.name.get_cloned()));
            }
            let children = n.children.lock_ref();
            walk(&children, predicate, out);
        }
    }
    let scene = app_state().scene.clone();
    let nodes = scene.nodes.lock_ref();
    let mut out = Vec::new();
    walk(&nodes, &predicate, &mut out);
    out
}

/// A `<select>` whose options are every node currently in the scene tree
/// whose kind satisfies `predicate`. `read_current` extracts the
/// currently-selected `NodeId` from the host kind; `write_new` mutates
/// the host kind in place with the picked `NodeId`.
pub(crate) fn node_id_select(
    node: Arc<Node>,
    predicate: fn(&NodeKind) -> bool,
    read_current: fn(&NodeKind) -> Option<NodeId>,
    write_new: fn(&mut NodeKind, NodeId),
) -> Dom {
    let kind = node.kind.clone();
    let candidates = collect_nodes_matching(predicate);
    let mut options = vec![html!("option", {
        .attr("value", "")
        .text("(none)")
    })];
    for (id, name) in &candidates {
        options.push(html!("option", {
            .attr("value", &id.0.to_string())
            .text(name)
        }));
    }

    html!("select" => web_sys::HtmlSelectElement, {
        .style("padding", "0.35rem 0.5rem")
        .style("background-color", ColorRaw::Darkest.value())
        .style("color", ColorText::SidebarHeader.value())
        .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .style("border-radius", "0.3rem")
        .style("font-size", "0.85rem")
        .style("cursor", "pointer")
        .children(options)
        .with_node!(select => {
            .future(clone!(kind, select => {
                kind.signal_cloned().for_each(move |k| {
                    let want = read_current(&k)
                        .map(|id| id.0.to_string())
                        .unwrap_or_default();
                    if select.value() != want {
                        select.set_value(&want);
                    }
                    async {}
                })
            }))
            .event(clone!(kind, select => move |_: events::Change| {
                let value = select.value();
                if value.is_empty() {
                    return;
                }
                let Ok(parsed) = uuid::Uuid::parse_str(&value) else {
                    return;
                };
                let new_id = NodeId(parsed);
                let mut k = kind.get_cloned();
                write_new(&mut k, new_id);
                kind.set(k);
            }))
        })
    })
}

/// Collects every `AssetSource::Texture` entry in the live asset table,
/// keyed by `AssetId`, paired with a human-readable label derived from
/// the texture's `TextureDef` variant.
pub(crate) fn collect_textures() -> Vec<(AssetId, String)> {
    use awsm_scene_schema::ProceduralTextureDef;
    let scene = app_state().scene.clone();
    let assets = scene.assets.lock().unwrap();
    let mut out: Vec<(AssetId, String)> = Vec::new();
    for (id, entry) in assets.entries.iter() {
        if let AssetSource::Texture(def) = &entry.source {
            let label = match def {
                TextureDef::Raster { display_name } => display_name.clone(),
                TextureDef::Procedural(ProceduralTextureDef::Checker { .. }) => {
                    "Procedural: Checker".to_string()
                }
                TextureDef::Procedural(ProceduralTextureDef::Gradient { .. }) => {
                    "Procedural: Gradient".to_string()
                }
                TextureDef::Procedural(ProceduralTextureDef::Noise { .. }) => {
                    "Procedural: Noise".to_string()
                }
            };
            out.push((*id, label));
        }
    }
    // Stable order — sort by label so the dropdown is predictable across
    // repaints. AssetId is a UUID and not naturally ordered.
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
}

/// A `<select>` whose options are every `AssetSource::Texture` entry in the
/// scene's asset table. `read_current` returns the host kind's currently-
/// referenced texture (if any); `write_new` mutates the host kind in place
/// with the picked (or cleared) reference.
///
/// Mirrors `node_id_select`'s shape so Sprite + Particle inspectors can wire
/// per-`TextureRef` slots without duplicating the dropdown machinery.
pub(crate) fn texture_ref_select(
    node: Arc<Node>,
    read_current: fn(&NodeKind) -> Option<TextureRef>,
    write_new: fn(&mut NodeKind, Option<TextureRef>),
) -> Dom {
    // The option list snapshots `collect_textures()` at construction; if
    // we built the `<select>` once and held onto it, adding a new
    // procedural-texture asset wouldn't appear in the dropdown until the
    // inspector re-rendered for some other reason. Mirroring
    // `material_ref_select`, wrap the body in a revision-driven
    // `child_signal` so any `scene.bump_revision()` rebuilds the options.
    let revision = app_state().scene.revision.clone();
    html!("div", {
        .child_signal(revision.signal().map(clone!(node => move |_rev| {
            Some(texture_ref_select_inner(node.clone(), read_current, write_new))
        })))
    })
}

fn texture_ref_select_inner(
    node: Arc<Node>,
    read_current: fn(&NodeKind) -> Option<TextureRef>,
    write_new: fn(&mut NodeKind, Option<TextureRef>),
) -> Dom {
    let kind = node.kind.clone();
    let candidates = collect_textures();
    let mut options = vec![html!("option", {
        .attr("value", "")
        .text("(none)")
    })];
    for (id, label) in &candidates {
        options.push(html!("option", {
            .attr("value", &id.0.to_string())
            .text(label)
        }));
    }

    html!("select" => web_sys::HtmlSelectElement, {
        .style("padding", "0.35rem 0.5rem")
        .style("background-color", ColorRaw::Darkest.value())
        .style("color", ColorText::SidebarHeader.value())
        .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .style("border-radius", "0.3rem")
        .style("font-size", "0.85rem")
        .style("cursor", "pointer")
        .children(options)
        .with_node!(select => {
            .future(clone!(kind, select => {
                kind.signal_cloned().for_each(move |k| {
                    let want = read_current(&k)
                        .map(|r| r.0.0.to_string())
                        .unwrap_or_default();
                    if select.value() != want {
                        select.set_value(&want);
                    }
                    async {}
                })
            }))
            .event(clone!(kind, select => move |_: events::Change| {
                let value = select.value();
                let new_ref = if value.is_empty() {
                    None
                } else {
                    let Ok(parsed) = uuid::Uuid::parse_str(&value) else {
                        return;
                    };
                    Some(TextureRef(AssetId(parsed)))
                };
                let mut k = kind.get_cloned();
                write_new(&mut k, new_ref);
                kind.set(k);
            }))
        })
    })
}

/// Collects every `AssetSource::Material(MaterialDef)` entry in the
/// live asset table. Uses the authored `MaterialDef.label` when set;
/// falls back to the short UUID prefix otherwise.
pub(crate) fn collect_materials() -> Vec<(AssetId, String)> {
    let scene = app_state().scene.clone();
    let assets = scene.assets.lock().unwrap();
    let mut out: Vec<(AssetId, String)> = Vec::new();
    for (id, entry) in assets.entries.iter() {
        if let AssetSource::Material(def) = &entry.source {
            let label = if def.label.is_empty() {
                format!("Material {}", &id.0.to_string()[..8])
            } else {
                def.label.clone()
            };
            out.push((*id, label));
        }
    }
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
}

/// A `<select>` whose options are every `AssetSource::Material` entry
/// in the scene's asset table, plus `(inline material)` for `None`.
/// Identical shape to `texture_ref_select` so the Primitive / Sweep /
/// Mesh editors can wire `Option<MaterialRef>` slots uniformly.
///
/// The option list is recomputed whenever `scene.revision` ticks —
/// that's how a fresh `+ Material Asset` click shows up here without
/// reloading the inspector.
pub(crate) fn material_ref_select(
    node: Arc<Node>,
    read_current: fn(&NodeKind) -> Option<awsm_scene_schema::MaterialRef>,
    write_new: fn(&mut NodeKind, Option<awsm_scene_schema::MaterialRef>),
) -> Dom {
    let revision = app_state().scene.revision.clone();
    html!("div", {
        .child_signal(revision.signal().map(clone!(node => move |_rev| {
            Some(material_ref_select_inner(node.clone(), read_current, write_new))
        })))
    })
}

fn material_ref_select_inner(
    node: Arc<Node>,
    read_current: fn(&NodeKind) -> Option<awsm_scene_schema::MaterialRef>,
    write_new: fn(&mut NodeKind, Option<awsm_scene_schema::MaterialRef>),
) -> Dom {
    use awsm_scene_schema::MaterialRef;
    let kind = node.kind.clone();
    let candidates = collect_materials();
    let mut options = vec![html!("option", {
        .attr("value", "")
        .text("(inline material)")
    })];
    for (id, label) in &candidates {
        options.push(html!("option", {
            .attr("value", &id.0.to_string())
            .text(label)
        }));
    }

    html!("select" => web_sys::HtmlSelectElement, {
        .style("padding", "0.35rem 0.5rem")
        .style("background-color", ColorRaw::Darkest.value())
        .style("color", ColorText::SidebarHeader.value())
        .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .style("border-radius", "0.3rem")
        .style("font-size", "0.85rem")
        .style("cursor", "pointer")
        .children(options)
        .with_node!(select => {
            .future(clone!(kind, select => {
                kind.signal_cloned().for_each(move |k| {
                    let want = read_current(&k)
                        .map(|r| r.0.0.to_string())
                        .unwrap_or_default();
                    if select.value() != want {
                        select.set_value(&want);
                    }
                    async {}
                })
            }))
            .event(clone!(kind, select => move |_: events::Change| {
                let value = select.value();
                let new_ref = if value.is_empty() {
                    None
                } else {
                    let Ok(parsed) = uuid::Uuid::parse_str(&value) else {
                        return;
                    };
                    Some(MaterialRef(AssetId(parsed)))
                };
                let mut k = kind.get_cloned();
                write_new(&mut k, new_ref);
                kind.set(k);
            }))
        })
    })
}
