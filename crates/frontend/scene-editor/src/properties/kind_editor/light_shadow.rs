//! Shadow sub-panel for the light inspector. Hosts every per-light
//! shadow knob from `awsm_scene_schema::LightShadowConfig` and writes
//! changes back through `node.kind`. The renderer-bridge's kind
//! observer destroys + reinserts the light on every change, which
//! propagates the new `LightShadowParams` through
//! `light_shadow_params_from_config` — heavy, but it keeps this panel
//! a pure presentation layer.

use std::sync::Arc;

use awsm_scene_schema::{
    CubeFaceUpdateRate, EvsmCutoff, FarCascadeUpdateRate, LightKind, LightShadowConfig,
    LightShadowHardness,
};
use web_sys::HtmlSelectElement;

use crate::prelude::*;
use crate::properties::kind_editor::{field_row, section_header};
use crate::properties::transform::number_input;
use crate::scene::{Node, NodeKind};
use crate::state::app_state;

/// Render the Shadows sub-panel for the currently-selected light. Lays
/// out shared controls first, then directional-only and point-only
/// sub-blocks driven by `kind.signal_ref(light_variant_tag)` so the
/// inputs don't tear down when the user edits something inside the
/// active variant.
pub(super) fn render(node: Arc<Node>) -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.5rem")
        .style("margin-top", "0.5rem")
        .style("padding-top", "0.5rem")
        .style("border-top", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .child(section_header("Shadows"))
        .child(field_row("Cast", cast_toggle(node.clone())))
        .child(field_row("Depth bias", shadow_scalar(node.clone(), ShadowScalarField::DepthBias)))
        .child(field_row("Normal bias", shadow_scalar(node.clone(), ShadowScalarField::NormalBias)))
        .child(field_row("Max distance", shadow_scalar(node.clone(), ShadowScalarField::MaxDistance)))
        .child(field_row("Resolution", resolution_select(node.clone())))
        .child(field_row("Hardness", hardness_select(node.clone())))
        // PCSS scale only matters when hardness=Pcss; gate so the row
        // isn't a noise. Signals on hardness so it appears/disappears
        // reactively without rebuilding the whole shadow panel.
        .child_signal(hardness_signal(node.clone()).map(clone!(node => move |h| {
            if matches!(h, LightShadowHardness::Pcss) {
                Some(field_row("PCSS scale", shadow_scalar(node.clone(), ShadowScalarField::PcssPenumbraScale)))
            } else {
                None
            }
        })))
        // Directional / point variant-specific blocks. Dedupe on
        // `LightKind` so editing a directional `cascade_count` doesn't
        // detach the dropdown mid-click.
        .child_signal(node.kind.signal_ref(variant_tag).dedupe().map(clone!(node => move |variant| {
            match variant {
                Some(LightKind::Directional) => Some(html!("div", {
                    .style("display", "flex")
                    .style("flex-direction", "column")
                    .style("gap", "0.5rem")
                    .child(field_row("Cascades", cascade_count_select(node.clone())))
                    .child(field_row("Split λ", shadow_scalar(node.clone(), ShadowScalarField::CascadeSplitLambda)))
                    .child(field_row("EVSM cutoff", evsm_cutoff_select(node.clone())))
                    .child(field_row("Far update", far_cascade_update_rate_select(node.clone())))
                })),
                Some(LightKind::Point) => Some(html!("div", {
                    .style("display", "flex")
                    .style("flex-direction", "column")
                    .style("gap", "0.5rem")
                    .child(field_row("Cube faces", cube_face_update_rate_select(node.clone())))
                })),
                Some(LightKind::Spot) | None => None,
            }
        })))
    })
}

fn variant_tag(k: &NodeKind) -> Option<LightKind> {
    match k {
        NodeKind::Light(cfg) => Some(cfg.kind()),
        _ => None,
    }
}

fn shadow_of(k: &NodeKind) -> Option<&LightShadowConfig> {
    match k {
        NodeKind::Light(cfg) => Some(cfg.shadow()),
        _ => None,
    }
}

