use crate::prelude::*;

pub struct Dropdown<T> {
    pub options: Vec<Arc<DropdownOption<T>>>,
    pub initial_selected: Option<T>,
    pub size: DropdownSize,
    pub bg_color: Option<ColorBackground>,
    pub on_change: Option<Arc<dyn Fn(&T)>>,
}

pub struct DropdownOption<T> {
    pub label: String,
    pub value: T,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DropdownSize {
    Sm,
    Md,
}

impl DropdownSize {
    pub fn text_size_class(&self) -> &'static str {
        match self {
            Self::Sm => FontSize::Sm.class(),
            Self::Md => FontSize::Md.class(),
        }
    }

    pub fn container_class(&self) -> &'static str {
        static SM: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("padding", "0.5rem")
            }
        });

        static MD: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("padding", "0.5rem")
            }
        });
        match self {
            Self::Sm => &SM,
            Self::Md => &MD,
        }
    }

    pub fn options_class(&self) -> &'static str {
        static SM: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("padding", "0.5rem")
            }
        });

        static MD: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("padding", "1rem")
            }
        });
        match self {
            Self::Sm => &SM,
            Self::Md => &MD,
        }
    }
}

impl<T> Dropdown<T>
where
    T: PartialEq + 'static,
{
    pub fn new() -> Self {
        Self {
            options: Vec::new(),
            initial_selected: None,
            size: DropdownSize::Md,
            on_change: None,
            bg_color: Some(ColorBackground::Dropdown),
        }
    }

    pub fn with_options(mut self, options: impl IntoIterator<Item = (String, T)>) -> Self {
        self.options = options
            .into_iter()
            .map(|(label, value)| Arc::new(DropdownOption { label, value }))
            .collect();
        self
    }

    pub fn with_bg_color(mut self, bg_color: ColorBackground) -> Self {
        self.bg_color = Some(bg_color);
        self
    }

    pub fn with_intial_selected(mut self, initial_selected: Option<T>) -> Self {
        self.initial_selected = initial_selected;
        self
    }

    pub fn with_size(mut self, size: DropdownSize) -> Self {
        self.size = size;
        self
    }

    pub fn with_on_change(mut self, on_change: impl Fn(&T) + 'static) -> Self {
        self.on_change = Some(Arc::new(on_change));
        self
    }

    pub fn render(self) -> Dom {
        static CONTAINER: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("display", "flex")
                .style("flex-direction", "column")
            }
        });

        static CONTENT: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("position", "relative")
                .style("border", "1px solid rgba(0, 0, 0, 0.3)")
                .style("border-radius", "6px")
                .style("cursor", "pointer")
            }
        });

        static LABEL_CONTAINER: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("display", "flex")
                .style("gap", "0.75rem")
                .style("justify-content", "space-between")
                .style("align-items", "center")
                .style("padding", "0.6rem 0.8rem")
            }
        });

        static OPTIONS_CONTAINER: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("position", "fixed")
                .style("z-index", Zindex::DropdownOptions.value())
            }
        });

        static OPTIONS_CONTENT: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("border", "1px solid rgba(0, 0, 0, 0.3)")
                .style("border-radius", "6px")
                .style("display", "flex")
                .style("flex-direction", "column")
                .style("gap", "0.25rem")
                .style("padding", "0.5rem")
                .style("max-height", "20rem")
                .style("overflow-y", "auto")
            }
        });

        static OPTION_ITEM: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("padding", "0.5rem 0.6rem")
                .style("border-radius", "4px")
                .style("transition", "background-color 0.15s")
                .style("transition", "color 0.15s")
                .style("cursor", "pointer")
                .style("color", ColorText::Label.value())
                .pseudo!(":hover", {
                    .style("background-color", "rgba(255, 255, 255, 0.2)")
                    .style("color", ColorText::Link.value())
                })
            }
        });

        let Self {
            options,
            initial_selected,
            size,
            on_change,
            bg_color,
        } = self;

        let showing = Mutable::new(false);
        let dropdown_rect: Mutable<Option<web_sys::DomRect>> = Mutable::new(None);

        let selected: Mutable<Option<Arc<DropdownOption<T>>>> =
            Mutable::new(initial_selected.and_then(|initial_selected| {
                options
                    .iter()
                    .find(|option| option.value == initial_selected)
                    .cloned()
            }));

        let selected_label = selected.signal_cloned().map(|selected| {
            selected
                .map(|selected| selected.label.clone())
                .unwrap_or_else(|| "Select...".to_string())
        });

        html!("div", {
            .class(&*CONTAINER)
            .class(&*USER_SELECT_NONE)
            .child(html!("div", {
                .class(&*CONTENT)
                .apply_if(bg_color.is_some(), |dom| {
                    dom.style("background-color", bg_color.unwrap_throw().value())
                })
                .with_node!(trigger_el => {
                    .child(html!("div", {
                        .class([&*LABEL_CONTAINER, size.container_class()])
                        .child(html!("div", {
                            .class(size.text_size_class())
                            .text_signal(selected_label)
                        }))
                        .child(html!("div", {
                            .class(size.text_size_class())
                            .text_signal(showing.signal().map(|showing| {
                                if showing {
                                    "▲"
                                } else {
                                    "▼"
                                }
                            }))
                        }))
                        .event(clone!(showing, dropdown_rect, trigger_el => move |_: events::Click| {
                            let rect = trigger_el.get_bounding_client_rect();
                            dropdown_rect.set(Some(rect));
                            showing.set(!showing.get());
                        }))
                    }))
                    .child_signal(showing.signal().map(clone!(on_change, showing, dropdown_rect => move |is_showing| {
                        if is_showing {
                            let rect = dropdown_rect.get_cloned();
                            Some(html!("div", {
                                .class(&*OPTIONS_CONTAINER)
                                .apply_if(rect.is_some(), |dom| {
                                    let rect = rect.unwrap_throw();
                                    dom.style("top", format!("{}px", rect.bottom() + 4.0))
                                       .style("left", format!("{}px", rect.left()))
                                       .style("min-width", format!("{}px", rect.width()))
                                })
                                .apply_if(bg_color.is_some(), |dom| {
                                    dom.style("background-color", bg_color.unwrap_throw().value())
                                })
                                .child(html!("div", {
                                    .class(&*OPTIONS_CONTENT)
                                    .children(options.iter().map(clone!(on_change, selected, showing => move |option| {
                                        html!("div", {
                                            .class([&*OPTION_ITEM, size.text_size_class()])
                                            .text(&option.label)
                                            .event({
                                                clone!(selected, option, showing, on_change => move |_: events::Click| {
                                                    selected.set(Some(option.clone()));
                                                    showing.set_neq(false);
                                                    if let Some(on_change) = &on_change {
                                                        on_change(&option.value);
                                                    }
                                                })
                                            })
                                        })
                                    })))
                                }))
                            }))
                        } else {
                            None
                        }
                    })))
                    .global_event(clone!(showing, trigger_el => move |evt: events::Click| {
                        if let Some(target) = evt.target() {
                            if !trigger_el.contains(Some(target.unchecked_ref())) {
                                showing.set_neq(false);
                            }
                        }
                    }))
                })
            }))
        })
    }
}
