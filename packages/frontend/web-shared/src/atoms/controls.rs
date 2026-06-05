//! Small controlled inputs from `ui.jsx`: `Toggle`, `Check`, `Segmented`,
//! `Swatch`, `Badge`, and `Slider` (the labeled range from `ui-extra.jsx`).
//!
//! State-bearing controls (`toggle`, `check`, `segmented`, `swatch`, `slider`)
//! take a caller-owned `Mutable` so the editor can both observe changes (to
//! dispatch a command) and reflect external state back into the control.

use std::cell::RefCell;
use std::rc::Rc;

use crate::atoms::icon::Icon;
use crate::prelude::*;

/// A pill switch bound to a `Mutable<bool>`.
pub fn toggle(value: Mutable<bool>) -> Dom {
    html!("button", {
        .class("t")
        .class("focusring")
        .attr("role", "switch")
        .style("width", "34px")
        .style("height", "19px")
        .style("border-radius", "11px")
        .style("position", "relative")
        .style("cursor", "pointer")
        .style("border-style", "solid")
        .style("border-width", "1px")
        .style_signal("border-color", value.signal().map(|v| if v { "transparent" } else { "var(--line)" }))
        .style_signal("background", value.signal().map(|v| if v { "var(--accent)" } else { "var(--bg-3)" }))
        .event(clone!(value => move |_: events::Click| value.set(!value.get())))
        .child(html!("span", {
            .class("t")
            .style("position", "absolute")
            .style("top", "1.5px")
            .style("width", "14px")
            .style("height", "14px")
            .style("border-radius", "50%")
            .style_signal("left", value.signal().map(|v| if v { "16px" } else { "1.5px" }))
            .style_signal("background", value.signal().map(|v| if v { "oklch(0.18 0.02 255)" } else { "var(--text-2)" }))
        }))
    })
}

/// A checkbox bound to a `Mutable<bool>`.
pub fn check(value: Mutable<bool>) -> Dom {
    html!("button", {
        .class("t")
        .class("focusring")
        .attr("role", "checkbox")
        .style("width", "17px")
        .style("height", "17px")
        .style("border-radius", "4px")
        .style("cursor", "pointer")
        .style("display", "flex")
        .style("align-items", "center")
        .style("justify-content", "center")
        .style("border-style", "solid")
        .style("border-width", "1px")
        .style_signal("border-color", value.signal().map(|v| if v { "transparent" } else { "var(--line)" }))
        .style_signal("background", value.signal().map(|v| if v { "var(--accent)" } else { "var(--bg-3)" }))
        .event(clone!(value => move |_: events::Click| value.set(!value.get())))
        .child_signal(value.signal().map(|v| if v {
            Some(Icon::new("check").size(12.0).stroke_width(2.4).color("oklch(0.18 0.02 255)").render())
        } else {
            None
        }))
    })
}

/// One entry in a [`segmented`] control.
pub struct SegOption {
    pub value: String,
    pub label: String,
    pub icon: Option<String>,
}

impl SegOption {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            icon: None,
        }
    }
    pub fn icon(mut self, name: impl Into<String>) -> Self {
        self.icon = Some(name.into());
        self
    }
}

/// A segmented button group bound to a `Mutable<String>` (the selected value).
pub fn segmented(
    selected: Mutable<String>,
    options: Vec<SegOption>,
    small: bool,
    full: bool,
) -> Dom {
    let h = if small { 24 } else { 30 };
    html!("div", {
        .style("display", if full { "flex" } else { "inline-flex" })
        .style("width", if full { "100%" } else { "auto" })
        .style("background", "var(--bg-3)")
        .style("border", "1px solid var(--line-soft)")
        .style("border-radius", "var(--r2)")
        .style("padding", "2px")
        .style("gap", "2px")
        .children(options.into_iter().map(move |o| {
            let v = o.value.clone();
            let on_sig = selected.signal_cloned().map(clone!(v => move |s| s == v));
            let on_sig2 = selected.signal_cloned().map(clone!(v => move |s| s == v));
            let on_sig3 = selected.signal_cloned().map(clone!(v => move |s| s == v));
            let mut kids: Vec<Dom> = Vec::new();
            if let Some(ic) = o.icon {
                kids.push(Icon::new(ic).size(14.0).render());
            }
            kids.push(html!("span", { .text(&o.label) }));
            html!("button", {
                .class("t")
                .style("display", "inline-flex")
                .style("align-items", "center")
                .style("justify-content", "center")
                .style("gap", "6px")
                .style("flex", if full { "1" } else { "0 0 auto" })
                .style("height", &format!("{}px", h - 4))
                .style("padding", "0 11px")
                .style("border-radius", "var(--r1)")
                .style("border-style", "none")
                .style("cursor", "pointer")
                .style("font-size", "12px")
                .style("white-space", "nowrap")
                .style_signal("font-weight", on_sig.map(|on| if on { "600" } else { "520" }))
                .style_signal("background", on_sig2.map(|on| if on { "var(--bg-active)" } else { "transparent" }))
                .style_signal("color", on_sig3.map(|on| if on { "var(--text-0)" } else { "var(--text-2)" }))
                .event(clone!(selected, v => move |_: events::Click| selected.set_neq(v.clone())))
                .children(kids)
            })
        }))
    })
}

