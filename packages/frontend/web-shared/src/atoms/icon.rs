//! Prototype icon set — the geometric 24×24 line icons. Each icon is a name →
//! list of svg child shapes; the [`Icon`] builder wraps them in an `<svg>` that
//! strokes
//! with `currentColor` (so the icon inherits the surrounding text color), at a
//! configurable pixel size + stroke width.
//!
//! This is the editor's primary icon vocabulary (kind glyphs, toolbar actions,
//! chevrons, etc.). The older free-function icons in [`super::icons`] remain for
//! the secret-input eye toggle and are unrelated.

use crate::prelude::*;

// --- shape builders (all take string coords to stay terse) ---------------

/// Stroked path (inherits `fill:none; stroke:currentColor` from the wrapper).
fn sp(d: &str) -> Dom {
    svg!("path", { .attr("d", d) })
}
/// Filled path with explicit opacity (the `material` glyph's shaded half).
fn fp_op(d: &str, opacity: &str) -> Dom {
    svg!("path", {
        .attr("d", d).attr("fill", "currentColor").attr("stroke", "none").attr("opacity", opacity)
    })
}
fn sc(cx: &str, cy: &str, r: &str) -> Dom {
    svg!("circle", { .attr("cx", cx).attr("cy", cy).attr("r", r) })
}
fn fc(cx: &str, cy: &str, r: &str) -> Dom {
    svg!("circle", {
        .attr("cx", cx).attr("cy", cy).attr("r", r).attr("fill", "currentColor").attr("stroke", "none")
    })
}
fn se(cx: &str, cy: &str, rx: &str, ry: &str) -> Dom {
    svg!("ellipse", { .attr("cx", cx).attr("cy", cy).attr("rx", rx).attr("ry", ry) })
}
fn sr(x: &str, y: &str, w: &str, h: &str, rx: &str) -> Dom {
    svg!("rect", { .attr("x", x).attr("y", y).attr("width", w).attr("height", h).attr("rx", rx) })
}

