//! Timeline **Curve / Graph editor** body (anim-curves.jsx `CurvesView`): the
//! freeze-pane graph view — a sticky-left channel list + pinned value axis, then
//! a scrollable SVG graph that shares the dock geometry (`Geo`) + playhead with
//! the Dope Sheet.
//!
//! Each track expands into one (scalar) or three (vec3 / rotation) display
//! **channels**. Vec3 tracks plot each component; rotation tracks plot an
//! **Euler-projection** (XYZ degrees) that is *bit-WYSIWYG with the GPU* (§10):
//! the quaternion curve is densely sampled (slerp between adjacent quat keys),
//! each sample converted quat→euler with continuity unwrapping so the curve never
//! jumps ±360° across a gimbal flip.
//!
//! Load-bearing rule (§0.2): every keyframe edit (dot drag = time+value, tangent
//! handle = in/out slope) dispatches `EditorCommand::SetKeyframe`; selection
//! dispatches `SetAnimSelection`. The hidden-channels set + the focused component
//! are pure view chrome (local `Mutable`s).

use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use glam::{EulerRot, Quat};

use crate::controller::animation::{
    AnimSel, CustomAnimation, Interp, Keyframe, Track, TrackValue, TransformProp,
};
use crate::controller::EditorCommand;
use crate::engine::scene::AssetId;
use crate::prelude::*;

use super::{Geo, NAMES_W};

/// Pinned value-axis width inside the freeze pane (mirrors the JSX).
const VALUE_AXIS_W: f64 = 42.0;
/// The graph height (the scroller above provides the outer overflow).
const GRAPH_H: f64 = 320.0;
/// Vertical padding inside the graph for the value-fit (px).
const PAD_Y: f64 = 16.0;
/// Channel-list row height.
const ROW_H: f64 = 24.0;
/// Tangent-handle line length (px).
const TAN_LEN: f64 = 38.0;
/// Quat sample resolution between keys (px) for the Euler-projection curve.
const QUAT_STEP_PX: f64 = 4.0;

/// One display channel = a track + a component index (0 for scalar tracks;
/// 0/1/2 = X/Y/Z for vec3 + rotation tracks).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct ChannelKey {
    track: usize,
    comp: usize,
}

/// A flattened channel descriptor (track + component + its label/color/arity).
#[derive(Clone)]
struct Channel {
    key: ChannelKey,
    label: String,
    name: String,
    color: String,
    /// Whether the underlying track is a rotation (Quat) track (Euler-projected).
    rotation: bool,
}

/// The Curve editor body. `clip` + `geo` mirror `dope::render`'s call convention
/// (the dock invokes this exactly like the Dope body).
pub fn render(clip: Arc<CustomAnimation>, geo: Geo) -> Dom {
    // Local view chrome: hidden channels + the focused component (within the
    // selected track). Selection itself stays controller state.
    let hidden: Mutable<HashSet<ChannelKey>> = Mutable::new(HashSet::new());
    let focus_comp: Mutable<usize> = Mutable::new(0);

    html!("div", {
        // Rebuild the whole view whenever the track set changes (channels list).
        .child_signal(clip.tracks.signal_vec_cloned().to_signal_cloned().map(
            clone!(clip, hidden, focus_comp => move |_tracks| {
                Some(view(clip.clone(), geo, hidden.clone(), focus_comp.clone()))
            }),
        ))
    })
}

/// The two-pane body: sticky-left (channel list + value axis) and the SVG graph.
fn view(
    clip: Arc<CustomAnimation>,
    geo: Geo,
    hidden: Mutable<HashSet<ChannelKey>>,
    focus_comp: Mutable<usize>,
) -> Dom {
    let channels = collect_channels(&clip);

    html!("div", {
        .style("display", "flex")
        // ── sticky left: channel list + pinned value axis ─────────────────────
        .child(html!("div", {
            .style("position", "sticky").style("left", "0").style("z-index", "5")
            .style("width", &format!("{NAMES_W}px")).style("flex", "0 0 auto")
            .style("display", "flex").style("background", "var(--bg-1)")
            .style("border-right", "1px solid var(--line)").style("height", &format!("{GRAPH_H}px"))
            // channel list
            .child(html!("div", {
                .style("width", &format!("{}px", NAMES_W - VALUE_AXIS_W))
                .style("overflow-y", "auto")
                .children(channels.iter().cloned().map(clone!(hidden, focus_comp => move |ch| {
                    channel_row(ch, hidden.clone(), focus_comp.clone())
                })))
            }))
            // value axis (rebuilt reactively from the shown channels' range)
            .child_signal(value_axis_signal(clip.clone(), channels.clone(), hidden.clone()))
        }))
        // ── graph ─────────────────────────────────────────────────────────────
        .child(graph(clip, geo, channels, hidden, focus_comp))
    })
}

