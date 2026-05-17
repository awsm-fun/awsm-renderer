// ─────────────────────────────────────────────────────────────────────
// SweepAlongCurve editor
// ─────────────────────────────────────────────────────────────────────
//
// The closure-based [`render_sweep_editor`] is the param-editor shape
// reused by both the live Sweep node inspector (which reads/writes
// through `node.kind`) and the Mesh asset inspector (which reads/writes
// through `MeshDef.source = CapturedSource::Sweep(_)` and triggers a
// `recapture_from_source_def` after each edit).

use crate::prelude::*;
use crate::properties::transform::number_input;
use crate::scene::{Node, NodeKind};
use awsm_scene_schema::{CrossSectionDef, SweepAlongCurveDef, SweepUvMode};

use super::{field_row, node_id_select, section_header};

pub fn render(node: Arc<Node>) -> Dom {
    let kind_for_read = node.kind.clone();
    let kind_for_write = node.kind.clone();
    let revision = crate::state::app_state().scene.revision.clone();
    let read = move || match kind_for_read.get_cloned() {
        NodeKind::SweepAlongCurve { def, .. } => def,
        _ => SweepAlongCurveDef::default(),
    };
    let write = move |new_def: SweepAlongCurveDef| {
        let mut k = kind_for_write.get_cloned();
        if let NodeKind::SweepAlongCurve { def, .. } = &mut k {
            *def = new_def;
            kind_for_write.set(k);
        }
    };
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.5rem")
        .child(section_header("Sweep Along Curve"))
        .child(field_row("Curve node", node_id_select(
            node.clone(),
            |k| matches!(k, NodeKind::Curve(_)),
            |k| match k {
                NodeKind::SweepAlongCurve { def, .. } => Some(def.curve_node),
                _ => None,
            },
            |k, new_id| {
                if let NodeKind::SweepAlongCurve { def, .. } = k {
                    def.curve_node = new_id;
                }
            },
        )))
        .child(render_sweep_editor(read, write, revision))
        // Optional shared-material picker (D-1c).
        .child(super::field_row("Material asset", super::material_ref_select(
            node.clone(),
            |k| match k {
                NodeKind::SweepAlongCurve { material, .. } => *material,
                _ => None,
            },
            |k, new_ref| {
                if let NodeKind::SweepAlongCurve { material, .. } = k {
                    *material = new_ref;
                }
            },
        )))
        // Material editor — see Primitive for the asset-vs-inline switch.
        .child(super::material::render_material_for_node(
            node.clone(),
            |k| match k {
                NodeKind::SweepAlongCurve { material, .. } => *material,
                _ => None,
            },
            |k| match k {
                NodeKind::SweepAlongCurve { inline_material, .. } => Some(inline_material),
                _ => None,
            },
            |k, new_def| {
                if let NodeKind::SweepAlongCurve { inline_material, .. } = k {
                    *inline_material = new_def;
                }
            },
        ))
        // F10: snapshot the current swept geometry into a shareable
        // Mesh asset and re-point the node at it.
        .child(super::capture_as_mesh_button(node))
    })
}

/// Render the editable param surface for a `SweepAlongCurveDef` held
/// outside a `NodeKind` — same UI shape as the live Sweep node
/// inspector, but driven by closures so the Mesh asset inspector can
/// reuse it against `MeshDef.source = CapturedSource::Sweep(_)`.
///
/// Curve-node selection is NOT covered here; the live-node inspector
/// renders its own `node_id_select` and the Mesh inspector relies on
/// its source-picker dropdown to re-capture from a different curve.
pub(crate) fn render_sweep_editor(
    read: impl Fn() -> SweepAlongCurveDef + Clone + 'static,
    write: impl Fn(SweepAlongCurveDef) + Clone + 'static,
    revision: Mutable<u64>,
) -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.5rem")
        .child(field_row(
            "Samples",
            samples_input(read.clone(), write.clone(), revision.clone()),
        ))
        .child(field_row(
            "Up hint",
            up_hint_inputs(read.clone(), write.clone(), revision.clone()),
        ))
        .child(field_row(
            "Cross-section",
            cross_section_select(read.clone(), write.clone(), revision.clone()),
        ))
        // Variant-specific cross-section params. Dedupe on the variant
        // tag so dragging a dim doesn't tear down the input mid-drag.
        .child_signal(revision.signal_cloned().map(clone!(read, write, revision => move |_rev| {
            let tag = CrossSectionTag::from(&read().cross_section);
            Some(cross_section_param_rows(read.clone(), write.clone(), revision.clone(), tag))
        })))
        .child(field_row(
            "UV mode",
            uv_mode_select(read.clone(), write.clone(), revision.clone()),
        ))
        .child_signal(revision.signal_cloned().map(clone!(read, write, revision => move |_rev| {
            let tag = SweepUvModeTag::from(&read().uv_mode);
            Some(uv_mode_param_rows(read.clone(), write.clone(), revision.clone(), tag))
        })))
    })
}

