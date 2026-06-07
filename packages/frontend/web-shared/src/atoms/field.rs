//! The prototype text input (`TextInput`) — a styled wrapper with an optional
//! leading icon, mono mode, placeholder, and an accent focus ring. Bound to a
//! caller-owned `Mutable<String>`.

use std::cell::RefCell;
use std::rc::Rc;

use crate::atoms::icon::Icon;
use crate::prelude::*;

type StrCb = Rc<RefCell<Box<dyn FnMut(String)>>>;

pub struct TextInput {
    value: Mutable<String>,
    placeholder: Option<String>,
    mono: bool,
    icon: Option<String>,
    on_change: Option<StrCb>,
}

impl TextInput {
    pub fn new(value: Mutable<String>) -> Self {
        Self {
            value,
            placeholder: None,
            mono: false,
            icon: None,
            on_change: None,
        }
    }
    pub fn placeholder(mut self, p: impl Into<String>) -> Self {
        self.placeholder = Some(p.into());
        self
    }
    pub fn mono(mut self, mono: bool) -> Self {
        self.mono = mono;
        self
    }
    pub fn icon(mut self, name: impl Into<String>) -> Self {
        self.icon = Some(name.into());
        self
    }
    pub fn on_change(mut self, f: impl FnMut(String) + 'static) -> Self {
        self.on_change = Some(Rc::new(RefCell::new(Box::new(f))));
        self
    }

    pub fn render(self) -> Dom {
        let foc = Mutable::new(false);
        let value = self.value;
        let on_change = self.on_change;

        html!("div", {
            .class("t")
            .style("display", "flex")
            .style("align-items", "center")
            .style("gap", "7px")
            .style("height", "var(--row-h)")
            .style("background", "var(--bg-3)")
            .style("border-radius", "var(--r1)")
            .style("border-style", "solid")
            .style("border-width", "1px")
            .style("padding", "0 9px")
            .style_signal("border-color", foc.signal().map(|f| if f { "var(--accent-line)" } else { "var(--line-soft)" }))
            .style_signal("box-shadow", foc.signal().map(|f| if f { "0 0 0 2px var(--accent-ghost)" } else { "none" }))
            .apply(|b| match self.icon {
                Some(name) => b.child(Icon::new(name).size(14.0).color("var(--text-3)").render()),
                None => b,
            })
            .child(html!("input" => web_sys::HtmlInputElement, {
                .apply(|b| if self.mono { b.class("mono") } else { b })
                .prop_signal("value", value.signal_cloned())
                .apply(|b| match self.placeholder {
                    Some(p) => b.attr("placeholder", &p),
                    None => b,
                })
                .style("width", "100%")
                .style("min-width", "0")
                .style("background", "transparent")
                .style("border-style", "none")
                .style("outline-style", "none")
                .style("color", "var(--text-0)")
                .style("font-size", "12.5px")
                .with_node!(input => {
                    .event(clone!(foc => move |_: events::Focus| foc.set_neq(true)))
                    .event(clone!(foc => move |_: events::Blur| foc.set_neq(false)))
                    .event(clone!(value => move |_: events::Input| {
                        let v = input.value();
                        value.set_neq(v.clone());
                        if let Some(cb) = &on_change {
                            (cb.borrow_mut())(v);
                        }
                    }))
                })
            }))
        })
    }
}
