//! Animation-mode **KeyInspector**: edits the selected timeline element — a
//! keyframe (Time / Value / Interp / tangents) or, when only a track is
//! selected, the track (Target / Property / Sampler / Channels). Mirrors
//! `material_mode`'s row + field-binding patterns.
//!
//! Load-bearing rule: every edit dispatches `SetKeyframe` /
//! `SetTrackSampler` through the one `EditorController`. Reads of
//! `anim_selection` + the active clip are read-only.

use std::sync::Arc;

use crate::controller::animation::{find_clip, AnimSel, CustomAnimation, Track, TrackTarget};
use crate::controller::{EditorCommand, Interp, SamplerKind, TrackValue};
use crate::prelude::*;

pub fn render() -> Dom {
    html!("div", {
        // Reactive on (current clip, selection): rebuild when either changes.
        .child_signal(map_ref! {
            let clip_id = controller().current_clip.signal(),
            let sel = controller().anim_selection.signal() =>
            Some(body(*clip_id, *sel))
        })
    })
}

fn body(clip_id: Option<crate::engine::scene::AssetId>, sel: Option<AnimSel>) -> Dom {
    let Some(clip) = clip_id.and_then(|id| find_clip(&controller().custom_animations, id)) else {
        return hint("No clip selected.");
    };
    let Some(sel) = sel else {
        return hint("Select a track or keyframe in the timeline to edit it.");
    };
    let Some(track) = clip.tracks.lock_ref().get(sel.track).cloned() else {
        return hint("Select a track or keyframe in the timeline to edit it.");
    };

    // KEYFRAME selected → keyframe editor; otherwise the TRACK editor.
    let kf = sel
        .keyframe
        .and_then(|i| track.keys.get_cloned().get(i).cloned().map(|k| (i, k)));

    html!("div", {
        .style("display", "flex").style("flex-direction", "column").style("min-height", "0")
        .child(header(&track, kf.is_some()))
        .child(html!("div", {
            .style("padding", "10px 12px 12px").style("display", "flex").style("flex-direction", "column")
            .style("gap", "var(--gap)").style("overflow", "auto")
            .apply(|b| match kf {
                Some((index, key)) => keyframe_rows(b, &clip, sel, index, &key, &track),
                None => track_rows(b, sel, &track),
            })
        }))
    })
}

/// Header (height 32): kicker + right-aligned target icon + `{target} · {prop}`.
fn header(track: &Arc<Track>, is_keyframe: bool) -> Dom {
    html!("div", {
        .style("display", "flex").style("align-items", "center").style("gap", "7px")
        .style("height", "32px").style("padding", "0 12px")
        .style("border-bottom", "1px solid var(--line-soft)").style("flex", "0 0 auto")
        .child(html!("span", { .class("kicker").text(if is_keyframe { "Keyframe" } else { "Track" }) }))
        .child(html!("div", { .style("flex", "1") }))
        .child(Icon::new(target_icon(&track.target)).size(13.0).color("var(--text-3)").render())
        .child(html!("span", {
            .class("mono").style("font-size", "10.5px").style("color", "var(--text-2)")
            .text(&format!("{} \u{00b7} {}", target_label(&track.target), prop_label(&track.target)))
        }))
    })
}

// ── KEYFRAME editor ──────────────────────────────────────────────────────────