/// The svg child shapes for a named icon. Unknown names fall back to `dot`.
fn icon_children(name: &str) -> Vec<Dom> {
    match name {
        "cube" => vec![
            sp("M12 3l8 4.5v9L12 21l-8-4.5v-9L12 3z"),
            sp("M4 7.5l8 4.5 8-4.5"),
            sp("M12 12v9"),
        ],
        "sphere" => vec![sc("12", "12", "8.5"), se("12", "12", "8.5", "3.4"), sp("M12 3.5v17")],
        // Flat-filled disc — the "Solid" (unlit/flat) shading-mode glyph. Reads
        // as a solid fill, contrasting the half-shaded `material` sphere and the
        // open-line `sphere` wireframe globe.
        "sphere-solid" => vec![fc("12", "12", "8.5")],
        "plane" => vec![sp("M3 15.5l9-5 9 5-9 4-9-4z")],
        "cylinder" => vec![
            se("12", "6", "6.5", "2.6"),
            sp("M5.5 6v12M18.5 6v12"),
            se("12", "18", "6.5", "2.6"),
        ],
        "light" => vec![
            sc("12", "10", "4.5"),
            sp("M12 2.5v1.5M12 16v1.5M4.7 10H3.2M20.8 10h-1.5M6.7 4.7L5.6 3.6M18.4 4.7l-1.1-1.1"),
            sp("M10 19h4M10.5 21.5h3"),
        ],
        "camera" => vec![sp("M3.5 8.5h4l1.5-2h6l1.5 2h4v10h-17z"), sc("12", "13", "3.2")],
        "curve" => vec![sp("M3 19c5 0 4-14 9-14s4 14 9 14")],
        "empty" => vec![sc("12", "12", "2"), sp("M12 4v4M12 16v4M4 12h4M16 12h4")],
        "collision" => vec![sp("M12 3l7 4v6c0 4-3 6.5-7 8-4-1.5-7-4-7-8V7z")],
        "material" => vec![sc("12", "12", "8.5"), fp_op("M12 3.5a8.5 8.5 0 000 17", "0.5")],
        "eye" => vec![sp("M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12z"), sc("12", "12", "2.8")],
        "eyeoff" => vec![
            sp("M4 4l16 16"),
            sp("M9.5 5.9A9.8 9.8 0 0112 5.5c6 0 9.5 6.5 9.5 6.5a16 16 0 01-2.6 3.3M6.4 7.8A15.7 15.7 0 002.5 12S6 18.5 12 18.5a9.6 9.6 0 003.2-.5"),
            sp("M9.9 9.9a2.8 2.8 0 004 4"),
        ],
        "lock" => vec![sr("5.5", "10.5", "13", "9", "1.6"), sp("M8 10.5V8a4 4 0 018 0v2.5")],
        "unlock" => vec![sr("5.5", "10.5", "13", "9", "1.6"), sp("M8 10.5V8a4 4 0 017.5-2")],
        "chevron" => vec![sp("M9 6l6 6-6 6")],
        "chevdown" => vec![sp("M6 9l6 6 6-6")],
        "plus" => vec![sp("M12 5v14M5 12h14")],
        "minus" => vec![sp("M5 12h14")],
        "trash" => vec![sp("M4.5 7h15M9 7V5.2A1.2 1.2 0 0110.2 4h3.6A1.2 1.2 0 0115 5.2V7M6.5 7l.8 12a1.5 1.5 0 001.5 1.4h6.4a1.5 1.5 0 001.5-1.4l.8-12")],
        "search" => vec![sc("11", "11", "6.5"), sp("M16 16l4 4")],
        "move" => vec![
            sp("M12 3v18M3 12h18"),
            sp("M12 3l-2.4 2.4M12 3l2.4 2.4M12 21l-2.4-2.4M12 21l2.4-2.4M3 12l2.4-2.4M3 12l2.4 2.4M21 12l-2.4-2.4M21 12l-2.4 2.4"),
        ],
        "rotate" => vec![sp("M20 12a8 8 0 10-2.6 5.9"), sp("M20 7.5V12h-4.5")],
        "scale" => vec![sp("M6 18L18 6"), sr("3.5", "16.5", "3.5", "3.5", "0.6"), sr("15", "4", "5", "5", "1")],
        "target" => vec![sc("12", "12", "7"), sp("M12 2v3.2M12 18.8V22M2 12h3.2M18.8 12H22"), fc("12", "12", "2")],
        "select" => vec![sp("M5 4l13 7-5.5 1.5L10 18z")],
        "code" => vec![sp("M9 8l-4 4 4 4M15 8l4 4-4 4")],
        "grid" => vec![sr("4", "4", "16", "16", "1.5"), sp("M4 10h16M4 15h16M10 4v16M15 4v16")],
        "layers" => vec![sp("M12 4l8 4-8 4-8-4 8-4z"), sp("M4 12l8 4 8-4M4 16l8 4 8-4")],
        "help" => vec![sc("12", "12", "8.5"), sp("M9.6 9.4a2.4 2.4 0 114 1.8c-1 .7-1.6 1.2-1.6 2.3"), fc("12", "16.6", "0.6")],
        "save" => vec![sp("M5 4.5h11l3 3v12h-14z"), sp("M8 4.5v5h7v-5M8 19.5v-6h8v6")],
        "folder" => vec![sp("M3.5 6.5h6l1.6 2h9.4v9.5a1 1 0 01-1 1h-15a1 1 0 01-1-1z")],
        "undo" => vec![sp("M9 7L4.5 11.5 9 16"), sp("M4.5 11.5H14a5.5 5.5 0 010 11h-3")],
        "redo" => vec![sp("M15 7l4.5 4.5L15 16"), sp("M19.5 11.5H10a5.5 5.5 0 000 11h3")],
        "more" => vec![fc("6", "12", "1.4"), fc("12", "12", "1.4"), fc("18", "12", "1.4")],
        "link" => vec![
            sp("M10 14a4 4 0 005.7 0l3-3a4 4 0 00-5.7-5.7l-1.5 1.5"),
            sp("M14 10a4 4 0 00-5.7 0l-3 3a4 4 0 005.7 5.7l1.5-1.5"),
        ],
        "check" => vec![sp("M5 12.5l4.5 4.5L19 7")],
        "dot" => vec![fc("12", "12", "3.5")],
        "texture" => vec![sr("4", "4", "16", "16", "1.5"), sc("9", "9", "1.6"), sp("M4 16l4.5-4 3 2.5L16 9l4 4.5")],
        "buffer" => vec![sr("4", "5", "16", "4", "1"), sr("4", "14", "16", "4", "1")],
        "sliders" => vec![
            sp("M5 6h9M18 6h1M5 12h3M12 12h7M5 18h11M19 18h0"),
            sc("16", "6", "2"),
            sc("10", "12", "2"),
            sc("18", "18", "2"),
        ],
        "reset" => vec![sp("M5 5v4.5h4.5"), sp("M5.6 9.5A8 8 0 1112 20")],
        "env" => vec![sc("12", "12", "8.5"), sp("M3.5 12h17M12 3.5c2.5 2.3 2.5 14.7 0 17M12 3.5c-2.5 2.3-2.5 14.7 0 17")],
        "settings" => vec![
            sc("12", "12", "3.2"),
            sp("M12 3.5v2.2M12 18.3v2.2M3.5 12h2.2M18.3 12h2.2M5.9 5.9l1.6 1.6M16.5 16.5l1.6 1.6M18.1 5.9l-1.6 1.6M7.5 16.5l-1.6 1.6"),
        ],
        "sprite" => vec![sr("4.5", "4.5", "15", "15", "2"), sc("9", "9.5", "1.5"), sp("M5 16l4-3.5 3 2.2 3.5-3.2 3.5 3")],
        "filter" => vec![sp("M4 5h16l-6 7v6l-4 2v-8z")],
        "drag" => vec![
            fc("9", "6", "1.3"), fc("15", "6", "1.3"),
            fc("9", "12", "1.3"), fc("15", "12", "1.3"),
            fc("9", "18", "1.3"), fc("15", "18", "1.3"),
        ],
        "warning" => vec![sp("M12 4l9 16H3z"), sp("M12 10v4.5"), fc("12", "17.5", "0.6")],
        "copy" => vec![sr("8.5", "8.5", "11", "11", "2"), sp("M5.5 15.5h-1a1 1 0 01-1-1v-9a1 1 0 011-1h9a1 1 0 011 1v1")],
        _ => vec![fc("12", "12", "3.5")], // dot fallback
    }
}

