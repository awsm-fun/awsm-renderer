// ─────────────────────────────────────────────────────────────────────
// Particle emitter editor.
//
// Top-of-form: "Preview" Play/Stop button (toggles the per-NodeId
// `playing` Mutable that `renderer_bridge::particles_sync` observes —
// see `D-1a` in the progress doc), then every authored param: spawn
// rate / burst / max-alive / one-shot / blend / space / spawn shape /
// speed range / lifetime range / size range / color-over-life /
// size-over-life / alpha-over-life / forces vec / texture picker.
// Each `particle_*` helper renders one row.
// ─────────────────────────────────────────────────────────────────────

use crate::prelude::*;
use crate::properties::transform::number_input;
use crate::scene::{Node, NodeKind};
use awsm_scene_schema::EmitterSpaceDef;

use super::{field_row, section_header, texture_ref_select};

const SPACE_VALUE_WORLD: &str = "world";
const SPACE_VALUE_LOCAL: &str = "local";

pub fn render(node: Arc<Node>) -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.5rem")
        .child(section_header("Particle Emitter"))
        .child(field_row("Preview", particle_play_button(node.clone())))
        .child(field_row("Spawn rate", particle_f32_input(node.clone(), ParticleField::SpawnRate)))
        .child(field_row("Burst", particle_u32_input(node.clone(), ParticleField::Burst)))
        .child(field_row("Max alive", particle_u32_input(node.clone(), ParticleField::MaxAlive)))
        .child(field_row("One shot", particle_bool_input(node.clone(), ParticleBool::OneShot)))
        .child(field_row("Blend", particle_bool_input(node.clone(), ParticleBool::Blend)))
        .child(field_row("Space", particle_space_select(node.clone())))
        .child(particle_spawn_shape_section(node.clone()))
        .child(field_row("Speed min", particle_f32_input(node.clone(), ParticleField::SpeedMin)))
        .child(field_row("Speed max", particle_f32_input(node.clone(), ParticleField::SpeedMax)))
        .child(field_row("Lifetime min", particle_f32_input(node.clone(), ParticleField::LifeMin)))
        .child(field_row("Lifetime max", particle_f32_input(node.clone(), ParticleField::LifeMax)))
        .child(field_row("Size min", particle_f32_input(node.clone(), ParticleField::SizeMin)))
        .child(field_row("Size max", particle_f32_input(node.clone(), ParticleField::SizeMax)))
        .child(particle_color_over_life_section(node.clone()))
        .child(particle_size_over_life_section(node.clone()))
        .child(particle_alpha_over_life_section(node.clone()))
        .child(particle_forces_section(node.clone()))
        .child(field_row("Texture", texture_ref_select(
            node,
            |k| match k {
                NodeKind::ParticleEmitter(p) => p.texture,
                _ => None,
            },
            |k, new_ref| {
                if let NodeKind::ParticleEmitter(p) = k {
                    p.texture = new_ref;
                }
            },
        )))
    })
}

/// Forces vec editor — renders one row per registered `ForceDef` with a
/// delete button, plus two "+ Add" buttons at the bottom. Re-renders the
/// whole sub-section whenever the *count* changes (so add/remove triggers
/// a redraw); individual numeric inputs read/write via signals so dragging
/// a value doesn't re-build the input element mid-drag.
fn particle_forces_section(node: Arc<Node>) -> Dom {
    use awsm_scene_schema::ForceDef;

    let kind = node.kind.clone();
    // Trigger on (count, variant tags) so adding / removing / changing a
    // variant rebuilds, but tweaking values inside an existing variant
    // doesn't.
    let layout_signal = kind.signal_ref(|k| match k {
        NodeKind::ParticleEmitter(p) => p
            .forces
            .iter()
            .map(|f| match f {
                ForceDef::Gravity { .. } => 0u8,
                ForceDef::LinearDrag { .. } => 1u8,
            })
            .collect::<Vec<u8>>(),
        _ => Vec::new(),
    });

    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.35rem")
        .child(section_header("Forces"))
        .child_signal(layout_signal.dedupe_cloned().map(clone!(kind => move |tags| {
            let mut rows: Vec<Dom> = Vec::with_capacity(tags.len());
            for (idx, tag) in tags.iter().enumerate() {
                rows.push(match tag {
                    0 => particle_force_gravity_row(kind.clone(), idx),
                    _ => particle_force_drag_row(kind.clone(), idx),
                });
            }
            Some(html!("div", {
                .style("display", "flex")
                .style("flex-direction", "column")
                .style("gap", "0.35rem")
                .children(rows)
                .child(html!("div", {
                    .style("display", "flex")
                    .style("gap", "0.35rem")
                    .style("margin-top", "0.25rem")
                    .child(particle_force_add_button(kind.clone(), "+ Gravity", || {
                        ForceDef::Gravity { acceleration: [0.0, -9.8, 0.0] }
                    }))
                    .child(particle_force_add_button(kind.clone(), "+ Linear Drag", || {
                        ForceDef::LinearDrag { coefficient_x1000: 100 }
                    }))
                }))
            }))
        })))
    })
}

