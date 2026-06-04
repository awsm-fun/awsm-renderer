//! Inline SVG icons for each `NodeKind`. Color-coded to make the tree
//! scannable at a glance:
//!
//! - Group   → neutral grey folder
//! - Model   → blue cube
//! - Light   → warm orange sun
//! - Collider → green shield
//! - Camera  → magenta camera body

use crate::prelude::*;
use crate::scene::NodeKind;

const ICON_SIZE_PX: u32 = 16;

pub fn for_kind(kind: &NodeKind) -> Dom {
    match kind {
        NodeKind::Group => group_icon(),
        NodeKind::Model(_) => model_icon(),
        NodeKind::Light(_) => light_icon(),
        NodeKind::Collider(_) => collider_icon(),
        NodeKind::Camera(_) => camera_icon(),
        NodeKind::Primitive { .. } | NodeKind::Mesh { .. } => primitive_icon(),
        NodeKind::Curve(_) => curve_icon(),
        NodeKind::SweepAlongCurve { .. } => sweep_icon(),
        NodeKind::InstancesAlongCurve(_) => instances_icon(),
        NodeKind::Line(_) => line_icon(),
        NodeKind::Sprite(_) => sprite_icon(),
        NodeKind::ParticleEmitter(_) => particle_icon(),
        NodeKind::Decal(_) => decal_icon(),
    }
}

fn decal_icon() -> Dom {
    icon(
        "#A78BFA",
        &[
            // Cube outline (front + back face)
            "M3 6 L8 4 L13 6 L13 11 L8 13 L3 11 Z",
            "M3 6 L8 8 L13 6",
            "M8 8 L8 13",
        ],
    )
}

fn primitive_icon() -> Dom {
    icon("#5BD9D9", &["M3 5 L8 2 L13 5 L13 11 L8 14 L3 11 Z"])
}

fn curve_icon() -> Dom {
    icon("#F2C94C", &["M2 12 C 5 4, 11 4, 14 12"])
}

fn sweep_icon() -> Dom {
    icon(
        "#F2C94C",
        &["M2 12 C 5 4, 11 4, 14 12", "M2 9 C 5 1, 11 1, 14 9"],
    )
}

fn instances_icon() -> Dom {
    icon(
        "#F2C94C",
        &["M3 12 L3 8", "M7 12 L7 6", "M11 12 L11 7", "M14 12 L14 9"],
    )
}

fn line_icon() -> Dom {
    icon("#FF80B0", &["M2 12 L14 4"])
}

fn sprite_icon() -> Dom {
    icon(
        "#FF80B0",
        &["M3 3 L13 3 L13 13 L3 13 Z", "M3 3 L13 13", "M13 3 L3 13"],
    )
}

fn particle_icon() -> Dom {
    icon(
        ColorRaw::Orange.value(),
        &[
            "M4 12 L4 8",
            "M8 12 L8 6",
            "M12 12 L12 9",
            "M4 7 m -0.6 0 a 0.6 0.6 0 1 0 1.2 0 a 0.6 0.6 0 1 0 -1.2 0",
            "M8 5 m -0.6 0 a 0.6 0.6 0 1 0 1.2 0 a 0.6 0.6 0 1 0 -1.2 0",
            "M12 8 m -0.6 0 a 0.6 0.6 0 1 0 1.2 0 a 0.6 0.6 0 1 0 -1.2 0",
        ],
    )
}

fn icon(color: &'static str, paths: &[&'static str]) -> Dom {
    svg!("svg", {
        .attr("xmlns", "http://www.w3.org/2000/svg")
        .attr("viewBox", "0 0 16 16")
        .attr("width", &ICON_SIZE_PX.to_string())
        .attr("height", &ICON_SIZE_PX.to_string())
        .attr("fill", "none")
        .attr("stroke", color)
        .attr("stroke-width", "1.4")
        .attr("stroke-linecap", "round")
        .attr("stroke-linejoin", "round")
        .children(paths.iter().map(|d| svg!("path", { .attr("d", d) })))
    })
}

fn group_icon() -> Dom {
    icon(
        ColorRaw::MidGrey.value(),
        &["M2 5 L6 5 L7.5 6.5 L14 6.5 L14 13 L2 13 Z"],
    )
}

fn model_icon() -> Dom {
    icon(
        ColorRaw::Accent.value(),
        &[
            "M8 2 L14 5 L14 11 L8 14 L2 11 L2 5 Z",
            "M2 5 L8 8 L14 5",
            "M8 8 L8 14",
        ],
    )
}

fn light_icon() -> Dom {
    icon(
        ColorRaw::Orange.value(),
        &[
            "M8 4 A3 3 0 1 1 7.99 4 Z",
            "M8 1.5 L8 2.5",
            "M8 13.5 L8 14.5",
            "M1.5 8 L2.5 8",
            "M13.5 8 L14.5 8",
            "M3.5 3.5 L4.2 4.2",
            "M11.8 11.8 L12.5 12.5",
            "M12.5 3.5 L11.8 4.2",
            "M4.2 11.8 L3.5 12.5",
        ],
    )
}

fn collider_icon() -> Dom {
    icon(
        ColorRaw::Green.value(),
        &["M8 2 L13 4 L13 9 Q13 12 8 14 Q3 12 3 9 L3 4 Z"],
    )
}

/// Camera icon — a rectangular body with a small viewfinder bump on
/// top + a circle for the lens. Magenta (not in the shared palette,
/// so inline) to read as distinct from collider-green and the cyan /
/// accent blues used elsewhere in the tree.
fn camera_icon() -> Dom {
    icon(
        "#E66EFF",
        &[
            // Camera body
            "M2 6 L14 6 L14 12 L2 12 Z",
            // Viewfinder bump on top
            "M6 6 L7 4 L10 4 L11 6",
            // Lens
            "M8 9 m -1.8 0 a 1.8 1.8 0 1 0 3.6 0 a 1.8 1.8 0 1 0 -3.6 0",
        ],
    )
}
