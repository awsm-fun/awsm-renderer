//! Animation-mode **Add-Track picker**: a popup
//! anchored under an "Add Track" trigger that lists the animatable **targets**
//! of the *real* scene, grouped by node. Picking a property row dispatches
//! `EditorCommand::AddTrack { clip, target }` and closes.
//!
//! Load-bearing rule: a row click NEVER mutates state directly — it
//! dispatches `AddTrack` through the one `EditorController` (`spawn_local`).
//!
//! Target families wired here: **Transform** (Translation/Rotation/Scale on any
//! node), **Light** (Intensity/Color always; Range/InnerAngle/OuterAngle only for
//! the light variants that carry them), **Camera** (FovY/Near/Far),
//! **BuiltinParam** (BaseColor/Metallic/Roughness/Emissive on mesh-bearing nodes),
//! **Morph** (a single "Morph 0" row on mesh-bearing nodes — see below), and
//! **Uniform** (one row per declared uniform slot of each *dynamic* custom
//! material, rendered as material-scoped groups).
//!
//! Morph caveat: the editor scene `Node` does NOT expose a mesh's morph-target
//! count, so we cannot enumerate one row per morph index. We emit a single
//! `Morph { index: 0 }` row for the common single-morph case (the renderer
//! resolves index 0 robustly); authoring higher indices is deferred until the
//! editor model carries per-mesh morph counts.

use std::sync::Arc;

use crate::controller::animation::{
    find_clip, target_key, BuiltinParamKind, CameraParamKind, CustomAnimation, LightParamKind,
    TexSlot, TexTransformProp, TrackTarget, TransformProp,
};
use crate::controller::custom_material::CustomMaterial;
use crate::engine::scene::node::Node;
use crate::engine::scene::types::LightKind;
use crate::engine::scene::{AssetId, NodeKind};
use crate::prelude::*;

/// Render a labeled "Add Track" button that opens the target picker on click
/// (the ribbon + dope-sheet empty-state sites). `variant`/`size` mirror the ghost
/// vs. primary styling at each site.
pub fn button(variant: BtnVariant, size: BtnSize) -> Dom {
    anchored(move |open| {
        Btn::new()
            .label("Add Track")
            .icon("target")
            .variant(variant)
            .size(size)
            .on_click(open)
            .render()
    })
}

/// Render a compact icon-only "Add track" trigger that opens the picker (the
/// dope-sheet names-column header).
pub fn icon_button() -> Dom {
    anchored(|open| {
        IconBtn::new("plus")
            .title("Add track")
            .size(14.0)
            .on_click(open)
            .render()
    })
}

/// The shared trigger + popup shell: `build` makes the trigger Dom given an
/// `open` callback that captures the trigger element's rect into the anchor
/// state, mounting the picker popup.
fn anchored(build: impl FnOnce(Box<dyn FnMut()>) -> Dom) -> Dom {
    let rect: Mutable<Option<AnchorRect>> = Mutable::new(None);

    html!("span", {
        .style("display", "inline-flex")
        .style("position", "relative")
        .with_node!(elem => {
            .child({
                let open: Box<dyn FnMut()> = Box::new(clone!(rect => move || {
                    let r = elem.get_bounding_client_rect();
                    rect.set(Some(AnchorRect {
                        left: r.left(),
                        right: r.right(),
                        top: r.top(),
                        bottom: r.bottom(),
                        width: r.width(),
                    }));
                }));
                build(open)
            })
        })
        .child_signal(rect.signal().map(clone!(rect => move |maybe| {
            maybe.map(clone!(rect => move |anchor| menu(anchor, rect.clone())))
        })))
    })
}

/// The picker popup body: a search field over a target-grouped property list.
/// Reads the *active* clip (`current_clip` → `custom_animations`) so it knows
/// which clip to add to (and which rows are already taken).
fn menu(anchor: AnchorRect, rect: Mutable<Option<AnchorRect>>) -> Dom {
    let clip = controller()
        .current_clip
        .get()
        .and_then(|id| find_clip(&controller().custom_animations, id));

    let body = match clip {
        Some(clip) => picker_body(clip, rect.clone()),
        None => no_clip_body(),
    };

    popup(
        anchor,
        Align::Left,
        Some(328.0),
        420.0,
        clone!(rect => move || rect.set(None)),
        vec![body],
    )
}