/// Toggles the per-emitter preview "playing" Mutable. Flipping it
/// false→true materializes the emitter runtime (mesh + simulator) via
/// `renderer_bridge::particles_sync`; true→false tears it down.
fn particle_play_button(node: Arc<Node>) -> Dom {
    let playing = crate::renderer_bridge::particles_sync::playing_state(node.id);
    let label_signal = playing
        .signal()
        .map(|p| if p { "■ Stop" } else { "▶ Play" });
    html!("button", {
        .style("padding", "0.35rem 0.6rem")
        .style("background-color", ColorRaw::Darkest.value())
        .style("color", ColorText::SidebarHeader.value())
        .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .style("border-radius", "0.3rem")
        .style("font-size", "0.8rem")
        .style("cursor", "pointer")
        .style("min-width", "5rem")
        .text_signal(label_signal)
        .event(clone!(playing => move |_: events::Click| {
            let cur = playing.get();
            playing.set(!cur);
        }))
    })
}

/// Renders an `+ X` button that, on click, pushes the result of `make()`
/// onto the host emitter's `forces` vec.
fn particle_force_add_button(
    kind: Mutable<NodeKind>,
    label: &'static str,
    make: fn() -> awsm_scene_schema::ForceDef,
) -> Dom {
    html!("button", {
        .style("padding", "0.35rem 0.6rem")
        .style("background-color", ColorRaw::Darkest.value())
        .style("color", ColorText::SidebarHeader.value())
        .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .style("border-radius", "0.3rem")
        .style("font-size", "0.8rem")
        .style("cursor", "pointer")
        .text(label)
        .event(clone!(kind => move |_: events::Click| {
            let mut k = kind.get_cloned();
            if let NodeKind::ParticleEmitter(ref mut p) = k {
                p.forces.push(make());
                kind.set(k);
            }
        }))
    })
}

/// Renders an `×` delete button next to a row. Removes `forces[idx]`.
fn particle_force_delete_button(kind: Mutable<NodeKind>, idx: usize) -> Dom {
    html!("button", {
        .style("padding", "0.15rem 0.5rem")
        .style("background-color", ColorRaw::Darkest.value())
        .style("color", ColorText::SidebarHeader.value())
        .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .style("border-radius", "0.3rem")
        .style("font-size", "0.8rem")
        .style("cursor", "pointer")
        .text("×")
        .event(clone!(kind => move |_: events::Click| {
            let mut k = kind.get_cloned();
            if let NodeKind::ParticleEmitter(ref mut p) = k {
                if idx < p.forces.len() {
                    p.forces.remove(idx);
                    kind.set(k);
                }
            }
        }))
    })
}

/// One row for a `ForceDef::Gravity { acceleration }` entry.
fn particle_force_gravity_row(kind: Mutable<NodeKind>, idx: usize) -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.25rem")
        .style("padding", "0.4rem")
        .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .style("border-radius", "0.3rem")
        .child(html!("div", {
            .style("display", "flex")
            .style("justify-content", "space-between")
            .style("align-items", "center")
            .child(html!("span", {
                .style("font-size", "0.8rem")
                .style("color", ColorText::SidebarHeader.value())
                .text("Gravity")
            }))
            .child(particle_force_delete_button(kind.clone(), idx))
        }))
        .child(field_row("Accel X", particle_force_gravity_axis(kind.clone(), idx, 0)))
        .child(field_row("Accel Y", particle_force_gravity_axis(kind.clone(), idx, 1)))
        .child(field_row("Accel Z", particle_force_gravity_axis(kind, idx, 2)))
    })
}

