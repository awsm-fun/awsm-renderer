//! Overlay primitives from `ui-extra.jsx`: `Popup`, `MenuItem`, `MenuSep`,
//! `DropButton`, `ModalCard` (the prototype's generic `Modal`), `RightDrawer`,
//! and `ContextMenu`.
//!
//! These are fixed-position layers. The prototype mounts/unmounts them via
//! React conditional rendering; here the *caller* controls mounting (typically
//! `child_signal` over an open-state `Mutable`), and each layer renders a
//! full-viewport backdrop that closes on click. `ModalCard` is named to avoid
//! colliding with [`super::modal::Modal`] (the app-level panic/error host).

use std::cell::RefCell;
use std::rc::Rc;

use crate::atoms::button::{Btn, BtnSize, BtnVariant};
use crate::atoms::icon::Icon;
use crate::prelude::*;

/// A dismiss callback handed to popup/menu builders so an item can close the
/// overlay before running its action.
pub type Close = Rc<RefCell<Box<dyn FnMut()>>>;

fn make_close(f: impl FnMut() + 'static) -> Close {
    Rc::new(RefCell::new(Box::new(f)))
}
fn fire(close: &Close) {
    (close.borrow_mut())();
}

/// The on-screen rect of a trigger, used to anchor a [`popup`].
#[derive(Clone, Copy)]
pub struct AnchorRect {
    pub left: f64,
    pub right: f64,
    pub top: f64,
    pub bottom: f64,
    pub width: f64,
}

impl AnchorRect {
    fn from_dom_rect(r: &web_sys::DomRect) -> Self {
        Self {
            left: r.left(),
            right: r.right(),
            top: r.top(),
            bottom: r.bottom(),
            width: r.width(),
        }
    }
}

/// Horizontal alignment of a popup relative to its anchor.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
}

/// A fixed popup anchored under a trigger, with a click-catching backdrop.
pub fn popup(
    anchor: AnchorRect,
    align: Align,
    width: Option<f64>,
    max_h: f64,
    on_close: impl FnMut() + 'static,
    children: Vec<Dom>,
) -> Dom {
    let close = make_close(on_close);
    let w = width.unwrap_or(220.0);
    let left = match align {
        Align::Right => anchor.right - w,
        Align::Left => anchor.left,
    };
    // Clamp horizontally so a popup anchored near the right edge (e.g. the
    // top-right overflow menu) doesn't spill off-screen — keep its right edge
    // within the viewport, then its left edge ≥ 8px.
    let viewport_w = web_sys::window()
        .and_then(|w| w.inner_width().ok())
        .and_then(|v| v.as_f64())
        .unwrap_or(1280.0);
    let left = left.min(viewport_w - w - 8.0).max(8.0);
    // Open upward when the trigger sits in the lower part of the window (e.g. the
    // viewport's bottom-right camera dropdown), so the menu doesn't spill off the
    // bottom; otherwise open downward as usual.
    let viewport_h = web_sys::window()
        .and_then(|w| w.inner_height().ok())
        .and_then(|v| v.as_f64())
        .unwrap_or(800.0);
    let open_up = anchor.bottom > viewport_h * 0.65;

    html!("div", {
        // backdrop
        .child(html!("div", {
            .style("position", "fixed")
            .style("inset", "0")
            .style("z-index", "250")
            .event(clone!(close => move |_: events::Click| fire(&close)))
            .event(clone!(close => move |e: events::ContextMenu| { e.prevent_default(); fire(&close); }))
        }))
        .child(html!("div", {
            .style("position", "fixed")
            .style("left", &format!("{left}px"))
            .apply(|b| if open_up {
                b.style("bottom", format!("{}px", (viewport_h - anchor.top + 4.0).max(8.0)))
            } else {
                b.style("top", format!("{}px", anchor.bottom + 4.0))
            })
            .apply(|b| match width {
                Some(w) => b.style("width", format!("{w}px")),
                None => b,
            })
            .style("min-width", "180px")
            .style("max-height", &format!("{max_h}px"))
            .style("overflow-y", "auto")
            .style("z-index", "251")
            .style("background", "var(--bg-2)")
            .style("border", "1px solid var(--line)")
            .style("border-radius", "var(--r3)")
            .style("box-shadow", "var(--shadow-3)")
            .style("padding", "5px")
            .children(children)
        }))
    })
}

