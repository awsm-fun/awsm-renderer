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
                .style("padding", "0.72rem 1.28rem")
            }
        });

        static SM_CLASS: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("padding", "0.5rem 0.9rem")
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
    Cyan,
    Green,
}

impl ButtonColor {
    pub fn bg_class(&self, style: ButtonStyle) -> &'static str {
        match style {
            ButtonStyle::Solid => match self {
                Self::Primary | Self::Cyan | Self::Green => ColorBackground::ButtonPrimary.class(),
                Self::Red => ColorBackground::ButtonRed.class(),
            },
            ButtonStyle::Outline => ColorBackground::Initial.class(),
        }
    }

    pub fn bg_hover_class(&self, style: ButtonStyle) -> &'static str {
        match style {
            ButtonStyle::Solid => match self {
                Self::Primary | Self::Cyan | Self::Green => {
                    ColorBackground::ButtonPrimaryHover.class()
                }
                Self::Red => ColorBackground::ButtonRedHover.class(),
            },
            ButtonStyle::Outline => ColorBackground::Initial.class(),
        }
    }

    pub fn border_class(&self, style: ButtonStyle) -> &'static str {
        match style {
            ButtonStyle::Solid => ColorBorder::Initial.class(),
            ButtonStyle::Outline => match self {
                Self::Primary | Self::Cyan => ColorBorder::ButtonOutlinePrimary.class(),
                Self::Red => ColorBorder::ButtonOutlineRed.class(),
                Self::Green => ColorBorder::ButtonOutlineGreen.class(),
            },
        }
    }

    pub fn border_hover_class(&self, style: ButtonStyle) -> &'static str {
        match style {
            ButtonStyle::Solid => ColorBorder::Initial.class(),
            ButtonStyle::Outline => match self {
                Self::Primary | Self::Cyan => ColorBorder::ButtonOutlinePrimaryHover.class(),
                Self::Red => ColorBorder::ButtonOutlineRedHover.class(),
                Self::Green => ColorBorder::ButtonOutlineGreenHover.class(),
            },
        }
    }

    pub fn color_class(&self, style: ButtonStyle) -> &'static str {
        match style {
            ButtonStyle::Solid => ColorText::ButtonPrimary.class(),
            ButtonStyle::Outline => match self {
                Self::Primary | Self::Cyan => ColorText::ButtonOutlinePrimary.class(),
                Self::Red => ColorText::ButtonOutlineRed.class(),
                Self::Green => ColorText::ButtonOutlineGreen.class(),
            },
        }
    }

    pub fn color_hover_class(&self, style: ButtonStyle) -> &'static str {
        match style {
            ButtonStyle::Solid => ColorText::ButtonPrimary.class(),
            ButtonStyle::Outline => match self {
                Self::Primary | Self::Cyan => ColorText::ButtonOutlinePrimaryHover.class(),
                Self::Red => ColorText::ButtonOutlineRedHover.class(),
                Self::Green => ColorText::ButtonOutlineGreenHover.class(),
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
            ButtonStyle::Outline => ColorBorder::UnderlineSecondary.class(),
        }
    }

    pub fn color_disabled_class(self, style: ButtonStyle) -> &'static str {
        match style {
            ButtonStyle::Solid => ColorText::ButtonPrimary.class(),
            ButtonStyle::Outline => ColorText::Byline.class(),
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
    button_type: String,
    text: String,
    disabled_signal: Option<Pin<Box<dyn Signal<Item = bool>>>>,
    on_click: Option<Box<dyn FnMut()>>,
    link: Option<String>,
    content_before: Option<Dom>,
    content_after: Option<Dom>,
    mixin: Option<Box<dyn MixinFnOnce<HtmlElement>>>,
}

impl Default for Button {
    fn default() -> Self {
        Self::new()
    }
}

impl Button {
    pub fn new() -> Self {
        Self {
            size: ButtonSize::Lg,
            style: ButtonStyle::Solid,
            color: ButtonColor::Primary,
            button_type: "button".to_string(),
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

    pub fn with_button_type(mut self, button_type: impl ToString) -> Self {
        self.button_type = button_type.to_string();
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
                .style("gap", "0.55rem")
                .style("border-radius", "0.82rem")
                .style("width", "fit-content")
                .style("min-height", "2.7rem")
                .style("letter-spacing", "0.01em")
                .style("font-weight", "600")
                .style("transition", "all 170ms ease")
            }
        });

