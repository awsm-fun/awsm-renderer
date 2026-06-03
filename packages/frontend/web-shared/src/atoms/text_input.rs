use crate::prelude::*;
use web_sys::HtmlInputElement;

pub struct TextInput {
    pub kind: TextInputKind,
    pub placeholder: Option<String>,
    pub autocomplete: Option<String>,
    pub name: Option<String>,
    pub required: bool,
    pub autofocus: bool,
    pub password_toggle_after: Option<Dom>,
    pub on_input: Option<Arc<dyn Fn(Option<String>)>>,
    pub initial_value: Option<String>,
    pub mixin: Option<Box<dyn MixinFnOnce<HtmlInputElement>>>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum TextInputKind {
    Email,
    Password,
    Text,
    Number,
    Date,
    Time,
}

impl Default for TextInput {
    fn default() -> Self {
        Self::new()
    }
}

impl TextInput {
    pub fn new() -> Self {
        Self {
            kind: TextInputKind::Text,
            placeholder: None,
            autocomplete: None,
            name: None,
            required: false,
            autofocus: false,
            password_toggle_after: None,
            on_input: None,
            initial_value: None,
            mixin: None,
        }
    }

    pub fn with_kind(mut self, kind: TextInputKind) -> Self {
        self.kind = kind;
        self
    }

    pub fn with_on_input(mut self, on_input: impl Fn(Option<String>) + 'static) -> Self {
        self.on_input = Some(Arc::new(on_input));
        self
    }

    pub fn with_placeholder(mut self, placeholder: impl ToString) -> Self {
        let placeholder = placeholder.to_string();
        self.placeholder = if placeholder.is_empty() {
            None
        } else {
            Some(placeholder)
        };
        self
    }

    pub fn with_autocomplete(mut self, autocomplete: impl ToString) -> Self {
        let autocomplete = autocomplete.to_string();
        self.autocomplete = if autocomplete.is_empty() {
            None
        } else {
            Some(autocomplete)
        };
        self
    }

    pub fn with_name(mut self, name: impl ToString) -> Self {
        let name = name.to_string();
        self.name = if name.is_empty() { None } else { Some(name) };
        self
    }

    pub fn with_required(mut self, required: bool) -> Self {
        self.required = required;
        self
    }

    /// Focus the input as soon as it mounts. Useful for modals and
    /// freshly-rendered forms — without it, the page-level focus
    /// stays where it was and the mouse cursor over the modal's
    /// (selectable) heading text shows a text I-beam, which reads
    /// as "stray caret on the heading."
    pub fn with_autofocus(mut self, autofocus: bool) -> Self {
        self.autofocus = autofocus;
        self
    }

    pub fn with_password_toggle_after(mut self, content: Dom) -> Self {
        self.password_toggle_after = Some(content);
        self
    }

    pub fn with_intial_value(mut self, value: impl ToString) -> Self {
        let initial_value = value.to_string();
        self.initial_value = if initial_value.is_empty() {
            None
        } else {
            Some(initial_value)
        };
        self
    }

    pub fn with_mixin(mut self, mixin: impl MixinFnOnce<HtmlInputElement> + 'static) -> Self {
        self.mixin = Some(Box::new(mixin));
        self
    }

    pub fn render(self) -> Dom {
        static CLASS: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("padding", "0.82rem 0.96rem")
                .style("border-radius", "0.82rem")
                .style("border-width", "1px")
                .style("border-style", "solid")
                .style("border-color", "rgba(112, 185, 255, 0.45)")
                .style("background", "rgba(7, 17, 34, 0.8)")
                .style("color", "#e9f1ff")
                .style("width", "100%")
                .style("max-width", "28rem")
                .style("outline", "none")
                .style("transition", "box-shadow 140ms ease, border-color 140ms ease, background 140ms ease")
                .pseudo!(":focus", {
                    .style("border-color", "rgba(114, 245, 255, 0.9)")
                    .style("box-shadow", "0 0 0 4px rgba(114, 245, 255, 0.22)")
                    .style("background", "rgba(9, 22, 42, 0.92)")
                })
                .pseudo!("::placeholder", {
                    .style("color", "#91a3c0")
                })
            }
        });

        let show_password = Mutable::new(false);

        let Self {
            kind,
            placeholder,
            autocomplete,
            name,
            required,
            autofocus,
            password_toggle_after,
            on_input,
            initial_value,
            mixin,
        } = self;

        html!("div", {
            .child(html!("input" => web_sys::HtmlInputElement, {
                .class(&*CLASS)
                .attrs!{
                    "spellcheck": "false",
                    "autocorrect": "off"
                }
                .attr_signal("type", show_password.signal().map(move |show_password| {
                    match kind {
                        TextInputKind::Email => "email",
                        TextInputKind::Password => if show_password { "text" } else {"password"},
                        TextInputKind::Text => "text",
                        TextInputKind::Number => "number",
                        TextInputKind::Date => "date",
                        TextInputKind::Time => "time",
                    }
                }))
                .apply_if(placeholder.is_some(), |dom| {
                    dom.attr("placeholder", &placeholder.unwrap_throw())
                })
                .apply_if(autocomplete.is_some(), |dom| {
                    dom.attr("autocomplete", &autocomplete.unwrap_throw())
                })
                .apply_if(name.is_some(), |dom| {
                    dom.attr("name", &name.unwrap_throw())
                })
                .apply_if(required, |dom| dom.attr("required", "true"))
                .apply_if(autofocus, |dom| {
                    // Both: HTML attribute (handles initial first-paint
                    // case browsers honor it) AND an explicit `.focus()`
                    // on insertion (covers the case where the input is
                    // mounted after page load — typical for modals,
                    // where the `autofocus` attribute alone is a no-op).
                    dom.attr("autofocus", "true")
                        .after_inserted(|elem: HtmlInputElement| {
                            let _ = elem.focus();
                        })
                })
                .apply_if(initial_value.is_some(), |dom| {
                    dom.attr("value", &initial_value.unwrap_throw())
                })

                .apply_if(mixin.is_some(), |dom| {
                    mixin.unwrap_throw()(dom)
                })

                .with_node!(elem => {
                    .apply_if(on_input.is_some(), move |dom| {
                        let on_input = on_input.unwrap_throw();
                        dom
                            .event(clone!(on_input => move |_:events::Input| {
                                let text = elem.value();
                                let text = if text.is_empty() {
                                    None
                                } else {
                                    Some(text)
                                };

                                on_input(text);
                            }))
                    })
                })
            }))

            .apply_if(kind == TextInputKind::Password, move |dom| {
                dom.child(html!("div", {
                    .style("margin-top", "0.5rem")
                    .style("display", "flex")
                    .style("justify-content", "space-between")
                    .style("align-items", "center")
                    .style("gap", "0.75rem")
                    .child(html!("div", {
                        .style("cursor", "pointer")
                        .class(FontSize::Md.class())
                        .class(&*USER_SELECT_NONE)
                        .style("color", "#91a3c0")
                        .text_signal(show_password.signal().map(|show_password| {
                            if show_password {
                                "Hide password".to_string()
                            } else {
                                "Show password".to_string()
                            }
                        }))
                        .event(clone!(show_password => move |_:events::Click| {
                            show_password.replace_with(|x| !*x);
                        }))
                    }))
                    .apply_if(password_toggle_after.is_some(), move |dom| {
                        dom.child(password_toggle_after.unwrap_throw())
                    })
                }))
            })
        })
    }
}
