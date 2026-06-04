// ─────────────────────────────────────────────────────────────────────
// Camera editor
// ─────────────────────────────────────────────────────────────────────

use crate::prelude::*;
use crate::properties::transform::number_input;
use crate::scene::{CameraConfig, CameraProjection, Node, NodeKind};
use crate::state::app_state;
use awsm_scene_schema::{CameraBehavior, NodeId};

use super::{collect_nodes_matching, field_row, section_header};

/// Variant tag for `CameraProjection`, mirroring the
/// `ColliderVariantTag` pattern. Used for the projection-input
/// `child_signal` dedupe so dragging FOV doesn't rebuild the select
/// element.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CameraProjectionTag {
    Perspective,
    Orthographic,
}

fn camera_projection_tag(k: &NodeKind) -> Option<CameraProjectionTag> {
    match k {
        NodeKind::Camera(cfg) => Some(match cfg.projection {
            CameraProjection::Perspective { .. } => CameraProjectionTag::Perspective,
            CameraProjection::Orthographic { .. } => CameraProjectionTag::Orthographic,
        }),
        _ => None,
    }
}

const CAMERA_VALUE_PERSP: &str = "perspective";
const CAMERA_VALUE_ORTHO: &str = "orthographic";

pub fn render(node: Arc<Node>) -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.5rem")
        .child(section_header("Camera"))
        .child(field_row("Projection", projection_select(node.clone())))
        // Variant-specific row (FOV vs half-height) — dedupe so flipping
        // the *value* of FOV doesn't tear down the number input.
        .child_signal(node.kind.signal_ref(camera_projection_tag).dedupe().map(clone!(node => move |variant| {
            match variant {
                Some(CameraProjectionTag::Perspective) => Some(
                    field_row("Vert. FOV (rad)", fov_y_input(node.clone()))
                ),
                Some(CameraProjectionTag::Orthographic) => Some(
                    field_row("Half-height", ortho_half_height_input(node.clone()))
                ),
                None => None,
            }
        })))
        .child(field_row("Near", camera_clip_input(node.clone(), CameraClip::Near)))
        .child(field_row("Far", camera_clip_input(node.clone(), CameraClip::Far)))
        .child(camera_behavior_section(node))
    })
}

/// Variant tag for `CameraBehavior` — used in the behavior section's
/// dedupe-and-rebuild signal so value drags inside the same variant
/// don't tear down the per-variant inputs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CameraBehaviorTag {
    Static,
    Follow,
    OrbitTarget,
    RailAlongCurve,
}

fn camera_behavior_tag(b: &CameraBehavior) -> CameraBehaviorTag {
    match b {
        CameraBehavior::Static => CameraBehaviorTag::Static,
        CameraBehavior::Follow { .. } => CameraBehaviorTag::Follow,
        CameraBehavior::OrbitTarget { .. } => CameraBehaviorTag::OrbitTarget,
        CameraBehavior::RailAlongCurve { .. } => CameraBehaviorTag::RailAlongCurve,
    }
}

const BEHAVIOR_VALUE_STATIC: &str = "static";
const BEHAVIOR_VALUE_FOLLOW: &str = "follow";
const BEHAVIOR_VALUE_ORBIT: &str = "orbit";
const BEHAVIOR_VALUE_RAIL: &str = "rail";

fn camera_behavior_section(node: Arc<Node>) -> Dom {
    let kind_sig = node.kind.clone();
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.4rem")
        .child(section_header("Behavior"))
        .child(field_row("Mode", camera_behavior_select(node.clone())))
        // Per-variant param rows — rebuild only on variant flips, NOT on
        // value tweaks inside an existing variant.
        .child_signal(kind_sig.signal_ref(|k| match k {
            NodeKind::Camera(cfg) => Some(camera_behavior_tag(&cfg.behavior)),
            _ => None,
        }).dedupe().map(clone!(node => move |tag| match tag {
            Some(CameraBehaviorTag::Static) => None,
            Some(CameraBehaviorTag::Follow) => Some(html!("div", {
                .style("display", "flex").style("flex-direction", "column").style("gap", "0.35rem")
                .child(field_row("Target", camera_behavior_target_select(node.clone())))
                .child(field_row("Offset X", camera_behavior_follow_offset(node.clone(), 0)))
                .child(field_row("Offset Y", camera_behavior_follow_offset(node.clone(), 1)))
                .child(field_row("Offset Z", camera_behavior_follow_offset(node.clone(), 2)))
                .child(field_row("Look at target", camera_behavior_follow_look_at(node.clone())))
            })),
            Some(CameraBehaviorTag::OrbitTarget) => Some(html!("div", {
                .style("display", "flex").style("flex-direction", "column").style("gap", "0.35rem")
                .child(field_row("Target", camera_behavior_target_select(node.clone())))
                .child(field_row("Distance", camera_behavior_orbit_f32(node.clone(), OrbitField::Distance)))
                .child(field_row("Pitch", camera_behavior_orbit_f32(node.clone(), OrbitField::Pitch)))
                .child(field_row("Yaw", camera_behavior_orbit_f32(node.clone(), OrbitField::Yaw)))
                .child(field_row("Auto-rotate", camera_behavior_orbit_f32(node.clone(), OrbitField::Auto)))
            })),
            Some(CameraBehaviorTag::RailAlongCurve) => Some(html!("div", {
                .style("display", "flex").style("flex-direction", "column").style("gap", "0.35rem")
                .child(field_row("Curve", camera_behavior_rail_curve_select(node.clone())))
                .child(field_row("Look-ahead", camera_behavior_rail_look_ahead(node.clone())))
                .child(field_row("Look-at target", camera_behavior_target_select(node.clone())))
            })),
            None => None,
        })))
    })
}