/// Mutate the active variant's shadow config in place and push the
/// modified `NodeKind` back into the node's mutable. A clone-modify-set
/// roundtrip is needed because `Mutable::set` requires owned `T`.
fn update_shadow<F: FnOnce(&mut LightShadowConfig)>(node: &Arc<Node>, f: F) {
    let mut k = node.kind.get_cloned();
    let NodeKind::Light(ref mut cfg) = k else {
        return;
    };
    f(cfg.shadow_mut());
    let state = app_state();
    let previous = state.snapshot_scene();
    node.kind.set(k);
    state.scene.bump_revision();
    state.commit_history(previous);
}

#[derive(Clone, Copy)]
enum ShadowScalarField {
    DepthBias,
    NormalBias,
    MaxDistance,
    PcssPenumbraScale,
    CascadeSplitLambda,
}

fn shadow_scalar(node: Arc<Node>, field: ShadowScalarField) -> Dom {
    let kind = node.kind.clone();
    let value_signal = kind.signal_cloned().map(move |k| match shadow_of(&k) {
        Some(s) => match field {
            ShadowScalarField::DepthBias => s.depth_bias,
            ShadowScalarField::NormalBias => s.normal_bias,
            ShadowScalarField::MaxDistance => s.max_distance,
            ShadowScalarField::PcssPenumbraScale => s.pcss_penumbra_scale,
            ShadowScalarField::CascadeSplitLambda => s.cascade_split_lambda,
        },
        None => 0.0,
    });
    number_input(value_signal, move |v| {
        update_shadow(&node, |s| match field {
            ShadowScalarField::DepthBias => s.depth_bias = v.max(0.0),
            ShadowScalarField::NormalBias => s.normal_bias = v.max(0.0),
            ShadowScalarField::MaxDistance => s.max_distance = v.max(0.01),
            ShadowScalarField::PcssPenumbraScale => s.pcss_penumbra_scale = v.max(0.01),
            ShadowScalarField::CascadeSplitLambda => s.cascade_split_lambda = v.clamp(0.0, 1.0),
        });
    })
}

fn cast_toggle(node: Arc<Node>) -> Dom {
    let kind = node.kind.clone();
    let checked = kind
        .signal_cloned()
        .map(|k| shadow_of(&k).map(|s| s.cast).unwrap_or(false));
    html!("input" => web_sys::HtmlInputElement, {
        .attr("type", "checkbox")
        .style("width", "1rem")
        .style("height", "1rem")
        .style("cursor", "pointer")
        .with_node!(input => {
            .future(clone!(input => {
                checked.for_each(move |c| {
                    if input.checked() != c {
                        input.set_checked(c);
                    }
                    async {}
                })
            }))
            .event(clone!(input => move |_: events::Change| {
                let v = input.checked();
                update_shadow(&node, |s| s.cast = v);
            }))
        })
    })
}

// --- Select / enum controls ----------------------------------------------

static SELECT: LazyLock<String> = LazyLock::new(|| {
    class! {
        .style("width", "100%")
        .style("padding", "0.25rem 0.4rem")
        .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .style("border-radius", "0.25rem")
        .style("background-color", ColorRaw::Darkest.value())
        .style("color", ColorText::SidebarHeader.value())
        .style("font-size", "0.8rem")
        .style("font-family", "monospace")
        .style("cursor", "pointer")
    }
});

fn hardness_signal(node: Arc<Node>) -> impl Signal<Item = LightShadowHardness> {
    node.kind.signal_cloned().map(|k| {
        shadow_of(&k)
            .map(|s| s.hardness)
            .unwrap_or(LightShadowHardness::Soft)
    })
}

fn shadow_signal<T: 'static>(
    node: &Arc<Node>,
    get: impl Fn(&LightShadowConfig) -> T + 'static,
    default: T,
) -> impl Signal<Item = T>
where
    T: Clone,
{
    node.kind.signal_cloned().map(move |k| match shadow_of(&k) {
        Some(s) => get(s),
        None => default.clone(),
    })
}

fn resolution_select(node: Arc<Node>) -> Dom {
    let sig = shadow_signal(&node, |s| s.resolution, 1024);
    enum_select(
        &[
            ("256", 256u32),
            ("512", 512),
            ("1024", 1024),
            ("2048", 2048),
            ("4096", 4096),
        ],
        sig,
        move |v| update_shadow(&node, |s| s.resolution = v),
    )
}

