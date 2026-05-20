// ─────────────────────────────────────────────────────────────────────
// Primitive editor
// ─────────────────────────────────────────────────────────────────────

use crate::prelude::*;
use crate::properties::transform::number_input;
use crate::scene::{Node, NodeKind};
use awsm_scene_schema::PrimitiveShape;

use super::{field_row, section_header};

#[derive(Clone, Copy, PartialEq, Eq)]
enum PrimitiveShapeTag {
    Plane,
    Box,
    Sphere,
    Cylinder,
    Cone,
    Torus,
}

fn primitive_shape_tag(k: &NodeKind) -> Option<PrimitiveShapeTag> {
    match k {
        NodeKind::Primitive { shape, .. } => Some(match shape {
            PrimitiveShape::Plane { .. } => PrimitiveShapeTag::Plane,
            PrimitiveShape::Box { .. } => PrimitiveShapeTag::Box,
            PrimitiveShape::Sphere { .. } => PrimitiveShapeTag::Sphere,
            PrimitiveShape::Cylinder { .. } => PrimitiveShapeTag::Cylinder,
            PrimitiveShape::Cone { .. } => PrimitiveShapeTag::Cone,
            PrimitiveShape::Torus { .. } => PrimitiveShapeTag::Torus,
        }),
        _ => None,
    }
}

pub fn render(node: Arc<Node>) -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.5rem")
        .child(section_header("Primitive"))
        .child(field_row("Shape", primitive_shape_select(node.clone())))
        // Variant-specific param rows. Dedupe on the shape tag so dragging
        // a dim value doesn't tear down the input mid-drag.
        .child_signal(node.kind.signal_ref(primitive_shape_tag).dedupe().map(clone!(node => move |variant| {
            variant.map(|tag| primitive_param_rows(node.clone(), tag))
        })))
        // Optional shared-material picker — `(inline material)` falls
        // back to the inline def; selecting a Material asset routes
        // through `renderer_bridge::material_cache::resolve` so multiple
        // nodes can share one `MaterialDef`.
        .child(super::field_row("Material asset", super::material_ref_select(
            node.clone(),
            |k| match k {
                NodeKind::Primitive { material, .. } => *material,
                _ => None,
            },
            |k, new_ref| {
                if let NodeKind::Primitive { material, .. } = k {
                    *material = new_ref;
                }
            },
        )))
        // Material editor — when a Material asset is picked, edits
        // route into the shared `MaterialDef`; otherwise into the
        // node's `inline_material`. Header label flips accordingly.
        .child(super::material::render_material_for_node(
            node.clone(),
            |k| match k {
                NodeKind::Primitive { material, .. } => *material,
                _ => None,
            },
            |k| match k {
                NodeKind::Primitive { inline_material, .. } => Some(inline_material),
                _ => None,
            },
            |k, new_def| {
                if let NodeKind::Primitive { inline_material, .. } = k {
                    *inline_material = new_def;
                }
            },
        ))
        // Cast / receive shadow toggles. Sit below material so the
        // shadow section reads like a "rendering" footer.
        .child(super::mesh_shadow::render(
            node.clone(),
            |k| match k {
                NodeKind::Primitive { shadow, .. } => Some(*shadow),
                _ => None,
            },
            |k, new_shadow| {
                if let NodeKind::Primitive { shadow, .. } = k {
                    *shadow = new_shadow;
                }
            },
        ))
        // F10: snapshot the current primitive geometry into a
        // shareable Mesh asset and re-point the node at it. The
        // material binding rides along onto NodeKind::Mesh so the
        // visual stays identical.
        .child(super::capture_as_mesh_button(node))
    })
}

