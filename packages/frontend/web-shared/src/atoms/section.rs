//! Layout primitives for inspector / drawer panels — `Section` (collapsible,
//! with a kicker title + optional right slot), `Row` (label + control grid),
//! and `DrawerSection` (a non-collapsing titled group). Ported from `ui.jsx` /
//! `ui-extra.jsx`.

use crate::atoms::icon::Icon;
use crate::prelude::*;

/// A collapsible inspector section. The header shows a rotating chevron, an
/// uppercase kicker title, and an optional right-aligned slot (whose clicks are
/// stopped so they don't toggle the section).
pub struct Section {
    title: String,
    children: Vec<Dom>,
    default_open: bool,
    dense: bool,
    right: Option<Dom>,
}

impl Section {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            children: Vec::new(),
            default_open: true,
            dense: false,
            right: None,
        }
    }
    pub fn child(mut self, child: Dom) -> Self {
        self.children.push(child);
        self
    }
    pub fn children(mut self, children: impl IntoIterator<Item = Dom>) -> Self {
        self.children.extend(children);
        self
    }
    pub fn default_open(mut self, open: bool) -> Self {
        self.default_open = open;
        self
    }
    pub fn dense(mut self, dense: bool) -> Self {
        self.dense = dense;
        self
    }
    pub fn right(mut self, right: Dom) -> Self {
        self.right = Some(right);
        self
    }

    pub fn render(self) -> Dom {
        let open = Mutable::new(self.default_open);
        let body_pad = if self.dense {
            "2px 12px 12px"
        } else {
            "2px 12px 13px"
        };

        html!("div", {
            .style("border-bottom", "1px solid var(--line-soft)")
            .child(html!("div", {
                .class("t")
                .style("display", "flex")
                .style("align-items", "center")
                .style("gap", "7px")
                .style("cursor", "pointer")
                .style("user-select", "none")
                .style("height", "32px")
                .style("padding", "0 12px")
                .event(clone!(open => move |_: events::Click| open.set(!open.get())))
                // The chevron rotates rather than swaps so it stays mounted
                // (Dom isn't Clone). Wrap in a div so the rotation can be a
                // style_signal driven by `open`.
                .child(html!("div", {
                    .style("display", "flex")
                    .style("transition", "transform .14s ease")
                    .style_signal("transform", open.signal().map(|o| if o { "rotate(90deg)" } else { "none" }))
                    .child(Icon::new("chevron").size(13.0).stroke_width(1.6).color("var(--text-3)").render())
                }))
                .child(html!("span", { .class("kicker").text(&self.title) }))
                .child(html!("div", {
                    .style("margin-left", "auto")
                    .style("display", "flex")
                    .style("align-items", "center")
                    .event(|e: events::Click| e.stop_propagation())
                    .apply(|b| match self.right {
                        Some(r) => b.child(r),
                        None => b,
                    })
                }))
            }))
            // Body is mounted once and display-toggled (the prototype unmounts,
            // but Dom isn't Clone so a signal that re-fires can't rebuild it;
            // display-toggle is the idiomatic dominator equivalent).
            .child(html!("div", {
                .style_signal("display", open.signal().map(|o| if o { "flex" } else { "none" }))
                .style("padding", body_pad)
                .style("flex-direction", "column")
                .style("gap", "var(--gap)")
                .children(self.children)
            }))
        })
    }
}

/// A label + control row: a `84px 1fr` grid with an ellipsized label.
pub fn row(label: impl Into<String>, control: Dom) -> Dom {
    html!("div", {
        .style("display", "grid")
        .style("grid-template-columns", "84px 1fr")
        .style("align-items", "center")
        .style("gap", "8px")
        .style("min-height", "var(--row-h)")
        .child(html!("span", {
            .style("font-size", "12px")
            .style("color", "var(--text-1)")
            .style("white-space", "nowrap")
            .style("overflow", "hidden")
            .style("text-overflow", "ellipsis")
            .text(&label.into())
        }))
        .child(html!("div", { .style("min-width", "0").child(control) }))
    })
}

/// A non-collapsing titled group used inside drawers / panels.
pub struct DrawerSection {
    title: String,
    children: Vec<Dom>,
    right: Option<Dom>,
}

impl DrawerSection {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            children: Vec::new(),
            right: None,
        }
    }
    pub fn child(mut self, child: Dom) -> Self {
        self.children.push(child);
        self
    }
    pub fn right(mut self, right: Dom) -> Self {
        self.right = Some(right);
        self
    }

    pub fn render(self) -> Dom {
        html!("div", {
            .style("border-bottom", "1px solid var(--line-soft)")
            .style("padding", "13px 16px")
            .child(html!("div", {
                .style("display", "flex")
                .style("align-items", "center")
                .style("margin-bottom", "10px")
                .child(html!("span", { .class("kicker").text(&self.title) }))
                .child(html!("div", {
                    .style("margin-left", "auto")
                    .apply(|b| match self.right {
                        Some(r) => b.child(r),
                        None => b,
                    })
                }))
            }))
            .child(html!("div", {
                .style("display", "flex")
                .style("flex-direction", "column")
                .style("gap", "var(--gap)")
                .children(self.children)
            }))
        })
    }
}