/// One channel-list row: a show/hide color square + `{target} {channel}` label.
fn channel_row(
    ch: Channel,
    hidden: Mutable<HashSet<ChannelKey>>,
    focus_comp: Mutable<usize>,
) -> Dom {
    let key = ch.key;
    let color = ch.color.clone();
    // Selected when this channel's track is the selection AND its component is
    // the focused one (single-track focus → component focus is local chrome).
    let selected = sel_focus_signal(key, focus_comp.clone());
    let off_sig = hidden.signal_ref(move |h| h.contains(&key));

    html!("div", {
        .class("t")
        .style("display", "flex").style("align-items", "center").style("gap", "7px")
        .style("height", &format!("{ROW_H}px")).style("padding", "0 8px")
        .style("cursor", "pointer").style("border-bottom", "1px solid var(--line-soft)")
        .style_signal("background", selected.map(|on| if on { "var(--accent-ghost)" } else { "transparent" }))
        .style_signal("opacity", off_sig.map(|off| if off { "0.4" } else { "1" }))
        .event(clone!(focus_comp => move |_: events::Click| {
            focus_comp.set_neq(key.comp);
            dispatch(EditorCommand::SetAnimSelection {
                sel: Some(AnimSel { track: key.track, keyframe: None }),
            });
        }))
        // show/hide color square
        .child(html!("button", {
            .class("t")
            .style("width", "9px").style("height", "9px").style("border-radius", "2px")
            .style("border", "1px solid transparent").style("cursor", "pointer").style("flex", "0 0 auto")
            .style("padding", "0")
            .style_signal("background", hidden.signal_ref(clone!(color => move |h| {
                if h.contains(&key) { "var(--text-3)".to_string() } else { color.clone() }
            })))
            .event(clone!(hidden => move |e: events::Click| {
                e.stop_propagation();
                let mut h = hidden.lock_mut();
                if !h.insert(key) { h.remove(&key); }
            }))
        }))
        .child(html!("span", {
            .class("mono")
            .style("font-size", "10.5px").style("color", "var(--text-1)")
            .style("white-space", "nowrap").style("overflow", "hidden").style("text-overflow", "ellipsis")
            .style("flex", "1").style("min-width", "0")
            .text(&ch.label)
        }))
        .child(html!("span", {
            .class("mono").style("font-size", "10px").style("color", "var(--text-3)")
            .text(&ch.name)
        }))
    })
}

/// The pinned value axis: rebuilt reactively whenever any shown channel's keys
/// change (so the auto-fit range tracks edits).
fn value_axis_signal(
    clip: Arc<CustomAnimation>,
    channels: Vec<Channel>,
    hidden: Mutable<HashSet<ChannelKey>>,
) -> impl Signal<Item = Option<Dom>> {
    // React on the hidden set + every shown track's keys (a coarse but correct
    // trigger: rebuild on any keyframe edit).
    let keys_versions = keys_version_signal(clip.clone());
    map_ref! {
        let h = hidden.signal_cloned(),
        let _v = keys_versions => {
            Some(value_axis(&clip, &channels, h))
        }
    }
}

fn value_axis(
    clip: &Arc<CustomAnimation>,
    channels: &[Channel],
    hidden: &HashSet<ChannelKey>,
) -> Dom {
    let (mn, mx) = value_range(clip, channels, hidden);
    let ticks = value_ticks(mn, mx);
    let y_of = move |v: f64| value_to_y(v, mn, mx);

    svg_axis(ticks, y_of)
}

/// The value-axis DOM (HTML labels positioned at each tick).
fn svg_axis(ticks: Vec<f64>, y_of: impl Fn(f64) -> f64 + 'static) -> Dom {
    html!("div", {
        .style("width", &format!("{VALUE_AXIS_W}px")).style("position", "relative")
        .style("border-left", "1px solid var(--line-soft)").style("background", "var(--bg-2)")
        .children(ticks.into_iter().map(move |v| {
            html!("span", {
                .class("mono")
                .style("position", "absolute").style("right", "4px")
                .style("top", &format!("{}px", y_of(v) - 6.0))
                .style("font-size", "9px").style("color", "var(--text-3)")
                .text(&fmt_num(v))
            })
        }))
    })
}

/// The scrollable SVG graph: gridlines, per-channel curves, keyframe dots,
/// tangent handles (focused cubic keys), and the playhead.
fn graph(
    clip: Arc<CustomAnimation>,
    geo: Geo,
    channels: Vec<Channel>,
    hidden: Mutable<HashSet<ChannelKey>>,
    focus_comp: Mutable<usize>,
) -> Dom {
    html!("div", {
        .style("position", "relative")
        .style("width", &format!("{}px", geo.content_w)).style("height", &format!("{GRAPH_H}px"))
        .style("background", "var(--bg-0)")
        // SVG layer (grid + curves) — rebuilt on edits via keys-version + hidden.
        .child_signal({
            let keys_versions = keys_version_signal(clip.clone());
            let clip = clip.clone();
            let channels = channels.clone();
            map_ref! {
                let h = hidden.signal_cloned(),
                let _v = keys_versions => {
                    Some(svg_layer(&clip, geo, &channels, h))
                }
            }
        })
        // Keyframe-dots + tangents overlay (HTML, for easy dragging) — same trigger.
        .child_signal({
            let keys_versions = keys_version_signal(clip.clone());
            let clip = clip.clone();
            let channels = channels.clone();
            map_ref! {
                let h = hidden.signal_cloned(),
                let fc = focus_comp.signal(),
                let _v = keys_versions => {
                    Some(dots_layer(clip.clone(), geo, &channels, h, *fc))
                }
            }
        })
        // playhead line spanning the graph
        .child(graph_playhead(geo))
    })
}

