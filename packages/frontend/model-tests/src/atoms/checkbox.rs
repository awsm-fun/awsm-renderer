use crate::prelude::*;
use std::{pin::Pin, sync::LazyLock};

pub struct Checkbox {
    style: CheckboxStyle,
    selected_signal: Option<Pin<Box<dyn Signal<Item = bool>>>>,
    on_click: Option<Arc<dyn Fn()>>,
    content_before: Option<Dom>,
    content_after: Option<Dom>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckboxStyle {
    Dark,
    Light,
}

impl CheckboxStyle {
    pub fn fill_value(self) -> &'static str {
        match self {
            CheckboxStyle::Dark => ColorBackground::CheckboxDark.value(),
            CheckboxStyle::Light => ColorBackground::CheckboxLight.value(),
        }
    }

    pub fn stroke_value(self) -> &'static str {
        match self {
            CheckboxStyle::Dark => ColorBorder::CheckboxDark.value(),
            CheckboxStyle::Light => ColorBorder::CheckboxLight.value(),
        }
    }

    pub fn color_value(self) -> &'static str {
        match self {
            CheckboxStyle::Dark => ColorText::CheckboxDark.value(),
            CheckboxStyle::Light => ColorText::CheckboxLight.value(),
        }
    }
}

impl Checkbox {
    pub fn new(style: CheckboxStyle) -> Self {
        Self {
            style,
            selected_signal: None,
            content_before: None,
            content_after: None,
            on_click: None,
        }
    }

    pub fn with_style(mut self, style: CheckboxStyle) -> Self {
        self.style = style;
        self
    }

