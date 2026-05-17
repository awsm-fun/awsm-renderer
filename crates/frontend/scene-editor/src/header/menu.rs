//! Shared menu / popup primitives used by the top-strip overflow menu
//! and the Insert-row dropdowns: a row button (text + click), a
//! checkbox row, a separator, an outline-style dropdown trigger that
//! anchors a popup below it, and the full-viewport backdrop that
//! closes the popup on outside-click.

use crate::prelude::*;

pub(super) type DropdownItem = (&'static str, Arc<dyn Fn()>);

pub(super) fn render_menu_button(
    label: &'static str,
    destructive: bool,
    on_click: impl Fn() + 'static,
) -> Dom {
    static ITEM: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("display", "flex")
            .style("align-items", "center")
            .style("width", "100%")
            .style("padding", "0.5rem 0.8rem")
            .style("border", "0")
            .style("background", "transparent")
            .style("cursor", "pointer")
            .style("font-size", "0.9rem")
            .style("text-align", "left")
            .pseudo!(":hover", {
                .style("background", ColorBackground::SidebarSelected.value())
            })
        }
    });

    html!("button", {
        .class(&*ITEM)
        .style("color", if destructive { ColorRaw::Red.value() } else { ColorText::SidebarHeader.value() })
        .text(label)
        .event(move |_: events::Click| on_click())
    })
}

/// Same as [`render_menu_button`] but takes an owned `String` for the
/// label, so callers building dynamic labels (e.g. embedding a count)
/// don't have to leak.
pub(super) fn render_menu_button_owned(
    label: String,
    destructive: bool,
    on_click: impl Fn() + 'static,
) -> Dom {
    static ITEM: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("display", "flex")
            .style("align-items", "center")
            .style("width", "100%")
            .style("padding", "0.5rem 0.8rem")
            .style("border", "0")
            .style("background", "transparent")
            .style("cursor", "pointer")
            .style("font-size", "0.9rem")
            .style("text-align", "left")
            .pseudo!(":hover", {
                .style("background", ColorBackground::SidebarSelected.value())
            })
        }
    });

    html!("button", {
        .class(&*ITEM)
        .style("color", if destructive { ColorRaw::Red.value() } else { ColorText::SidebarHeader.value() })
        .text(&label)
        .event(move |_: events::Click| on_click())
    })
}

pub(super) fn render_menu_checkbox(label: &'static str, value: Mutable<bool>) -> Dom {
    html!("div", {
        .style("padding", "0.4rem 0.8rem")
        .child(Checkbox::new(CheckboxStyle::Dark)
            .with_selected_signal(value.signal())
            .with_content_after(html!("span", {
                .style("font-size", "0.9rem")
                .text(label)
            }))
            .with_on_click(clone!(value => move || {
                value.set(!value.get());
            }))
            .render())
    })
}

pub(super) fn render_menu_separator() -> Dom {
    html!("div", {
        .style("height", "1px")
        .style("margin", "0.3rem 0")
        .style("background-color", ColorBackground::UnderlineSecondary.value())
    })
}

/// Outline-style button that opens a small popup menu anchored below it.
/// `items` is `(label, on_click)` pairs; clicking a row runs the action and
/// closes the popup.
pub(super) fn render_dropdown_button(label: &'static str, items: Vec<DropdownItem>) -> Dom {
    let open = Mutable::new(false);

    html!("div", {
        .style("position", "relative")
        .child(Button::new()
            .with_text(label)
            .with_style(ButtonStyle::Outline)
            .with_size(ButtonSize::Sm)
            .with_on_click(clone!(open => move || {
                open.set(!open.get());
            }))
            .render())
        .child_signal(open.signal().map(clone!(open => move |is_open| {
            if is_open {
                Some(render_dropdown_popup(open.clone(), items.clone()))
            } else {
                None
            }
        })))
    })
}

fn render_dropdown_popup(open: Mutable<bool>, items: Vec<DropdownItem>) -> Dom {
    static POPUP: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("position", "absolute")
            .style("top", "100%")
            .style("left", "0")
            .style("margin-top", "0.3rem")
            .style("min-width", "10rem")
            .style("background-color", ColorBackground::Sidebar.value())
            .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
            .style("border-radius", "0.4rem")
            .style("box-shadow", "0 6px 24px rgba(0, 0, 0, 0.35)")
            .style("padding", "0.35rem 0")
            .style("z-index", "50")
        }
    });

    html!("div", {
        // The outer frag puts the invisible full-screen catch BEHIND the
        // popup content so clicks outside the popup trigger close but
        // clicks inside bubble to the content first.
        .child(render_popup_backdrop(open.clone()))
        .child(html!("div", {
            .class(&*POPUP)
            .children(items.into_iter().map(|(label, action)| {
                render_menu_button(label, false, clone!(open => move || {
                    open.set(false);
                    action();
                }))
            }))
        }))
    })
}

/// Invisible full-viewport overlay that sits BEHIND a popup. Any click
/// that lands on it (i.e. outside the popup) closes the popup.
pub(super) fn render_popup_backdrop(open: Mutable<bool>) -> Dom {
    html!("div", {
        .style("position", "fixed")
        .style("inset", "0")
        .style("z-index", "49")
        .style("background", "transparent")
        .event(clone!(open => move |event: events::PointerDown| {
            // PointerDown, not Click — close as soon as the user starts
            // pressing outside, so the actual click isn't wasted.
            event.stop_propagation();
            open.set(false);
        }))
    })
}
