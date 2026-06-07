//! Numeric field (`NumField`) + 3-axis vector (`Vec3`).
//!
//! `NumField` is an axis-tinted numeric input with **drag-to-scrub**: pressing
//! the axis chip and dragging horizontally scrubs the value by `step` per pixel.
//! The move/up listeners are `global_event`s (window-bound, tied to the node's
//! lifetime) so the drag keeps tracking when the cursor leaves the field.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::prelude::*;

/// Which transform axis a [`NumField`] represents (drives the tint + chip).
/// `W` is the quaternion scalar component (neutral tint).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    X,
    Y,
    Z,
    W,
}

impl Axis {
    fn color(self) -> &'static str {
        match self {
            Axis::X => "var(--axis-x)",
            Axis::Y => "var(--axis-y)",
            Axis::Z => "var(--axis-z)",
            Axis::W => "var(--text-3)",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Axis::X => "X",
            Axis::Y => "Y",
            Axis::Z => "Z",
            Axis::W => "W",
        }
    }
}

type ChangeCb = Rc<RefCell<Option<Box<dyn FnMut(f64)>>>>;

/// Round to `step` and clamp to `[min, max]`, mirroring the prototype's commit.
fn clamp_round(mut n: f64, step: f64, min: Option<f64>, max: Option<f64>, round: bool) -> f64 {
    if round && step > 0.0 {
        n = (n / step).round() * step;
        n = (n * 10000.0).round() / 10000.0; // trim float fuzz (prototype toFixed(4))
    }
    if let Some(mn) = min {
        n = n.max(mn);
    }
    if let Some(mx) = max {
        n = n.min(mx);
    }
    n
}

/// Format a value for display — integers without a trailing `.0`, fractionals
/// rounded to 5 decimals so f32→f64 representation fuzz (`0.30000001192…`,
/// `0.69999999…`) renders as `0.3` / `0.7` rather than a noisy tail.
fn fmt(n: f64) -> String {
    if n == n.trunc() && n.abs() < 1e15 {
        return format!("{}", n as i64);
    }
    let mut s = format!("{n:.5}");
    while s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    s
}

fn call(cb: &ChangeCb, n: f64) {
    if let Some(f) = cb.borrow_mut().as_mut() {
        f(n);
    }
}

pub struct NumField {
    value: f64,
    step: f64,
    axis: Option<Axis>,
    suffix: Option<String>,
    min: Option<f64>,
    max: Option<f64>,
    on_change: Option<Box<dyn FnMut(f64)>>,
    /// External value source (e.g. a gizmo drag) — updates the displayed value
    /// live, but only while the field is neither focused (mid-edit) nor being
    /// scrubbed, so user input is never clobbered.
    value_signal: Option<std::pin::Pin<Box<dyn Signal<Item = f64>>>>,
}

impl NumField {
    pub fn new(value: f64) -> Self {
        Self {
            value,
            step: 0.1,
            axis: None,
            suffix: None,
            min: None,
            max: None,
            on_change: None,
            value_signal: None,
        }
    }
    pub fn step(mut self, step: f64) -> Self {
        self.step = step;
        self
    }
    pub fn axis(mut self, axis: Axis) -> Self {
        self.axis = Some(axis);
        self
    }
    pub fn suffix(mut self, suffix: impl Into<String>) -> Self {
        self.suffix = Some(suffix.into());
        self
    }
    pub fn min(mut self, min: f64) -> Self {
        self.min = Some(min);
        self
    }
    pub fn max(mut self, max: f64) -> Self {
        self.max = Some(max);
        self
    }
    pub fn on_change(mut self, f: impl FnMut(f64) + 'static) -> Self {
        self.on_change = Some(Box::new(f));
        self
    }
    /// Drive the displayed value from an external signal (e.g. a live gizmo
    /// drag). Ignored while the field is focused or being scrubbed.
    pub fn value_signal(mut self, sig: impl Signal<Item = f64> + 'static) -> Self {
        self.value_signal = Some(Box::pin(sig));
        self
    }

