//! Timeline **Mixer / NLA** body: the freeze-pane
//! layer stack — a sticky-left layer list (name · REPLACE/ADD mode · weight
//! slider · bone-mask · additive base-clip) + a scrollable lanes area where each
//! layer's clip **strips** are placed, trimmed, and repeat-toggled.
//!
//! Strips reference clips by `AssetId`; the strip header resolves the clip's
//! name + color from `controller().custom_animations`. The Mixer timeline length
//! = `max(strip.start + strip.len)` (a local [`mixer_duration`] helper), so the
//! lanes can be longer than the active clip — but the shared ruler/playhead still
//! apply.
//!
//! Load-bearing rule: every edit is an `EditorCommand` dispatched through
//! the one `EditorController` (`AddLayer` · `SetLayerMode` · `SetLayerWeight` ·
//! `SetLayerMask` · `AddStrip` · `MoveStrip` · `TrimStrip` · `SetStripRepeat` …).
//! Only drag-preview anchoring lives in local `Cell`s. The renderer mixer +
//! lowering already exist (engine/bridge/animation_sync) — this view only edits
//! the `anim_mixer` doc; sync + GPU playback come for free.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use crate::controller::animation::{
    find_clip, CustomAnimation, LayerDoc, LayerModeDoc, MixerDoc, StripDoc,
};
use crate::controller::EditorCommand;
use crate::engine::scene::AssetId;
use crate::prelude::*;

use super::{Geo, NAMES_W};

/// Per-layer lane height.
const LANE_H: f64 = 58.0;
/// Minimum strip length / drag floor (seconds).
const MIN_LEN: f64 = 0.2;
/// Trim-handle hit width (px).
const HANDLE_W: f64 = 6.0;

/// The Mixer body. Matches the dock's `(Arc<CustomAnimation>, Geo)` call
/// convention; the active clip is only used for the "+ Strip" default (its
/// duration), while the layer/strip data is read reactively from `anim_mixer`.
pub fn render(active_clip: Arc<CustomAnimation>, geo: Geo) -> Dom {
    html!("div", {
        // Rebuild the two panes only on a **structural** signal (layer count), so a
        // weight-slider drag (a native <input> whose DOM identity must survive) is
        // not destroyed mid-gesture. Within a row, weight/mode/mask/strips are
        // reactive sub-bindings read by index from `anim_mixer` (see below).
        .child_signal(layer_count_signal().map(
            clone!(active_clip => move |n| Some(view(n, active_clip.clone(), geo))),
        ))
    })
}

/// A signal of the mixer's layer count (the structural rebuild trigger).
fn layer_count_signal() -> impl Signal<Item = usize> {
    controller()
        .anim_mixer
        .signal_ref(|d| d.layers.len())
        .dedupe()
}

/// The two-pane body: a sticky-left layer list and the lanes area, sized to the
/// **mixer duration** (which may exceed the active clip's duration). Built once
/// per layer count; per-layer content updates reactively by index.
fn view(n_layers: usize, active_clip: Arc<CustomAnimation>, geo: Geo) -> Dom {
    let total_h = n_layers as f64 * LANE_H + LANE_H; // + the add-layer row

    html!("div", {
        .style("display", "flex").style("position", "relative")
        // ── sticky-left layer list ────────────────────────────────────────────
        .child(html!("div", {
            .style("position", "sticky").style("left", "0").style("z-index", "5")
            .style("width", &format!("{NAMES_W}px")).style("flex", "0 0 auto")
            .style("background", "var(--bg-1)").style("border-right", "1px solid var(--line)")
            .children((0..n_layers).map(layer_row))
            .child(add_layer_row())
        }))
        // ── lanes area (width/gridlines track the live mixer duration) ─────────
        .child(html!("div", {
            .style("position", "relative")
            .style("height", &format!("{total_h}px")).style("background", "var(--bg-0)")
            .style_signal("width", lane_geo_signal(geo).map(|g| format!("{}px", g.content_w)))
            // time gridlines spanning the lanes (rebuilt as the duration changes)
            .child_signal(lane_geo_signal(geo).map(|g| Some(gridlines(g))))
            // one lane per layer
            .children((0..n_layers).map(clone!(active_clip => move |li| {
                lane(li, active_clip.clone(), geo)
            })))
            // the shared body playhead spanning all lanes
            .child(body_playhead_signal(geo, total_h))
        }))
    })
}

