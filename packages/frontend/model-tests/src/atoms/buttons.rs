use crate::prelude::*;
use std::pin::Pin;
use web_sys::HtmlElement;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ButtonSize {
    Sm,
    Lg,
    Xlg,
}

impl ButtonSize {
    pub fn text_size_class(self) -> &'static str {
        match self {
            Self::Sm => FontSize::Sm.class(),
            Self::Lg => FontSize::Lg.class(),
            Self::Xlg => FontSize::Xlg.class(),
        }
    }

    pub fn container_class(self) -> &'static str {
        static DEFAULT_CLASS: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("padding", "0.625rem 1.875rem")
            }
        });

        static SM_CLASS: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("padding", "0.375rem 1.25rem")
            }
        });

        match self {
            Self::Sm => &SM_CLASS,
            _ => &DEFAULT_CLASS,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ButtonColor {
    Primary,
    Red,
}

impl ButtonColor {
    pub fn bg_class(&self, style: ButtonStyle) -> &'static str {
        match style {
            ButtonStyle::Solid => match self {
                Self::Primary => ColorBackground::ButtonPrimary.class(),
                Self::Red => ColorBackground::ButtonRed.class(),
            },
            ButtonStyle::Outline => ColorBackground::Initial.class(),
        }
    }

    pub fn bg_hover_class(&self, style: ButtonStyle) -> &'static str {
        match style {
            ButtonStyle::Solid => match self {
                Self::Primary => ColorBackground::ButtonPrimaryHover.class(),
                Self::Red => ColorBackground::ButtonRedHover.class(),
            },
            ButtonStyle::Outline => ColorBackground::Initial.class(),
        }
    }

    pub fn border_class(&self, style: ButtonStyle) -> &'static str {
        match style {
            ButtonStyle::Solid => ColorBorder::Initial.class(),
            ButtonStyle::Outline => match self {
                Self::Primary => ColorBorder::ButtonOutlinePrimary.class(),
                Self::Red => ColorBorder::ButtonOutlineRed.class(),
            },
        }
    }

    pub fn border_hover_class(&self, style: ButtonStyle) -> &'static str {
        match style {
            ButtonStyle::Solid => ColorBorder::Initial.class(),
            ButtonStyle::Outline => match self {
                Self::Primary => ColorBorder::ButtonOutlinePrimaryHover.class(),
                Self::Red => ColorBorder::ButtonOutlineRedHover.class(),
            },
        }
    }

    pub fn color_class(&self, style: ButtonStyle) -> &'static str {
        match style {
            ButtonStyle::Solid => match self {
                Self::Primary => ColorText::ButtonPrimary.class(),
                Self::Red => ColorText::ButtonPrimary.class(),
            },
            ButtonStyle::Outline => match self {
                Self::Primary => ColorText::ButtonOutlinePrimary.class(),
                Self::Red => ColorText::ButtonOutlineRed.class(),
            },
        }
    }

    pub fn color_hover_class(&self, style: ButtonStyle) -> &'static str {
        match style {
            ButtonStyle::Solid => match self {
                Self::Primary => ColorText::ButtonPrimary.class(),
                Self::Red => ColorText::ButtonPrimary.class(),
            },
            ButtonStyle::Outline => match self {
                Self::Primary => ColorText::ButtonOutlinePrimaryHover.class(),
                Self::Red => ColorText::ButtonOutlineRedHover.class(),
            },
        }
    }

    pub fn bg_disabled_class(self, style: ButtonStyle) -> &'static str {
        match style {
            ButtonStyle::Solid => ColorBackground::ButtonDisabled.class(),
            ButtonStyle::Outline => ColorBackground::Initial.class(),
        }
    }

    pub fn border_disabled_class(self, style: ButtonStyle) -> &'static str {
        match style {
            ButtonStyle::Solid => ColorBorder::Initial.class(),
            ButtonStyle::Outline => ColorBorder::ButtonDisabled.class(),
        }
    }

    pub fn color_disabled_class(self, style: ButtonStyle) -> &'static str {
        match style {
            ButtonStyle::Solid => match self {
                Self::Primary => ColorText::ButtonPrimary.class(),
                Self::Red => ColorText::ButtonPrimary.class(),
            },
            ButtonStyle::Outline => ColorBackground::Initial.class(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ButtonStyle {
    Solid,
    Outline,
}

pub struct Button {
    size: ButtonSize,
    style: ButtonStyle,
    color: ButtonColor,
    text: String,
    disabled_signal: Option<Pin<Box<dyn Signal<Item = bool>>>>,
    on_click: Option<Box<dyn FnMut()>>,
    link: Option<String>,
    content_before: Option<Dom>,
    content_after: Option<Dom>,
    mixin: Option<Box<dyn MixinFnOnce<HtmlElement>>>,
}

impl Button {
    pub fn new() -> Self {
        Self {
            size: ButtonSize::Lg,
            style: ButtonStyle::Solid,
            color: ButtonColor::Primary,
            text: "".to_string(),
            content_before: None,
            content_after: None,
            disabled_signal: None,
            on_click: None,
            mixin: None,
            link: None,
        }
    }

    pub fn with_text(mut self, text: impl ToString) -> Self {
        self.text = text.to_string();
        self
    }

    pub fn with_content_before(mut self, content: Dom) -> Self {
        self.content_before = Some(content);
        self
    }

    pub fn with_content_after(mut self, content: Dom) -> Self {
        self.content_after = Some(content);
        self
    }

    pub fn with_style(mut self, style: ButtonStyle) -> Self {
        self.style = style;
        self
    }

    pub fn with_link(mut self, link: impl ToString) -> Self {
        self.link = Some(link.to_string());
        self
    }

    pub fn with_size(mut self, size: ButtonSize) -> Self {
        self.size = size;
        self
    }

    pub fn with_color(mut self, color: ButtonColor) -> Self {
        self.color = color;
        self
    }

    pub fn with_disabled_signal(
        mut self,
        disabled_signal: impl Signal<Item = bool> + 'static,
    ) -> Self {
        self.disabled_signal = Some(Box::pin(disabled_signal));
        self
    }

    pub fn with_on_click(mut self, on_click: impl FnMut() + 'static) -> Self {
        self.on_click = Some(Box::new(on_click));
        self
    }

    pub fn with_mixin(mut self, mixin: impl MixinFnOnce<HtmlElement> + 'static) -> Self {
        self.mixin = Some(Box::new(mixin));
        self
    }

    pub fn render(self) -> Dom {
        static CLASS: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("display", "inline-flex")
                .style("justify-content", "center")
                .style("align-items", "center")
                .style("gap", "0.625rem")
                .style("border-radius", "0.25rem")
                .style("width", "fit-content")
            }
        });

        static BORDER_CLASS: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("border-width", "1px")
                .style("border-style", "solid")
            }
        });

        let Self {
            size,
            color,
            text,
            disabled_signal,
            content_before,
            content_after,
            mut on_click,
            style,
            mixin,
            link,
        } = self;

        let hovering = Mutable::new(false);

        // doing this instead of a Broadcaster because we want to:
        // 1. prevent the on_click handler being called if disabled signal is true
        // 2. show cursor style of not-allowed if disabled signal is true (so setting pointer-events: none doesn't work here)
        let disabled = Mutable::new(false);

        let neither_hover_nor_disabled_signal = || {
            map_ref! {
                let disabled = disabled.signal(),
                let hovering = hovering.signal() => {
                    !*disabled && !*hovering
                }
            }
        };

        let hover_but_not_disabled_signal = || {
            map_ref! {
                let disabled = disabled.signal(),
                let hovering = hovering.signal() => {
                    !*disabled && *hovering
                }
            }
        };

        let cursor_signal = map_ref! {
            let disabled = disabled.signal(),
            let hovering = hovering.signal() => {
                if *disabled {
                    "not-allowed"
                } else if *hovering {
                    "pointer"
                } else {
                    "auto"
                }
            }
        };

        let ret = html!("div", {
            .apply_if(disabled_signal.is_some(), clone!(disabled => move |dom| {
                dom
                    .future(disabled_signal.unwrap_throw().for_each(clone!(disabled => move |is_disabled| {
                        clone!(disabled => async move {
                            disabled.set_neq(is_disabled);
                        })
                    })))
            }))
            .class([&*USER_SELECT_NONE, &*CLASS, size.container_class(), size.text_size_class()])
            .apply(set_on_hover(&hovering))
            .style_signal("cursor", cursor_signal)
            .apply_if(style == ButtonStyle::Outline, |dom| {
                dom.class(&*BORDER_CLASS)
            })
            .class_signal([color.bg_class(style), color.border_class(style)], neither_hover_nor_disabled_signal())
            .class_signal([color.bg_hover_class(style), color.border_hover_class(style)], hover_but_not_disabled_signal())
            .class_signal([color.bg_disabled_class(style), color.border_disabled_class(style)], disabled.signal())
            .apply(handle_on_click(clone!(disabled => move || {
                if !disabled.get() {
                    if let Some(on_click) = &mut on_click {
                        on_click();
                    }
                }
            })))
            .apply_if(mixin.is_some(), |dom| {
                mixin.unwrap_throw()(dom)
            })
            .apply_if(content_before.is_some(), |dom| {
                dom.child(content_before.unwrap_throw())
            })
            .child(html!("div", {
                    .class_signal(color.color_disabled_class(style), disabled.signal())
                    .class_signal(color.color_hover_class(style), hover_but_not_disabled_signal())
                    .class_signal(color.color_class(style), neither_hover_nor_disabled_signal())
                    .text(&text)
            }))
            .apply_if(content_after.is_some(), |dom| {
                dom.child(content_after.unwrap_throw())
            })
        });

        match link {
            Some(link) => {
                link!(link, {
                    .child(ret)
                })
            }
            None => ret,
        }
    }
}