    pub fn render(self) -> Dom {
        let display = Mutable::new(fmt(self.value));
        let foc = Mutable::new(false);
        let ah = Mutable::new(false);
        let on_change: ChangeCb = Rc::new(RefCell::new(self.on_change));
        let step = self.step;
        let (min, max) = (self.min, self.max);
        // (startX, startVal) while scrubbing, else None.
        let drag: Rc<Cell<Option<(f64, f64)>>> = Rc::new(Cell::new(None));

        // External value source → live-update the display, but never while the
        // user is focused (editing) or scrubbing the field.
        if let Some(sig) = self.value_signal {
            wasm_bindgen_futures::spawn_local(clone!(display, foc, drag => async move {
                sig.for_each(move |v| {
                    if !foc.get() && drag.get().is_none() {
                        display.set_neq(fmt(v));
                    }
                    async {}
                })
                .await;
            }));
        }
        let has_axis = self.axis.is_some();
        let pad = if has_axis { "0 7px" } else { "0 8px" };

        let mut children: Vec<Dom> = Vec::new();

        if let Some(axis) = self.axis {
            let color = axis.color();
            children.push(html!("span", {
                .class("mono")
                .attr("title", "Drag left/right to scrub the value")
                .style("cursor", "ew-resize")
                .style("padding", "0 5px")
                .style("font-size", "10.5px")
                .style("font-weight", "700")
                .style("color", color)
                .style("height", "100%")
                .style("display", "flex")
                .style("align-items", "center")
                .style("justify-content", "center")
                .style("user-select", "none")
                .style("transition", "min-width .1s")
                .style_signal("min-width", ah.signal().map(|h| if h { "26px" } else { "16px" }))
                .style_signal("background", ah.signal().map(move |h| {
                    let pct = if h { "30%" } else { "14%" };
                    format!("color-mix(in oklch, {color} {pct}, transparent)")
                }))
                .text_signal(ah.signal().map(move |h| if h { "\u{21c4}".to_string() } else { axis.label().to_string() }))
                .event(clone!(ah => move |_: events::MouseEnter| ah.set_neq(true)))
                .event(clone!(ah => move |_: events::MouseLeave| ah.set_neq(false)))
                .event(clone!(drag, display => move |e: events::MouseDown| {
                    let start_val = display.get_cloned().parse::<f64>().unwrap_or(0.0);
                    drag.set(Some((e.x(), start_val)));
                }))
            }));
        }

        children.push(html!("input" => web_sys::HtmlInputElement, {
            .class("mono")
            .attr("inputmode", "decimal")
            .prop_signal("value", display.signal_cloned())
            .style("width", "100%")
            .style("min-width", "0")
            .style("background", "transparent")
            .style("border-style", "none")
            .style("outline-style", "none")
            .style("color", "var(--text-0)")
            .style("font-size", "12px")
            .style("padding", pad)
            .style("height", "100%")
            .with_node!(input => {
                .event(clone!(foc => move |_: events::Focus| foc.set_neq(true)))
                .event(clone!(input, display, foc, on_change => move |_: events::Blur| {
                    foc.set_neq(false);
                    if let Ok(n) = input.value().parse::<f64>() {
                        let n = clamp_round(n, step, min, max, false);
                        display.set(fmt(n));
                        call(&on_change, n);
                    }
                }))
                .event(clone!(input, display => move |_: events::Input| display.set(input.value())))
                .event(clone!(input => move |e: events::KeyDown| {
                    if e.key() == "Enter" {
                        input.blur().ok();
                    }
                }))
            })
        }));

        if let Some(suffix) = self.suffix {
            children.push(html!("span", {
                .class("mono")
                .style("padding-right", "8px")
                .style("font-size", "10.5px")
                .style("color", "var(--text-3)")
                .text(&suffix)
            }));
        }

        html!("div", {
            .class("t")
            .style("display", "flex")
            .style("align-items", "center")
            .style("height", "var(--row-h)")
            .style("background", "var(--bg-3)")
            .style("border-radius", "var(--r1)")
            .style("border-style", "solid")
            .style("border-width", "1px")
            .style("overflow", "hidden")
            .style_signal("border-color", foc.signal().map(|f| if f { "var(--accent-line)" } else { "var(--line-soft)" }))
            .style_signal("box-shadow", foc.signal().map(|f| if f { "0 0 0 2px var(--accent-ghost)" } else { "none" }))
            // Window-bound scrub listeners (active only while `drag` is Some).
            .global_event(clone!(drag, display, on_change => move |e: events::MouseMove| {
                if let Some((start_x, start_val)) = drag.get() {
                    let dx = e.x() - start_x;
                    let n = clamp_round(start_val + dx * step, step, min, max, true);
                    display.set(fmt(n));
                    call(&on_change, n);
                }
            }))
            .global_event(clone!(drag => move |_: events::MouseUp| {
                if drag.get().is_some() {
                    drag.set(None);
                }
            }))
            .children(children)
        })
    }
}