fn primitive_param_rows(node: Arc<Node>, tag: PrimitiveShapeTag) -> Dom {
    let rows: Vec<Dom> = match tag {
        PrimitiveShapeTag::Plane => vec![
            field_row(
                "Width",
                primitive_f32_input(node.clone(), PrimField::PlaneWidth),
            ),
            field_row(
                "Depth",
                primitive_f32_input(node.clone(), PrimField::PlaneDepth),
            ),
            field_row(
                "Segs X",
                primitive_u32_input(node.clone(), PrimField::PlaneSegX),
            ),
            field_row("Segs Z", primitive_u32_input(node, PrimField::PlaneSegZ)),
        ],
        PrimitiveShapeTag::Box => vec![
            field_row("Width", primitive_f32_input(node.clone(), PrimField::BoxX)),
            field_row("Height", primitive_f32_input(node.clone(), PrimField::BoxY)),
            field_row("Depth", primitive_f32_input(node, PrimField::BoxZ)),
        ],
        PrimitiveShapeTag::Sphere => vec![
            field_row(
                "Radius",
                primitive_f32_input(node.clone(), PrimField::SphereRadius),
            ),
            field_row(
                "Segs long",
                primitive_u32_input(node.clone(), PrimField::SphereLong),
            ),
            field_row("Segs lat", primitive_u32_input(node, PrimField::SphereLat)),
        ],
        PrimitiveShapeTag::Cylinder => vec![
            field_row(
                "Radius",
                primitive_f32_input(node.clone(), PrimField::CylRadius),
            ),
            field_row(
                "Height",
                primitive_f32_input(node.clone(), PrimField::CylHeight),
            ),
            field_row("Segments", primitive_u32_input(node, PrimField::CylSeg)),
        ],
        PrimitiveShapeTag::Cone => vec![
            field_row(
                "Radius",
                primitive_f32_input(node.clone(), PrimField::ConeRadius),
            ),
            field_row(
                "Height",
                primitive_f32_input(node.clone(), PrimField::ConeHeight),
            ),
            field_row("Segments", primitive_u32_input(node, PrimField::ConeSeg)),
        ],
        PrimitiveShapeTag::Torus => vec![
            field_row(
                "Radius",
                primitive_f32_input(node.clone(), PrimField::TorusRadius),
            ),
            field_row(
                "Thickness",
                primitive_f32_input(node.clone(), PrimField::TorusThick),
            ),
            field_row(
                "Segs maj",
                primitive_u32_input(node.clone(), PrimField::TorusMajor),
            ),
            field_row("Segs min", primitive_u32_input(node, PrimField::TorusMinor)),
        ],
    };
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.4rem")
        .children(rows)
    })
}

#[derive(Clone, Copy)]
enum PrimField {
    PlaneWidth,
    PlaneDepth,
    PlaneSegX,
    PlaneSegZ,
    BoxX,
    BoxY,
    BoxZ,
    SphereRadius,
    SphereLong,
    SphereLat,
    CylRadius,
    CylHeight,
    CylSeg,
    ConeRadius,
    ConeHeight,
    ConeSeg,
    TorusRadius,
    TorusThick,
    TorusMajor,
    TorusMinor,
}

fn primitive_field_read(shape: &PrimitiveShape, field: PrimField) -> f32 {
    match (shape, field) {
        (PrimitiveShape::Plane { width, .. }, PrimField::PlaneWidth) => *width,
        (PrimitiveShape::Plane { depth, .. }, PrimField::PlaneDepth) => *depth,
        (PrimitiveShape::Plane { segments_x, .. }, PrimField::PlaneSegX) => *segments_x as f32,
        (PrimitiveShape::Plane { segments_z, .. }, PrimField::PlaneSegZ) => *segments_z as f32,
        (PrimitiveShape::Box { dims }, PrimField::BoxX) => dims[0],
        (PrimitiveShape::Box { dims }, PrimField::BoxY) => dims[1],
        (PrimitiveShape::Box { dims }, PrimField::BoxZ) => dims[2],
        (PrimitiveShape::Sphere { radius, .. }, PrimField::SphereRadius) => *radius,
        (PrimitiveShape::Sphere { segments_long, .. }, PrimField::SphereLong) => {
            *segments_long as f32
        }
        (PrimitiveShape::Sphere { segments_lat, .. }, PrimField::SphereLat) => *segments_lat as f32,
        (PrimitiveShape::Cylinder { radius, .. }, PrimField::CylRadius) => *radius,
        (PrimitiveShape::Cylinder { height, .. }, PrimField::CylHeight) => *height,
        (
            PrimitiveShape::Cylinder {
                radial_segments, ..
            },
            PrimField::CylSeg,
        ) => *radial_segments as f32,
        (PrimitiveShape::Cone { radius, .. }, PrimField::ConeRadius) => *radius,
        (PrimitiveShape::Cone { height, .. }, PrimField::ConeHeight) => *height,
        (
            PrimitiveShape::Cone {
                radial_segments, ..
            },
            PrimField::ConeSeg,
        ) => *radial_segments as f32,
        (PrimitiveShape::Torus { radius, .. }, PrimField::TorusRadius) => *radius,
        (PrimitiveShape::Torus { thickness, .. }, PrimField::TorusThick) => *thickness,
        (PrimitiveShape::Torus { segments_major, .. }, PrimField::TorusMajor) => {
            *segments_major as f32
        }
        (PrimitiveShape::Torus { segments_minor, .. }, PrimField::TorusMinor) => {
            *segments_minor as f32
        }
        _ => 0.0,
    }
}

