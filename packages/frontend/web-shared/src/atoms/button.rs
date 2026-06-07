//! Prototype buttons — `Btn` (ghost / quiet / primary / solid, sm/md/lg) and
//! `IconBtn` (square icon action with optional label, active + danger states).
//! Hover state is a per-button `Mutable<bool>` so the background + text color
//! flip together (CSS `:hover` can't express the variant-specific color pairs
//! as cleanly).

use crate::atoms::icon::Icon;
use crate::prelude::*;

/// Visual variant for [`Btn`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BtnVariant {
    /// Transparent with a soft outline; the default panel button.
    Ghost,
    /// Transparent, no outline — for low-emphasis inline actions.
    Quiet,
    /// Accent-filled primary call-to-action.
    Primary,
    /// Filled neutral (elevated surface) with a line border.
    Solid,
}

/// Button size.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BtnSize {
    Sm,
    Md,
    Lg,
}

impl BtnSize {
    fn height(self) -> &'static str {
        match self {
            BtnSize::Sm => "26px",
            BtnSize::Md => "30px",
            BtnSize::Lg => "34px",
        }
    }
}

struct VariantStyle {
    bg: &'static str,
    bg_hover: &'static str,
    fg: &'static str,
    fg_hover: &'static str,
    bd: &'static str,
    fw: &'static str,
}

fn variant_style(v: BtnVariant) -> VariantStyle {
    match v {
        BtnVariant::Primary => VariantStyle {
            bg: "var(--accent)",
            bg_hover: "var(--accent-bright)",
            fg: "oklch(0.16 0.02 255)",
            fg_hover: "oklch(0.16 0.02 255)",
            bd: "transparent",
            fw: "600",
        },
        BtnVariant::Solid => VariantStyle {
            bg: "var(--bg-2)",
            bg_hover: "var(--bg-active)",
            fg: "var(--text-0)",
            fg_hover: "var(--text-0)",
            bd: "var(--line)",
            fw: "540",
        },
        BtnVariant::Ghost => VariantStyle {
            bg: "transparent",
            bg_hover: "var(--bg-hover)",
            fg: "var(--text-1)",
            fg_hover: "var(--text-0)",
            bd: "var(--line-soft)",
            fw: "520",
        },
        BtnVariant::Quiet => VariantStyle {
            bg: "transparent",
            bg_hover: "var(--bg-hover)",
            fg: "var(--text-1)",
            fg_hover: "var(--text-0)",
            bd: "transparent",
            fw: "520",
        },
    }
}

/// A text button with optional leading icon + trailing children.
pub struct Btn {
    label: Option<String>,
    icon: Option<String>,
    variant: BtnVariant,
    size: BtnSize,
    full: bool,
    disabled: bool,
    title: Option<String>,
    trailing: Vec<Dom>,
    on_click: Option<Box<dyn FnMut()>>,
}