/// Like [`vec3`] but the displayed values track an external signal (e.g. a live
/// gizmo drag), fanned out to the three fields. Each field still commits user
/// edits via `on_change`, and ignores the signal while focused/scrubbed. The
/// un-edited axes are filled from the latest signalled value.
pub fn vec3_signal(
    value_signal: impl Signal<Item = [f64; 3]> + 'static,
    step: f64,
    on_change: impl FnMut([f64; 3]) + 'static,
) -> Dom {
    use futures_signals::signal::Broadcaster;
    let axes = [Axis::X, Axis::Y, Axis::Z];
    let on_change = Rc::new(RefCell::new(on_change));
    // Shared latest value: feeds each field's display + fills un-edited axes.
    let current = Mutable::new([0.0_f64; 3]);
    let bc = Rc::new(Broadcaster::new(value_signal));
    wasm_bindgen_futures::spawn_local(clone!(current, bc => async move {
        bc.signal().for_each(move |v| { current.set_neq(v); async {} }).await;
    }));

    html!("div", {
        .style("display", "grid")
        .style("grid-template-columns", "1fr 1fr 1fr")
        .style("gap", "5px")
        .children((0..3).map(clone!(bc, current, on_change => move |i| {
            let on_change = on_change.clone();
            let current = current.clone();
            NumField::new(current.get()[i])
                .axis(axes[i])
                .step(step)
                .value_signal(bc.signal().map(move |v| v[i]))
                .on_change(move |n| {
                    let mut v = current.get();
                    v[i] = n;
                    current.set(v);
                    (on_change.borrow_mut())(v);
                })
                .render()
        })))
    })
}

/// Like [`vec3_signal`] but 4 fields (X/Y/Z/W) for a quaternion. The displayed
/// values track the external signal; user edits commit the full `[x,y,z,w]`.
pub fn vec4_signal(
    value_signal: impl Signal<Item = [f64; 4]> + 'static,
    step: f64,
    on_change: impl FnMut([f64; 4]) + 'static,
) -> Dom {
    use futures_signals::signal::Broadcaster;
    let axes = [Axis::X, Axis::Y, Axis::Z, Axis::W];
    let on_change = Rc::new(RefCell::new(on_change));
    let current = Mutable::new([0.0_f64; 4]);
    let bc = Rc::new(Broadcaster::new(value_signal));
    wasm_bindgen_futures::spawn_local(clone!(current, bc => async move {
        bc.signal().for_each(move |v| { current.set_neq(v); async {} }).await;
    }));

    html!("div", {
        .style("display", "grid")
        .style("grid-template-columns", "1fr 1fr 1fr 1fr")
        .style("gap", "5px")
        .children((0..4).map(clone!(bc, current, on_change => move |i| {
            let on_change = on_change.clone();
            let current = current.clone();
            NumField::new(current.get()[i])
                .axis(axes[i])
                .step(step)
                .value_signal(bc.signal().map(move |v| v[i]))
                .on_change(move |n| {
                    let mut v = current.get();
                    v[i] = n;
                    current.set(v);
                    (on_change.borrow_mut())(v);
                })
                .render()
        })))
    })
}

/// Three axis-tinted [`NumField`]s in a row for a `[x, y, z]` vector.
pub fn vec3(value: [f64; 3], step: f64, on_change: impl FnMut([f64; 3]) + 'static) -> Dom {
    let current = Rc::new(Cell::new(value));
    let axes = [Axis::X, Axis::Y, Axis::Z];
    let on_change = Rc::new(RefCell::new(on_change));

    html!("div", {
        .style("display", "grid")
        .style("grid-template-columns", "1fr 1fr 1fr")
        .style("gap", "5px")
        .children((0..3).map(move |i| {
            let current = current.clone();
            let on_change = on_change.clone();
            NumField::new(value[i])
                .axis(axes[i])
                .step(step)
                .on_change(move |n| {
                    let mut v = current.get();
                    v[i] = n;
                    current.set(v);
                    (on_change.borrow_mut())(v);
                })
                .render()
        }))
    })
}