fn camera_behavior_select(node: Arc<Node>) -> Dom {
    let kind = node.kind.clone();
    html!("select" => web_sys::HtmlSelectElement, {
        .style("padding", "0.35rem 0.5rem")
        .style("background-color", ColorRaw::Darkest.value())
        .style("color", ColorText::SidebarHeader.value())
        .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .style("border-radius", "0.3rem")
        .style("font-size", "0.85rem")
        .style("cursor", "pointer")
        .child(html!("option", { .attr("value", BEHAVIOR_VALUE_STATIC).text("Static") }))
        .child(html!("option", { .attr("value", BEHAVIOR_VALUE_FOLLOW).text("Follow") }))
        .child(html!("option", { .attr("value", BEHAVIOR_VALUE_ORBIT).text("Orbit Target") }))
        .child(html!("option", { .attr("value", BEHAVIOR_VALUE_RAIL).text("Rail Along Curve") }))
        .with_node!(select => {
            .future(clone!(kind, select => {
                kind.signal_cloned().for_each(move |k| {
                    if let NodeKind::Camera(cfg) = k {
                        let want = match cfg.behavior {
                            CameraBehavior::Static => BEHAVIOR_VALUE_STATIC,
                            CameraBehavior::Follow { .. } => BEHAVIOR_VALUE_FOLLOW,
                            CameraBehavior::OrbitTarget { .. } => BEHAVIOR_VALUE_ORBIT,
                            CameraBehavior::RailAlongCurve { .. } => BEHAVIOR_VALUE_RAIL,
                        };
                        if select.value() != want { select.set_value(want); }
                    }
                    async {}
                })
            }))
            .event(clone!(kind, select => move |_: events::Change| {
                let new_behavior = match select.value().as_str() {
                    BEHAVIOR_VALUE_STATIC => CameraBehavior::Static,
                    BEHAVIOR_VALUE_FOLLOW => CameraBehavior::Follow {
                        target: NodeId(uuid::Uuid::nil()),
                        offset: [0.0, 0.0, 0.0],
                        look_at_target: true,
                    },
                    BEHAVIOR_VALUE_ORBIT => CameraBehavior::OrbitTarget {
                        target: NodeId(uuid::Uuid::nil()),
                        distance: 5.0,
                        pitch: 0.3,
                        yaw: 0.0,
                        auto_rotate_speed: 0.0,
                    },
                    _ => CameraBehavior::RailAlongCurve {
                        curve: NodeId(uuid::Uuid::nil()),
                        look_ahead_distance: 1.0,
                        target: None,
                    },
                };
                let mut k = kind.get_cloned();
                if let NodeKind::Camera(ref mut cfg) = k {
                    let same = std::mem::discriminant(&cfg.behavior) == std::mem::discriminant(&new_behavior);
                    if !same {
                        cfg.behavior = new_behavior;
                        kind.set(k);
                    }
                }
            }))
        })
    })
}