/// A color swatch bound to a `Mutable<String>` (a CSS color the native picker
/// edits). The hidden `<input type=color>` overlays the swatch.
pub fn swatch(color: Mutable<String>, size: f64) -> Dom {
    let sz = format!("{size}px");
    html!("div", {
        .class("t")
        .class("focusring")
        .style("width", &sz)
        .style("height", &sz)
        .style("border-radius", "5px")
        .style("border", "1px solid var(--line-strong)")
        .style("cursor", "pointer")
        .style("position", "relative")
        .style("box-shadow", "inset 0 0 0 1px oklch(1 0 0 / 0.06)")
        .style_signal("background", color.signal_cloned())
        .child(html!("input" => web_sys::HtmlInputElement, {
            .attr("type", "color")
            .prop_signal("value", color.signal_cloned())
            .style("position", "absolute")
            .style("inset", "0")
            .style("opacity", "0")
            .style("width", "100%")
            .style("height", "100%")
            .style("cursor", "pointer")
            .with_node!(input => {
                .event(clone!(color => move |_: events::Input| color.set_neq(input.value())))
            })
        }))
    })
}

/// Badge tone.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Accent,
    Ok,
    Warn,
    Danger,
}

fn tone_colors(t: Tone) -> (&'static str, &'static str, &'static str) {
    // (background, foreground, border)
    match t {
        Tone::Neutral => ("var(--bg-2)", "var(--text-2)", "var(--line-soft)"),
        Tone::Accent => (
            "var(--accent-ghost)",
            "var(--accent-bright)",
            "var(--accent-line)",
        ),
        Tone::Ok => (
            "oklch(0.74 0.13 150 / 0.14)",
            "var(--ok)",
            "oklch(0.74 0.13 150 / 0.35)",
        ),
        Tone::Warn => (
            "oklch(0.80 0.13 85 / 0.14)",
            "var(--warn)",
            "oklch(0.80 0.13 85 / 0.35)",
        ),
        Tone::Danger => (
            "var(--danger-soft)",
            "var(--danger)",
            "oklch(0.65 0.17 25 / 0.4)",
        ),
    }
}

/// A small uppercase mono badge.
pub fn badge(text: impl AsRef<str>, tone: Tone) -> Dom {
    let (bg, fg, bd) = tone_colors(tone);
    html!("span", {
        .class("mono")
        .style("font-size", "9.5px")
        .style("font-weight", "600")
        .style("letter-spacing", "0.04em")
        .style("text-transform", "uppercase")
        .style("padding", "2px 6px")
        .style("border-radius", "4px")
        .style("white-space", "nowrap")
        .style("background", bg)
        .style("color", fg)
        .style("border", &format!("1px solid {bd}"))
        .text(text.as_ref())
    })
}

type F64Cb = Rc<RefCell<Box<dyn FnMut(f64)>>>;

/// A labeled range slider bound to a `Mutable<f64>`.
pub struct Slider {
    value: Mutable<f64>,
    min: f64,
    max: f64,
    step: f64,
    unit: Option<String>,
    decimals: Option<usize>,
    on_change: Option<F64Cb>,
}

impl Slider {
    pub fn new(value: Mutable<f64>) -> Self {
        Self {
            value,
            min: 0.0,
            max: 1.0,
            step: 0.01,
            unit: None,
            decimals: None,
            on_change: None,
        }
    }
    pub fn range(mut self, min: f64, max: f64) -> Self {
        self.min = min;
        self.max = max;
        self
    }
    pub fn step(mut self, step: f64) -> Self {
        self.step = step;
        self
    }
    pub fn unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = Some(unit.into());
        self
    }
    pub fn decimals(mut self, d: usize) -> Self {
        self.decimals = Some(d);
        self
    }
    pub fn on_change(mut self, f: impl FnMut(f64) + 'static) -> Self {
        self.on_change = Some(Rc::new(RefCell::new(Box::new(f))));
        self
    }

    pub fn render(self) -> Dom {
        let value = self.value;
        let decimals = self.decimals;
        let unit = self.unit.unwrap_or_default();
        let on_change = self.on_change;
        let readout = value.signal().map(move |v| {
            let num = match decimals {
                Some(d) => format!("{v:.*}", d),
                None => format!("{}", (v * 100.0).round() / 100.0),
            };
            format!("{num}{unit}")
        });
        html!("div", {
            .style("display", "flex")
            .style("align-items", "center")
            .style("gap", "10px")
            .child(html!("input" => web_sys::HtmlInputElement, {
                .attr("type", "range")
                .attr("min", &format!("{}", self.min))
                .attr("max", &format!("{}", self.max))
                .attr("step", &format!("{}", self.step))
                .prop_signal("value", value.signal().map(|v| format!("{v}")))
                .style("flex", "1")
                .style("accent-color", "var(--accent)")
                .style("height", "4px")
                .with_node!(input => {
                    .event(clone!(value => move |_: events::Input| {
                        if let Ok(v) = input.value().parse::<f64>() {
                            value.set_neq(v);
                            if let Some(cb) = &on_change {
                                (cb.borrow_mut())(v);
                            }
                        }
                    }))
                })
            }))
            .child(html!("span", {
                .class("mono")
                .style("font-size", "11px")
                .style("color", "var(--text-1)")
                .style("width", "42px")
                .style("text-align", "right")
                .text_signal(readout)
            }))
        })
    }
}