/// One row for a `ForceDef::LinearDrag { coefficient_x1000 }` entry.
fn particle_force_drag_row(kind: Mutable<NodeKind>, idx: usize) -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.25rem")
        .style("padding", "0.4rem")
        .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .style("border-radius", "0.3rem")
        .child(html!("div", {
            .style("display", "flex")
            .style("justify-content", "space-between")
            .style("align-items", "center")
            .child(html!("span", {
                .style("font-size", "0.8rem")
                .style("color", ColorText::SidebarHeader.value())
                .text("Linear Drag")
            }))
            .child(particle_force_delete_button(kind.clone(), idx))
        }))
        .child(field_row("Coefficient (×1000)", particle_force_drag_coef(kind, idx)))
    })
}

fn particle_force_gravity_axis(kind: Mutable<NodeKind>, idx: usize, axis: usize) -> Dom {
    use awsm_scene_schema::ForceDef;
    let kind_for_signal = kind.clone();
    let value_signal = kind_for_signal.signal_cloned().map(move |k| match k {
        NodeKind::ParticleEmitter(p) => match p.forces.get(idx) {
            Some(ForceDef::Gravity { acceleration }) => acceleration[axis],
            _ => 0.0,
        },
        _ => 0.0,
    });
    number_input(value_signal, move |v| {
        let mut k = kind.get_cloned();
        if let NodeKind::ParticleEmitter(ref mut p) = k {
            if let Some(ForceDef::Gravity {
                ref mut acceleration,
            }) = p.forces.get_mut(idx)
            {
                acceleration[axis] = v;
                kind.set(k);
            }
        }
    })
}

fn particle_force_drag_coef(kind: Mutable<NodeKind>, idx: usize) -> Dom {
    use awsm_scene_schema::ForceDef;
    let kind_for_signal = kind.clone();
    let value_signal = kind_for_signal.signal_cloned().map(move |k| match k {
        NodeKind::ParticleEmitter(p) => match p.forces.get(idx) {
            Some(ForceDef::LinearDrag { coefficient_x1000 }) => *coefficient_x1000 as f32,
            _ => 0.0,
        },
        _ => 0.0,
    });
    number_input(value_signal, move |v| {
        let mut k = kind.get_cloned();
        if let NodeKind::ParticleEmitter(ref mut p) = k {
            if let Some(ForceDef::LinearDrag {
                ref mut coefficient_x1000,
            }) = p.forces.get_mut(idx)
            {
                *coefficient_x1000 = v.max(0.0) as u32;
                kind.set(k);
            }
        }
    })
}

#[derive(Clone, Copy)]
enum ParticleField {
    SpawnRate,
    Burst,
    MaxAlive,
    SpeedMin,
    SpeedMax,
    LifeMin,
    LifeMax,
    SizeMin,
    SizeMax,
}

#[derive(Clone, Copy)]
enum ParticleBool {
    OneShot,
    Blend,
}

fn particle_f32_input(node: Arc<Node>, field: ParticleField) -> Dom {
    let kind = node.kind.clone();
    let value_signal = kind.signal_cloned().map(move |k| match &k {
        NodeKind::ParticleEmitter(p) => match field {
            ParticleField::SpawnRate => p.spawn_rate,
            ParticleField::Burst => p.burst_count as f32,
            ParticleField::MaxAlive => p.max_alive as f32,
            ParticleField::SpeedMin => p.initial_speed[0],
            ParticleField::SpeedMax => p.initial_speed[1],
            ParticleField::LifeMin => p.lifetime[0],
            ParticleField::LifeMax => p.lifetime[1],
            ParticleField::SizeMin => p.size[0],
            ParticleField::SizeMax => p.size[1],
        },
        _ => 0.0,
    });
    number_input(value_signal, move |new_value| {
        let mut k = kind.get_cloned();
        if let NodeKind::ParticleEmitter(ref mut p) = k {
            match field {
                ParticleField::SpawnRate => p.spawn_rate = new_value.max(0.0),
                ParticleField::Burst => {
                    p.burst_count = (new_value.max(0.0) as u32).clamp(0, 100_000);
                }
                ParticleField::MaxAlive => {
                    p.max_alive = (new_value.max(1.0) as u32).clamp(1, 10_000);
                }
                ParticleField::SpeedMin => p.initial_speed[0] = new_value.max(0.0),
                ParticleField::SpeedMax => p.initial_speed[1] = new_value.max(p.initial_speed[0]),
                ParticleField::LifeMin => p.lifetime[0] = new_value.max(0.01),
                ParticleField::LifeMax => p.lifetime[1] = new_value.max(p.lifetime[0]),
                ParticleField::SizeMin => p.size[0] = new_value.max(0.001),
                ParticleField::SizeMax => p.size[1] = new_value.max(p.size[0]),
            }
            kind.set(k);
        }
    })
}