fn samples_input(
    read: impl Fn() -> SweepAlongCurveDef + Clone + 'static,
    write: impl Fn(SweepAlongCurveDef) + Clone + 'static,
    revision: Mutable<u64>,
) -> Dom {
    use futures_signals::signal::SignalExt;
    let read_for_signal = read.clone();
    let value_signal = revision
        .signal()
        .map(move |_| read_for_signal().samples as f32);
    number_input(value_signal, move |new_value| {
        let mut def = read();
        def.samples = (new_value.max(2.0) as u32).clamp(2, 4096);
        write(def);
    })
}

fn up_hint_inputs(
    read: impl Fn() -> SweepAlongCurveDef + Clone + 'static,
    write: impl Fn(SweepAlongCurveDef) + Clone + 'static,
    revision: Mutable<u64>,
) -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("gap", "0.35rem")
        .child(up_hint_axis(read.clone(), write.clone(), revision.clone(), 0))
        .child(up_hint_axis(read.clone(), write.clone(), revision.clone(), 1))
        .child(up_hint_axis(read, write, revision, 2))
    })
}

fn up_hint_axis(
    read: impl Fn() -> SweepAlongCurveDef + Clone + 'static,
    write: impl Fn(SweepAlongCurveDef) + Clone + 'static,
    revision: Mutable<u64>,
    axis: usize,
) -> Dom {
    use futures_signals::signal::SignalExt;
    let read_for_signal = read.clone();
    let value_signal = revision
        .signal()
        .map(move |_| read_for_signal().up_hint[axis]);
    number_input(value_signal, move |new_value| {
        let mut def = read();
        def.up_hint[axis] = new_value;
        write(def);
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CrossSectionTag {
    Strip,
    Tube,
    Wall,
    Profile,
}

impl From<&CrossSectionDef> for CrossSectionTag {
    fn from(d: &CrossSectionDef) -> Self {
        match d {
            CrossSectionDef::Strip { .. } => CrossSectionTag::Strip,
            CrossSectionDef::Tube { .. } => CrossSectionTag::Tube,
            CrossSectionDef::Wall { .. } => CrossSectionTag::Wall,
            CrossSectionDef::Profile { .. } => CrossSectionTag::Profile,
        }
    }
}

const CS_STRIP: &str = "strip";
const CS_TUBE: &str = "tube";
const CS_WALL: &str = "wall";
const CS_PROFILE: &str = "profile";

fn cross_section_select(
    read: impl Fn() -> SweepAlongCurveDef + Clone + 'static,
    write: impl Fn(SweepAlongCurveDef) + Clone + 'static,
    revision: Mutable<u64>,
) -> Dom {
    use futures_signals::signal::SignalExt;
    html!("select" => web_sys::HtmlSelectElement, {
        .style("padding", "0.35rem 0.5rem")
        .style("background-color", ColorRaw::Darkest.value())
        .style("color", ColorText::SidebarHeader.value())
        .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .style("border-radius", "0.3rem")
        .style("font-size", "0.85rem")
        .style("cursor", "pointer")
        .child(html!("option", { .attr("value", CS_STRIP).text("Strip") }))
        .child(html!("option", { .attr("value", CS_TUBE).text("Tube") }))
        .child(html!("option", { .attr("value", CS_WALL).text("Wall") }))
        .child(html!("option", { .attr("value", CS_PROFILE).text("Profile") }))
        .with_node!(select => {
            .future(clone!(read, select => {
                revision.signal().for_each(move |_| {
                    let want = match CrossSectionTag::from(&read().cross_section) {
                        CrossSectionTag::Strip => CS_STRIP,
                        CrossSectionTag::Tube => CS_TUBE,
                        CrossSectionTag::Wall => CS_WALL,
                        CrossSectionTag::Profile => CS_PROFILE,
                    };
                    if select.value() != want {
                        select.set_value(want);
                    }
                    async {}
                })
            }))
            .event(clone!(select => move |_: events::Change| {
                let new_section = match select.value().as_str() {
                    CS_STRIP => CrossSectionDef::default_strip(),
                    CS_WALL => CrossSectionDef::default_wall(),
                    CS_PROFILE => CrossSectionDef::default_profile(),
                    _ => CrossSectionDef::default_tube(),
                };
                let mut def = read();
                if CrossSectionTag::from(&def.cross_section) != CrossSectionTag::from(&new_section) {
                    def.cross_section = new_section;
                    write(def);
                }
            }))
        })
    })
}

fn cross_section_param_rows(
    read: impl Fn() -> SweepAlongCurveDef + Clone + 'static,
    write: impl Fn(SweepAlongCurveDef) + Clone + 'static,
    revision: Mutable<u64>,
    tag: CrossSectionTag,
) -> Dom {
    let rows: Vec<Dom> = match tag {
        CrossSectionTag::Strip => vec![
            field_row(
                "Width",
                cs_scalar(
                    read.clone(),
                    write.clone(),
                    revision.clone(),
                    CsField::StripWidth,
                ),
            ),
            field_row(
                "Y offset",
                cs_scalar(read, write, revision, CsField::StripYOffset),
            ),
        ],
        CrossSectionTag::Tube => vec![
            field_row(
                "Radius",
                cs_scalar(
                    read.clone(),
                    write.clone(),
                    revision.clone(),
                    CsField::TubeRadius,
                ),
            ),
            field_row(
                "Segments",
                cs_scalar(read, write, revision, CsField::TubeSegments),
            ),
        ],
        CrossSectionTag::Wall => vec![
            field_row(
                "Width",
                cs_scalar(
                    read.clone(),
                    write.clone(),
                    revision.clone(),
                    CsField::WallWidth,
                ),
            ),
            field_row(
                "Height",
                cs_scalar(read, write, revision, CsField::WallHeight),
            ),
        ],
        CrossSectionTag::Profile => vec![field_row(
            "Points",
            // Profile editing needs a full 2D point list UI — deferred.
            // Drop a readonly summary so the user at least knows how
            // many points they're rendering with.
            profile_points_summary(read.clone()),
        )],
    };
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.35rem")
        .children(rows)
    })
}

fn profile_points_summary(read: impl Fn() -> SweepAlongCurveDef + Clone + 'static) -> Dom {
    let def = read();
    let count = match &def.cross_section {
        CrossSectionDef::Profile { points, closed } => Some((points.len(), *closed)),
        _ => None,
    };
    html!("div", {
        .style("font-size", "0.8rem")
        .style("color", ColorText::Byline.value())
        .text(&match count {
            Some((n, true)) => format!("{n} points, closed (edit via project.json)"),
            Some((n, false)) => format!("{n} points, open (edit via project.json)"),
            None => "(switch variant to edit)".to_string(),
        })
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CsField {
    StripWidth,
    StripYOffset,
    TubeRadius,
    TubeSegments,
    WallWidth,
    WallHeight,
}

fn cs_field_read(def: &SweepAlongCurveDef, field: CsField) -> f32 {
    match (&def.cross_section, field) {
        (CrossSectionDef::Strip { width, .. }, CsField::StripWidth) => *width,
        (CrossSectionDef::Strip { y_offset, .. }, CsField::StripYOffset) => *y_offset,
        (CrossSectionDef::Tube { radius, .. }, CsField::TubeRadius) => *radius,
        (
            CrossSectionDef::Tube {
                radial_segments, ..
            },
            CsField::TubeSegments,
        ) => *radial_segments as f32,
        (CrossSectionDef::Wall { width, .. }, CsField::WallWidth) => *width,
        (CrossSectionDef::Wall { height, .. }, CsField::WallHeight) => *height,
        _ => 0.0,
    }
}

fn cs_field_write(def: &mut SweepAlongCurveDef, field: CsField, new_value: f32) {
    match (&mut def.cross_section, field) {
        (CrossSectionDef::Strip { width, .. }, CsField::StripWidth) => {
            *width = new_value.max(1.0e-3);
        }
        (CrossSectionDef::Strip { y_offset, .. }, CsField::StripYOffset) => {
            *y_offset = new_value;
        }
        (CrossSectionDef::Tube { radius, .. }, CsField::TubeRadius) => {
            *radius = new_value.max(1.0e-3);
        }
        (
            CrossSectionDef::Tube {
                radial_segments, ..
            },
            CsField::TubeSegments,
        ) => {
            *radial_segments = (new_value.max(3.0) as u32).clamp(3, 256);
        }
        (CrossSectionDef::Wall { width, .. }, CsField::WallWidth) => {
            *width = new_value.max(1.0e-3);
        }
        (CrossSectionDef::Wall { height, .. }, CsField::WallHeight) => {
            *height = new_value.max(1.0e-3);
        }
        _ => {}
    }
}

fn cs_scalar(
    read: impl Fn() -> SweepAlongCurveDef + Clone + 'static,
    write: impl Fn(SweepAlongCurveDef) + Clone + 'static,
    revision: Mutable<u64>,
    field: CsField,
) -> Dom {
    use futures_signals::signal::SignalExt;
    let read_for_signal = read.clone();
    let value_signal = revision
        .signal()
        .map(move |_| cs_field_read(&read_for_signal(), field));
    number_input(value_signal, move |new_value| {
        let mut def = read();
        cs_field_write(&mut def, field, new_value);
        write(def);
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SweepUvModeTag {
    StretchOnce,
    RepeatByLength,
}

impl From<&SweepUvMode> for SweepUvModeTag {
    fn from(m: &SweepUvMode) -> Self {
        match m {
            SweepUvMode::StretchOnce => SweepUvModeTag::StretchOnce,
            SweepUvMode::RepeatByLength { .. } => SweepUvModeTag::RepeatByLength,
        }
    }
}

const UV_STRETCH: &str = "stretch";
const UV_REPEAT: &str = "repeat";

fn uv_mode_select(
    read: impl Fn() -> SweepAlongCurveDef + Clone + 'static,
    write: impl Fn(SweepAlongCurveDef) + Clone + 'static,
    revision: Mutable<u64>,
) -> Dom {
    use futures_signals::signal::SignalExt;
    html!("select" => web_sys::HtmlSelectElement, {
        .style("padding", "0.35rem 0.5rem")
        .style("background-color", ColorRaw::Darkest.value())
        .style("color", ColorText::SidebarHeader.value())
        .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .style("border-radius", "0.3rem")
        .style("font-size", "0.85rem")
        .style("cursor", "pointer")
        .child(html!("option", { .attr("value", UV_STRETCH).text("Stretch once") }))
        .child(html!("option", { .attr("value", UV_REPEAT).text("Repeat by length") }))
        .with_node!(select => {
            .future(clone!(read, select => {
                revision.signal().for_each(move |_| {
                    let want = match SweepUvModeTag::from(&read().uv_mode) {
                        SweepUvModeTag::StretchOnce => UV_STRETCH,
                        SweepUvModeTag::RepeatByLength => UV_REPEAT,
                    };
                    if select.value() != want {
                        select.set_value(want);
                    }
                    async {}
                })
            }))
            .event(clone!(select => move |_: events::Change| {
                let new_mode = match select.value().as_str() {
                    UV_REPEAT => SweepUvMode::RepeatByLength {
                        u_repeat: 1.0,
                        v_repeat_per_unit: 1.0,
                    },
                    _ => SweepUvMode::StretchOnce,
                };
                let mut def = read();
                if SweepUvModeTag::from(&def.uv_mode) != SweepUvModeTag::from(&new_mode) {
                    def.uv_mode = new_mode;
                    write(def);
                }
            }))
        })
    })
}

fn uv_mode_param_rows(
    read: impl Fn() -> SweepAlongCurveDef + Clone + 'static,
    write: impl Fn(SweepAlongCurveDef) + Clone + 'static,
    revision: Mutable<u64>,
    tag: SweepUvModeTag,
) -> Dom {
    let rows: Vec<Dom> = match tag {
        SweepUvModeTag::StretchOnce => vec![],
        SweepUvModeTag::RepeatByLength => vec![
            field_row(
                "U repeat",
                uv_scalar(
                    read.clone(),
                    write.clone(),
                    revision.clone(),
                    UvField::URepeat,
                ),
            ),
            field_row(
                "V repeat/unit",
                uv_scalar(read, write, revision, UvField::VRepeatPerUnit),
            ),
        ],
    };
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.35rem")
        .children(rows)
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum UvField {
    URepeat,
    VRepeatPerUnit,
}

fn uv_field_read(def: &SweepAlongCurveDef, field: UvField) -> f32 {
    match (&def.uv_mode, field) {
        (SweepUvMode::RepeatByLength { u_repeat, .. }, UvField::URepeat) => *u_repeat,
        (
            SweepUvMode::RepeatByLength {
                v_repeat_per_unit, ..
            },
            UvField::VRepeatPerUnit,
        ) => *v_repeat_per_unit,
        _ => 0.0,
    }
}

fn uv_field_write(def: &mut SweepAlongCurveDef, field: UvField, new_value: f32) {
    if let SweepUvMode::RepeatByLength {
        u_repeat,
        v_repeat_per_unit,
    } = &mut def.uv_mode
    {
        match field {
            UvField::URepeat => *u_repeat = new_value,
            UvField::VRepeatPerUnit => *v_repeat_per_unit = new_value,
        }
    }
}

fn uv_scalar(
    read: impl Fn() -> SweepAlongCurveDef + Clone + 'static,
    write: impl Fn(SweepAlongCurveDef) + Clone + 'static,
    revision: Mutable<u64>,
    field: UvField,
) -> Dom {
    use futures_signals::signal::SignalExt;
    let read_for_signal = read.clone();
    let value_signal = revision
        .signal()
        .map(move |_| uv_field_read(&read_for_signal(), field));
    number_input(value_signal, move |new_value| {
        let mut def = read();
        uv_field_write(&mut def, field, new_value);
        write(def);
    })
}