fn primitive_field_write(shape: &mut PrimitiveShape, field: PrimField, value: f32) {
    match (shape, field) {
        (PrimitiveShape::Plane { width, .. }, PrimField::PlaneWidth) => *width = value.max(0.001),
        (PrimitiveShape::Plane { depth, .. }, PrimField::PlaneDepth) => *depth = value.max(0.001),
        (PrimitiveShape::Plane { segments_x, .. }, PrimField::PlaneSegX) => {
            *segments_x = (value.max(1.0) as u32).clamp(1, 256);
        }
        (PrimitiveShape::Plane { segments_z, .. }, PrimField::PlaneSegZ) => {
            *segments_z = (value.max(1.0) as u32).clamp(1, 256);
        }
        (PrimitiveShape::Box { dims }, PrimField::BoxX) => dims[0] = value.max(0.001),
        (PrimitiveShape::Box { dims }, PrimField::BoxY) => dims[1] = value.max(0.001),
        (PrimitiveShape::Box { dims }, PrimField::BoxZ) => dims[2] = value.max(0.001),
        (PrimitiveShape::Sphere { radius, .. }, PrimField::SphereRadius) => {
            *radius = value.max(0.001);
        }
        (PrimitiveShape::Sphere { segments_long, .. }, PrimField::SphereLong) => {
            *segments_long = (value.max(3.0) as u32).clamp(3, 128);
        }
        (PrimitiveShape::Sphere { segments_lat, .. }, PrimField::SphereLat) => {
            *segments_lat = (value.max(2.0) as u32).clamp(2, 128);
        }
        (PrimitiveShape::Cylinder { radius, .. }, PrimField::CylRadius) => {
            *radius = value.max(0.001);
        }
        (PrimitiveShape::Cylinder { height, .. }, PrimField::CylHeight) => {
            *height = value.max(0.001);
        }
        (
            PrimitiveShape::Cylinder {
                radial_segments, ..
            },
            PrimField::CylSeg,
        ) => {
            *radial_segments = (value.max(3.0) as u32).clamp(3, 256);
        }
        (PrimitiveShape::Cone { radius, .. }, PrimField::ConeRadius) => *radius = value.max(0.001),
        (PrimitiveShape::Cone { height, .. }, PrimField::ConeHeight) => *height = value.max(0.001),
        (
            PrimitiveShape::Cone {
                radial_segments, ..
            },
            PrimField::ConeSeg,
        ) => {
            *radial_segments = (value.max(3.0) as u32).clamp(3, 256);
        }
        (PrimitiveShape::Torus { radius, .. }, PrimField::TorusRadius) => {
            *radius = value.max(0.001);
        }
        (PrimitiveShape::Torus { thickness, .. }, PrimField::TorusThick) => {
            *thickness = value.max(0.001);
        }
        (PrimitiveShape::Torus { segments_major, .. }, PrimField::TorusMajor) => {
            *segments_major = (value.max(3.0) as u32).clamp(3, 256);
        }
        (PrimitiveShape::Torus { segments_minor, .. }, PrimField::TorusMinor) => {
            *segments_minor = (value.max(3.0) as u32).clamp(3, 256);
        }
        _ => {}
    }
}

fn primitive_f32_input(node: Arc<Node>, field: PrimField) -> Dom {
    let kind = node.kind.clone();
    let value_signal = kind.signal_cloned().map(move |k| match &k {
        NodeKind::Primitive { shape, .. } => primitive_field_read(shape, field),
        _ => 0.0,
    });
    number_input(value_signal, move |new_value| {
        let mut k = kind.get_cloned();
        if let NodeKind::Primitive { shape, .. } = &mut k {
            primitive_field_write(shape, field, new_value);
            kind.set(k);
        }
    })
}