/// The SVG layer: value gridlines (zero emphasized), time gridlines, curve paths.
fn svg_layer(
    clip: &Arc<CustomAnimation>,
    geo: Geo,
    channels: &[Channel],
    hidden: &HashSet<ChannelKey>,
) -> Dom {
    let (mn, mx) = value_range(clip, channels, hidden);
    let y_of = move |v: f64| value_to_y(v, mn, mx);
    let vticks = value_ticks(mn, mx);

    let tracks = clip.tracks.lock_ref();
    let shown = shown_channels(channels, hidden);

    svg!("svg", {
        .attr("width", &geo.content_w.to_string()).attr("height", &GRAPH_H.to_string())
        .attr("style", "display:block;position:absolute;inset:0")
        // value gridlines (zero line emphasized)
        .children(vticks.iter().map(clone!(y_of => move |&v| {
            let zero = v.abs() < 1e-6;
            svg!("line", {
                .attr("x1", "0").attr("x2", &geo.content_w.to_string())
                .attr("y1", &y_of(v).to_string()).attr("y2", &y_of(v).to_string())
                .attr("stroke", if zero { "var(--line)" } else { "var(--line-soft)" })
                .attr("stroke-width", if zero { "1" } else { "0.6" })
            })
        })))
        // time gridlines
        .children(time_ticks(geo).into_iter().map(move |s| {
            let x = geo.time_to_x(s);
            svg!("line", {
                .attr("x1", &x.to_string()).attr("x2", &x.to_string())
                .attr("y1", "0").attr("y2", &GRAPH_H.to_string())
                .attr("stroke", "var(--line-soft)").attr("stroke-width", "0.5")
            })
        }))
        // curve paths (one per shown channel)
        .children(shown.into_iter().filter_map(clone!(y_of => move |ch| {
            let track = tracks.get(ch.key.track)?;
            let focused = is_track_selected(ch.key.track);
            Some(curve_path(track, ch, geo, &y_of, focused))
        })))
    })
}

/// One channel's curve `<path>` (plus a faint fill under it).
fn curve_path(
    track: &Arc<Track>,
    ch: Channel,
    geo: Geo,
    y_of: &impl Fn(f64) -> f64,
    focused: bool,
) -> Dom {
    let samples = sample_channel(track, &ch, geo);
    let d = path_d(&samples, geo, y_of);
    let color = ch.color.clone();

    svg!("g", {
        // faint fill under the curve
        .child(svg!("path", {
            .attr("d", &format!("{d} L {} {GRAPH_H} L 0 {GRAPH_H} Z", geo.time_to_x(geo.dur)))
            .attr("fill", &color).attr("opacity", "0.06").attr("stroke", "none")
        }))
        .child(svg!("path", {
            .attr("d", &d).attr("fill", "none").attr("stroke", &color)
            .attr("stroke-width", if focused { "2" } else { "1.4" })
            .attr("opacity", if focused { "1" } else { "0.85" })
        }))
    })
}

/// Build an SVG path `d` from `(t, value)` samples.
fn path_d(samples: &[(f64, f64)], geo: Geo, y_of: &impl Fn(f64) -> f64) -> String {
    if samples.is_empty() {
        return "M0 0".to_string();
    }
    if samples.len() == 1 {
        let y = y_of(samples[0].1);
        return format!("M0 {y} L {} {y}", geo.content_w);
    }
    let mut d = String::new();
    // flat lead-in from x=0 to the first key
    let (t0, v0) = samples[0];
    d.push_str(&format!(
        "M 0 {} L {} {}",
        y_of(v0),
        geo.time_to_x(t0),
        y_of(v0)
    ));
    for &(t, v) in &samples[1..] {
        d.push_str(&format!(" L {} {}", geo.time_to_x(t), y_of(v)));
    }
    // flat tail-out to content end
    let (_, vl) = samples[samples.len() - 1];
    d.push_str(&format!(" L {} {}", geo.content_w, y_of(vl)));
    d
}

/// The keyframe-dots + tangent-handles overlay (HTML, draggable).
fn dots_layer(
    clip: Arc<CustomAnimation>,
    geo: Geo,
    channels: &[Channel],
    hidden: &HashSet<ChannelKey>,
    focus_comp: usize,
) -> Dom {
    let (mn, mx) = value_range(&clip, channels, hidden);
    let y_of = move |v: f64| value_to_y(v, mn, mx);
    let v_of = move |y: f64| y_to_value(y, mn, mx);

    let shown = shown_channels(channels, hidden);
    let tracks_ref = clip.tracks.lock_ref();
    let mut doms: Vec<Dom> = Vec::new();
    for ch in shown {
        let Some(track) = tracks_ref.get(ch.key.track) else {
            continue;
        };
        let times = track.times.get_cloned();
        let keys = track.keys.get_cloned();
        // The channel is "focused" (gets tangent handles) when its track is the
        // selection AND its component is the focused one.
        let focused = is_track_selected(ch.key.track) && ch.key.comp == focus_comp;
        for (ki, (&t, key)) in times.iter().zip(keys.iter()).enumerate() {
            let v = display_value(&ch, key);
            // tangent handles for the focused channel's cubic keys
            if focused && matches!(key.interp, Interp::Cubic) {
                doms.push(tangents(
                    clip.id,
                    geo,
                    ch.clone(),
                    ki,
                    t,
                    v,
                    key,
                    &y_of,
                    &v_of,
                ));
            }
            doms.push(dot(clip.id, geo, ch.clone(), ki, t, v, key, &y_of, &v_of));
        }
    }

    html!("div", {
        .style("position", "absolute").style("inset", "0")
        .children(doms)
    })
}