/// A menu row. `on_click` fires the action; callers that open it from a popup
/// should close the popup inside that callback.
pub struct MenuItem {
    label: String,
    icon: Option<String>,
    danger: bool,
    checked: Option<bool>,
    hint: Option<String>,
    disabled: bool,
    on_click: Option<Box<dyn FnMut()>>,
}

impl MenuItem {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            icon: None,
            danger: false,
            checked: None,
            hint: None,
            disabled: false,
            on_click: None,
        }
    }
    pub fn icon(mut self, name: impl Into<String>) -> Self {
        self.icon = Some(name.into());
        self
    }
    pub fn danger(mut self, danger: bool) -> Self {
        self.danger = danger;
        self
    }
    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self
    }
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
    pub fn on_click(mut self, f: impl FnMut() + 'static) -> Self {
        self.on_click = Some(Box::new(f));
        self
    }

    pub fn render(self) -> Dom {
        let hover = Mutable::new(false);
        let danger = self.danger;
        let disabled = self.disabled;
        let mut on_click = self.on_click;

        html!("button", {
            .class("t")
            .attr("type", "button")
            .style("display", "flex")
            .style("align-items", "center")
            .style("gap", "9px")
            .style("width", "100%")
            .style("height", "30px")
            .style("padding", "0 9px")
            .style("border-style", "none")
            .style("border-radius", "var(--r1)")
            .style("text-align", "left")
            .style("font-size", "12.5px")
            .style("cursor", if disabled { "default" } else { "pointer" })
            .style_signal("background", hover.signal().map(move |h| {
                if h && !disabled { "var(--bg-hover)" } else { "transparent" }
            }))
            .style_signal("color", hover.signal().map(move |h| {
                if disabled { "var(--text-3)" }
                else if danger { "var(--danger)" }
                else if h { "var(--text-0)" }
                else { "var(--text-1)" }
            }))
            .event(clone!(hover => move |_: events::MouseEnter| hover.set_neq(true)))
            .event(clone!(hover => move |_: events::MouseLeave| hover.set_neq(false)))
            .event(move |_: events::Click| {
                if !disabled {
                    if let Some(f) = on_click.as_mut() {
                        f();
                    }
                }
            })
            // checkmark gutter
            .apply(|b| match self.checked {
                Some(c) => b.child(html!("span", {
                    .style("width", "14px")
                    .style("display", "flex")
                    .style("color", "var(--accent-bright)")
                    .apply(move |bb| if c {
                        bb.child(Icon::new("check").size(13.0).render())
                    } else {
                        bb
                    })
                })),
                None => b,
            })
            .apply(|b| match self.icon {
                Some(name) => b.child(
                    Icon::new(name).size(15.0)
                        .color(if danger { "var(--danger)" } else { "var(--text-2)" })
                        .render(),
                ),
                None => b,
            })
            .child(html!("span", { .style("flex", "1").text(&self.label) }))
            .apply(|b| match self.hint {
                Some(h) => b.child(html!("span", {
                    .class("mono").style("font-size", "10px").style("color", "var(--text-3)").text(&h)
                })),
                None => b,
            })
        })
    }
}

/// A 1px menu separator.
pub fn menu_sep() -> Dom {
    html!("div", {
        .style("height", "1px")
        .style("background", "var(--line-soft)")
        .style("margin", "5px 6px")
    })
}

/// A trigger button that opens a menu of [`MenuItem`]s on click. Items are
/// supplied by a builder closure so each can capture the close action.
pub struct DropButton {
    label: Option<String>,
    icon: Option<String>,
    variant: BtnVariant,
    size: BtnSize,
    chevron: bool,
    // Builds the menu rows on each open (handed a `Close` callback). `Fn` (not
    // `FnOnce`) so reopening the dropdown rebuilds fresh rows.
    items: Option<Rc<dyn Fn(Close) -> Vec<Dom>>>,
}