impl Btn {
    pub fn new() -> Self {
        Self {
            label: None,
            icon: None,
            variant: BtnVariant::Ghost,
            size: BtnSize::Md,
            full: false,
            disabled: false,
            title: None,
            trailing: Vec::new(),
            on_click: None,
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
    pub fn full(mut self, full: bool) -> Self {
        self.full = full;
        self
    }
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
    pub fn title(mut self, t: impl Into<String>) -> Self {
        self.title = Some(t.into());
        self
    }
    /// Append a trailing child (e.g. a chevron for a dropdown trigger).
    pub fn push(mut self, child: Dom) -> Self {
        self.trailing.push(child);
        self
    }
    pub fn on_click(mut self, f: impl FnMut() + 'static) -> Self {
        self.on_click = Some(Box::new(f));
        self
    }

    pub fn render(self) -> Dom {
        let st = variant_style(self.variant);
        let disabled = self.disabled;
        let hover = Mutable::new(false);
        let mut on_click = self.on_click;

        let mut children: Vec<Dom> = Vec::new();
        if let Some(name) = self.icon {
            children.push(Icon::new(name).size(15.0).render());
        }
        if let Some(label) = self.label {
            children.push(html!("span", { .text(&label) }));
        }
        children.extend(self.trailing);

        html!("button", {
            .class("t")
            .class("focusring")
            .apply(|b| match self.title {
                Some(t) => b.attr("title", &t),
                None => b,
            })
            .style("display", "inline-flex")
            .style("align-items", "center")
            .style("justify-content", "center")
            .style("gap", "7px")
            .style("height", self.size.height())
            .style("padding", "0 13px")
            .style("width", if self.full { "100%" } else { "auto" })
            .style("border-radius", "var(--r2)")
            .style("border-style", "solid")
            .style("border-width", "1px")
            .style("border-color", st.bd)
            .style("font-size", "12.5px")
            .style("font-weight", st.fw)
            .style("white-space", "nowrap")
            .style("cursor", if disabled { "not-allowed" } else { "pointer" })
            .style("opacity", if disabled { "0.42" } else { "1" })
            .style_signal("background", hover.signal().map(move |h| {
                if h && !disabled { st.bg_hover } else { st.bg }
            }))
            .style_signal("color", hover.signal().map(move |h| {
                if h && !disabled { st.fg_hover } else { st.fg }
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
            .children(children)
        })
    }
}

impl Default for Btn {
    fn default() -> Self {
        Self::new()
    }
}

/// A square icon button (28px) with optional inline label, `active` (accent)
/// and `danger` states.
pub struct IconBtn {
    icon: Option<String>,
    size: f64,
    label: Option<String>,
    title: Option<String>,
    active: bool,
    danger: bool,
    extra_style: Vec<(String, String)>,
    on_click: Option<Box<dyn FnMut()>>,
}

impl IconBtn {
    pub fn new(icon: impl Into<String>) -> Self {
        Self {
            icon: Some(icon.into()),
            size: 15.0,
            label: None,
            title: None,
            active: false,
            danger: false,
            extra_style: Vec::new(),
            on_click: None,
        }
    }
    pub fn size(mut self, size: f64) -> Self {
        self.size = size;
        self
    }
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
    pub fn title(mut self, t: impl Into<String>) -> Self {
        self.title = Some(t.into());
        self
    }
    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }
    pub fn danger(mut self, danger: bool) -> Self {
        self.danger = danger;
        self
    }
    pub fn style(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.extra_style.push((k.into(), v.into()));
        self
    }
    pub fn on_click(mut self, f: impl FnMut() + 'static) -> Self {
        self.on_click = Some(Box::new(f));
        self
    }

    pub fn render(self) -> Dom {
        let hover = Mutable::new(false);
        let active = self.active;
        let danger = self.danger;
        let has_label = self.label.is_some();
        let mut on_click = self.on_click;
        let extra = self.extra_style;

        let mut children: Vec<Dom> = Vec::new();
        if let Some(name) = self.icon {
            children.push(Icon::new(name).size(self.size).render());
        }
        if let Some(label) = self.label {
            children.push(html!("span", { .text(&label) }));
        }

        html!("button", {
            .class("t")
            .class("focusring")
            .apply(|b| match self.title {
                Some(t) => b.attr("title", &t),
                None => b,
            })
            .style("display", "inline-flex")
            .style("align-items", "center")
            .style("justify-content", "center")
            .style("gap", "6px")
            .style("height", "28px")
            .style("min-width", "28px")
            .style("padding", if has_label { "0 9px" } else { "0" })
            .style("border", "1px solid transparent")
            .style("border-radius", "var(--r2)")
            .style("cursor", "pointer")
            .style("font-size", "12px")
            .style("font-weight", "520")
            .style_signal("background", hover.signal().map(move |h| {
                if active { "var(--accent-ghost)" } else if h { "var(--bg-hover)" } else { "transparent" }
            }))
            .style_signal("color", hover.signal().map(move |h| {
                if danger { "var(--danger)" }
                else if active { "var(--accent-bright)" }
                else if h { "var(--text-0)" }
                else { "var(--text-1)" }
            }))
            .apply(move |mut b| {
                for (k, v) in &extra {
                    b = b.style(k.as_str(), v.as_str());
                }
                b
            })
            .event(clone!(hover => move |_: events::MouseEnter| hover.set_neq(true)))
            .event(clone!(hover => move |_: events::MouseLeave| hover.set_neq(false)))
            .event(move |_: events::Click| {
                if let Some(f) = on_click.as_mut() {
                    f();
                }
            })
            .children(children)
        })
    }
}