/// The "create a clip first" hint shown when there is no active clip.
fn no_clip_body() -> Dom {
    html!("div", {
        .style("padding", "16px 14px")
        .style("font-size", "12px").style("color", "var(--text-3)").style("line-height", "1.5")
        .text("Create or select a clip first \u{2014} then add a track to it.")
    })
}

/// The search box + grouped target list for `clip`.
fn picker_body(clip: Arc<CustomAnimation>, rect: Mutable<Option<AnchorRect>>) -> Dom {
    // Already-added identity keys (target_key) of the clip's existing tracks.
    let existing: Arc<Vec<String>> = Arc::new(
        clip.tracks
            .lock_ref()
            .iter()
            .map(|t| target_key(&t.target))
            .collect(),
    );

    let groups = Arc::new(collect_groups());
    let query = Mutable::new(String::new());

    html!("div", {
        // ── search ───────────────────────────────────────────────────────────
        .child(html!("div", {
            .style("padding", "9px 10px").style("border-bottom", "1px solid var(--line-soft)")
            .child(
                TextInput::new(query.clone())
                    .placeholder("Search nodes & properties\u{2026}")
                    .icon("search")
                    .render()
            )
        }))
        // ── scrollable grouped list (reactive on the search query) ─────────────
        .child(html!("div", {
            .style("max-height", "330px").style("overflow", "auto").style("padding", "6px 0")
            .child_signal(query.signal_cloned().map({
                let clip_id = clip.id;
                clone!(groups, existing, rect => move |q| {
                    Some(list(&groups, &existing, &q, clip_id, rect.clone()))
                })
            }))
        }))
    })
}

/// The filtered, grouped list of property rows (rebuilt as the query changes).
fn list(
    groups: &[TargetGroup],
    existing: &[String],
    query: &str,
    clip: AssetId,
    rect: Mutable<Option<AnchorRect>>,
) -> Dom {
    let ql = query.to_lowercase();

    // Filter each group's rows by the query (matching target name OR prop label).
    let filtered: Vec<(&TargetGroup, Vec<&PropRow>)> = groups
        .iter()
        .filter_map(|g| {
            let rows: Vec<&PropRow> = g
                .rows
                .iter()
                .filter(|r| {
                    ql.is_empty()
                        || format!("{} {} {}", g.name, r.label, g.kind)
                            .to_lowercase()
                            .contains(&ql)
                })
                .collect();
            (!rows.is_empty()).then_some((g, rows))
        })
        .collect();

    if filtered.is_empty() {
        return html!("div", {
            .style("padding", "14px").style("font-size", "12px").style("color", "var(--text-3)")
            .text("No matching targets.")
        });
    }

    html!("div", {
        .children(filtered.into_iter().map(clone!(rect => move |(group, rows)| {
            group_block(group, rows, existing, clip, rect.clone())
        })))
    })
}

/// One target group: a header (icon · name · kind) then its property rows.
fn group_block(
    group: &TargetGroup,
    rows: Vec<&PropRow>,
    existing: &[String],
    clip: AssetId,
    rect: Mutable<Option<AnchorRect>>,
) -> Dom {
    html!("div", {
        // group header
        .child(html!("div", {
            .style("display", "flex").style("align-items", "center").style("gap", "7px")
            .style("padding", "6px 12px 3px")
            .child(Icon::new(group.icon).size(13.0).color("var(--text-3)").render())
            .child(html!("span", { .class("kicker").style("font-size", "9.5px").text(&group.name) }))
            .child(html!("span", {
                .class("mono").style("font-size", "9px").style("color", "var(--text-3)")
                .text(group.kind)
            }))
        }))
        .children(rows.into_iter().map(clone!(rect => move |row| {
            prop_button(row, existing, clip, rect.clone())
        })))
    })
}