fn keyframe_rows(
    b: dominator::DomBuilder<web_sys::HtmlElement>,
    clip: &Arc<CustomAnimation>,
    sel: AnimSel,
    index: usize,
    key: &crate::controller::animation::Keyframe,
    track: &Arc<Track>,
) -> dominator::DomBuilder<web_sys::HtmlElement> {
    let clip_id = clip.id;
    let track_idx = sel.track;
    let dur = clip.duration.get();
    let t = track.times.get_cloned().get(index).copied().unwrap_or(0.0);
    let step = value_step(&track.target);

    // Channel (mono, colored to match the value channel — single-channel here).
    let mut b = b.child(row(
        "Channel",
        html!("span", {
            .class("mono").style("font-size", "12px").style("color", "var(--accent-bright)")
            .text(&channels_label(&track.target))
        }),
    ));

    // Time (NumField, step 0.01, min 0, max dur, suffix "s").
    b = b.child(row(
        "Time",
        NumField::new(t)
            .min(0.0)
            .max(dur)
            .step(0.01)
            .suffix("s")
            .on_change(move |v| {
                dispatch(EditorCommand::SetKeyframe {
                    clip: clip_id,
                    track: track_idx,
                    index,
                    t: Some(v),
                    value: None,
                    interp: None,
                    in_tangent: None,
                    out_tangent: None,
                });
            })
            .render(),
    ));

    // Value — editable. A scalar is one field; a vec3 / quaternion is edited
    // per component (X·Y·Z, +W for rotation). Each field dispatches a full
    // `SetKeyframe` with the other components preserved.
    b = match key.value {
        TrackValue::Scalar(s) => b.child(row(
            "Value",
            NumField::new(s as f64)
                .step(step)
                .on_change(move |v| {
                    dispatch(EditorCommand::SetKeyframe {
                        clip: clip_id,
                        track: track_idx,
                        index,
                        t: None,
                        value: Some(TrackValue::Scalar(v as f32)),
                        interp: None,
                        in_tangent: None,
                        out_tangent: None,
                    });
                })
                .render(),
        )),
        TrackValue::Vec3(v) => {
            let mut bb = b;
            for (i, lab) in ["X", "Y", "Z"].iter().enumerate() {
                bb = bb.child(row(
                    *lab,
                    NumField::new(v[i] as f64)
                        .step(step)
                        .on_change(move |val| {
                            let mut nv = v;
                            nv[i] = val as f32;
                            dispatch(EditorCommand::SetKeyframe {
                                clip: clip_id,
                                track: track_idx,
                                index,
                                t: None,
                                value: Some(TrackValue::Vec3(nv)),
                                interp: None,
                                in_tangent: None,
                                out_tangent: None,
                            });
                        })
                        .render(),
                ));
            }
            bb
        }
        TrackValue::Quat(q) => {
            let mut bb = b;
            for (i, lab) in ["X", "Y", "Z", "W"].iter().enumerate() {
                bb = bb.child(row(
                    *lab,
                    NumField::new(q[i] as f64)
                        .step(step)
                        .on_change(move |val| {
                            let mut nq = q;
                            nq[i] = val as f32;
                            dispatch(EditorCommand::SetKeyframe {
                                clip: clip_id,
                                track: track_idx,
                                index,
                                t: None,
                                value: Some(TrackValue::Quat(nq)),
                                interp: None,
                                in_tangent: None,
                                out_tangent: None,
                            });
                        })
                        .render(),
                ));
            }
            bb
        }
    };

    // Interp (Select: Constant / Linear / Cubic spline).
    let interp = Mutable::new(interp_key(key.interp).to_string());
    spawn_local(clone!(interp => async move {
        let mut first = true;
        interp.signal_cloned().for_each(move |k| {
            let fire = !first;
            first = false;
            let parsed = interp_from_key(&k);
            async move {
                if fire {
                    dispatch(EditorCommand::SetKeyframe {
                        clip: clip_id, track: track_idx, index,
                        t: None, value: None, interp: Some(parsed),
                        in_tangent: None, out_tangent: None,
                    });
                }
            }
        }).await;
    }));
    b = b.child(row(
        "Interp",
        select(
            interp,
            vec![
                ("step".into(), "Constant".into()),
                ("linear".into(), "Linear".into()),
                ("cubic".into(), "Cubic spline".into()),
            ],
        ),
    ));

    // In/Out tangent (NumField, step 0.1) — only when interp == cubic + scalar.
    if matches!(key.interp, Interp::Cubic) {
        if let TrackValue::Scalar(in_t) = key.in_tangent {
            b = b.child(row(
                "In tangent",
                NumField::new(in_t as f64)
                    .step(0.1)
                    .on_change(move |v| {
                        dispatch(EditorCommand::SetKeyframe {
                            clip: clip_id,
                            track: track_idx,
                            index,
                            t: None,
                            value: None,
                            interp: None,
                            in_tangent: Some(TrackValue::Scalar(v as f32)),
                            out_tangent: None,
                        });
                    })
                    .render(),
            ));
        }
        if let TrackValue::Scalar(out_t) = key.out_tangent {
            b = b.child(row(
                "Out tangent",
                NumField::new(out_t as f64)
                    .step(0.1)
                    .on_change(move |v| {
                        dispatch(EditorCommand::SetKeyframe {
                            clip: clip_id,
                            track: track_idx,
                            index,
                            t: None,
                            value: None,
                            interp: None,
                            in_tangent: None,
                            out_tangent: Some(TrackValue::Scalar(v as f32)),
                        });
                    })
                    .render(),
            ));
        }
    }

    // Delete this keyframe.
    b = b.child(html!("div", {
        .style("padding", "10px 0 2px")
        .child(html!("button", {
            .class("t")
            .attr("title", "Delete this keyframe")
            .style("width", "100%").style("height", "28px")
            .style("display", "flex").style("align-items", "center").style("justify-content", "center")
            .style("border", "1px solid var(--line-soft)").style("border-radius", "var(--r2)")
            .style("background", "transparent").style("color", "#f7768e").style("cursor", "pointer")
            .style("font-size", "11.5px")
            .text("Delete keyframe")
            .event(move |_: events::Click| {
                dispatch(EditorCommand::DeleteKeyframe {
                    clip: clip_id,
                    track: track_idx,
                    index,
                });
            })
        }))
    }));

    b
}