/// The lanes' own geometry (shared px/sec + unit/fps, duration = the live mixer
/// arrangement length), as a signal so width/gridlines/playhead track edits.
fn lane_geo_signal(geo: Geo) -> impl Signal<Item = Geo> {
    controller()
        .anim_mixer
        .signal_ref(move |d| Geo::new(geo.px_per_sec, mixer_duration(d), geo.fps, geo.unit))
}

/// Time gridlines spanning the lanes for the given (mixer-duration) geometry.
fn gridlines(g: Geo) -> Dom {
    html!("div", {
        .style("position", "absolute").style("inset", "0").style("pointer-events", "none")
        .children(grid_secs(g.dur).into_iter().map(move |s| {
            html!("div", {
                .style("position", "absolute").style("top", "0").style("bottom", "0").style("width", "1px")
                .style("left", &format!("{}px", g.time_to_x(s)))
                .style("background", "var(--line-soft)")
            })
        }))
    })
}

// ── layer list (sticky-left) ───────────────────────────────────────────────────

/// One layer row (h=LANE_H): icon · name · REPLACE/ADD badge · weight slider, and
/// for additive layers a base-clip select, plus the bone-mask control. Built once
/// per layer count; each control binds reactively to layer `li` of `anim_mixer`.
fn layer_row(li: usize) -> Dom {
    html!("div", {
        .style("height", &format!("{LANE_H}px")).style("padding", "6px 10px")
        .style("border-bottom", "1px solid var(--line-soft)")
        .style("display", "flex").style("flex-direction", "column").style("gap", "5px").style("justify-content", "center")
        // top line: icon · name · mode badge · delete
        .child(html!("div", {
            .style("display", "flex").style("align-items", "center").style("gap", "7px")
            .child(Icon::new("layers").size(13.0).color("var(--text-2)").render())
            .child(html!("span", {
                .style("flex", "1").style("min-width", "0")
                .style("font-size", "12px").style("font-weight", "540").style("color", "var(--text-0)")
                .style("white-space", "nowrap").style("overflow", "hidden").style("text-overflow", "ellipsis")
                .text(&format!("Layer {}", li + 1))
            }))
            .child(mode_badge(li))
            .child(html!("button", {
                .class("t")
                .attr("title", "Delete layer")
                .style("width", "18px").style("height", "18px")
                .style("display", "flex").style("align-items", "center").style("justify-content", "center")
                .style("border", "1px solid transparent").style("background", "transparent").style("cursor", "pointer")
                .style("border-radius", "4px")
                .event(move |e: events::Click| {
                    e.stop_propagation();
                    dispatch(EditorCommand::DeleteLayer { layer: li });
                })
                .child(Icon::new("trash").size(12.0).color("var(--text-3)").render())
            }))
        }))
        // weight slider + readout
        .child(html!("div", {
            .style("display", "flex").style("align-items", "center").style("gap", "7px")
            .attr("title", "Layer weight — how strongly this layer contributes to the final pose (0 = off, 1 = full).")
            .child(html!("span", {
                .class("mono").style("font-size", "9px").style("color", "var(--text-3)").style("width", "34px")
                .text_signal(layer_field_signal(li, 1.0, |l| l.weight).map(|w| format!("w {w:.2}")))
            }))
            .child(weight_slider(li))
        }))
        // mode-specific + mask controls
        .child(html!("div", {
            .style("display", "flex").style("align-items", "center").style("gap", "6px")
            .child(mask_button(li))
            // base-clip select appears only for additive layers (reactive on mode).
            .child_signal(layer_field_signal(li, false, |l| matches!(l.mode, LayerModeDoc::Additive { .. }))
                .map(move |additive| additive.then(|| base_clip_select(li))))
        }))
    })
}