fn hardness_select(node: Arc<Node>) -> Dom {
    let sig = shadow_signal(&node, |s| s.hardness, LightShadowHardness::Soft);
    enum_select(
        &[
            ("Hard", LightShadowHardness::Hard),
            ("Soft", LightShadowHardness::Soft),
            ("PCSS", LightShadowHardness::Pcss),
        ],
        sig,
        move |v| update_shadow(&node, |s| s.hardness = v),
    )
}

fn cascade_count_select(node: Arc<Node>) -> Dom {
    let sig = shadow_signal(&node, |s| s.cascade_count, 4);
    enum_select(
        &[("1", 1u8), ("2", 2), ("3", 3), ("4", 4)],
        sig,
        move |v| update_shadow(&node, |s| s.cascade_count = v),
    )
}

fn evsm_cutoff_select(node: Arc<Node>) -> Dom {
    let sig = shadow_signal(&node, |s| s.evsm_cutoff, EvsmCutoff::LastCascade);
    enum_select(
        &[
            ("Off", EvsmCutoff::Off),
            ("Last cascade", EvsmCutoff::LastCascade),
            ("Last 2 cascades", EvsmCutoff::LastTwoCascades),
        ],
        sig,
        move |v| update_shadow(&node, |s| s.evsm_cutoff = v),
    )
}

fn far_cascade_update_rate_select(node: Arc<Node>) -> Dom {
    let sig = shadow_signal(
        &node,
        |s| s.far_cascade_update_rate,
        FarCascadeUpdateRate::default(),
    );
    enum_select(
        &[
            ("Every frame", FarCascadeUpdateRate::EveryFrame),
            ("Every 2 frames", FarCascadeUpdateRate::Every2Frames),
            ("Every 4 frames", FarCascadeUpdateRate::Every4Frames),
            ("Every 8 frames", FarCascadeUpdateRate::Every8Frames),
        ],
        sig,
        move |v| update_shadow(&node, |s| s.far_cascade_update_rate = v),
    )
}

fn cube_face_update_rate_select(node: Arc<Node>) -> Dom {
    let sig = shadow_signal(
        &node,
        |s| s.cube_face_update_rate,
        CubeFaceUpdateRate::default(),
    );
    enum_select(
        &[
            ("Every frame", CubeFaceUpdateRate::EveryFrame),
            ("Every 2 frames", CubeFaceUpdateRate::Every2Frames),
            ("Every 4 frames", CubeFaceUpdateRate::Every4Frames),
            ("Every 8 frames", CubeFaceUpdateRate::Every8Frames),
        ],
        sig,
        move |v| update_shadow(&node, |s| s.cube_face_update_rate = v),
    )
}

/// Generic `<select>` widget driven by a `Signal<Item = T>` (so external
/// changes — undo, programmatic edits — sync automatically) and a
/// commit callback. Option ids are derived via `Debug` so callers don't
/// have to provide a `Display` for each enum.
fn enum_select<T, S>(options: &[(&'static str, T)], value_signal: S, write: impl Fn(T) + 'static) -> Dom
where
    T: Copy + PartialEq + std::fmt::Debug + 'static,
    S: Signal<Item = T> + 'static,
{
    let entries: Vec<(String, String, T)> = options
        .iter()
        .map(|(label, v)| (label.to_string(), format!("{:?}", v), *v))
        .collect();
    let entries_for_change = entries.clone();
    let entries_for_sync = entries.clone();

    html!("select" => HtmlSelectElement, {
        .class(&*SELECT)
        .apply(move |dom| {
            let mut dom = dom;
            for (label, id, _) in &entries {
                dom = dom.child(html!("option", {
                    .attr("value", id)
                    .text(label)
                }));
            }
            dom
        })
        .with_node!(select => {
            // Sync the displayed value when the source signal changes
            // (undo / history rewind / programmatic edit). `set_value`
            // is a no-op when the value is already correct.
            .future(clone!(select => {
                value_signal.for_each(move |v| {
                    if let Some(id) = entries_for_sync
                        .iter()
                        .find(|(_, _, val)| *val == v)
                        .map(|(_, id, _)| id.clone())
                    {
                        if select.value() != id {
                            select.set_value(&id);
                        }
                    }
                    async {}
                })
            }))
            .event(clone!(select => move |_: events::Change| {
                let new_id = select.value();
                if let Some((_, _, v)) = entries_for_change.iter().find(|(_, id, _)| *id == new_id) {
                    write(*v);
                }
            }))
        })
    })
}