fn particle_u32_input(node: Arc<Node>, field: ParticleField) -> Dom {
    particle_f32_input(node, field)
}

fn particle_bool_input(node: Arc<Node>, field: ParticleBool) -> Dom {
    let kind = node.kind.clone();
    html!("input" => web_sys::HtmlInputElement, {
        .attr("type", "checkbox")
        .style("cursor", "pointer")
        .with_node!(input => {
            .future(clone!(kind, input => {
                kind.signal_cloned().for_each(move |k| {
                    if let NodeKind::ParticleEmitter(p) = k {
                        let want = match field {
                            ParticleBool::OneShot => p.one_shot,
                            ParticleBool::Blend => p.blend,
                        };
                        if input.checked() != want {
                            input.set_checked(want);
                        }
                    }
                    async {}
                })
            }))
            .event(clone!(kind, input => move |_: events::Change| {
                let mut k = kind.get_cloned();
                if let NodeKind::ParticleEmitter(ref mut p) = k {
                    match field {
                        ParticleBool::OneShot => p.one_shot = input.checked(),
                        ParticleBool::Blend => p.blend = input.checked(),
                    }
                    kind.set(k);
                }
            }))
        })
    })
}

fn particle_space_select(node: Arc<Node>) -> Dom {
    let kind = node.kind.clone();
    html!("select" => web_sys::HtmlSelectElement, {
        .style("padding", "0.35rem 0.5rem")
        .style("background-color", ColorRaw::Darkest.value())
        .style("color", ColorText::SidebarHeader.value())
        .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .style("border-radius", "0.3rem")
        .style("font-size", "0.85rem")
        .style("cursor", "pointer")
        .child(html!("option", { .attr("value", SPACE_VALUE_WORLD).text("World") }))
        .child(html!("option", { .attr("value", SPACE_VALUE_LOCAL).text("Local") }))
        .with_node!(select => {
            .future(clone!(kind, select => {
                kind.signal_cloned().for_each(move |k| {
                    if let NodeKind::ParticleEmitter(p) = k {
                        let want = match p.space {
                            EmitterSpaceDef::World => SPACE_VALUE_WORLD,
                            EmitterSpaceDef::Local => SPACE_VALUE_LOCAL,
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
                if let NodeKind::ParticleEmitter(ref mut p) = k {
                    p.space = match select.value().as_str() {
                        SPACE_VALUE_WORLD => EmitterSpaceDef::World,
                        _ => EmitterSpaceDef::Local,
                    };
                    kind.set(k);
                }
            }))
        })
    })
}

// ─────────────────────────────────────────────────────────────────────
// Particle spawn shape variant picker
// ─────────────────────────────────────────────────────────────────────

const SHAPE_VALUE_POINT: &str = "point";
const SHAPE_VALUE_SPHERE: &str = "sphere";
const SHAPE_VALUE_CONE: &str = "cone";

/// Render the spawn-shape section (Point / Sphere{radius} / Cone{angle, dir}).
fn particle_spawn_shape_section(node: Arc<Node>) -> Dom {
    use awsm_scene_schema::SpawnShapeDef;
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.4rem")
        .child(field_row("Shape", particle_spawn_shape_select(node.clone())))
        .child_signal(node.kind.signal_ref(|k| match k {
            NodeKind::ParticleEmitter(p) => match p.shape {
                SpawnShapeDef::Point => Some(0u8),
                SpawnShapeDef::Sphere { .. } => Some(1u8),
                SpawnShapeDef::Cone { .. } => Some(2u8),
            },
            _ => None,
        }).dedupe().map(clone!(node => move |variant| {
            match variant {
                Some(1) => Some(field_row(
                    "Radius",
                    particle_sphere_radius_input(node.clone()),
                )),
                Some(2) => Some(html!("div", {
                    .style("display", "flex")
                    .style("flex-direction", "column")
                    .style("gap", "0.4rem")
                    .child(field_row("Angle (rad)", particle_cone_angle_input(node.clone())))
                    .child(field_row("Dir X", particle_cone_dir_input(node.clone(), 0)))
                    .child(field_row("Dir Y", particle_cone_dir_input(node.clone(), 1)))
                    .child(field_row("Dir Z", particle_cone_dir_input(node.clone(), 2)))
                })),
                _ => None,
            }
        })))
    })
}

fn particle_spawn_shape_select(node: Arc<Node>) -> Dom {
    use awsm_scene_schema::SpawnShapeDef;
    let kind = node.kind.clone();
    html!("select" => web_sys::HtmlSelectElement, {
        .style("padding", "0.35rem 0.5rem")
        .style("background-color", ColorRaw::Darkest.value())
        .style("color", ColorText::SidebarHeader.value())
        .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .style("border-radius", "0.3rem")
        .style("font-size", "0.85rem")
        .style("cursor", "pointer")
        .child(html!("option", { .attr("value", SHAPE_VALUE_POINT).text("Point") }))
        .child(html!("option", { .attr("value", SHAPE_VALUE_SPHERE).text("Sphere") }))
        .child(html!("option", { .attr("value", SHAPE_VALUE_CONE).text("Cone") }))
        .with_node!(select => {
            .future(clone!(kind, select => {
                kind.signal_cloned().for_each(move |k| {
                    if let NodeKind::ParticleEmitter(p) = k {
                        let want = match p.shape {
                            SpawnShapeDef::Point => SHAPE_VALUE_POINT,
                            SpawnShapeDef::Sphere { .. } => SHAPE_VALUE_SPHERE,
                            SpawnShapeDef::Cone { .. } => SHAPE_VALUE_CONE,
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
                if let NodeKind::ParticleEmitter(ref mut p) = k {
                    let new_shape = match select.value().as_str() {
                        SHAPE_VALUE_POINT => SpawnShapeDef::Point,
                        SHAPE_VALUE_SPHERE => SpawnShapeDef::Sphere { radius: 0.5 },
                        _ => SpawnShapeDef::default_cone(),
                    };
                    let same_variant =
                        std::mem::discriminant(&p.shape) == std::mem::discriminant(&new_shape);
                    if !same_variant {
                        p.shape = new_shape;
                        kind.set(k);
                    }
                }
            }))
        })
    })
}

fn particle_sphere_radius_input(node: Arc<Node>) -> Dom {
    use awsm_scene_schema::SpawnShapeDef;
    let kind = node.kind.clone();
    let value_signal = kind.signal_cloned().map(|k| match k {
        NodeKind::ParticleEmitter(p) => match p.shape {
            SpawnShapeDef::Sphere { radius } => radius,
            _ => 0.5,
        },
        _ => 0.5,
    });
    number_input(value_signal, move |new_value| {
        let mut k = kind.get_cloned();
        if let NodeKind::ParticleEmitter(ref mut p) = k {
            if let SpawnShapeDef::Sphere { radius } = &mut p.shape {
                *radius = new_value.max(0.001);
                kind.set(k);
            }
        }
    })
}

fn particle_cone_angle_input(node: Arc<Node>) -> Dom {
    use awsm_scene_schema::SpawnShapeDef;
    let kind = node.kind.clone();
    let value_signal = kind.signal_cloned().map(|k| match k {
        NodeKind::ParticleEmitter(p) => match p.shape {
            SpawnShapeDef::Cone { angle_radians, .. } => angle_radians,
            _ => 0.4,
        },
        _ => 0.4,
    });
    number_input(value_signal, move |new_value| {
        let mut k = kind.get_cloned();
        if let NodeKind::ParticleEmitter(ref mut p) = k {
            if let SpawnShapeDef::Cone { angle_radians, .. } = &mut p.shape {
                *angle_radians = new_value.clamp(0.0, std::f32::consts::PI);
                kind.set(k);
            }
        }
    })
}

fn particle_cone_dir_input(node: Arc<Node>, axis: usize) -> Dom {
    use awsm_scene_schema::SpawnShapeDef;
    let kind = node.kind.clone();
    let value_signal = kind.signal_cloned().map(move |k| match k {
        NodeKind::ParticleEmitter(p) => match p.shape {
            SpawnShapeDef::Cone { direction, .. } => direction[axis],
            _ => 0.0,
        },
        _ => 0.0,
    });
    number_input(value_signal, move |new_value| {
        let mut k = kind.get_cloned();
        if let NodeKind::ParticleEmitter(ref mut p) = k {
            if let SpawnShapeDef::Cone { direction, .. } = &mut p.shape {
                direction[axis] = new_value;
                kind.set(k);
            }
        }
    })
}

// ─────────────────────────────────────────────────────────────────────
// Particle over-life variant pickers (color / size / alpha)
// ─────────────────────────────────────────────────────────────────────

const OVER_LIFE_CONST: &str = "const";
const OVER_LIFE_LINEAR: &str = "linear";
const OVER_LIFE_LINEAR_ONE_TO_ZERO: &str = "linear_one_to_zero";

fn particle_color_over_life_section(node: Arc<Node>) -> Dom {
    use awsm_scene_schema::ColorOverLifeDef;
    let kind = node.kind.clone();
    let select = html!("select" => web_sys::HtmlSelectElement, {
        .style("padding", "0.35rem 0.5rem")
        .style("background-color", ColorRaw::Darkest.value())
        .style("color", ColorText::SidebarHeader.value())
        .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .style("border-radius", "0.3rem")
        .style("font-size", "0.85rem")
        .style("cursor", "pointer")
        .child(html!("option", { .attr("value", OVER_LIFE_CONST).text("Const") }))
        .child(html!("option", { .attr("value", OVER_LIFE_LINEAR).text("Linear") }))
        .with_node!(select => {
            .future(clone!(kind, select => {
                kind.signal_cloned().for_each(move |k| {
                    if let NodeKind::ParticleEmitter(p) = k {
                        let want = match p.color_over_life {
                            ColorOverLifeDef::Const(_) => OVER_LIFE_CONST,
                            ColorOverLifeDef::Linear { .. } => OVER_LIFE_LINEAR,
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
                if let NodeKind::ParticleEmitter(ref mut p) = k {
                    let new_var = match select.value().as_str() {
                        OVER_LIFE_CONST => ColorOverLifeDef::Const([1.0, 1.0, 1.0, 1.0]),
                        _ => ColorOverLifeDef::Linear {
                            start: [1.0, 1.0, 1.0, 1.0],
                            end: [1.0, 1.0, 1.0, 0.0],
                        },
                    };
                    let same = std::mem::discriminant(&p.color_over_life)
                        == std::mem::discriminant(&new_var);
                    if !same {
                        p.color_over_life = new_var;
                        kind.set(k);
                    }
                }
            }))
        })
    });
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.4rem")
        .child(field_row("Color/life", select))
        .child(field_row("Color start A", color_over_life_rgba_input(node.clone(), false, 3)))
        .child(field_row("Color end A", color_over_life_rgba_input(node, true, 3)))
    })
}

fn color_over_life_rgba_input(node: Arc<Node>, is_end: bool, axis: usize) -> Dom {
    use awsm_scene_schema::ColorOverLifeDef;
    let kind = node.kind.clone();
    let value_signal = kind.signal_cloned().map(move |k| match k {
        NodeKind::ParticleEmitter(p) => match (&p.color_over_life, is_end) {
            (ColorOverLifeDef::Const(c), _) => c[axis],
            (ColorOverLifeDef::Linear { start, .. }, false) => start[axis],
            (ColorOverLifeDef::Linear { end, .. }, true) => end[axis],
        },
        _ => 1.0,
    });
    number_input(value_signal, move |new_value| {
        let v = new_value.clamp(0.0, 1.0);
        let mut k = kind.get_cloned();
        if let NodeKind::ParticleEmitter(ref mut p) = k {
            match (&mut p.color_over_life, is_end) {
                (ColorOverLifeDef::Const(c), _) => c[axis] = v,
                (ColorOverLifeDef::Linear { start, .. }, false) => start[axis] = v,
                (ColorOverLifeDef::Linear { end, .. }, true) => end[axis] = v,
            }
            kind.set(k);
        }
    })
}

fn particle_size_over_life_section(node: Arc<Node>) -> Dom {
    use awsm_scene_schema::SizeOverLifeDef;
    let kind = node.kind.clone();
    let select = html!("select" => web_sys::HtmlSelectElement, {
        .style("padding", "0.35rem 0.5rem")
        .style("background-color", ColorRaw::Darkest.value())
        .style("color", ColorText::SidebarHeader.value())
        .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .style("border-radius", "0.3rem")
        .style("font-size", "0.85rem")
        .style("cursor", "pointer")
        .child(html!("option", { .attr("value", OVER_LIFE_CONST).text("Const") }))
        .child(html!("option", { .attr("value", OVER_LIFE_LINEAR).text("Linear") }))
        .with_node!(select => {
            .future(clone!(kind, select => {
                kind.signal_cloned().for_each(move |k| {
                    if let NodeKind::ParticleEmitter(p) = k {
                        let want = match p.size_over_life {
                            SizeOverLifeDef::Const(_) => OVER_LIFE_CONST,
                            SizeOverLifeDef::Linear { .. } => OVER_LIFE_LINEAR,
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
                if let NodeKind::ParticleEmitter(ref mut p) = k {
                    let new_var = match select.value().as_str() {
                        OVER_LIFE_CONST => SizeOverLifeDef::Const(1.0),
                        _ => SizeOverLifeDef::Linear { start: 1.0, end: 0.5 },
                    };
                    let same = std::mem::discriminant(&p.size_over_life)
                        == std::mem::discriminant(&new_var);
                    if !same {
                        p.size_over_life = new_var;
                        kind.set(k);
                    }
                }
            }))
        })
    });
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.4rem")
        .child(field_row("Size/life", select))
        .child(field_row("Size start", size_over_life_input(node.clone(), false)))
        .child(field_row("Size end", size_over_life_input(node, true)))
    })
}

fn size_over_life_input(node: Arc<Node>, is_end: bool) -> Dom {
    use awsm_scene_schema::SizeOverLifeDef;
    let kind = node.kind.clone();
    let value_signal = kind.signal_cloned().map(move |k| match k {
        NodeKind::ParticleEmitter(p) => match (p.size_over_life, is_end) {
            (SizeOverLifeDef::Const(v), _) => v,
            (SizeOverLifeDef::Linear { start, .. }, false) => start,
            (SizeOverLifeDef::Linear { end, .. }, true) => end,
        },
        _ => 1.0,
    });
    number_input(value_signal, move |new_value| {
        let v = new_value.max(0.0);
        let mut k = kind.get_cloned();
        if let NodeKind::ParticleEmitter(ref mut p) = k {
            match (&mut p.size_over_life, is_end) {
                (SizeOverLifeDef::Const(c), _) => *c = v,
                (SizeOverLifeDef::Linear { start, .. }, false) => *start = v,
                (SizeOverLifeDef::Linear { end, .. }, true) => *end = v,
            }
            kind.set(k);
        }
    })
}

fn particle_alpha_over_life_section(node: Arc<Node>) -> Dom {
    use awsm_scene_schema::AlphaOverLifeDef;
    let kind = node.kind.clone();
    let select = html!("select" => web_sys::HtmlSelectElement, {
        .style("padding", "0.35rem 0.5rem")
        .style("background-color", ColorRaw::Darkest.value())
        .style("color", ColorText::SidebarHeader.value())
        .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .style("border-radius", "0.3rem")
        .style("font-size", "0.85rem")
        .style("cursor", "pointer")
        .child(html!("option", { .attr("value", OVER_LIFE_CONST).text("Const") }))
        .child(html!("option", { .attr("value", OVER_LIFE_LINEAR).text("Linear") }))
        .child(html!("option", { .attr("value", OVER_LIFE_LINEAR_ONE_TO_ZERO).text("Linear 1→0") }))
        .with_node!(select => {
            .future(clone!(kind, select => {
                kind.signal_cloned().for_each(move |k| {
                    if let NodeKind::ParticleEmitter(p) = k {
                        let want = match p.alpha_over_life {
                            AlphaOverLifeDef::Const(_) => OVER_LIFE_CONST,
                            AlphaOverLifeDef::Linear { .. } => OVER_LIFE_LINEAR,
                            AlphaOverLifeDef::LinearOneToZero => OVER_LIFE_LINEAR_ONE_TO_ZERO,
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
                if let NodeKind::ParticleEmitter(ref mut p) = k {
                    let new_var = match select.value().as_str() {
                        OVER_LIFE_CONST => AlphaOverLifeDef::Const(1.0),
                        OVER_LIFE_LINEAR_ONE_TO_ZERO => AlphaOverLifeDef::LinearOneToZero,
                        _ => AlphaOverLifeDef::Linear { start: 1.0, end: 0.0 },
                    };
                    let same = std::mem::discriminant(&p.alpha_over_life)
                        == std::mem::discriminant(&new_var);
                    if !same {
                        p.alpha_over_life = new_var;
                        kind.set(k);
                    }
                }
            }))
        })
    });
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.4rem")
        .child(field_row("Alpha/life", select))
    })
}