/// The REPLACE/ADD badge button — toggles the layer's composite mode (reactive on
/// the live mode). Switching to Additive keeps no base clip (rest pose) by default.
fn mode_badge(li: usize) -> Dom {
    let additive = layer_field_signal(li, false, |l| {
        matches!(l.mode, LayerModeDoc::Additive { .. })
    });
    html!("button", {
        .class("t")
        .style("border", "1px solid transparent").style("background", "transparent")
        .style("cursor", "pointer").style("padding", "0")
        .attr("title", "Toggle Replace / Additive. Replace blends toward this layer's pose by weight; Additive adds its motion as a delta on the layers below.")
        .event(move |e: events::Click| {
            e.stop_propagation();
            let next = match current_layer(li).map(|l| l.mode) {
                Some(LayerModeDoc::Additive { .. }) => LayerModeDoc::Replace,
                _ => LayerModeDoc::Additive { base_clip: None },
            };
            dispatch(EditorCommand::SetLayerMode { layer: li, mode: next });
        })
        .child_signal(additive.map(|a| Some(badge(
            if a { "ADD" } else { "REPLACE" },
            if a { Tone::Accent } else { Tone::Neutral },
        ))))
    })
}

/// The per-layer weight slider (0–1). Its local `Mutable` is seeded from the live
/// weight and kept in sync with external changes, while edits dispatch
/// `SetLayerWeight` (coalescing). DOM identity survives doc rebuilds (gated on
/// layer count), so a drag is never interrupted.
fn weight_slider(li: usize) -> Dom {
    let value = Mutable::new(current_layer(li).map(|l| l.weight).unwrap_or(1.0));
    // external weight change → reflect into the thumb (skips no-op echoes).
    spawn_local(clone!(value => async move {
        layer_field_signal(li, 1.0, |l| l.weight).for_each(move |w| {
            value.set_neq(w);
            async {}
        }).await;
    }));
    Slider::new(value)
        .range(0.0, 1.0)
        .step(0.05)
        .decimals(2)
        .on_change(move |w| {
            dispatch(EditorCommand::SetLayerWeight {
                layer: li,
                weight: w,
            })
        })
        .render()
}

/// The bone-mask control (reactive on the layer's mask): shows the current mask
/// size (or "Whole rig") and, on click, sets the mask to the **current scene
/// selection** (with descendants), or clears it when the selection is empty.
///
/// NOTE (stubbed): a full node-tree multi-select mask picker is out of scope —
/// this binds the mask to the live `controller().selected` set instead.
fn mask_button(li: usize) -> Dom {
    let masked_border = layer_field_signal(li, false, |l| !l.mask_nodes.is_empty());
    let masked_bg = layer_field_signal(li, false, |l| !l.mask_nodes.is_empty());
    let masked_color = layer_field_signal(li, false, |l| !l.mask_nodes.is_empty());
    let label = layer_field_signal(li, "Whole rig".to_string(), |l| {
        let n = l.mask_nodes.len();
        if n == 0 {
            "Whole rig".to_string()
        } else {
            format!("Mask: {n} node{}", if n == 1 { "" } else { "s" })
        }
    });
    html!("button", {
        .class("t")
        .style("display", "flex").style("align-items", "center").style("gap", "5px")
        .style("height", "20px").style("padding", "0 7px").style("border-radius", "4px")
        .style("cursor", "pointer")
        .style_signal("border", masked_border.map(|m| if m { "1px solid var(--accent-line)" } else { "1px solid var(--line-soft)" }))
        .style_signal("background", masked_bg.map(|m| if m { "var(--accent-ghost)" } else { "var(--bg-2)" }))
        .attr("title", "Set this layer's bone mask to the current scene selection (with descendants). Click with nothing selected to clear (whole rig).")
        .event(move |e: events::Click| {
            e.stop_propagation();
            let sel = controller().selected.get_cloned();
            let layer = current_layer(li);
            let was_masked = layer.as_ref().map(|l| !l.mask_nodes.is_empty()).unwrap_or(false);
            let include_descendants = if was_masked {
                layer.map(|l| l.include_descendants).unwrap_or(true)
            } else {
                true
            };
            // Empty selection → clear the mask; otherwise mask to the selection.
            dispatch(EditorCommand::SetLayerMask { layer: li, nodes: sel, include_descendants });
        })
        .child(Icon::new("filter").size(11.0).color("var(--text-3)").render())
        .child(html!("span", {
            .style("font-size", "10px").style("white-space", "nowrap")
            .style_signal("color", masked_color.map(|m| if m { "var(--accent-bright)" } else { "var(--text-3)" }))
            .text_signal(label)
        }))
    })
}