fn primitive_u32_input(node: Arc<Node>, field: PrimField) -> Dom {
    // Same as f32 input; field writers clamp to integer ranges internally.
    primitive_f32_input(node, field)
}

const PRIM_VALUE_PLANE: &str = "plane";
const PRIM_VALUE_BOX: &str = "box";
const PRIM_VALUE_SPHERE: &str = "sphere";
const PRIM_VALUE_CYLINDER: &str = "cylinder";
const PRIM_VALUE_CONE: &str = "cone";
const PRIM_VALUE_TORUS: &str = "torus";

/// Render the variant select + per-variant param rows for a
/// `PrimitiveShape` held outside a `NodeKind` — e.g. on
/// `MeshDef.source`. Same UI shape as the Primitive node inspector;
/// the only difference is read/write happen via the supplied closures
/// instead of `Node::kind`.
pub(crate) fn render_shape_editor(
    read: impl Fn() -> PrimitiveShape + Clone + 'static,
    write: impl Fn(PrimitiveShape) + Clone + 'static,
    revision: Mutable<u64>,
) -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.5rem")
        .child(field_row("Shape", shape_select_for_closure(read.clone(), write.clone(), revision.clone())))
        // Dedupe on the variant tag so dragging a dim doesn't tear the
        // input down mid-drag.
        .child_signal(revision.signal_cloned().map(clone!(read, write => move |_rev| {
            let shape = read();
            let tag = match shape {
                PrimitiveShape::Plane { .. } => PrimitiveShapeTag::Plane,
                PrimitiveShape::Box { .. } => PrimitiveShapeTag::Box,
                PrimitiveShape::Sphere { .. } => PrimitiveShapeTag::Sphere,
                PrimitiveShape::Cylinder { .. } => PrimitiveShapeTag::Cylinder,
                PrimitiveShape::Cone { .. } => PrimitiveShapeTag::Cone,
                PrimitiveShape::Torus { .. } => PrimitiveShapeTag::Torus,
            };
            Some(param_rows_for_closure(read.clone(), write.clone(), revision.clone(), tag))
        })))
    })
}

fn shape_select_for_closure(
    read: impl Fn() -> PrimitiveShape + Clone + 'static,
    write: impl Fn(PrimitiveShape) + 'static,
    revision: Mutable<u64>,
) -> Dom {
    html!("select" => web_sys::HtmlSelectElement, {
        .style("padding", "0.35rem 0.5rem")
        .style("background-color", ColorRaw::Darkest.value())
        .style("color", ColorText::SidebarHeader.value())
        .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .style("border-radius", "0.3rem")
        .style("font-size", "0.85rem")
        .style("cursor", "pointer")
        .child(html!("option", { .attr("value", PRIM_VALUE_PLANE).text("Plane") }))
        .child(html!("option", { .attr("value", PRIM_VALUE_BOX).text("Box") }))
        .child(html!("option", { .attr("value", PRIM_VALUE_SPHERE).text("Sphere") }))
        .child(html!("option", { .attr("value", PRIM_VALUE_CYLINDER).text("Cylinder") }))
        .child(html!("option", { .attr("value", PRIM_VALUE_CONE).text("Cone") }))
        .child(html!("option", { .attr("value", PRIM_VALUE_TORUS).text("Torus") }))
        .with_node!(select => {
            .future(clone!(read, select => {
                revision.signal().for_each(move |_| {
                    let want = match read() {
                        PrimitiveShape::Plane { .. } => PRIM_VALUE_PLANE,
                        PrimitiveShape::Box { .. } => PRIM_VALUE_BOX,
                        PrimitiveShape::Sphere { .. } => PRIM_VALUE_SPHERE,
                        PrimitiveShape::Cylinder { .. } => PRIM_VALUE_CYLINDER,
                        PrimitiveShape::Cone { .. } => PRIM_VALUE_CONE,
                        PrimitiveShape::Torus { .. } => PRIM_VALUE_TORUS,
                    };
                    if select.value() != want {
                        select.set_value(want);
                    }
                    async {}
                })
            }))
            .event(clone!(select => move |_: events::Change| {
                let new_shape = match select.value().as_str() {
                    PRIM_VALUE_PLANE => PrimitiveShape::default_plane(),
                    PRIM_VALUE_SPHERE => PrimitiveShape::default_sphere(),
                    PRIM_VALUE_CYLINDER => PrimitiveShape::default_cylinder(),
                    PRIM_VALUE_CONE => PrimitiveShape::default_cone(),
                    PRIM_VALUE_TORUS => PrimitiveShape::default_torus(),
                    _ => PrimitiveShape::default_box(),
                };
                let current = read();
                if std::mem::discriminant(&current) != std::mem::discriminant(&new_shape) {
                    write(new_shape);
                }
            }))
        })
    })
}

