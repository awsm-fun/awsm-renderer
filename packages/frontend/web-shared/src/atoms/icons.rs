use crate::prelude::*;

/// Open eye icon — indicates the field content is visible.
pub fn icon_eye() -> Dom {
    svg!("svg", {
        .attrs! {
            "viewBox": "0 0 24 24",
            "width": "20",
            "height": "20",
            "fill": "none",
            "stroke": "currentColor",
            "stroke-width": "1.8",
            "stroke-linecap": "round",
            "stroke-linejoin": "round",
        }
        .children([
            svg!("path", { .attrs! { "d": "M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z" } }),
            svg!("circle", { .attrs! { "cx": "12", "cy": "12", "r": "3" } }),
        ])
    })
}

/// Crossed-out eye icon — indicates the field content is hidden.
pub fn icon_eye_off() -> Dom {
    svg!("svg", {
        .attrs! {
            "viewBox": "0 0 24 24",
            "width": "20",
            "height": "20",
            "fill": "none",
            "stroke": "currentColor",
            "stroke-width": "1.8",
            "stroke-linecap": "round",
            "stroke-linejoin": "round",
        }
        .children([
            svg!("path", { .attrs! { "d": "M17.94 17.94A10.07 10.07 0 0 1 12 20c-7 0-11-8-11-8a18.45 18.45 0 0 1 5.06-5.94" } }),
            svg!("path", { .attrs! { "d": "M9.9 4.24A9.12 9.12 0 0 1 12 4c7 0 11 8 11 8a18.5 18.5 0 0 1-2.16 3.19" } }),
            svg!("circle", { .attrs! { "cx": "12", "cy": "12", "r": "3" } }),
            svg!("line", { .attrs! { "x1": "1", "y1": "1", "x2": "23", "y2": "23" } }),
        ])
    })
}

/// External-link icon — a square with an arrow exiting the top-right
/// corner. Pair with a button or link that opens a URL in a new tab so
/// users can predict the navigation behaviour.
pub fn icon_external_open() -> Dom {
    svg!("svg", {
        .attrs! {
            "viewBox": "0 0 24 24",
            "aria-hidden": "true",
            "width": "1rem",
            "height": "1rem",
        }
        .child(svg!("path", {
            .attrs! {
                "d": "M14 5h5v5h-1.9V8.2l-6.5 6.5-1.1-1.1 6.5-6.5H14V5zm-8 2h6v2H7v8h8v-5h2v6a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1V8a1 1 0 0 1 1-1z",
                "fill": "currentColor",
                "opacity": "0.9",
            }
        }))
    })
}

/// Stroked clipboard icon — pair with copy-to-clipboard actions.
pub fn icon_clipboard() -> Dom {
    svg!("svg", {
        .attrs! {
            "viewBox": "0 0 24 24",
            "width": "1rem",
            "height": "1rem",
            "fill": "none",
            "stroke": "currentColor",
            "stroke-width": "2",
            "stroke-linecap": "round",
            "stroke-linejoin": "round",
        }
        .children([
            svg!("rect", { .attrs! { "x": "9", "y": "9", "width": "13", "height": "13", "rx": "2" } }),
            svg!("path", { .attrs! { "d": "M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" } }),
        ])
    })
}

/// Plain checkmark icon — pair with confirmations such as "copied!"
/// feedback after a clipboard write.
pub fn icon_checkmark() -> Dom {
    svg!("svg", {
        .attrs! {
            "viewBox": "0 0 24 24",
            "width": "1rem",
            "height": "1rem",
            "fill": "none",
            "stroke": "currentColor",
            "stroke-width": "2",
            "stroke-linecap": "round",
            "stroke-linejoin": "round",
        }
        .child(svg!("polyline", { .attrs! { "points": "20,6 9,17 4,12" } }))
    })
}

/// Circled question mark icon — indicates contextual help is available.
pub fn icon_help_circle() -> Dom {
    svg!("svg", {
        .attrs! {
            "viewBox": "0 0 24 24",
            "width": "18",
            "height": "18",
            "fill": "none",
            "stroke": "currentColor",
            "stroke-width": "1.9",
            "stroke-linecap": "round",
            "stroke-linejoin": "round",
        }
        .children([
            svg!("circle", { .attrs! { "cx": "12", "cy": "12", "r": "9.25" } }),
            svg!("path", { .attrs! { "d": "M9.3 9.1a2.7 2.7 0 1 1 5.25.95c0 1.78-2.55 2.32-2.55 4.05" } }),
            svg!("circle", { .attrs! { "cx": "12", "cy": "17.2", "r": "0.85", "fill": "currentColor", "stroke": "none" } }),
        ])
    })
}