// ── TRACK editor ─────────────────────────────────────────────────────────────

fn track_rows(
    b: dominator::DomBuilder<web_sys::HtmlElement>,
    sel: AnimSel,
    track: &Arc<Track>,
) -> dominator::DomBuilder<web_sys::HtmlElement> {
    let clip_id = controller().current_clip.get();
    let track_idx = sel.track;

    // "Target · Property" identifies the track (e.g. `Skeleton_neck_joint_1 ·
    // rotation`). The internal renderer binding ("Lowers to") is a dev detail —
    // omitted from the inspector to avoid confusion.
    let mut b = b
        .child(row(
            "Target",
            mono(target_label(&track.target), "var(--text-0)", 12.0),
        ))
        .child(row(
            "Property",
            mono(prop_label(&track.target), "var(--text-0)", 12.0),
        ));

    // Sampler (Select: Step / Linear / CubicSpline).
    let sampler = Mutable::new(sampler_key(track.sampler.get()).to_string());
    spawn_local(clone!(sampler => async move {
        let mut first = true;
        sampler.signal_cloned().for_each(move |k| {
            let fire = !first;
            first = false;
            let parsed = sampler_from_key(&k);
            async move {
                if fire {
                    if let Some(clip) = clip_id {
                        dispatch(EditorCommand::SetTrackSampler {
                            clip, track: track_idx, sampler: parsed,
                        });
                    }
                }
            }
        }).await;
    }));
    b = b.child(row(
        "Sampler",
        select(
            sampler,
            vec![
                ("step".into(), "Step".into()),
                ("linear".into(), "Linear".into()),
                ("cubic".into(), "CubicSpline".into()),
            ],
        ),
    ));

    b.child(row(
        "Channels",
        mono(channels_label(&track.target), "var(--text-2)", 11.5),
    ))
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn hint(text: &str) -> Dom {
    html!("div", {
        .style("padding", "14px 14px").style("font-size", "12px").style("color", "var(--text-3)").style("line-height", "1.5")
        .text(text)
    })
}

fn mono(text: impl AsRef<str>, color: &str, size: f64) -> Dom {
    html!("span", {
        .class("mono").style("font-size", &format!("{size}px")).style("color", color)
        .text(text.as_ref())
    })
}

/// The Lucide icon name for a target kind (mirrors the timeline row icons).
fn target_icon(t: &TrackTarget) -> &'static str {
    match t {
        TrackTarget::Transform { .. } | TrackTarget::Morph { .. } => "cube",
        TrackTarget::Uniform { .. } | TrackTarget::BuiltinParam { .. } => "material",
        TrackTarget::Light { .. } => "light",
        TrackTarget::Camera { .. } => "camera",
    }
}

/// A short human label for the target object — resolves the scene node's /
/// material's name (shared with the dope sheet).
fn target_label(t: &TrackTarget) -> String {
    super::timeline::target_label(t)
}

/// The property this track drives.
fn prop_label(t: &TrackTarget) -> String {
    match t {
        TrackTarget::Transform { prop, .. } => format!("{prop:?}").to_lowercase(),
        TrackTarget::Morph { index, .. } => format!("morph {index}"),
        TrackTarget::Uniform { name, .. } => name.clone(),
        TrackTarget::BuiltinParam { param, .. } => format!("{param:?}").to_lowercase(),
        TrackTarget::Light { param, .. } => format!("{param:?}").to_lowercase(),
        TrackTarget::Camera { param, .. } => format!("{param:?}").to_lowercase(),
    }
}

/// The per-value channel names (shared with the dope sheet's expanded lane).
fn channels_label(t: &TrackTarget) -> String {
    super::timeline::channels_label(t)
}

/// The NumField step for a track's value field.
fn value_step(t: &TrackTarget) -> f64 {
    match t {
        TrackTarget::Transform { .. } => 0.05,
        _ => 0.05,
    }
}

fn interp_key(i: Interp) -> &'static str {
    match i {
        Interp::Step => "step",
        Interp::Linear => "linear",
        Interp::Cubic => "cubic",
    }
}

fn interp_from_key(k: &str) -> Interp {
    match k {
        "step" => Interp::Step,
        "cubic" => Interp::Cubic,
        _ => Interp::Linear,
    }
}

fn sampler_key(s: SamplerKind) -> &'static str {
    match s {
        SamplerKind::Step => "step",
        SamplerKind::Linear => "linear",
        SamplerKind::Cubic => "cubic",
    }
}

fn sampler_from_key(k: &str) -> SamplerKind {
    match k {
        "step" => SamplerKind::Step,
        "cubic" => SamplerKind::Cubic,
        _ => SamplerKind::Linear,
    }
}

fn dispatch(cmd: EditorCommand) {
    spawn_local(async move {
        let _ = controller().dispatch(cmd).await;
    });
}