/// The additive base-clip select (the reference pose): options = the clips plus
/// "(rest)" = None. Dispatches `SetLayerMode { Additive { base_clip } }`.
fn base_clip_select(li: usize) -> Dom {
    let current = match current_layer(li).map(|l| l.mode) {
        Some(LayerModeDoc::Additive { base_clip }) => base_clip,
        _ => None,
    };
    // Build (value, label) options; the value is the AssetId uuid string ("" = rest).
    let mut options: Vec<(String, String)> = vec![("".to_string(), "(rest)".to_string())];
    for c in controller().custom_animations.lock_ref().iter() {
        options.push((c.id.to_string(), c.name.get_cloned()));
    }
    let value = Mutable::new(current.map(|id| id.to_string()).unwrap_or_default());

    // user change → resolve the value back to an AssetId by matching the clip list.
    spawn_local(clone!(value => async move {
        let mut first = true;
        value.signal_cloned().for_each(move |v| {
            let fire = !first;
            first = false;
            async move {
                if !fire {
                    return;
                }
                let base_clip = resolve_clip_id(&v);
                dispatch(EditorCommand::SetLayerMode {
                    layer: li,
                    mode: LayerModeDoc::Additive { base_clip },
                });
            }
        }).await;
    }));

    html!("div", {
        .style("flex", "1").style("min-width", "0")
        .attr("title", "Additive base clip — the reference pose subtracted to produce the delta. \u{201c}(rest)\u{201d} uses the bind pose.")
        .child(select(value, options))
    })
}

/// A signal of a derived field of layer `li` (falling back to `default` when the
/// layer is gone), deduped so it only fires on real changes.
fn layer_field_signal<T, F>(li: usize, default: T, f: F) -> impl Signal<Item = T>
where
    T: PartialEq + Clone,
    F: Fn(&LayerDoc) -> T + 'static,
{
    controller()
        .anim_mixer
        .signal_ref(move |d| d.layers.get(li).map(&f).unwrap_or_else(|| default.clone()))
        .dedupe_cloned()
}

/// A snapshot of layer `li` (for click handlers that need the current mode/mask).
fn current_layer(li: usize) -> Option<LayerDoc> {
    controller().anim_mixer.lock_ref().layers.get(li).cloned()
}

/// Resolve a select value (AssetId uuid string, or "" = rest) back to an
/// `Option<AssetId>` by matching the live clip library.
fn resolve_clip_id(v: &str) -> Option<AssetId> {
    if v.is_empty() {
        return None;
    }
    controller()
        .custom_animations
        .lock_ref()
        .iter()
        .find(|c| c.id.to_string() == v)
        .map(|c| c.id)
}