/// One keyframe dot: square=step, diamond(rotated)=linear, circle=cubic; drag
/// moves it in (time, value) → `SetKeyframe { t, value }`.
#[allow(clippy::too_many_arguments)]
fn dot(
    clip: AssetId,
    geo: Geo,
    ch: Channel,
    key_idx: usize,
    t: f64,
    v: f64,
    key: &Keyframe,
    y_of: &impl Fn(f64) -> f64,
    v_of: &(impl Fn(f64) -> f64 + Clone + 'static),
) -> Dom {
    let x = geo.time_to_x(t);
    let y = y_of(v);
    let track_idx = ch.key.track;
    let color = ch.color.clone();
    let interp = key.interp;
    let key = key.clone();
    let v_of = v_of.clone();

    // shape by interp
    let (radius, transform) = match interp {
        Interp::Step => ("1px", "none"),
        Interp::Linear => ("0", "rotate(45deg)"),
        Interp::Cubic => ("50%", "none"),
    };
    // Captured graph (left, top) viewport coords for the active drag.
    let origin: Rc<Cell<Option<(f64, f64)>>> = Rc::new(Cell::new(None));

    html!("div" => web_sys::HtmlElement, {
        .style("position", "absolute")
        .style("left", &format!("{}px", x - 5.0)).style("top", &format!("{}px", y - 5.0))
        .style("width", "10px").style("height", "10px")
        .style("border-radius", radius).style("transform", transform)
        .style("cursor", "move")
        .style_signal("z-index", sel_signal(track_idx, Some(key_idx)).map(|s| if s { "7" } else { "5" }))
        .style_signal("background", sel_signal(track_idx, Some(key_idx)).map(clone!(color => move |s| {
            if s { "var(--text-0)".to_string() } else { color.clone() }
        })))
        .style_signal("border", sel_signal(track_idx, Some(key_idx)).map(clone!(color => move |s| {
            if s {
                "1.5px solid var(--accent-bright)".to_string()
            } else {
                format!("1.5px solid color-mix(in oklch, {color} 55%, black)")
            }
        })))
        .style_signal("box-shadow", sel_signal(track_idx, Some(key_idx)).map(|s| if s { "0 0 0 2px var(--accent-ghost)" } else { "none" }))
        // mousedown: select this key + capture the graph (left, top) for the drag.
        .with_node!(el => {
            .event(clone!(origin => move |e: events::MouseDown| {
                e.stop_propagation();
                dispatch(EditorCommand::SetAnimSelection {
                    sel: Some(AnimSel { track: track_idx, keyframe: Some(key_idx) }),
                });
                // The dots overlay (the dot's offset parent) anchors both axes:
                // its left edge == graph x=0, its top edge == graph y=0.
                if let Some(p) = el.parent_element() {
                    let r = p.get_bounding_client_rect();
                    origin.set(Some((r.left(), r.top())));
                }
            }))
        })
        .global_event(clone!(origin, key, v_of, ch => move |e: events::MouseMove| {
            if let Some((left, top)) = origin.get() {
                let nt = geo.x_to_time(e.x() - left).clamp(0.0, geo.dur);
                let new_v = v_of(e.y() - top);
                let new_value = write_value(&ch, &key, new_v);
                dispatch(EditorCommand::SetKeyframe {
                    clip,
                    track: track_idx,
                    index: key_idx,
                    t: Some((nt * 1e4).round() / 1e4),
                    value: Some(new_value),
                    interp: None,
                    in_tangent: None,
                    out_tangent: None,
                });
            }
        }))
        .global_event(clone!(origin => move |_: events::MouseUp| {
            origin.set(None);
        }))
    })
}

/// Tangent handles for one focused cubic key: a line through the key with two
/// endpoint handles; dragging an endpoint sets that key's in/out tangent slope.
#[allow(clippy::too_many_arguments)]
fn tangents(
    clip: AssetId,
    geo: Geo,
    ch: Channel,
    key_idx: usize,
    t: f64,
    v: f64,
    key: &Keyframe,
    y_of: &impl Fn(f64) -> f64,
    _v_of: &impl Fn(f64) -> f64,
) -> Dom {
    let x = geo.time_to_x(t);
    let y = y_of(v);
    // px-per-value-unit (positive scale).
    let ppv = (y_of(0.0) - y_of(1.0)).abs().max(1.0);

    let out_slope = display_tangent(&ch, &key.out_tangent);
    let in_slope = display_tangent(&ch, &key.in_tangent);
    let out_pt = handle_point(x, y, out_slope, 1.0, geo.px_per_sec, ppv);
    let in_pt = handle_point(x, y, in_slope, -1.0, geo.px_per_sec, ppv);

    let key = key.clone();
    let ch_out = ch.clone();
    let key_out = key.clone();
    let ch_in = ch.clone();
    let key_in = key;

    html!("div", {
        .style("position", "absolute").style("left", "0").style("top", "0")
        .style("pointer-events", "none")
        // the connecting line
        .child(svg!("svg", {
            .attr("style", "position:absolute;left:0;top:0;overflow:visible;pointer-events:none;z-index:6")
            .attr("width", "1").attr("height", "1")
            .child(svg!("line", {
                .attr("x1", &in_pt.0.to_string()).attr("y1", &in_pt.1.to_string())
                .attr("x2", &out_pt.0.to_string()).attr("y2", &out_pt.1.to_string())
                .attr("stroke", "var(--accent-line)").attr("stroke-width", "1")
            }))
        }))
        .child(tangent_handle(clip, geo, ch_out, key_idx, x, y, ppv, out_pt, true, key_out))
        .child(tangent_handle(clip, geo, ch_in, key_idx, x, y, ppv, in_pt, false, key_in))
    })
}

/// One tangent endpoint handle. Drag → recompute the slope from the cursor
/// position relative to the key, then dispatch `SetKeyframe { in/out_tangent }`.
#[allow(clippy::too_many_arguments)]
fn tangent_handle(
    clip: AssetId,
    geo: Geo,
    ch: Channel,
    key_idx: usize,
    kx: f64,
    ky: f64,
    ppv: f64,
    pt: (f64, f64),
    is_out: bool,
    key: Keyframe,
) -> Dom {
    let track_idx = ch.key.track;
    // Captured graph (left, top) viewport coords for the active drag.
    let origin: Rc<Cell<Option<(f64, f64)>>> = Rc::new(Cell::new(None));

    html!("div" => web_sys::HtmlElement, {
        .style("position", "absolute")
        .style("left", &format!("{}px", pt.0 - 3.5)).style("top", &format!("{}px", pt.1 - 3.5))
        .style("width", "7px").style("height", "7px").style("border-radius", "50%")
        .style("background", "var(--accent-bright)").style("border", "1px solid var(--bg-0)")
        .style("cursor", "crosshair").style("z-index", "7").style("pointer-events", "auto")
        .with_node!(el => {
            .event(clone!(origin => move |e: events::MouseDown| {
                e.stop_propagation();
                // The dots overlay is the offset parent (left/top viewport edges).
                if let Some(p) = el.parent_element().and_then(|p| p.parent_element()) {
                    let r = p.get_bounding_client_rect();
                    origin.set(Some((r.left(), r.top())));
                }
            }))
        })
        .global_event(clone!(origin, ch, key => move |e: events::MouseMove| {
            if let Some((left, top)) = origin.get() {
                let px = e.x() - left;
                let py = e.y() - top;
                // slope (value-units per second) from the cursor offset to the key.
                // (Both handles compute the same `dval/dsec`; the in-handle sits on
                // the opposite side, so its slope reads correctly without negation.)
                let dsec = (px - kx) / geo.px_per_sec;
                let dval = (py - ky) / -ppv;
                let slope = if dsec.abs() > 1e-3 { dval / dsec } else { 0.0 };
                let slope = (slope * 1e3).round() / 1e3;
                let tan_value = write_tangent(&ch, &key, slope);
                let mut cmd = EditorCommand::SetKeyframe {
                    clip, track: track_idx, index: key_idx,
                    t: None, value: None, interp: None,
                    in_tangent: None, out_tangent: None,
                };
                if let EditorCommand::SetKeyframe { in_tangent, out_tangent, .. } = &mut cmd {
                    if is_out { *out_tangent = Some(tan_value); } else { *in_tangent = Some(tan_value); }
                }
                dispatch(cmd);
            }
        }))
        .global_event(clone!(origin => move |_: events::MouseUp| {
            origin.set(None);
        }))
    })
}

/// The graph playhead line (left = `t * px_per_sec`, the graph starts at x=0).
fn graph_playhead(geo: Geo) -> Dom {
    html!("div", {
        .style("position", "absolute").style("top", "0").style("bottom", "0")
        .style("width", "1.5px").style("background", "var(--accent-bright)")
        .style("z-index", "6").style("pointer-events", "none")
        .style("box-shadow", "0 0 0 0.5px oklch(0 0 0 / 0.3)")
        .style_signal("left", controller().playhead.signal().map(move |t| {
            format!("{}px", geo.time_to_x(t))
        }))
    })
}

// ── channel model ─────────────────────────────────────────────────────────────

/// Flatten the clip's tracks into display channels (1 scalar, or 3 X/Y/Z).
fn collect_channels(clip: &Arc<CustomAnimation>) -> Vec<Channel> {
    const COMP_NAMES: [&str; 3] = ["x", "y", "z"];
    const COMP_COLORS: [&str; 3] = ["var(--axis-x)", "var(--axis-y)", "var(--axis-z)"];

    let mut out = Vec::new();
    for (ti, track) in clip.tracks.lock_ref().iter().enumerate() {
        let label = super::target_label(&track.target);
        let prop = super::prop_label(&track.target);
        match channel_arity(track) {
            Arity::Scalar => out.push(Channel {
                key: ChannelKey { track: ti, comp: 0 },
                label,
                name: prop,
                color: "var(--accent)".to_string(),
                rotation: false,
            }),
            Arity::Vec3 { rotation } => {
                for c in 0..3 {
                    out.push(Channel {
                        key: ChannelKey { track: ti, comp: c },
                        label: label.clone(),
                        name: format!("{prop} {}", COMP_NAMES[c]),
                        color: COMP_COLORS[c].to_string(),
                        rotation,
                    });
                }
            }
        }
    }
    out
}

enum Arity {
    Scalar,
    Vec3 { rotation: bool },
}

/// The display arity of a track (from its first key's value shape, falling back
/// to the target kind for empty tracks).
fn channel_arity(track: &Arc<Track>) -> Arity {
    if let Some(k) = track.keys.lock_ref().first() {
        return match k.value {
            TrackValue::Scalar(_) => Arity::Scalar,
            TrackValue::Vec3(_) => Arity::Vec3 { rotation: false },
            TrackValue::Quat(_) => Arity::Vec3 { rotation: true },
        };
    }
    // empty track: infer from the target
    match &track.target {
        crate::controller::animation::TrackTarget::Transform { prop, .. } => match prop {
            TransformProp::Rotation => Arity::Vec3 { rotation: true },
            TransformProp::Translation | TransformProp::Scale => Arity::Vec3 { rotation: false },
        },
        _ => Arity::Scalar,
    }
}

/// The channels currently shown (not hidden).
fn shown_channels(channels: &[Channel], hidden: &HashSet<ChannelKey>) -> Vec<Channel> {
    channels
        .iter()
        .filter(|c| !hidden.contains(&c.key))
        .cloned()
        .collect()
}

// ── sampling (display-only; mirrors the renderer, never calls it) ─────────────

/// Sample a channel into `(t, value)` points across the timeline. Step/Linear get
/// the exact key polyline; Cubic samples hermite densely. Rotation channels are
/// Euler-projected (dense quat slerp → euler XYZ degrees with unwrapping).
fn sample_channel(track: &Arc<Track>, ch: &Channel, geo: Geo) -> Vec<(f64, f64)> {
    let times = track.times.get_cloned();
    let keys = track.keys.get_cloned();
    if times.is_empty() || keys.len() != times.len() {
        return Vec::new();
    }
    if ch.rotation {
        return sample_rotation(&times, &keys, ch.key.comp, geo);
    }
    if times.len() == 1 {
        return vec![(times[0], scalar_at(&keys[0], ch.key.comp))];
    }

    let mut out: Vec<(f64, f64)> = Vec::new();
    out.push((times[0], scalar_at(&keys[0], ch.key.comp)));
    for i in 0..times.len() - 1 {
        let (ta, tb) = (times[i], times[i + 1]);
        let va = scalar_at(&keys[i], ch.key.comp);
        let vb = scalar_at(&keys[i + 1], ch.key.comp);
        match keys[i].interp {
            Interp::Step => {
                out.push((tb, va));
                out.push((tb, vb));
            }
            Interp::Linear => out.push((tb, vb)),
            Interp::Cubic => {
                // dense hermite sampling between the two keys
                let (xa, xb) = (geo.time_to_x(ta), geo.time_to_x(tb));
                let m0 = scalar_at_tan(&keys[i].out_tangent, ch.key.comp);
                let m1 = scalar_at_tan(&keys[i + 1].in_tangent, ch.key.comp);
                let dt = (tb - ta).max(1e-6);
                let mut x = xa + QUAT_STEP_PX;
                while x < xb {
                    let tt = geo.x_to_time(x);
                    let s = ((tt - ta) / dt).clamp(0.0, 1.0);
                    out.push((tt, hermite(va, vb, m0 * dt, m1 * dt, s)));
                    x += QUAT_STEP_PX;
                }
                out.push((tb, vb));
            }
        }
    }
    out
}

/// Sample a rotation track's Euler-projection curve for component `comp` (degrees),
/// densely slerping between adjacent quat keys with continuity unwrapping.
///
/// NOTE (M-A4 approximation): `Cubic` rotation segments are approximated by
/// **slerp** (same as Linear) — a true cubic-quaternion (squad) is deferred.
fn sample_rotation(times: &[f64], keys: &[Keyframe], comp: usize, geo: Geo) -> Vec<(f64, f64)> {
    let quats: Vec<Quat> = keys.iter().map(quat_of).collect();
    if quats.len() == 1 {
        let e = quat_to_euler_deg(quats[0]);
        return vec![(times[0], e[comp] as f64)];
    }
    let mut out: Vec<(f64, f64)> = Vec::new();
    // running "previous euler" for per-axis unwrapping (start from the first key).
    let mut prev = quat_to_euler_deg(quats[0]);
    out.push((times[0], prev[comp] as f64));
    for i in 0..times.len() - 1 {
        let (ta, tb) = (times[i], times[i + 1]);
        let (xa, xb) = (geo.time_to_x(ta), geo.time_to_x(tb));
        let step = keys[i].interp == Interp::Step;
        let dt = (tb - ta).max(1e-6);
        let mut x = xa + QUAT_STEP_PX;
        while x < xb {
            let tt = geo.x_to_time(x);
            let s = ((tt - ta) / dt).clamp(0.0, 1.0) as f32;
            let q = if step {
                quats[i]
            } else {
                quats[i].slerp(quats[i + 1], s)
            };
            let e = unwrap_euler(quat_to_euler_deg(q), prev);
            prev = e;
            out.push((tt, e[comp] as f64));
            x += QUAT_STEP_PX;
        }
        // land exactly on the next key
        let e = unwrap_euler(quat_to_euler_deg(quats[i + 1]), prev);
        prev = e;
        out.push((tb, e[comp] as f64));
    }
    out
}

/// The displayed scalar value of a channel at one keyframe (for dot positions).
fn display_value(ch: &Channel, key: &Keyframe) -> f64 {
    if ch.rotation {
        let e = quat_to_euler_deg(quat_of(key));
        return e[ch.key.comp] as f64;
    }
    scalar_at(key, ch.key.comp)
}

/// A keyframe's component value (scalar tracks ignore `comp`).
fn scalar_at(key: &Keyframe, comp: usize) -> f64 {
    match key.value {
        TrackValue::Scalar(s) => s as f64,
        TrackValue::Vec3(v) => v[comp.min(2)] as f64,
        TrackValue::Quat(q) => q[comp.min(3)] as f64,
    }
}

/// A tangent value's component (for cubic hermite display).
fn scalar_at_tan(tan: &TrackValue, comp: usize) -> f64 {
    match tan {
        TrackValue::Scalar(s) => *s as f64,
        TrackValue::Vec3(v) => v[comp.min(2)] as f64,
        TrackValue::Quat(q) => q[comp.min(3)] as f64,
    }
}

/// The displayed tangent slope for a channel's in/out tangent (rotation tracks
/// report 0 — quat tangents have no scalar-degree slope; handles edit nothing
/// meaningful, so they read flat).
fn display_tangent(ch: &Channel, tan: &TrackValue) -> f64 {
    if ch.rotation {
        return 0.0;
    }
    scalar_at_tan(tan, ch.key.comp)
}

/// Cubic hermite scalar interpolation on `[0,1]` (m0/m1 are per-unit-s slopes
/// pre-multiplied by `dt`).
fn hermite(p0: f64, p1: f64, m0: f64, m1: f64, s: f64) -> f64 {
    let s2 = s * s;
    let s3 = s2 * s;
    (2.0 * s3 - 3.0 * s2 + 1.0) * p0
        + (s3 - 2.0 * s2 + s) * m0
        + (-2.0 * s3 + 3.0 * s2) * p1
        + (s3 - s2) * m1
}

// ── write-back ────────────────────────────────────────────────────────────────

/// Build the new `TrackValue` for a dot drag: scalar/vec3 set the component;
/// rotation converts the edited euler triple back to a quat.
fn write_value(ch: &Channel, key: &Keyframe, new_v: f64) -> TrackValue {
    match key.value {
        TrackValue::Scalar(_) => TrackValue::Scalar(new_v as f32),
        TrackValue::Vec3(mut v) => {
            v[ch.key.comp.min(2)] = new_v as f32;
            TrackValue::Vec3(v)
        }
        TrackValue::Quat(_) => {
            // current euler, replace the dragged component, back to quat.
            let mut e = quat_to_euler_deg(quat_of(key));
            e[ch.key.comp.min(2)] = new_v as f32;
            let q = Quat::from_euler(
                EulerRot::XYZ,
                e[0].to_radians(),
                e[1].to_radians(),
                e[2].to_radians(),
            );
            TrackValue::Quat(q.to_array())
        }
    }
}

/// Build the new tangent `TrackValue` for a tangent-handle drag (scalar/vec3
/// only; rotation tangents are left untouched — return the existing one).
fn write_tangent(ch: &Channel, key: &Keyframe, slope: f64) -> TrackValue {
    match key.out_tangent {
        TrackValue::Scalar(_) => TrackValue::Scalar(slope as f32),
        TrackValue::Vec3(mut v) => {
            v[ch.key.comp.min(2)] = slope as f32;
            TrackValue::Vec3(v)
        }
        // rotation: no scalar slope; echo the current tangent unchanged.
        TrackValue::Quat(q) => TrackValue::Quat(q),
    }
}

// ── euler projection (§10) ────────────────────────────────────────────────────

fn quat_of(key: &Keyframe) -> Quat {
    match key.value {
        TrackValue::Quat(q) => Quat::from_array(q).normalize(),
        _ => Quat::IDENTITY,
    }
}

/// Convert a quat → Euler XYZ degrees `[x,y,z]`.
fn quat_to_euler_deg(q: Quat) -> [f32; 3] {
    let (x, y, z) = q.to_euler(EulerRot::XYZ);
    [x.to_degrees(), y.to_degrees(), z.to_degrees()]
}

/// Per-axis continuity unwrapping: shift each axis by ±360° to minimize the
/// delta from the previous sample (avoids ±360 jumps / gimbal flips).
fn unwrap_euler(mut e: [f32; 3], prev: [f32; 3]) -> [f32; 3] {
    for i in 0..3 {
        while e[i] - prev[i] > 180.0 {
            e[i] -= 360.0;
        }
        while e[i] - prev[i] < -180.0 {
            e[i] += 360.0;
        }
    }
    e
}

// ── value range + ticks ───────────────────────────────────────────────────────

/// Auto-fit the value range across the shown channels (with ~18% padding).
fn value_range(
    clip: &Arc<CustomAnimation>,
    channels: &[Channel],
    hidden: &HashSet<ChannelKey>,
) -> (f64, f64) {
    let tracks = clip.tracks.lock_ref();
    let (mut mn, mut mx) = (f64::INFINITY, f64::NEG_INFINITY);
    for ch in channels.iter().filter(|c| !hidden.contains(&c.key)) {
        if let Some(track) = tracks.get(ch.key.track) {
            for key in track.keys.lock_ref().iter() {
                let v = display_value(ch, key);
                mn = mn.min(v);
                mx = mx.max(v);
            }
        }
    }
    if !mn.is_finite() || !mx.is_finite() {
        mn = -1.0;
        mx = 1.0;
    }
    if mx - mn < 1e-6 {
        mn -= 1.0;
        mx += 1.0;
    }
    let pad = (mx - mn) * 0.18;
    (mn - pad, mx + pad)
}

fn value_to_y(v: f64, mn: f64, mx: f64) -> f64 {
    PAD_Y + (1.0 - (v - mn) / (mx - mn)) * (GRAPH_H - 2.0 * PAD_Y)
}
fn y_to_value(y: f64, mn: f64, mx: f64) -> f64 {
    mn + (1.0 - (y - PAD_Y) / (GRAPH_H - 2.0 * PAD_Y)) * (mx - mn)
}

/// "Nice" value tick step targeting ~5 divisions over the span.
fn value_ticks(mn: f64, mx: f64) -> Vec<f64> {
    let step = nice_step(((mx - mn) / 5.0).max(1e-9));
    let mut out = Vec::new();
    let mut s = (mn / step).ceil() * step;
    while s <= mx {
        out.push((s * 1e4).round() / 1e4);
        s += step;
    }
    out
}

fn nice_step(raw: f64) -> f64 {
    let p = 10f64.powf(raw.max(1e-9).log10().floor());
    let n = raw / p;
    let m = if n < 1.5 {
        1.0
    } else if n < 3.0 {
        2.0
    } else if n < 7.0 {
        5.0
    } else {
        10.0
    };
    m * p
}

/// Time gridlines (coarser than the ruler ticks; mirrors the JSX `timeTicks`).
fn time_ticks(geo: Geo) -> Vec<f64> {
    let step = if geo.dur > 4.0 {
        1.0
    } else if geo.dur > 1.5 {
        0.5
    } else {
        0.25
    };
    let mut out = Vec::new();
    let mut s = 0.0;
    while s <= geo.dur + 1e-6 {
        out.push(s);
        s += step;
    }
    out
}

fn fmt_num(v: f64) -> String {
    let a = v.abs();
    if a >= 100.0 {
        format!("{v:.0}")
    } else if a >= 1.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.2}")
    }
}