impl DropButton {
    pub fn new() -> Self {
        Self {
            label: None,
            icon: None,
            variant: BtnVariant::Ghost,
            size: BtnSize::Sm,
            chevron: true,
            items: None,
        }
    }
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
    pub fn icon(mut self, name: impl Into<String>) -> Self {
        self.icon = Some(name.into());
        self
    }
    pub fn variant(mut self, v: BtnVariant) -> Self {
        self.variant = v;
        self
    }
    pub fn size(mut self, s: BtnSize) -> Self {
        self.size = s;
        self
    }
    pub fn chevron(mut self, chevron: bool) -> Self {
        self.chevron = chevron;
        self
    }
    /// `items` builds the menu rows on each open; it is handed a `Close` it can
    /// call inside any item to dismiss the popup.
    pub fn items(mut self, f: impl Fn(Close) -> Vec<Dom> + 'static) -> Self {
        self.items = Some(Rc::new(f));
        self
    }

    pub fn render(self) -> Dom {
        let rect: Mutable<Option<AnchorRect>> = Mutable::new(None);
        let items = self.items;

        let mut trigger = Btn::new().variant(self.variant).size(self.size);
        if let Some(l) = self.label {
            trigger = trigger.label(l);
        }
        if let Some(ic) = self.icon {
            trigger = trigger.icon(ic);
        }
        if self.chevron {
            trigger = trigger.push(
                Icon::new("chevdown")
                    .size(12.0)
                    .style("margin-left", "-1px")
                    .style("opacity", "0.7")
                    .render(),
            );
        }

        html!("span", {
            .style("display", "inline-flex")
            .style("position", "relative")
            .with_node!(elem => {
                .child(trigger.on_click(clone!(rect => move || {
                    let r = elem.get_bounding_client_rect();
                    rect.set(Some(AnchorRect::from_dom_rect(&r)));
                })).render())
            })
            .child_signal(rect.signal().map(clone!(rect, items => move |maybe| {
                maybe.map(clone!(rect, items => move |anchor| {
                    let close = {
                        let rect = rect.clone();
                        make_close(move || rect.set(None))
                    };
                    let rows = match &items {
                        Some(f) => f(close.clone()),
                        None => Vec::new(),
                    };
                    popup(anchor, Align::Left, None, 360.0, clone!(rect => move || rect.set(None)), rows)
                }))
            })))
        })
    }
}

impl Default for DropButton {
    fn default() -> Self {
        Self::new()
    }
}

/// A generic centered modal card. The caller controls mounting (e.g. via
/// `child_signal`). Named `ModalCard` to avoid colliding with the app-level
/// [`super::modal::Modal`] error/panic host.
pub struct ModalCard {
    title: String,
    subtitle: Option<String>,
    width: f64,
    body: Vec<Dom>,
    footer: Vec<Dom>,
}

impl ModalCard {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            subtitle: None,
            width: 460.0,
            body: Vec::new(),
            footer: Vec::new(),
        }
    }
    pub fn subtitle(mut self, s: impl Into<String>) -> Self {
        self.subtitle = Some(s.into());
        self
    }
    pub fn width(mut self, w: f64) -> Self {
        self.width = w;
        self
    }
    pub fn child(mut self, d: Dom) -> Self {
        self.body.push(d);
        self
    }
    pub fn footer(mut self, d: Dom) -> Self {
        self.footer.push(d);
        self
    }

    pub fn render(self) -> Dom {
        let has_footer = !self.footer.is_empty();
        // ModalCard is the *content* of the global `Modal` host (see
        // `super::modal::Modal`). The host owns the backdrop, the fixed
        // centered container, the close button, the outer padding, and
        // `max-height`/scroll. ModalCard therefore renders strictly
        // IN-FLOW — a title header, a body, and an optional footer — and
        // must NOT add its own `position: fixed` backdrop or card.
        //
        // It used to render a fixed, self-centered card with its own
        // backdrop. Placed inside the host that double-wrapped every
        // editor modal: the host container had only out-of-flow children
        // so it collapsed to a ~40px strip, while the real card hid
        // behind the host's (higher z-index) backdrop. `width` is now a
        // `max-width` so narrow modals (confirmations) stay compact and
        // centered within the host container.
        html!("div", {
            .style("width", "100%")
            .style("max-width", &format!("{}px", self.width))
            .style("margin", "0 auto")
            .style("display", "flex")
            .style("flex-direction", "column")
            .child(html!("div", {
                .style("padding", "0 0 12px")
                .child(html!("div", {
                    .style("font-size", "15px").style("font-weight", "650").style("color", "var(--text-0)")
                    .text(&self.title)
                }))
                .apply(|b| match self.subtitle {
                    Some(s) => b.child(html!("div", {
                        .style("font-size", "12.5px").style("color", "var(--text-2)")
                        .style("margin-top", "4px").style("line-height", "1.45").text(&s)
                    })),
                    None => b,
                })
            }))
            .child(html!("div", {
                .children(self.body)
            }))
            .apply(move |b| if has_footer {
                b.child(html!("div", {
                    .style("display", "flex")
                    .style("justify-content", "flex-end")
                    .style("gap", "8px")
                    .style("padding", "16px 0 0")
                    .children(self.footer)
                }))
            } else {
                b
            })
        })
    }
}

