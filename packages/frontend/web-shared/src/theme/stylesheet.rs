use crate::{
    prelude::*,
    theme::{
        responsive::Breakpoint,
        typography::{FONT_FAMILY_BODY, FONT_FAMILY_DISPLAY},
    },
};
use dominator::stylesheet;

pub fn init() {
    stylesheet!(":root", {
        .style("box-sizing", "border-box")
        .style_signal("font-size", Breakpoint::signal().map(|breakpoint| {
            breakpoint.font_size()
        }))
    });

    stylesheet!("*, ::before, ::after", {
        .style("box-sizing", "inherit")
    });

    stylesheet!("html, body", {
        .style("margin", "0")
        .style("padding", "0")
        .style("font-family", FONT_FAMILY_BODY)
        .style("background", "radial-gradient(circle at 20% -10%, #15223e 0%, #05070d 50%)")
        .style("color", ColorRaw::Whiteish.value())
        .style("overflow-x", "hidden")
    });

    stylesheet!("a", {
        .style("all", "unset")
        .style("cursor", "pointer")
        .style("color", "inherit")
    });

    stylesheet!("h1", {
        .style("font-size", FontSize::H1.value())
        .style("font-family", FONT_FAMILY_DISPLAY)
        .style("font-weight", "700")
    });

    stylesheet!("h2", {
        .style("font-size", FontSize::H2.value())
        .style("font-family", FONT_FAMILY_DISPLAY)
        .style("font-weight", "700")
    });

    stylesheet!("h3", {
        .style("font-size", FontSize::H3.value())
        .style("font-family", FONT_FAMILY_DISPLAY)
        .style("font-weight", "700")
    });

    stylesheet!("input, button", {
        .style("font-family", FONT_FAMILY_BODY)
    });

    // Normalize native button rendering so interaction states never fall back
    // to browser default white/blue styles after click/focus.
    stylesheet!("button", {
        .style("appearance", "none")
        .style("-webkit-appearance", "none")
        .style("background", "none")
        .style("background-color", "transparent")
        .style("background-image", "none")
        .style("border", "0")
        .style("color", "inherit")
    });

    stylesheet!("::selection", {
        .style("background", "rgba(87, 169, 255, 0.35)")
        .style("color", "#f4f8ff")
    });
}
