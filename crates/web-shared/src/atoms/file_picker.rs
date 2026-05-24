//! Reusable file-picker atom: drag-and-drop OR click to browse.
//!
//! Lifted from the dashboard's agent-WASM upload flow. The pattern is
//! the same wherever the user selects a single binary file — the only
//! variation is the `accept` filter (file extension), the placeholder
//! text, and whether validation rejects mismatched extensions.
//!
//! Caller-owned reactive state:
//!
//! * `selected_file: Mutable<Option<web_sys::File>>` — populated when
//!   the user accepts a file. Caller observes via signal to drive the
//!   downstream upload / parse / etc.
//! * `error: Mutable<Option<String>>` — populated when the user drops
//!   or selects a file with the wrong extension. Caller renders this.
//!
//! Usage:
//!
//! ```ignore
//! let selected: Mutable<Option<web_sys::File>> = Mutable::new(None);
//! let err: Mutable<Option<String>> = Mutable::new(None);
//! FilePicker::new()
//!     .with_accept(".archive")
//!     .with_placeholder("Drag & drop a session archive, or click to browse")
//!     .render(selected.clone(), err.clone())
//! ```

use std::sync::LazyLock;

use dominator::{attrs, class, clone, events, html, svg, with_node, Dom, EventOptions};
use futures_signals::signal::{Mutable, SignalExt};

use crate::theme::{
    chrome::{ChromeColor, ChromeFill},
    color::ColorRaw,
    typography::FontSize,
};

#[derive(Default)]
pub struct FilePicker {
    accept: Option<&'static str>,
    placeholder: Option<&'static str>,
}

impl FilePicker {
    pub fn new() -> Self {
        Self::default()
    }

    /// File extension filter, with a leading dot. Affects both the
    /// `<input accept="...">` attribute (file dialog) AND drag-drop
    /// validation (the dialog filter doesn't apply to drops).
    /// Example: `".archive"` or `".wasm"`. Multiple extensions can be
    /// given comma-separated, matching the `accept` attribute syntax
    /// (`".wasm,.zip"`); drag-drop validation accepts a file whose
    /// name ends with any of the listed extensions.
    pub fn with_accept(mut self, accept: &'static str) -> Self {
        self.accept = Some(accept);
        self
    }

    /// Placeholder text shown when no file is selected.
    pub fn with_placeholder(mut self, placeholder: &'static str) -> Self {
        self.placeholder = Some(placeholder);
        self
    }

    pub fn render(
        self,
        selected_file: Mutable<Option<web_sys::File>>,
        error: Mutable<Option<String>>,
    ) -> Dom {
        let accept = self.accept.unwrap_or("*");
        let placeholder = self
            .placeholder
            .unwrap_or("Drag & drop a file, or click to browse");
        let drag_over = Mutable::new(false);

        html!("label", {
            .class(&*DROP_ZONE_CLASS)
            .class_signal(&*DROP_ZONE_ACTIVE_CLASS, drag_over.signal())
            .event_with_options(&EventOptions::preventable(), clone!(drag_over => move |e: events::DragOver| {
                e.prevent_default();
                drag_over.set_neq(true);
            }))
            .event(clone!(drag_over => move |_: events::DragEnter| {
                drag_over.set_neq(true);
            }))
            .event(clone!(drag_over => move |_: events::DragLeave| {
                drag_over.set_neq(false);
            }))
            .event_with_options(&EventOptions::preventable(), clone!(selected_file, drag_over, error => move |e: events::Drop| {
                e.prevent_default();
                drag_over.set(false);
                let file = e
                    .data_transfer()
                    .and_then(|dt| dt.files())
                    .and_then(|fl| fl.get(0));
                accept_file(&selected_file, &error, accept, file);
            }))
            .child(html!("input" => web_sys::HtmlInputElement, {
                .style("display", "none")
                .attr("type", "file")
                .attr("accept", accept)
                .with_node!(input => {
                    .event(clone!(selected_file, error => move |_: events::Change| {
                        let file = input.files().and_then(|fl| fl.get(0));
                        accept_file(&selected_file, &error, accept, file);
                    }))
                })
            }))
            .child(render_upload_icon())
            .child_signal(selected_file.signal_cloned().map(move |file| {
                Some(match file {
                    None => html!("span", {
                        .class(FontSize::Sm.class())
                        .style("color", ColorRaw::MidGrey.value())
                        .text(placeholder)
                    }),
                    Some(file) => html!("span", {
                        .class(FontSize::Sm.class())
                        .style("color", ColorRaw::Whiteish.value())
                        .text(&file.name())
                    }),
                })
            }))
        })
    }
}