fn param_rows_for_closure(
    read: impl Fn() -> PrimitiveShape + Clone + 'static,
    write: impl Fn(PrimitiveShape) + Clone + 'static,
    revision: Mutable<u64>,
    tag: PrimitiveShapeTag,
) -> Dom {
    let rows: Vec<Dom> = match tag {
        PrimitiveShapeTag::Plane => vec![
            field_row(
                "Width",
                scalar_for_closure(
                    read.clone(),
                    write.clone(),
                    revision.clone(),
                    PrimField::PlaneWidth,
                ),
            ),
            field_row(
                "Depth",
                scalar_for_closure(
                    read.clone(),
                    write.clone(),
                    revision.clone(),
                    PrimField::PlaneDepth,
                ),
            ),
            field_row(
                "Segs X",
                scalar_for_closure(
                    read.clone(),
                    write.clone(),
                    revision.clone(),
                    PrimField::PlaneSegX,
                ),
            ),
            field_row(
                "Segs Z",
                scalar_for_closure(read, write, revision, PrimField::PlaneSegZ),
            ),
        ],
        PrimitiveShapeTag::Box => vec![
            field_row(
                "Width",
                scalar_for_closure(
                    read.clone(),
                    write.clone(),
                    revision.clone(),
                    PrimField::BoxX,
                ),
            ),
            field_row(
                "Height",
                scalar_for_closure(
                    read.clone(),
                    write.clone(),
                    revision.clone(),
                    PrimField::BoxY,
                ),
            ),
            field_row(
                "Depth",
                scalar_for_closure(read, write, revision, PrimField::BoxZ),
            ),
        ],
        PrimitiveShapeTag::Sphere => vec![
            field_row(
                "Radius",
                scalar_for_closure(
                    read.clone(),
                    write.clone(),
                    revision.clone(),
                    PrimField::SphereRadius,
                ),
            ),
            field_row(
                "Segs long",
                scalar_for_closure(
                    read.clone(),
                    write.clone(),
                    revision.clone(),
                    PrimField::SphereLong,
                ),
            ),
            field_row(
                "Segs lat",
                scalar_for_closure(read, write, revision, PrimField::SphereLat),
            ),
        ],
        PrimitiveShapeTag::Cylinder => vec![
            field_row(
                "Radius",
                scalar_for_closure(
                    read.clone(),
                    write.clone(),
                    revision.clone(),
                    PrimField::CylRadius,
                ),
            ),
            field_row(
                "Height",
                scalar_for_closure(
                    read.clone(),
                    write.clone(),
                    revision.clone(),
                    PrimField::CylHeight,
                ),
            ),
            field_row(
                "Segments",
                scalar_for_closure(read, write, revision, PrimField::CylSeg),
            ),
        ],
        PrimitiveShapeTag::Cone => vec![
            field_row(
                "Radius",
                scalar_for_closure(
                    read.clone(),
                    write.clone(),
                    revision.clone(),
                    PrimField::ConeRadius,
                ),
            ),
            field_row(
                "Height",
                scalar_for_closure(
                    read.clone(),
                    write.clone(),
                    revision.clone(),
                    PrimField::ConeHeight,
                ),
            ),
            field_row(
                "Segments",
                scalar_for_closure(read, write, revision, PrimField::ConeSeg),
            ),
        ],
        PrimitiveShapeTag::Torus => vec![
            field_row(
                "Radius",
                scalar_for_closure(
                    read.clone(),
                    write.clone(),
                    revision.clone(),
                    PrimField::TorusRadius,
                ),
            ),
            field_row(
                "Thickness",
                scalar_for_closure(
                    read.clone(),
                    write.clone(),
                    revision.clone(),
                    PrimField::TorusThick,
                ),
            ),
            field_row(
                "Segs maj",
                scalar_for_closure(
                    read.clone(),
                    write.clone(),
                    revision.clone(),
                    PrimField::TorusMajor,
                ),
            ),
            field_row(
                "Segs min",
                scalar_for_closure(read, write, revision, PrimField::TorusMinor),
            ),
        ],
    };
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.4rem")
        .children(rows)
    })
}