/// The trailing "＋ Add layer" row.
fn add_layer_row() -> Dom {
    html!("div", {
        .class("t")
        .style("height", &format!("{LANE_H}px"))
        .style("display", "flex").style("align-items", "center").style("justify-content", "center")
        .style("gap", "5px").style("cursor", "pointer")
        .style("color", "var(--text-3)").style("font-size", "11.5px")
        .event(|_: events::Click| dispatch(EditorCommand::AddLayer))
        .child(Icon::new("plus").size(13.0).color("var(--text-3)").render())
        .child(html!("span", { .text("Add layer") }))
    })
}

// ── lanes ──────────────────────────────────────────────────────────────────────

/// One layer's lane (height LANE_H): its strips + a "+ Strip" affordance. The
/// lane's left edge is the strips' offset parent (== content x = 0). The strip
/// set rebuilds reactively on this layer's strips (strip drag is window-level, so
/// a mid-drag rebuild is harmless — same as the dope-sheet diamonds).
fn lane(li: usize, active_clip: Arc<CustomAnimation>, geo: Geo) -> Dom {
    let top = li as f64 * LANE_H;
    html!("div", {
        .style("position", "absolute").style("left", "0").style("right", "0")
        .style("top", &format!("{top}px")).style("height", &format!("{LANE_H}px"))
        .style("border-bottom", "1px solid var(--line-soft)")
        .style("background", if li % 2 == 1 { "oklch(1 0 0 / 0.012)" } else { "transparent" })
        // strips (rebuilt as the layer's strips/weight change)
        .child_signal(layer_field_signal(li, (Vec::new(), 1.0), |l| (l.strips.clone(), l.weight))
            .map(move |(strips, weight)| Some(html!("div", {
                .style("position", "absolute").style("inset", "0")
                .children(strips.iter().enumerate().map(move |(si, strip)| {
                    strip_block(li, si, strip, weight, geo)
                }))
            }))))
        // "+ Strip" affordance: adds the active clip at the playhead.
        .child(add_strip_button(li, active_clip.clone()))
    })
}

/// One clip strip: a draggable body (moves `start`), a colored header (name +
/// repeat glyph), a `{len}s` body with a faint repeat hatch, and left/right trim
/// handles. Move → `MoveStrip`; trim → `TrimStrip`; repeat glyph → `SetStripRepeat`.
fn strip_block(li: usize, si: usize, strip: &StripDoc, weight: f64, geo: Geo) -> Dom {
    let (name, color) = resolve_clip(strip.clip);
    let x = geo.time_to_x(strip.start);
    let w = (strip.len * geo.px_per_sec).max(14.0);
    let opacity = 0.5 + weight * 0.5;
    let start = strip.start;
    let len = strip.len;
    let repeat = strip.repeat;

    html!("div", {
        .style("position", "absolute").style("top", "8px")
        .style("left", &format!("{x}px")).style("width", &format!("{w}px"))
        .style("height", &format!("{}px", LANE_H - 16.0))
        .style("border-radius", "6px").style("overflow", "hidden")
        .style("cursor", "grab")
        .style("background", &format!("color-mix(in oklch, {color} 22%, var(--bg-2))"))
        .style("border", &format!("1px solid {color}"))
        .style("box-shadow", "var(--shadow-1)").style("opacity", &format!("{opacity}"))
        .attr("title", &format!("{name} \u{00b7} {len:.2}s{}", if repeat { " \u{00b7} repeat" } else { "" }))
        // body drag → MoveStrip (start)
        .apply(move |b| strip_drag(b, li, si, start, len, geo, DragMode::Move))
        // colored header bar: name + repeat-toggle glyph
        .child(html!("div", {
            .style("height", "16px").style("display", "flex").style("align-items", "center").style("gap", "5px")
            .style("padding", "0 7px")
            .style("background", &format!("color-mix(in oklch, {color} 60%, var(--bg-2))"))
            .child(html!("span", {
                .style("flex", "1").style("min-width", "0")
                .style("font-size", "10.5px").style("font-weight", "600").style("color", "var(--text-0)")
                .style("white-space", "nowrap").style("overflow", "hidden").style("text-overflow", "ellipsis")
                .text(&name)
            }))
            .child(html!("span", {
                .class("mono")
                .style("font-size", "10px").style("cursor", "pointer")
                .style("color", if repeat { "var(--text-0)" } else { "var(--text-3)" })
                .style("opacity", if repeat { "0.9" } else { "0.6" })
                .attr("title", "Toggle repeat (wrap) fill")
                .event(move |e: events::Click| {
                    e.stop_propagation();
                    dispatch(EditorCommand::SetStripRepeat { layer: li, strip: si, repeat: !repeat });
                })
                .text("\u{21bb}")
            }))
        }))
        // body: repeat hatch + {len}s label
        .child(html!("div", {
            .style("position", "absolute").style("left", "0").style("right", "0").style("top", "16px").style("bottom", "0")
            .apply(move |b| if repeat {
                let cell = (len.max(MIN_LEN) / 4.0 * geo.px_per_sec).max(8.0);
                b.style("background-image", format!(
                    "repeating-linear-gradient(90deg, transparent 0 {cell}px, color-mix(in oklch, {color} 40%, transparent) {cell}px {}px)",
                    cell + 1.0
                ))
            } else {
                b
            })
            .child(html!("span", {
                .class("mono")
                .style("position", "absolute").style("left", "7px").style("bottom", "4px")
                .style("font-size", "9px").style("color", "var(--text-2)")
                .text(&format!("{len:.2}s"))
            }))
        }))
        // trim handles (left moves start+len, right moves len)
        .child(trim_handle(li, si, start, len, geo, DragMode::TrimLeft))
        .child(trim_handle(li, si, start, len, geo, DragMode::TrimRight))
    })
}