        static BORDER_CLASS: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("border-width", "1px")
                .style("border-style", "solid")
                .style("border-color", "transparent")
            }
        });

        let Self {
            size,
            color,
            button_type,
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

        let box_shadow_signal = map_ref! {
            let disabled = disabled.signal(),
            let hovering = hovering.signal() => {
                if *disabled {
                    "none"
                } else if *hovering {
                    "0 10px 28px rgba(26, 92, 168, 0.28)"
                } else {
                    "0 3px 10px rgba(9, 20, 36, 0.18)"
                }
            }
        };

        let opacity_signal = map_ref! {
            let disabled = disabled.signal() => {
                if *disabled { "0.62" } else { "1" }
            }
        };

        let filter_signal = map_ref! {
            let disabled = disabled.signal() => {
                if !*disabled {
                    "none"
                } else if style == ButtonStyle::Solid {
                    "grayscale(0.55) saturate(0.3) brightness(0.78)"
                } else {
                    "grayscale(0.45) saturate(0.35)"
                }
            }
        };

        let outline_disabled_background_signal = map_ref! {
            let disabled = disabled.signal() => {
                if style == ButtonStyle::Outline && *disabled {
                    "rgba(23, 35, 54, 0.58)"
                } else {
                    "initial"
                }
            }
        };

        let primary_solid_background_signal = || {
            map_ref! {
                let disabled = disabled.signal(),
                let hovering = hovering.signal() => {
                    if *disabled {
                        "linear-gradient(165deg, rgba(45, 55, 69, 0.95), rgba(36, 44, 57, 0.95))"
                    } else if *hovering {
                        "linear-gradient(165deg, rgba(47, 103, 159, 0.98), rgba(37, 79, 129, 0.98))"
                    } else {
                        "linear-gradient(165deg, rgba(39, 89, 139, 0.95), rgba(30, 66, 109, 0.95))"
                    }
                }
            }
        };

        let primary_solid_border_signal = || {
            map_ref! {
                let disabled = disabled.signal(),
                let hovering = hovering.signal() => {
                    if *disabled {
                        "rgba(122, 135, 153, 0.52)"
                    } else if *hovering {
                        "rgba(114, 245, 255, 0.6)"
                    } else {
                        "rgba(114, 245, 255, 0.46)"
                    }
                }
            }
        };

        let cyan_solid_background_signal = || {
            map_ref! {
                let disabled = disabled.signal() => {
                    if *disabled {
                        "linear-gradient(120deg, rgba(45, 55, 69, 0.3), rgba(36, 44, 57, 0.3))"
                    } else {
                        ChromeFill::ButtonCyan.value()
                    }
                }
            }
        };

        let cyan_solid_border_signal = || {
            map_ref! {
                let disabled = disabled.signal(),
                let hovering = hovering.signal() => {
                    if *disabled {
                        "rgba(122, 135, 153, 0.3)"
                    } else if *hovering {
                        ChromeColor::ButtonCyanBorderHover.value()
                    } else {
                        ChromeColor::ButtonCyanBorder.value()
                    }
                }
            }
        };

        let cyan_solid_shadow_signal = || {
            map_ref! {
                let disabled = disabled.signal(),
                let hovering = hovering.signal() => {
                    if *disabled || !*hovering {
                        ChromeShadow::None.value()
                    } else {
                        ChromeShadow::ButtonCyanHover.value()
                    }
                }
            }
        };

        let green_solid_background_signal = || {
            map_ref! {
                let disabled = disabled.signal() => {
                    if *disabled {
                        "linear-gradient(120deg, rgba(45, 55, 69, 0.3), rgba(36, 44, 57, 0.3))"
                    } else {
                        ChromeFill::ButtonGreen.value()
                    }
                }
            }
        };

        let green_solid_border_signal = || {
            map_ref! {
                let disabled = disabled.signal(),
                let hovering = hovering.signal() => {
                    if *disabled {
                        "rgba(122, 135, 153, 0.3)"
                    } else if *hovering {
                        ChromeColor::ButtonGreenBorderHover.value()
                    } else {
                        ChromeColor::ButtonGreenBorder.value()
                    }
                }
            }
        };

        let green_solid_shadow_signal = || {
            map_ref! {
                let disabled = disabled.signal(),
                let hovering = hovering.signal() => {
                    if *disabled || !*hovering {
                        ChromeShadow::None.value()
                    } else {
                        ChromeShadow::ButtonGreenHover.value()
                    }
                }
            }
        };

        let ret = html!("button", {
            .attr("type", &button_type)
            .apply_if(disabled_signal.is_some(), clone!(disabled => move |dom| {
                dom
                    .future(disabled_signal.unwrap_throw().for_each(clone!(disabled => move |is_disabled| {
                        clone!(disabled => async move {
                            disabled.set_neq(is_disabled);
                        })
                    })))
            }))
            .class([&*USER_SELECT_NONE, &*CLASS, size.container_class(), size.text_size_class()])
            .style("appearance", "none")
            .style("outline", "none")
            .style("font-family", "inherit")
            .apply(set_on_hover(&hovering))
            .style_signal("cursor", cursor_signal)
            .style_signal("box-shadow", box_shadow_signal)
            .style_signal("opacity", opacity_signal)
            .style_signal("filter", filter_signal)
            .style_signal("transform", hover_but_not_disabled_signal().map(|hover| {
                if hover { "translateY(-1px)" } else { "translateY(0)" }
            }))
            .apply_if(style == ButtonStyle::Outline, |dom| {
                dom.class(&*BORDER_CLASS)
                    .style_signal("background", outline_disabled_background_signal)
            })
            .class_signal([color.bg_class(style), color.border_class(style)], neither_hover_nor_disabled_signal())
            .class_signal([color.bg_hover_class(style), color.border_hover_class(style)], hover_but_not_disabled_signal())
            .class_signal([color.bg_disabled_class(style), color.border_disabled_class(style)], disabled.signal())
            .apply_if(style == ButtonStyle::Solid && color == ButtonColor::Primary, |dom| {
                dom.style("border", "1px solid rgba(114, 245, 255, 0.46)")
                    .style_signal("background", primary_solid_background_signal())
                    .style_signal("border-color", primary_solid_border_signal())
            })
            .apply_if(style == ButtonStyle::Solid && color == ButtonColor::Cyan, |dom| {
                dom.style("border", format!("1px solid {}", ChromeColor::ButtonCyanBorder.value()))
                    .style_signal("background", cyan_solid_background_signal())
                    .style_signal("border-color", cyan_solid_border_signal())
                    .style_signal("box-shadow", cyan_solid_shadow_signal())
            })
            .apply_if(style == ButtonStyle::Solid && color == ButtonColor::Green, |dom| {
                dom.style("border", format!("1px solid {}", ChromeColor::ButtonGreenBorder.value()))
                    .style_signal("background", green_solid_background_signal())
                    .style_signal("border-color", green_solid_border_signal())
                    .style_signal("box-shadow", green_solid_shadow_signal())
            })
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