/// Target NodeId picker for camera-behavior variants. Refuses to commit a
/// change that would create a cycle (target points back at the camera,
/// directly or via a chain of camera-behavior references). Mirrors the
/// load-time backstop in `scene::camera_driver::validate_no_cycles`.
fn camera_behavior_target_select(node: Arc<Node>) -> Dom {
    let host_id = node.id;
    let kind = node.kind.clone();
    let candidates = collect_nodes_matching(|_| true);
    let mut options = vec![html!("option", { .attr("value", "").text("(none)") })];
    for (id, name) in &candidates {
        if *id == host_id {
            continue;
        }
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
                    let want = match k {
                        NodeKind::Camera(cfg) => match cfg.behavior {
                            CameraBehavior::Follow { target, .. }
                            | CameraBehavior::OrbitTarget { target, .. } => target.0.to_string(),
                            CameraBehavior::RailAlongCurve { target, .. } => {
                                target.map(|t| t.0.to_string()).unwrap_or_default()
                            }
                            CameraBehavior::Static => String::new(),
                        },
                        _ => String::new(),
                    };
                    if select.value() != want { select.set_value(&want); }
                    async {}
                })
            }))
            .event(clone!(kind, select => move |_: events::Change| {
                let value = select.value();
                let parsed = if value.is_empty() {
                    None
                } else {
                    match uuid::Uuid::parse_str(&value) {
                        Ok(u) => Some(NodeId(u)),
                        Err(_) => return,
                    }
                };
                let mut k = kind.get_cloned();
                if let NodeKind::Camera(ref mut cfg) = k {
                    let proposed = parsed;
                    // Refuse cycles: if the proposed target itself targets
                    // this camera (transitively), the camera-driver would
                    // loop. The load-time backstop downgrades cycles to
                    // Static + logs; we'd rather surface the conflict here.
                    if let Some(target) = proposed {
                        if camera_behavior_would_cycle(host_id, target) {
                            tracing::warn!(
                                "camera-behavior edit refused: setting target {target:?} on camera {host_id:?} would create a dependency cycle"
                            );
                            return;
                        }
                    }
                    match cfg.behavior {
                        CameraBehavior::Follow { ref mut target, .. }
                        | CameraBehavior::OrbitTarget { ref mut target, .. } => {
                            if let Some(t) = proposed { *target = t; }
                        }
                        CameraBehavior::RailAlongCurve { ref mut target, .. } => {
                            *target = proposed;
                        }
                        CameraBehavior::Static => return,
                    }
                    kind.set(k);
                }
            }))
        })
    })
}

/// Returns `true` if setting `target` on the camera with id `camera_id`
/// would create a cycle through the existing camera-behavior chain.
///
/// Walks the target/curve graph: if the target is a camera with its own
/// behavior, follow that behavior's target/curve, and so on. If we ever
/// hit `camera_id`, the proposed edit is cyclic.
fn camera_behavior_would_cycle(camera_id: NodeId, target: NodeId) -> bool {
    use std::collections::HashSet;
    let mut visited: HashSet<NodeId> = HashSet::new();
    let mut frontier = vec![target];
    while let Some(current) = frontier.pop() {
        if current == camera_id {
            return true;
        }
        if !visited.insert(current) {
            continue;
        }
        // Walk into the current node — if it's a camera, push its target/curve.
        let scene = app_state().scene.clone();
        let node = crate::scene::mutate::find_by_id(&scene, current);
        let Some(node) = node else { continue };
        let snapshot = node.kind.lock_ref().clone();
        if let NodeKind::Camera(cfg) = snapshot {
            match cfg.behavior {
                CameraBehavior::Follow { target, .. }
                | CameraBehavior::OrbitTarget { target, .. } => frontier.push(target),
                CameraBehavior::RailAlongCurve { curve, target, .. } => {
                    frontier.push(curve);
                    if let Some(t) = target {
                        frontier.push(t);
                    }
                }
                CameraBehavior::Static => {}
            }
        }
    }
    false
}

#[derive(Clone, Copy)]
enum OrbitField {
    Distance,
    Pitch,
    Yaw,
    Auto,
}

fn camera_behavior_orbit_f32(node: Arc<Node>, field: OrbitField) -> Dom {
    let kind = node.kind.clone();
    let value_signal = kind.signal_cloned().map(move |k| match k {
        NodeKind::Camera(cfg) => match cfg.behavior {
            CameraBehavior::OrbitTarget {
                distance,
                pitch,
                yaw,
                auto_rotate_speed,
                ..
            } => match field {
                OrbitField::Distance => distance,
                OrbitField::Pitch => pitch,
                OrbitField::Yaw => yaw,
                OrbitField::Auto => auto_rotate_speed,
            },
            _ => 0.0,
        },
        _ => 0.0,
    });
    number_input(value_signal, move |v| {
        let mut k = kind.get_cloned();
        if let NodeKind::Camera(ref mut cfg) = k {
            if let CameraBehavior::OrbitTarget {
                ref mut distance,
                ref mut pitch,
                ref mut yaw,
                ref mut auto_rotate_speed,
                ..
            } = cfg.behavior
            {
                match field {
                    OrbitField::Distance => *distance = v.max(0.01),
                    OrbitField::Pitch => *pitch = v,
                    OrbitField::Yaw => *yaw = v,
                    OrbitField::Auto => *auto_rotate_speed = v,
                }
                kind.set(k);
            }
        }
    })
}