/// A 6px trim handle on a strip edge.
fn trim_handle(li: usize, si: usize, start: f64, len: f64, geo: Geo, mode: DragMode) -> Dom {
    let left = matches!(mode, DragMode::TrimLeft);
    html!("div", {
        .style("position", "absolute").style("top", "0").style("bottom", "0")
        .style("width", &format!("{HANDLE_W}px")).style("cursor", "ew-resize")
        .apply(|b| if left { b.style("left", "0") } else { b.style("right", "0") })
        .apply(move |b| strip_drag(b, li, si, start, len, geo, mode))
    })
}

/// The "+ Strip" affordance — adds the active clip at the playhead (clamped to
/// [0, clip start]), with the clip's duration as the default length.
fn add_strip_button(li: usize, active_clip: Arc<CustomAnimation>) -> Dom {
    html!("button", {
        .class("t")
        .attr("title", "Add a strip of the active clip at the playhead")
        .style("position", "absolute").style("right", "6px").style("top", "6px")
        .style("width", "20px").style("height", "20px").style("z-index", "2")
        .style("display", "flex").style("align-items", "center").style("justify-content", "center")
        .style("border", "1px solid var(--line-soft)").style("background", "var(--bg-2)")
        .style("border-radius", "4px").style("cursor", "pointer")
        .event(move |e: events::Click| {
            e.stop_propagation();
            let start = controller().playhead.get().max(0.0);
            let len = active_clip.duration.get().max(MIN_LEN);
            dispatch(EditorCommand::AddStrip {
                layer: li,
                clip: active_clip.id,
                start,
                len,
            });
        })
        .child(Icon::new("plus").size(12.0).color("var(--text-2)").render())
    })
}

// ── drag (mirror the dope-sheet window-drag pattern) ───────────────────────────

#[derive(Clone, Copy)]
enum DragMode {
    Move,
    TrimLeft,
    TrimRight,
}