    pub fn with_selected_signal(
        mut self,
        selected_signal: impl Signal<Item = bool> + 'static,
    ) -> Self {
        self.selected_signal = Some(Box::pin(selected_signal));
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

    pub fn with_on_click(mut self, on_click: impl Fn() + 'static) -> Self {
        self.on_click = Some(Arc::new(on_click));
        self
    }

    pub fn render(self) -> Dom {
        static CONTAINER: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("display", "inline-flex")
                .style("gap", "0.6rem")
                .style("align-items", "center")
                .style("padding", "0.5rem 0.75rem")
                .style("border-radius", "6px")
                .style("transition", "background-color 0.15s")
                .pseudo!(":hover", {
                    .style("background-color", "rgba(255, 255, 255, 0.1)")
                })
            }
        });

        static SVG: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("width", "20px")
                .style("height", "20px")
                .style("flex-shrink", "0")
            }
        });

        static LABEL: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("font-size", FontSize::Md.value())
                .style("transition", "color 0.15s")
            }
        });

        let Self {
            style,
            selected_signal,
            content_before,
            content_after,
            on_click,
        } = self;

        html!("div", {
            .class([&*CONTAINER, &*CURSOR_POINTER, &*USER_SELECT_NONE])
            .apply_if(on_click.is_some(), |dom| {
                let on_click = on_click.unwrap_throw();
                dom.event(clone!(on_click => move |_: events::Click| {
                    on_click();
                }))
            })
            .apply_if(content_before.is_some(), |dom| {
                dom.child(html!("span", {
                    .class(&*LABEL)
                    .style("color", style.color_value())
                    .child(content_before.unwrap_throw())
                }))
            })
            .child(svg!("svg", {
                .class(&*SVG)
                .attrs!{
                    "xmlns": "http://www.w3.org/2000/svg",
                    "viewBox": "0 0 41 41",
                    "fill": "none",
                }
                .apply(|dom| {
                    match selected_signal {
                        Some(selected_signal) => {
                            dom.child_signal(selected_signal.map(move |selected| {
                                Some(if selected {
                                    svg!("path", {
                                        .attr("d", "M34.2919 0.447449H6.59963C4.96808 0.44923 3.40386 1.09815 2.25017 2.25184C1.09649 3.40552 0.447567 4.96974 0.445786 6.60129V34.2936C0.447567 35.9252 1.09649 37.4894 2.25017 38.6431C3.40386 39.7967 4.96808 40.4457 6.59963 40.4474H34.2919C35.9235 40.4457 37.4877 39.7967 38.6414 38.6431C39.7951 37.4894 40.444 35.9252 40.4458 34.2936V6.60129C40.444 4.96974 39.7951 3.40552 38.6414 2.25184C37.4877 1.09815 35.9235 0.44923 34.2919 0.447449ZM30.8544 13.7446L17.9314 29.1292C17.7896 29.298 17.6132 29.4344 17.4141 29.5292C17.2151 29.6239 16.998 29.6747 16.7775 29.6782H16.7516C16.5359 29.6781 16.3227 29.6327 16.1257 29.5449C15.9288 29.4571 15.7525 29.3289 15.6083 29.1686L10.0698 23.0148C9.92917 22.8656 9.81975 22.6898 9.748 22.4977C9.67626 22.3056 9.64363 22.1011 9.65204 21.8963C9.66045 21.6914 9.70973 21.4903 9.79697 21.3047C9.88422 21.1192 10.0077 20.9529 10.1601 20.8158C10.3125 20.6786 10.4908 20.5733 10.6845 20.5061C10.8782 20.4388 11.0833 20.4109 11.288 20.4241C11.4926 20.4372 11.6925 20.4912 11.876 20.5827C12.0595 20.6742 12.2228 20.8015 12.3564 20.9571L16.7112 25.7955L28.4987 11.7657C28.7631 11.46 29.1372 11.2707 29.5401 11.2386C29.943 11.2064 30.3423 11.3342 30.6518 11.5941C30.9613 11.8541 31.156 12.2254 31.1939 12.6278C31.2319 13.0302 31.1099 13.4314 30.8544 13.7446Z")
                                        .attr("fill", style.fill_value())
                                    })
                                } else {
                                    svg!("rect", {
                                        .attrs!{
                                            "rx": "5",
                                            "x": "1.4458",
                                            "y": "1.44727",
                                            "width": "38",
                                            "height": "38",
                                            "stroke-width": "2",
                                        }
                                        .attr("stroke", style.stroke_value())
                                    })
                                })
                            }))
                        },
                        None => {
                            tracing::warn!("Checkbox without selected_signal will always be checked");
                            dom.child(svg!("path", {
                                .attr("d", "M34.2919 0.447449H6.59963C4.96808 0.44923 3.40386 1.09815 2.25017 2.25184C1.09649 3.40552 0.447567 4.96974 0.445786 6.60129V34.2936C0.447567 35.9252 1.09649 37.4894 2.25017 38.6431C3.40386 39.7967 4.96808 40.4457 6.59963 40.4474H34.2919C35.9235 40.4457 37.4877 39.7967 38.6414 38.6431C39.7951 37.4894 40.444 35.9252 40.4458 34.2936V6.60129C40.444 4.96974 39.7951 3.40552 38.6414 2.25184C37.4877 1.09815 35.9235 0.44923 34.2919 0.447449ZM30.8544 13.7446L17.9314 29.1292C17.7896 29.298 17.6132 29.4344 17.4141 29.5292C17.2151 29.6239 16.998 29.6747 16.7775 29.6782H16.7516C16.5359 29.6781 16.3227 29.6327 16.1257 29.5449C15.9288 29.4571 15.7525 29.3289 15.6083 29.1686L10.0698 23.0148C9.92917 22.8656 9.81975 22.6898 9.748 22.4977C9.67626 22.3056 9.64363 22.1011 9.65204 21.8963C9.66045 21.6914 9.70973 21.4903 9.79697 21.3047C9.88422 21.1192 10.0077 20.9529 10.1601 20.8158C10.3125 20.6786 10.4908 20.5733 10.6845 20.5061C10.8782 20.4388 11.0833 20.4109 11.288 20.4241C11.4926 20.4372 11.6925 20.4912 11.876 20.5827C12.0595 20.6742 12.2228 20.8015 12.3564 20.9571L16.7112 25.7955L28.4987 11.7657C28.7631 11.46 29.1372 11.2707 29.5401 11.2386C29.943 11.2064 30.3423 11.3342 30.6518 11.5941C30.9613 11.8541 31.156 12.2254 31.1939 12.6278C31.2319 13.0302 31.1099 13.4314 30.8544 13.7446Z")
                                .attr("fill", style.fill_value())
                            }))
                        }
                    }
                })
            }))
            .apply_if(content_after.is_some(), |dom| {
                dom.child(html!("span", {
                    .class(&*LABEL)
                    .style("color", style.color_value())
                    .child(content_after.unwrap_throw())
                }))
            })
        })
    }
}