fn camera_behavior_follow_offset(node: Arc<Node>, axis: usize) -> Dom {
    let kind = node.kind.clone();
    let value_signal = kind.signal_cloned().map(move |k| match k {
        NodeKind::Camera(cfg) => match cfg.behavior {
            CameraBehavior::Follow { offset, .. } => offset[axis],
            _ => 0.0,
        },
        _ => 0.0,
    });
    number_input(value_signal, move |v| {
        let mut k = kind.get_cloned();
        if let NodeKind::Camera(ref mut cfg) = k {
            if let CameraBehavior::Follow { ref mut offset, .. } = cfg.behavior {
                offset[axis] = v;
                kind.set(k);
            }
        }
    })
}

fn camera_behavior_follow_look_at(node: Arc<Node>) -> Dom {
    let kind = node.kind.clone();
    html!("input" => web_sys::HtmlInputElement, {
        .attr("type", "checkbox")
        .style("cursor", "pointer")
        .with_node!(input => {
            .future(clone!(kind, input => {
                kind.signal_cloned().for_each(move |k| {
                    if let NodeKind::Camera(cfg) = k {
                        if let CameraBehavior::Follow { look_at_target, .. } = cfg.behavior {
                            input.set_checked(look_at_target);
                        }
                    }
                    async {}
                })
            }))
            .event(clone!(kind, input => move |_: events::Change| {
                let mut k = kind.get_cloned();
                if let NodeKind::Camera(ref mut cfg) = k {
                    if let CameraBehavior::Follow {
                        ref mut look_at_target,
                        ..
                    } = cfg.behavior
                    {
                        *look_at_target = input.checked();
                        kind.set(k);
                    }
                }
            }))
        })
    })
}

fn camera_behavior_rail_curve_select(node: Arc<Node>) -> Dom {
    let host_id = node.id;
    let kind = node.kind.clone();
    let candidates = collect_nodes_matching(|k| matches!(k, NodeKind::Curve(_)));
    let mut options = vec![html!("option", { .attr("value", "").text("(none)") })];
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
                    let want = match k {
                        NodeKind::Camera(cfg) => match cfg.behavior {
                            CameraBehavior::RailAlongCurve { curve, .. } => curve.0.to_string(),
                            _ => String::new(),
                        },
                        _ => String::new(),
                    };
                    if select.value() != want { select.set_value(&want); }
                    async {}
                })
            }))
            .event(clone!(kind, select => move |_: events::Change| {
                let value = select.value();
                if value.is_empty() { return; }
                let Ok(parsed) = uuid::Uuid::parse_str(&value) else { return };
                let new_curve = NodeId(parsed);
                if camera_behavior_would_cycle(host_id, new_curve) {
                    tracing::warn!(
                        "camera-behavior edit refused: setting curve {new_curve:?} on camera {host_id:?} would create a dependency cycle"
                    );
                    return;
                }
                let mut k = kind.get_cloned();
                if let NodeKind::Camera(ref mut cfg) = k {
                    if let CameraBehavior::RailAlongCurve { ref mut curve, .. } = cfg.behavior {
                        *curve = new_curve;
                        kind.set(k);
                    }
                }
            }))
        })
    })
}

fn camera_behavior_rail_look_ahead(node: Arc<Node>) -> Dom {
    let kind = node.kind.clone();
    let value_signal = kind.signal_cloned().map(move |k| match k {
        NodeKind::Camera(cfg) => match cfg.behavior {
            CameraBehavior::RailAlongCurve {
                look_ahead_distance,
                ..
            } => look_ahead_distance,
            _ => 0.0,
        },
        _ => 0.0,
    });
    number_input(value_signal, move |v| {
        let mut k = kind.get_cloned();
        if let NodeKind::Camera(ref mut cfg) = k {
            if let CameraBehavior::RailAlongCurve {
                ref mut look_ahead_distance,
                ..
            } = cfg.behavior
            {
                *look_ahead_distance = v;
                kind.set(k);
            }
        }
    })
}