/// Attach the window-level strip drag (mirrors dope.rs/ruler.rs): mousedown
/// captures the press `clientX` + the original start/len; window mousemove computes
/// `dt = x_to_time(now) - x_to_time(down)` (a pure delta — no element rect needed,
/// since `px_per_sec` is the only scale) and dispatches the coalescing Move/Trim
/// command; mouseup ends.
fn strip_drag(
    b: dominator::DomBuilder<web_sys::HtmlElement>,
    li: usize,
    si: usize,
    orig_start: f64,
    orig_len: f64,
    geo: Geo,
    mode: DragMode,
) -> dominator::DomBuilder<web_sys::HtmlElement> {
    // The press clientX (px) captured on mousedown for the active drag.
    let drag: Rc<Cell<Option<f64>>> = Rc::new(Cell::new(None));

    b.event(clone!(drag => move |e: events::MouseDown| {
        e.stop_propagation();
        drag.set(Some(e.x()));
    }))
    .global_event(clone!(drag => move |e: events::MouseMove| {
        if let Some(down_x) = drag.get() {
            // dt = time(now) - time(down); px/sec mapping via geo.
            let dt = geo.x_to_time(e.x() - down_x);
            match mode {
                DragMode::Move => {
                    let start = round3((orig_start + dt).max(0.0));
                    dispatch(EditorCommand::MoveStrip { layer: li, strip: si, start });
                }
                DragMode::TrimRight => {
                    let len = round3((orig_len + dt).max(MIN_LEN));
                    dispatch(EditorCommand::TrimStrip { layer: li, strip: si, start: orig_start, len });
                }
                DragMode::TrimLeft => {
                    let len = (orig_len - dt).max(MIN_LEN);
                    let start = (orig_start + (orig_len - len)).max(0.0);
                    dispatch(EditorCommand::TrimStrip {
                        layer: li,
                        strip: si,
                        start: round3(start),
                        len: round3(len),
                    });
                }
            }
        }
    }))
    .global_event(clone!(drag => move |_: events::MouseUp| {
        drag.set(None);
    }))
}

// ── helpers ────────────────────────────────────────────────────────────────────

/// The Mixer arrangement length = `max(strip.start + strip.len)` (a floor of 4s,
/// + 1s of trailing room).
fn mixer_duration(doc: &MixerDoc) -> f64 {
    let mut m = 0.0_f64;
    for layer in &doc.layers {
        for s in &layer.strips {
            m = m.max(s.start + s.len);
        }
    }
    (m + 1.0).max(4.0)
}

/// Time gridlines (coarser for long arrangements).
fn grid_secs(dur: f64) -> Vec<f64> {
    let step = if dur > 6.0 { 1.0 } else { 0.5 };
    let mut out = Vec::new();
    let mut s = 0.0;
    while s <= dur + 1e-6 {
        out.push(s);
        s += step;
    }
    out
}

/// Resolve a strip's clip → `(name, color)` from the live library (fallbacks for
/// a dangling reference).
fn resolve_clip(id: AssetId) -> (String, String) {
    match find_clip(&controller().custom_animations, id) {
        Some(c) => (c.name.get_cloned(), c.color.get_cloned()),
        None => ("(missing clip)".to_string(), "var(--text-3)".to_string()),
    }
}

/// The vertical body playhead line at `t * px_per_sec` (the lanes start at x = 0).
/// `px_per_sec` is constant across the dock + lane geometry, so the active clip's
/// `geo` maps time → x identically to the lanes.
fn body_playhead_signal(geo: Geo, height: f64) -> Dom {
    html!("div", {
        .style("position", "absolute").style("top", "0")
        .style("height", &format!("{height}px"))
        .style("width", "1.5px").style("background", "var(--accent-bright)")
        .style("z-index", "6").style("pointer-events", "none")
        .style("box-shadow", "0 0 0 0.5px oklch(0 0 0 / 0.3)")
        .style_signal("left", controller().playhead.signal().map(move |t| {
            format!("{}px", geo.time_to_x(t))
        }))
    })
}

/// Round to 3 decimals (drag snapping).
fn round3(v: f64) -> f64 {
    (v * 1e3).round() / 1e3
}

fn dispatch(cmd: EditorCommand) {
    spawn_local(async move {
        let _ = controller().dispatch(cmd).await;
    });
}