/// One property row: prop name · optional kind badge · "lowers to" hint (or a
/// dimmed, disabled "added" when the target+prop already exists in the clip).
fn prop_button(
    row: &PropRow,
    existing: &[String],
    clip: AssetId,
    rect: Mutable<Option<AnchorRect>>,
) -> Dom {
    let added = existing.iter().any(|k| *k == target_key(&row.target));
    let hover = Mutable::new(false);
    let target = row.target.clone();
    let badge = row.badge;
    let hint = row.hint.clone();

    html!("button", {
        .class("t")
        .attr("type", "button")
        .style("display", "flex").style("align-items", "center").style("gap", "9px")
        .style("width", "100%").style("padding", "6px 12px 6px 30px")
        .style("border", "1px solid transparent")
        .style("text-align", "left")
        .style("cursor", if added { "default" } else { "pointer" })
        .style("opacity", if added { "0.45" } else { "1" })
        .style_signal("background", hover.signal().map(move |h| {
            if h && !added { "var(--bg-hover)" } else { "transparent" }
        }))
        .event(clone!(hover => move |_: events::MouseEnter| hover.set_neq(true)))
        .event(clone!(hover => move |_: events::MouseLeave| hover.set_neq(false)))
        .event(clone!(rect, target => move |_: events::Click| {
            if !added {
                dispatch(EditorCommand::AddTrack { clip, target: target.clone() });
                rect.set(None);
            }
        }))
        // prop name
        .child(html!("span", {
            .style("flex", "1").style("font-size", "12.5px").style("color", "var(--text-0)")
            .text(&row.label)
        }))
        // kind badge (UNIFORM / MORPH / LIGHT / CAMERA)
        .apply(move |b| match badge {
            Some(text) => b.child(kind_badge(text)),
            None => b,
        })
        // right-aligned "lowers to" hint, or "added" when taken
        .child(html!("span", {
            .class("mono").style("font-size", "9.5px").style("color", "var(--text-3)")
            .style("min-width", "96px").style("text-align", "right")
            .text(if added { "added" } else { &hint })
        }))
    })
}

/// A small uppercase pill badge for special target kinds.
fn kind_badge(text: &str) -> Dom {
    html!("span", {
        .class("mono")
        .style("font-size", "8.5px").style("font-weight", "700").style("letter-spacing", "0.04em")
        .style("padding", "1px 5px").style("border-radius", "999px")
        .style("color", "var(--accent-bright)")
        .style("background", "var(--accent-ghost)")
        .style("border", "1px solid var(--accent-line)")
        .text(text)
    })
}

// ── target enumeration ───────────────────────────────────────────────────────

/// A property row in the picker: its serializable target + display chrome.
struct PropRow {
    /// The serializable binding dispatched on click.
    target: TrackTarget,
    /// The property name (left column).
    label: String,
    /// An optional kind pill (e.g. `LIGHT` / `CAMERA`).
    badge: Option<&'static str>,
    /// The right-aligned "lowers to" hint (a `String` so runtime types — e.g. a
    /// custom material's declared uniform WGSL type — can be carried directly).
    hint: String,
}

/// One node's group of property rows.
struct TargetGroup {
    /// The node's display name (group header).
    name: String,
    /// The kind label (mono, header right).
    kind: &'static str,
    /// The kind glyph (Lucide name).
    icon: &'static str,
    rows: Vec<PropRow>,
}

/// Walk the live scene tree and build one [`TargetGroup`] per node that offers
/// at least one animatable property.
fn collect_groups() -> Vec<TargetGroup> {
    fn walk(nodes: &[Arc<Node>], out: &mut Vec<TargetGroup>) {
        for node in nodes {
            if let Some(group) = node_group(node) {
                out.push(group);
            }
            let children = node.children.lock_ref();
            walk(children.as_slice(), out);
        }
    }
    let mut out = Vec::new();
    let scene = controller().scene.clone();
    let nodes = scene.nodes.lock_ref();
    walk(nodes.as_slice(), &mut out);
    // Material-scoped uniform groups (one per dynamic custom material with declared
    // uniform slots) follow the per-node groups — they aren't tied to a node.
    out.extend(collect_uniform_groups());
    out
}

