//! Segmented control — a pill group of mutually-exclusive options bound to a
//! `Mutable<T>`. Used for the top-bar Scene/Material switch, the material
//! alpha-mode selector, the layout switch, etc. (prototype `Segmented`).

use crate::prelude::*;

/// Builder for a segmented control. `T` is the option value type — any
/// `Copy + PartialEq` enum/id. The selected value lives in a `Mutable<T>`
/// the caller owns; clicking an option writes to it.
pub struct Segmented<T> {
    selected: Mutable<T>,
    options: Vec<(T, String)>,
    small: bool,
    full: bool,
    on_change: Option<Box<dyn Fn(T)>>,
}

impl<T: Copy + PartialEq + 'static> Segmented<T> {
    pub fn new(selected: Mutable<T>) -> Self {
        Self {
            selected,
            options: Vec::new(),
            small: false,
            full: false,
            on_change: None,
        }
    }

    /// Append an option with its display label.
    pub fn option(mut self, value: T, label: impl Into<String>) -> Self {
        self.options.push((value, label.into()));
        self
    }

    /// Compact sizing (top-bar / rail use).
    pub fn small(mut self) -> Self {
        self.small = true;
        self
    }

    /// Stretch options to fill the container width equally.
    pub fn full(mut self) -> Self {
        self.full = true;
        self
    }

    /// Side-effect to run (in addition to setting the mutable) when the
    /// selection changes via a click.
    pub fn on_change(mut self, f: impl Fn(T) + 'static) -> Self {
        self.on_change = Some(Box::new(f));
        self
    }

    pub fn render(self) -> Dom {
        let Segmented {
            selected,
            options,
            small,
            full,
            on_change,
        } = self;

        let pad = if small { "3px" } else { "4px" };
        let on_change: Option<Arc<dyn Fn(T)>> = on_change.map(Arc::from);

        html!("div", {
            .style("display", if full { "flex" } else { "inline-flex" })
            .style("align-items", "center")
            .style("gap", "2px")
            .style("padding", pad)
            .style("background", "var(--bg-3)")
            .style("border", "1px solid var(--line-soft)")
            .style("border-radius", "var(--r2)")
            .children(options.into_iter().map(clone!(selected, on_change => move |(value, label)| {
                let on_change = on_change.clone();
                html!("button", {
                    .class("t")
                    .style("display", "inline-flex")
                    .style("align-items", "center")
                    .style("justify-content", "center")
                    .style("gap", "5px")
                    .style("cursor", "pointer")
                    .style("border", "1px solid transparent")
                    .style("border-radius", "var(--r1)")
                    .style("white-space", "nowrap")
                    .style("font-weight", "560")
                    .style("font-size", if small { "11.5px" } else { "12.5px" })
                    .style("padding", if small { "3px 9px" } else { "5px 12px" })
                    .apply_if(full, |b| b.style("flex", "1 1 0").style("min-width", "0"))
                    .style_signal("background", selected.signal().map(move |s| {
                        if s == value { "var(--accent-dim)" } else { "transparent" }
                    }))
                    .style_signal("color", selected.signal().map(move |s| {
                        if s == value { "var(--text-0)" } else { "var(--text-2)" }
                    }))
                    .style_signal("border-color", selected.signal().map(move |s| {
                        if s == value { "var(--accent-line)" } else { "transparent" }
                    }))
                    .text(&label)
                    .event(clone!(selected => move |_: events::Click| {
                        selected.set_neq(value);
                        if let Some(cb) = &on_change {
                            cb(value);
                        }
                    }))
                })
            })))
        })
    }
}