/// A right-anchored slide drawer (settings / help).
pub struct RightDrawer {
    title: String,
    icon: Option<String>,
    width: f64,
    body: Vec<Dom>,
    footer: Vec<Dom>,
    on_close: Option<Box<dyn FnMut()>>,
}

impl RightDrawer {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            icon: None,
            width: 360.0,
            body: Vec::new(),
            footer: Vec::new(),
            on_close: None,
        }
    }
    pub fn icon(mut self, name: impl Into<String>) -> Self {
        self.icon = Some(name.into());
        self
    }
    pub fn width(mut self, w: f64) -> Self {
        self.width = w;
        self
    }
    pub fn child(mut self, d: Dom) -> Self {
        self.body.push(d);
        self
    }
    pub fn footer(mut self, d: Dom) -> Self {
        self.footer.push(d);
        self
    }
    pub fn on_close(mut self, f: impl FnMut() + 'static) -> Self {
        self.on_close = Some(Box::new(f));
        self
    }

    pub fn render(self) -> Dom {
        let close = make_close(self.on_close.unwrap_or_else(|| Box::new(|| {})));
        let has_footer = !self.footer.is_empty();
        html!("div", {
            .child(html!("div", {
                .style("position", "fixed")
                .style("inset", "0")
                .style("background", "oklch(0 0 0 / 0.4)")
                .style("z-index", "200")
                .event(clone!(close => move |_: events::Click| fire(&close)))
            }))
            .child(html!("div", {
                .style("position", "fixed")
                .style("top", "0")
                .style("right", "0")
                .style("bottom", "0")
                .style("width", &format!("{}px", self.width))
                .style("background", "var(--bg-1)")
                .style("border-left", "1px solid var(--line)")
                .style("box-shadow", "var(--shadow-3)")
                .style("z-index", "201")
                .style("display", "flex")
                .style("flex-direction", "column")
                .child(html!("div", {
                    .style("display", "flex")
                    .style("align-items", "center")
                    .style("height", "46px")
                    .style("padding", "0 10px 0 16px")
                    .style("border-bottom", "1px solid var(--line-soft)")
                    .style("flex", "0 0 auto")
                    .apply(|b| match self.icon {
                        Some(name) => b.child(Icon::new(name).size(16.0).color("var(--accent-bright)").render()),
                        None => b,
                    })
                    .child(html!("span", {
                        .style("font-size", "13.5px").style("font-weight", "650").style("margin-left", "8px")
                        .text(&self.title)
                    }))
                    .child(crate::atoms::button::IconBtn::new("minus").title("Close")
                        .style("margin-left", "auto")
                        .on_click(clone!(close => move || fire(&close))).render())
                }))
                .child(html!("div", {
                    .style("flex", "1").style("overflow-y", "auto").children(self.body)
                }))
                .apply(move |b| if has_footer {
                    b.child(html!("div", {
                        .style("border-top", "1px solid var(--line-soft)").style("padding", "12px").children(self.footer)
                    }))
                } else {
                    b
                })
            }))
        })
    }
}