/// Builder for a single prototype icon. Defaults are 16px, 1.6 stroke width,
/// `currentColor`.
pub struct Icon {
    name: String,
    size: f64,
    sw: f64,
    styles: Vec<(String, String)>,
}

impl Icon {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            size: 16.0,
            sw: 1.6,
            styles: Vec::new(),
        }
    }
    /// Pixel width/height.
    pub fn size(mut self, size: f64) -> Self {
        self.size = size;
        self
    }
    /// Stroke width.
    pub fn stroke_width(mut self, sw: f64) -> Self {
        self.sw = sw;
        self
    }
    /// Override the icon color (otherwise it inherits the parent text color).
    pub fn color(mut self, color: impl Into<String>) -> Self {
        self.styles.push(("color".to_string(), color.into()));
        self
    }
    /// Arbitrary inline style passthrough (e.g. a `transform` for a rotating
    /// chevron, or a `margin`).
    pub fn style(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.styles.push((key.into(), value.into()));
        self
    }

    pub fn render(self) -> Dom {
        let size = format!("{}", self.size);
        let sw = format!("{}", self.sw);
        // SVG elements don't take dominator's `.style()` builder method the way
        // HTML elements do, so compose all inline styles into the `style`
        // attribute string instead (display/flex-shrink baseline + overrides).
        let mut style = String::from("display:block;flex-shrink:0;");
        for (k, v) in &self.styles {
            style.push_str(k);
            style.push(':');
            style.push_str(v);
            style.push(';');
        }
        svg!("svg", {
            .attr("viewBox", "0 0 24 24")
            .attr("width", &size)
            .attr("height", &size)
            .attr("fill", "none")
            .attr("stroke", "currentColor")
            .attr("stroke-width", &sw)
            .attr("stroke-linecap", "round")
            .attr("stroke-linejoin", "round")
            .attr("aria-hidden", "true")
            .attr("style", &style)
            .children(icon_children(&self.name))
        })
    }
}

/// Shorthand for `Icon::new(name).render()` at default size.
pub fn icon(name: &str) -> Dom {
    Icon::new(name).render()
}