/// The animatable-property group for one node (or `None` if it offers nothing —
/// it always offers Transform, so this currently never returns `None`, but the
/// shape leaves room for future kind-gated nodes).
fn node_group(node: &Arc<Node>) -> Option<TargetGroup> {
    let id = node.id;
    let name = node.name.get_cloned();
    let kind = node.kind.lock_ref();

    // Every node carries a Transform.
    let mut rows = vec![
        PropRow {
            target: TrackTarget::Transform {
                node: id,
                prop: TransformProp::Translation,
            },
            label: "Translation".into(),
            badge: None,
            hint: "vec3 \u{00b7} TRS".into(),
        },
        PropRow {
            target: TrackTarget::Transform {
                node: id,
                prop: TransformProp::Rotation,
            },
            label: "Rotation".into(),
            badge: None,
            hint: "quat \u{00b7} TRS".into(),
        },
        PropRow {
            target: TrackTarget::Transform {
                node: id,
                prop: TransformProp::Scale,
            },
            label: "Scale".into(),
            badge: None,
            hint: "vec3 \u{00b7} TRS".into(),
        },
    ];

    let (kind_label, icon) = match &*kind {
        NodeKind::Light(cfg) => {
            rows.extend(light_rows(id, cfg.kind()));
            ("light", "light")
        }
        NodeKind::Camera(_) => {
            rows.extend(camera_rows(id));
            ("camera", "camera")
        }
        // Mesh-bearing nodes carry a first-party material (its built-in factors
        // always exist) and a (possibly morphable) mesh.
        NodeKind::Mesh { .. } => {
            rows.extend(mesh_material_rows(id));
            ("mesh", "cube")
        }
        // A skinned mesh carries the same first-party material as a Mesh (its
        // skeletal deformation is driven by its bone nodes' transform tracks).
        NodeKind::SkinnedMesh { .. } => {
            rows.extend(mesh_material_rows(id));
            ("skinned mesh", "cube")
        }
        _ => ("node", "cube"),
    };

    Some(TargetGroup {
        name,
        kind: kind_label,
        icon,
        rows,
    })
}

/// The light-parameter rows for a light node. Intensity + Color always; Range
/// only for Point/Spot; Inner/OuterAngle only for Spot.
fn light_rows(node: crate::engine::scene::NodeId, kind: LightKind) -> Vec<PropRow> {
    let mut rows = vec![
        PropRow {
            target: TrackTarget::Light {
                node,
                param: LightParamKind::Intensity,
            },
            label: "Intensity".into(),
            badge: Some("LIGHT"),
            hint: "scalar \u{00b7} light".into(),
        },
        PropRow {
            target: TrackTarget::Light {
                node,
                param: LightParamKind::Color,
            },
            label: "Color".into(),
            badge: Some("LIGHT"),
            hint: "vec3 \u{00b7} light".into(),
        },
    ];
    if matches!(kind, LightKind::Point | LightKind::Spot) {
        rows.push(PropRow {
            target: TrackTarget::Light {
                node,
                param: LightParamKind::Range,
            },
            label: "Range".into(),
            badge: Some("LIGHT"),
            hint: "scalar \u{00b7} light".into(),
        });
    }
    if matches!(kind, LightKind::Spot) {
        rows.push(PropRow {
            target: TrackTarget::Light {
                node,
                param: LightParamKind::InnerAngle,
            },
            label: "Inner Angle".into(),
            badge: Some("LIGHT"),
            hint: "scalar \u{00b7} light".into(),
        });
        rows.push(PropRow {
            target: TrackTarget::Light {
                node,
                param: LightParamKind::OuterAngle,
            },
            label: "Outer Angle".into(),
            badge: Some("LIGHT"),
            hint: "scalar \u{00b7} light".into(),
        });
    }
    rows
}

/// The camera-parameter rows for a camera node (FovY/Near/Far). Camera tracks
/// lower later — still offered here.
fn camera_rows(node: crate::engine::scene::NodeId) -> Vec<PropRow> {
    vec![
        PropRow {
            target: TrackTarget::Camera {
                node,
                param: CameraParamKind::FovY,
            },
            label: "Field of View".into(),
            badge: Some("CAMERA"),
            hint: "scalar \u{00b7} camera".into(),
        },
        PropRow {
            target: TrackTarget::Camera {
                node,
                param: CameraParamKind::Near,
            },
            label: "Near".into(),
            badge: Some("CAMERA"),
            hint: "scalar \u{00b7} camera".into(),
        },
        PropRow {
            target: TrackTarget::Camera {
                node,
                param: CameraParamKind::Far,
            },
            label: "Far".into(),
            badge: Some("CAMERA"),
            hint: "scalar \u{00b7} camera".into(),
        },
    ]
}