// Drop-zone CSS classes are exposed at module level so callers
// (e.g. the dashboard's `bundle_picker` atom, which composes a
// drop zone with extra controls) can reuse the same look.
pub fn drop_zone_class() -> &'static str {
    &DROP_ZONE_CLASS
}

pub fn drop_zone_active_class() -> &'static str {
    &DROP_ZONE_ACTIVE_CLASS
}

/// Re-export the upload-icon helper so atoms that build a custom
/// drop-zone shell (different button layout, multiple inputs) can
/// reuse the same visual.
pub fn upload_icon() -> Dom {
    render_upload_icon()
}

/// Read a `web_sys::File` into bytes. Async — returns when the
/// browser has streamed the file through `array_buffer()`.
pub async fn read_file_bytes(file: web_sys::File) -> Result<Vec<u8>, String> {
    let array_buffer = wasm_bindgen_futures::JsFuture::from(file.array_buffer())
        .await
        .map_err(|e| format!("failed to read file: {e:?}"))?;
    Ok(js_sys::Uint8Array::new(&array_buffer).to_vec())
}

fn accept_file(
    selected_file: &Mutable<Option<web_sys::File>>,
    error: &Mutable<Option<String>>,
    accept: &'static str,
    file: Option<web_sys::File>,
) {
    let Some(file) = file else { return };
    if accept == "*" || matches_accept(&file.name(), accept) {
        error.set(None);
        selected_file.set(Some(file));
    } else {
        error.set(Some(format!("Only {accept} files are accepted")));
    }
}

/// True if `name` ends with any of the comma-separated extensions in
/// `accept`. Mirrors the relaxed matching the HTML `accept` attribute
/// does in file dialogs (which is also why drag-drop validation has
/// to live on this side).
fn matches_accept(name: &str, accept: &str) -> bool {
    let name_lower = name.to_lowercase();
    accept
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .any(|ext| !ext.is_empty() && name_lower.ends_with(&ext))
}

fn render_upload_icon() -> Dom {
    html!("div", {
        .style("width", "2.5rem")
        .style("height", "2.5rem")
        .style("color", ColorRaw::BlueSilver.value())
        .style("opacity", "0.6")
        .child(svg!("svg", {
            .attrs! {
                "viewBox": "0 0 24 24",
                "width": "100%",
                "height": "100%",
                "fill": "none",
                "stroke": "currentColor",
                "stroke-width": "1.8",
                "stroke-linecap": "round",
                "stroke-linejoin": "round",
            }
            .children([
                svg!("path",     { .attrs! { "d":"M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" } }),
                svg!("polyline", { .attrs! { "points":"17 8 12 3 7 8" } }),
                svg!("line",     { .attrs! { "x1":"12","y1":"3","x2":"12","y2":"15" } }),
            ])
        }))
    })
}

static DROP_ZONE_CLASS: LazyLock<String> = LazyLock::new(|| {
    class! {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("align-items", "center")
        .style("justify-content", "center")
        .style("gap", "0.75rem")
        .style("padding", "2rem 1.5rem")
        .style("border", format!("2px dashed {}", ChromeColor::PanelBorder.value()))
        .style("border-radius", "1rem")
        .style("background", ChromeFill::Canvas.value())
        .style("cursor", "pointer")
        .style(["-moz-user-select", "user-select", "-webkit-user-select"], "none")
        .style("transition", "border-color 170ms ease, background 170ms ease")
    }
});

static DROP_ZONE_ACTIVE_CLASS: LazyLock<String> = LazyLock::new(|| {
    class! {
        .style_important("border-color", ColorRaw::BlueIce.value())
        .style_important("background", ChromeFill::ContentPanel.value())
    }
});
