use crate::prelude::*;

use super::icons;

/// Wraps an input `Dom` with a reveal/hide toggle button (eye icon).
///
/// The caller is responsible for:
/// - Wiring `visible.signal()` into the input's `type` attribute
///   (e.g. `"text"` when visible, `"password"` when hidden)
/// - Adding right-padding to the input so text doesn't overlap the button
///   (the toggle sits at `right: 0.75rem`, so ~3rem padding-right is recommended)
///
/// # Example
/// ```ignore
/// let visible = Mutable::new(false);
///
/// secret_input_wrap(
///     &visible,
///     html!("input" => HtmlInputElement, {
///         .attr_signal("type", visible.signal().map(|v| if v { "text" } else { "password" }))
///         .style("padding-right", "3rem")
///         // ... other attrs ...
///     }),
/// )
/// ```
pub fn secret_input_wrap(visible: &Mutable<bool>, input: Dom) -> Dom {
    let hovering = Mutable::new(false);

    html!("div", {
        .style("position", "relative")
        .style("width", "100%")
        .child(input)
        .child(html!("button", {
            .attr("type", "button")
            .attr("tabindex", "-1")
            .attr("aria-label", "Toggle secret visibility")
            .class(&*TOGGLE_CLASS)
            .apply(set_on_hover(&hovering))
            .style_signal("color", hovering.signal().map(|h| {
                if h { ColorRaw::Whiteish.value() } else { ColorRaw::MidGrey.value() }
            }))
            .event(clone!(visible => move |_: events::Click| {
                visible.replace_with(|x| !*x);
            }))
            .child_signal(visible.signal().map(|show| {
                Some(if show { icons::icon_eye_off() } else { icons::icon_eye() })
            }))
        }))
    })
}

static TOGGLE_CLASS: LazyLock<String> = LazyLock::new(|| {
    class! {
        .style("position", "absolute")
        .style("right", "0.75rem")
        .style("top", "50%")
        .style("transform", "translateY(-50%)")
        .style("background", "none")
        .style("border", "0")
        .style("cursor", "pointer")
        .style("padding", "0.25rem")
        .style("display", "flex")
        .style("align-items", "center")
        .style("justify-content", "center")
        .style("transition", "color 150ms ease")
    }
});