fn projection_select(node: Arc<Node>) -> Dom {
    let kind = node.kind.clone();
    html!("select" => web_sys::HtmlSelectElement, {
        .style("padding", "0.35rem 0.5rem")
        .style("background-color", ColorRaw::Darkest.value())
        .style("color", ColorText::SidebarHeader.value())
        .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .style("border-radius", "0.3rem")
        .style("font-size", "0.85rem")
        .style("cursor", "pointer")
        .child(html!("option", { .attr("value", CAMERA_VALUE_PERSP).text("Perspective") }))
        .child(html!("option", { .attr("value", CAMERA_VALUE_ORTHO).text("Orthographic") }))
        .with_node!(select => {
            .future(clone!(kind, select => {
                kind.signal_cloned().for_each(move |k| {
                    if let NodeKind::Camera(cfg) = k {
                        let want = match cfg.projection {
                            CameraProjection::Perspective { .. } => CAMERA_VALUE_PERSP,
                            CameraProjection::Orthographic { .. } => CAMERA_VALUE_ORTHO,
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
                if let NodeKind::Camera(ref mut cfg) = k {
                    let new_value = select.value();
                    let new_projection = match new_value.as_str() {
                        CAMERA_VALUE_ORTHO => CameraProjection::Orthographic {
                            half_height: 5.0,
                        },
                        _ => CameraProjection::Perspective {
                            fov_y_rad: std::f32::consts::FRAC_PI_3,
                        },
                    };
                    // Replace only if the variant actually changed; preserve
                    // the value of the matching kind so flipping back and
                    // forth doesn't keep clobbering edits in progress.
                    let same_variant = matches!(
                        (&cfg.projection, &new_projection),
                        (CameraProjection::Perspective { .. }, CameraProjection::Perspective { .. })
                        | (CameraProjection::Orthographic { .. }, CameraProjection::Orthographic { .. })
                    );
                    if !same_variant {
                        cfg.projection = new_projection;
                        kind.set(k);
                    }
                }
            }))
        })
    })
}

fn fov_y_input(node: Arc<Node>) -> Dom {
    let kind = node.kind.clone();
    let value_signal = kind.signal_cloned().map(|k| match k {
        NodeKind::Camera(CameraConfig {
            projection: CameraProjection::Perspective { fov_y_rad },
            ..
        }) => fov_y_rad,
        _ => 0.0,
    });
    number_input(value_signal, move |new_value| {
        let mut k = kind.get_cloned();
        if let NodeKind::Camera(CameraConfig {
            projection: CameraProjection::Perspective { ref mut fov_y_rad },
            ..
        }) = k
        {
            // Clamp into a sane range — degenerate FOV produces a NaN
            // projection matrix and a black frame at runtime.
            *fov_y_rad = new_value.clamp(0.05, std::f32::consts::PI - 0.05);
            kind.set(k);
        }
    })
}

fn ortho_half_height_input(node: Arc<Node>) -> Dom {
    let kind = node.kind.clone();
    let value_signal = kind.signal_cloned().map(|k| match k {
        NodeKind::Camera(CameraConfig {
            projection: CameraProjection::Orthographic { half_height },
            ..
        }) => half_height,
        _ => 0.0,
    });
    number_input(value_signal, move |new_value| {
        let mut k = kind.get_cloned();
        if let NodeKind::Camera(CameraConfig {
            projection:
                CameraProjection::Orthographic {
                    ref mut half_height,
                },
            ..
        }) = k
        {
            *half_height = new_value.max(0.001);
            kind.set(k);
        }
    })
}

#[derive(Clone, Copy)]
enum CameraClip {
    Near,
    Far,
}

fn camera_clip_input(node: Arc<Node>, which: CameraClip) -> Dom {
    let kind = node.kind.clone();
    let value_signal = kind.signal_cloned().map(move |k| match k {
        NodeKind::Camera(cfg) => match which {
            CameraClip::Near => cfg.near,
            CameraClip::Far => cfg.far,
        },
        _ => 0.0,
    });
    number_input(value_signal, move |new_value| {
        let mut k = kind.get_cloned();
        if let NodeKind::Camera(ref mut cfg) = k {
            match which {
                CameraClip::Near => cfg.near = new_value.max(0.001),
                CameraClip::Far => cfg.far = new_value.max(0.001),
            }
            // Keep near < far so the frustum never inverts. The relaxed
            // floor (`+1e-3`) lets the user tweak one side without the
            // input jumping; on a Build, the per-game packer can enforce
            // a stricter invariant if needed.
            if cfg.near >= cfg.far {
                match which {
                    CameraClip::Near => cfg.far = cfg.near + 0.001,
                    CameraClip::Far => cfg.near = (cfg.far - 0.001).max(0.001),
                }
            }
            kind.set(k);
        }
    })
}