/// The built-in-material + morph rows for a mesh-bearing node (Primitive / Mesh /
/// Model). The four PBR factors always exist on a first-party material, so they're
/// offered unconditionally. Morph emits a single `index: 0` row (see module doc).
fn mesh_material_rows(node: crate::engine::scene::NodeId) -> Vec<PropRow> {
    vec![
        PropRow {
            target: TrackTarget::BuiltinParam {
                node,
                param: BuiltinParamKind::BaseColor,
            },
            label: "Base Color".into(),
            badge: Some("BUILTIN"),
            hint: "vec3 \u{00b7} builtin".into(),
        },
        PropRow {
            target: TrackTarget::BuiltinParam {
                node,
                param: BuiltinParamKind::Metallic,
            },
            label: "Metallic".into(),
            badge: Some("BUILTIN"),
            hint: "f32 \u{00b7} builtin".into(),
        },
        PropRow {
            target: TrackTarget::BuiltinParam {
                node,
                param: BuiltinParamKind::Roughness,
            },
            label: "Roughness".into(),
            badge: Some("BUILTIN"),
            hint: "f32 \u{00b7} builtin".into(),
        },
        PropRow {
            target: TrackTarget::BuiltinParam {
                node,
                param: BuiltinParamKind::Emissive,
            },
            label: "Emissive".into(),
            badge: Some("BUILTIN"),
            hint: "vec3 \u{00b7} builtin".into(),
        },
        // Base-color UV transform — the "scrolling/rotating texture" case. The
        // apply seeds an identity transform on the slot if it has none, so these
        // are offered unconditionally (a no-op if the slot is untextured).
        PropRow {
            target: TrackTarget::TextureTransform {
                node,
                slot: TexSlot::BaseColor,
                prop: TexTransformProp::Offset,
            },
            label: "Base Color UV Offset".into(),
            badge: Some("UV"),
            hint: "vec2 \u{00b7} texture".into(),
        },
        PropRow {
            target: TrackTarget::TextureTransform {
                node,
                slot: TexSlot::BaseColor,
                prop: TexTransformProp::Scale,
            },
            label: "Base Color UV Scale".into(),
            badge: Some("UV"),
            hint: "vec2 \u{00b7} texture".into(),
        },
        PropRow {
            target: TrackTarget::TextureTransform {
                node,
                slot: TexSlot::BaseColor,
                prop: TexTransformProp::Rotation,
            },
            label: "Base Color UV Rotation".into(),
            badge: Some("UV"),
            hint: "f32 \u{00b7} texture".into(),
        },
        // Single morph row — the editor scene model doesn't carry per-mesh
        // morph-target counts, so we can't enumerate indices. Index 0 covers the
        // common single-morph case and the renderer resolves it robustly.
        PropRow {
            target: TrackTarget::Morph { node, index: 0 },
            label: "Morph 0".into(),
            badge: Some("MORPH"),
            hint: "f32 \u{00b7} morph".into(),
        },
    ]
}

/// Build one [`TargetGroup`] per *dynamic* custom material that declares at least
/// one uniform slot. These targets are material-scoped (keyed by `AssetId`, not a
/// node).
///
/// Only dynamic (WGSL) materials are offered: a built-in material (`builtin ==
/// Some`) animates via the per-node BuiltinParam rows above, not named uniforms.
fn collect_uniform_groups() -> Vec<TargetGroup> {
    let materials = controller().custom_materials.clone();
    let mats = materials.lock_ref();
    mats.iter().filter_map(uniform_group).collect()
}

/// The uniform group for one custom material, or `None` if it's a built-in
/// material or declares no uniform slots.
fn uniform_group(mat: &Arc<CustomMaterial>) -> Option<TargetGroup> {
    // Built-in materials animate via BuiltinParam, not named uniforms.
    if mat.builtin.lock_ref().is_some() {
        return None;
    }
    let material = mat.id;
    let rows: Vec<PropRow> = mat
        .uniforms
        .lock_ref()
        .iter()
        .map(|slot| PropRow {
            target: TrackTarget::Uniform {
                material,
                name: slot.name.clone(),
            },
            label: slot.name.clone(),
            badge: Some("UNIFORM"),
            // The slot's declared WGSL type is the "lowers to" hint.
            hint: slot.ty.clone(),
        })
        .collect();

    (!rows.is_empty()).then(|| TargetGroup {
        name: mat.name.get_cloned(),
        kind: "uniform",
        icon: "sliders",
        rows,
    })
}

/// Dispatch a command through the one controller (`spawn_local`).
fn dispatch(cmd: EditorCommand) {
    spawn_local(async move {
        let _ = controller().dispatch(cmd).await;
    });
}