fn scalar_for_closure(
    read: impl Fn() -> PrimitiveShape + Clone + 'static,
    write: impl Fn(PrimitiveShape) + Clone + 'static,
    revision: Mutable<u64>,
    field: PrimField,
) -> Dom {
    use futures_signals::signal::SignalExt;
    let read_for_signal = read.clone();
    let value_signal = revision
        .signal()
        .map(move |_| primitive_field_read(&read_for_signal(), field));
    number_input(value_signal, move |new_value| {
        let mut shape = read();
        primitive_field_write(&mut shape, field, new_value);
        write(shape);
    })
}

fn primitive_shape_select(node: Arc<Node>) -> Dom {
    let kind = node.kind.clone();
    html!("select" => web_sys::HtmlSelectElement, {
        .style("padding", "0.35rem 0.5rem")
        .style("background-color", ColorRaw::Darkest.value())
        .style("color", ColorText::SidebarHeader.value())
        .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .style("border-radius", "0.3rem")
        .style("font-size", "0.85rem")
        .style("cursor", "pointer")
        .child(html!("option", { .attr("value", PRIM_VALUE_PLANE).text("Plane") }))
        .child(html!("option", { .attr("value", PRIM_VALUE_BOX).text("Box") }))
        .child(html!("option", { .attr("value", PRIM_VALUE_SPHERE).text("Sphere") }))
        .child(html!("option", { .attr("value", PRIM_VALUE_CYLINDER).text("Cylinder") }))
        .child(html!("option", { .attr("value", PRIM_VALUE_CONE).text("Cone") }))
        .child(html!("option", { .attr("value", PRIM_VALUE_TORUS).text("Torus") }))
        .with_node!(select => {
            .future(clone!(kind, select => {
                kind.signal_cloned().for_each(move |k| {
                    if let NodeKind::Primitive { shape, .. } = k {
                        let want = match shape {
                            PrimitiveShape::Plane { .. } => PRIM_VALUE_PLANE,
                            PrimitiveShape::Box { .. } => PRIM_VALUE_BOX,
                            PrimitiveShape::Sphere { .. } => PRIM_VALUE_SPHERE,
                            PrimitiveShape::Cylinder { .. } => PRIM_VALUE_CYLINDER,
                            PrimitiveShape::Cone { .. } => PRIM_VALUE_CONE,
                            PrimitiveShape::Torus { .. } => PRIM_VALUE_TORUS,
                        };
                        if select.value() != want {
                            select.set_value(want);
                        }
                    }
                    async {}
                })
            }))
            .event(clone!(kind, select => move |_: events::Change| {
                let mut k = kind.get_cloned();
                if let NodeKind::Primitive { shape, .. } = &mut k {
                    let new_shape = match select.value().as_str() {
                        PRIM_VALUE_PLANE => PrimitiveShape::default_plane(),
                        PRIM_VALUE_SPHERE => PrimitiveShape::default_sphere(),
                        PRIM_VALUE_CYLINDER => PrimitiveShape::default_cylinder(),
                        PRIM_VALUE_CONE => PrimitiveShape::default_cone(),
                        PRIM_VALUE_TORUS => PrimitiveShape::default_torus(),
                        _ => PrimitiveShape::default_box(),
                    };
                    // Preserve current variant if already matching (same
                    // dedupe pattern as the camera projection select).
                    let same_variant = std::mem::discriminant(shape) == std::mem::discriminant(&new_shape);
                    if !same_variant {
                        *shape = new_shape;
                        kind.set(k);
                    }
                }
            }))
        })
    })
}