/// Endpoint of a tangent handle: a normalized px vector of length `TAN_LEN` from
/// the key, in the `sign` direction along time.
fn handle_point(x: f64, y: f64, slope: f64, sign: f64, px_per_sec: f64, ppv: f64) -> (f64, f64) {
    let mut vx = sign * px_per_sec;
    let mut vy = -slope * ppv;
    let len = vx.hypot(vy).max(1.0);
    vx = vx / len * TAN_LEN;
    vy = vy / len * TAN_LEN;
    (x + vx, y + vy)
}

// ── selection helpers ─────────────────────────────────────────────────────────

/// Is this (track, keyframe?) the current selection?
fn sel_signal(track_idx: usize, keyframe: Option<usize>) -> impl Signal<Item = bool> {
    controller().anim_selection.signal().map(move |sel| {
        sel == Some(AnimSel {
            track: track_idx,
            keyframe,
        })
    })
}

/// Is this channel the focused one (its track selected AND its component focused)?
fn sel_focus_signal(key: ChannelKey, focus_comp: Mutable<usize>) -> impl Signal<Item = bool> {
    map_ref! {
        let sel = controller().anim_selection.signal(),
        let fc = focus_comp.signal() => {
            sel.map(|s| s.track) == Some(key.track) && *fc == key.comp
        }
    }
}

/// Synchronous: is this track the current selection (any keyframe)?
fn is_track_selected(track_idx: usize) -> bool {
    controller()
        .anim_selection
        .get()
        .map(|s| s.track == track_idx)
        .unwrap_or(false)
}

/// A signal that fires whenever any track's keys/times change — used to rebuild
/// the value-dependent layers (curves, dots, value axis) on edits.
fn keys_version_signal(clip: Arc<CustomAnimation>) -> impl Signal<Item = usize> {
    clip.tracks
        .signal_vec_cloned()
        .map_signal(|track| {
            map_ref! {
                let t = track.times.signal_cloned(),
                let k = track.keys.signal_cloned() => t.len().wrapping_add(k.len())
            }
        })
        .to_signal_map(|lens| lens.iter().fold(0usize, |a, b| a.wrapping_add(*b)))
}

fn dispatch(cmd: EditorCommand) {
    spawn_local(async move {
        let _ = controller().dispatch(cmd).await;
    });
}