/// A context menu opened at an arbitrary viewport point.
pub fn context_menu(x: f64, y: f64, on_close: impl FnMut() + 'static, rows: Vec<Dom>) -> Dom {
    let close = make_close(on_close);
    html!("div", {
        .child(html!("div", {
            .style("position", "fixed")
            .style("inset", "0")
            .style("z-index", "260")
            .event(clone!(close => move |_: events::Click| fire(&close)))
            .event(clone!(close => move |e: events::ContextMenu| { e.prevent_default(); fire(&close); }))
        }))
        .child(html!("div", {
            .style("position", "fixed")
            .style("left", &format!("{x}px"))
            .style("top", &format!("{y}px"))
            .style("z-index", "261")
            .style("min-width", "190px")
            .style("background", "var(--bg-2)")
            .style("border", "1px solid var(--line)")
            .style("border-radius", "var(--r3)")
            .style("box-shadow", "var(--shadow-3)")
            .style("padding", "5px")
            .children(rows)
        }))
    })
}

/// The simple bordered select (anchored popup of checked menu rows) from
/// `ui.jsx`. Bound to a `Mutable<String>` selected value.
pub fn select(selected: Mutable<String>, options: Vec<(String, String)>) -> Dom {
    let rect: Mutable<Option<AnchorRect>> = Mutable::new(None);
    let opts = Rc::new(options);
    let label_for = {
        let opts = opts.clone();
        move |v: &str| -> String {
            opts.iter()
                .find(|(val, _)| val == v)
                .map(|(_, l)| l.clone())
                .unwrap_or_else(|| "\u{2014}".into())
        }
    };

    // Outer container holds the clickable *trigger* and the popup as **siblings**.
    // The click handler lives on the trigger only — so a menu-item click (which
    // bubbles up to this container, not the trigger) can't re-toggle the popup and
    // swallow the selection.
    html!("div", {
        .style("position", "relative")
        .style("min-width", "0")
        .child(html!("div", {
            .class("t")
            .style("display", "flex")
            .style("align-items", "center")
            .style("height", "var(--row-h)")
            .style("background", "var(--bg-3)")
            .style("cursor", "pointer")
            .style("border-radius", "var(--r1)")
            .style("border-style", "solid")
            .style("border-width", "1px")
            .style("padding", "0 8px 0 9px")
            .style("min-width", "0")
            .style_signal("border-color", rect.signal().map(|r| if r.is_some() { "var(--accent-line)" } else { "var(--line-soft)" }))
            .style_signal("box-shadow", rect.signal().map(|r| if r.is_some() { "0 0 0 2px var(--accent-ghost)" } else { "none" }))
            .with_node!(elem => {
                .event(clone!(rect => move |_: events::Click| {
                    if rect.get().is_some() {
                        rect.set(None);
                    } else {
                        let r = elem.get_bounding_client_rect();
                        rect.set(Some(AnchorRect::from_dom_rect(&r)));
                    }
                }))
            })
            .child(html!("span", {
                .style("flex", "1")
                .style("min-width", "0")
                .style("font-size", "12.5px")
                .style("color", "var(--text-0)")
                .style("white-space", "nowrap")
                .style("overflow", "hidden")
                .style("text-overflow", "ellipsis")
                .text_signal(selected.signal_cloned().map(clone!(label_for => move |v| label_for(&v))))
            }))
            .child(Icon::new("chevdown").size(13.0).color("var(--text-3)").style("flex-shrink", "0").style("margin-left", "4px").render())
        }))
        .child_signal(rect.signal().map(move |maybe| {
            maybe.map(|anchor| {
                let width = anchor.width.max(150.0);
                let rows: Vec<Dom> = opts.iter().map(|(v, l)| {
                    let v = v.clone();
                    MenuItem::new(l.clone())
                        .checked(selected.get_cloned() == v)
                        .on_click(clone!(rect, selected, v => move || {
                            rect.set(None);
                            selected.set_neq(v.clone());
                        }))
                        .render()
                }).collect();
                popup(anchor, Align::Left, Some(width), 300.0, clone!(rect => move || rect.set(None)), rows)
            })
        }))
    })
}
