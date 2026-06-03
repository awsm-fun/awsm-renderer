//! Buffer Converter modal — populates a `BufferSlot`'s default bytes
//! from a user-supplied file (`.bin`) or pasted JSON array.
//!
//! Triggered from the Definition pane's Buffers section by clicking
//! the "Edit data…" button next to a slot row. Sets
//! [`crate::state::EditState::converter_open_for_slot`] to the slot
//! name; this module's `render` watches that signal and shows/hides
//! itself accordingly.
//!
//! The bytes the user provides land in
//! [`crate::state::EditState::buffer_defaults`] keyed by slot name.
//! The recompile pipeline reads that map and threads the bytes into
//! [`awsm_renderer::dynamic_materials::MaterialRegistration::buffer_defaults`],
//! so the live preview reflects the dropped-in data without requiring
//! a disk write to `assets/materials/<name>/<slot>.bin` first.
//!
//! Two input formats:
//! - **`.bin`** — raw little-endian u32 words. File size must be a
//!   multiple of 4 (matches the renderer-side loader's contract per
//!   `BufferSlot` doc comments).
//! - **`.json`** — JSON array of numbers. Each number is encoded as
//!   `f32` and then bit-cast to `u32` via `to_le_bytes` / `from_le_bytes`
//!   so the resulting `extras_load_f32` reads on the WGSL side recover
//!   the exact author-supplied value. Useful for hand-authored UV-rect
//!   atlas data (e.g. `[[x, y, w, h], …]` flattened).

use dominator::{clone, events, html, with_node, Dom};
use futures_signals::signal::{Mutable, SignalExt};
use std::sync::Arc;
use wasm_bindgen::{closure::Closure, JsCast};

use crate::state::EditState;

const HELP_TEXT: &str = ".bin files are read as raw little-endian u32 words (size must be a 4-byte multiple). .json files are parsed as an array of numbers; each value is encoded as f32 then bit-cast to u32 (so extras_load_f32 reads it back unchanged).";

/// Renders the modal — a fixed-position overlay that shows only when
/// [`EditState::converter_open_for_slot`] is `Some`. Returns a single
/// `Dom` the caller attaches at the root level (so the absolute
/// positioning isn't constrained by the parent pane's grid cell).
pub fn render(state: &EditState) -> Dom {
    let open_for = state.converter_open_for_slot.clone();
    let buffer_defaults = state.buffer_defaults.clone();
    html!("div", {
        .child_signal(open_for.signal_cloned().map(clone!(open_for, buffer_defaults => move |slot| {
            slot.map(|slot_name| {
                modal_body(slot_name, open_for.clone(), buffer_defaults.clone())
            })
        })))
    })
}

fn modal_body(
    slot_name: String,
    open_for: Arc<Mutable<Option<String>>>,
    buffer_defaults: Arc<Mutable<std::collections::HashMap<String, Vec<u32>>>>,
) -> Dom {
    // The parsed-bytes preview is a Mutable so the file-picker + JSON
    // paste paths can both update it without re-rendering the whole
    // modal. The summary line below renders from this signal.
    let pending: Arc<Mutable<Option<Vec<u32>>>> = Arc::new(Mutable::new(
        buffer_defaults
            .lock_ref()
            .get(&slot_name)
            .cloned()
            .map(Some)
            .unwrap_or(None),
    ));
    let status: Arc<Mutable<String>> = Arc::new(Mutable::new(
        pending
            .lock_ref()
            .as_ref()
            .map(|v| format!("{} u32 words loaded ({} bytes).", v.len(), v.len() * 4))
            .unwrap_or_else(|| "Drop a .bin or .json file, or paste a JSON array.".to_string()),
    ));

    html!("div", {
        // Backdrop — covers the whole window; clicking it cancels the
        // modal. The inner card stops propagation so clicks inside
        // don't dismiss.
        .style("position", "fixed")
        .style("top", "0")
        .style("left", "0")
        .style("right", "0")
        .style("bottom", "0")
        .style("background", "rgba(0, 0, 0, 0.6)")
        .style("display", "flex")
        .style("align-items", "center")
        .style("justify-content", "center")
        .style("z-index", "2000")
        .event(clone!(open_for => move |_: events::Click| {
            open_for.set(None);
        }))
        .child(html!("div", {
            .style("background", "#1a1a1a")
            .style("color", "#ddd")
            .style("border", "1px solid #444")
            .style("border-radius", "4px")
            .style("padding", "16px")
            .style("min-width", "420px")
            .style("max-width", "560px")
            .style("font-size", "12px")
            .event(|e: events::Click| {
                e.stop_propagation();
            })
            .child(html!("h3", {
                .style("margin-top", "0")
                .text(&format!("Buffer data — {slot_name}"))
            }))
            .child(html!("p", {
                .style("color", "#999")
                .style("line-height", "1.4")
                .text(HELP_TEXT)
            }))
            .child(file_picker(slot_name.clone(), pending.clone(), status.clone()))
            .child(html!("h4", {
                .style("margin-top", "12px")
                .style("color", "#aaa")
                .text("…or paste JSON")
            }))
            .child(json_textarea(pending.clone(), status.clone()))
            .child(html!("div", {
                .style("margin-top", "12px")
                .style("padding", "8px")
                .style("background", "#111")
                .style("border", "1px solid #333")
                .style("border-radius", "3px")
                .style("color", "#cce")
                .text_signal(status.signal_cloned())
            }))
            .child(html!("div", {
                .style("display", "flex")
                .style("justify-content", "flex-end")
                .style("gap", "8px")
                .style("margin-top", "12px")
                .child(html!("button", {
                    .text("Cancel")
                    .event(clone!(open_for => move |_: events::Click| {
                        open_for.set(None);
                    }))
                }))
                .child(html!("button", {
                    .style("background", "#2a4")
                    .style("color", "#fff")
                    .style("border", "1px solid #2a4")
                    .style("padding", "4px 10px")
                    .style("border-radius", "3px")
                    .text("Apply")
                    .event(clone!(open_for, pending, buffer_defaults => move |_: events::Click| {
                        let pending_val = pending.lock_ref().clone();
                        let mut defaults = buffer_defaults.lock_mut();
                        match pending_val {
                            Some(data) if !data.is_empty() => {
                                defaults.insert(slot_name.clone(), data);
                            }
                            _ => {
                                defaults.remove(&slot_name);
                            }
                        }
                        drop(defaults);
                        open_for.set(None);
                    }))
                }))
            }))
        }))
    })
}

fn file_picker(
    slot_name: String,
    pending: Arc<Mutable<Option<Vec<u32>>>>,
    status: Arc<Mutable<String>>,
) -> Dom {
    let _ = slot_name; // slot_name is only used for the status message context
    html!("input" => web_sys::HtmlInputElement, {
        .attr("type", "file")
        .attr("accept", ".bin,.json,application/octet-stream,application/json")
        .style("margin-top", "8px")
        .style("color", "#ddd")
        .with_node!(elem => {
            .event(clone!(elem, pending, status => move |_: events::Change| {
                let Some(files) = elem.files() else { return; };
                let Some(file) = files.get(0) else { return; };
                let name = file.name();
                let is_json = name.ends_with(".json");
                let reader = match web_sys::FileReader::new() {
                    Ok(r) => r,
                    Err(_) => {
                        status.set("FileReader unavailable".to_string());
                        return;
                    }
                };
                let reader_for_closure = reader.clone();
                let pending_for_closure = pending.clone();
                let status_for_closure = status.clone();
                // `Closure::once` returns a Closure handle; the
                // `set_onload` call below borrows its underlying
                // Function. We then `.forget()` the handle so the
                // closure stays alive until the FileReader fires.
                // Without `forget`, the Closure (and its underlying
                // JS function) drops at end-of-scope here — before
                // the async `read_as_*` resolves — and the load
                // callback would throw or no-op. This is the
                // standard wasm-bindgen pattern for one-shot async
                // callbacks; the small leak per file load (a few
                // hundred bytes) is acceptable for editor authoring.
                let onload = Closure::once(move || {
                    let result = match reader_for_closure.result() {
                        Ok(v) => v,
                        Err(_) => {
                            status_for_closure.set("FileReader returned no result".to_string());
                            return;
                        }
                    };
                    let (parsed, msg) = if is_json {
                        let text = result.as_string().unwrap_or_default();
                        parse_json(&text)
                    } else {
                        let array = js_sys::Uint8Array::new(&result);
                        let bytes = array.to_vec();
                        parse_bin(&bytes)
                    };
                    match parsed {
                        Some(words) => {
                            let summary = format!(
                                "{} u32 words loaded from {} ({} bytes).",
                                words.len(),
                                name,
                                words.len() * 4,
                            );
                            pending_for_closure.set(Some(words));
                            status_for_closure.set(summary);
                        }
                        None => {
                            status_for_closure.set(msg);
                        }
                    }
                });
                reader.set_onload(Some(onload.as_ref().unchecked_ref()));
                onload.forget();
                let result = if is_json {
                    reader.read_as_text(&file)
                } else {
                    reader.read_as_array_buffer(&file)
                };
                if let Err(e) = result {
                    status.set(format!("FileReader read failed: {:?}", e));
                }
            }))
        })
    })
}

fn json_textarea(pending: Arc<Mutable<Option<Vec<u32>>>>, status: Arc<Mutable<String>>) -> Dom {
    html!("textarea" => web_sys::HtmlTextAreaElement, {
        .attr("placeholder", "[0.0, 1.0, 0.5, 0.25, …]")
        .style("width", "100%")
        .style("min-height", "72px")
        .style("background", "#0b0b0b")
        .style("color", "#cce")
        .style("border", "1px solid #333")
        .style("padding", "6px")
        .style("font-family", "monospace")
        .style("font-size", "12px")
        .with_node!(elem => {
            .event(clone!(elem, pending, status => move |_: events::Input| {
                let text = elem.value();
                if text.trim().is_empty() {
                    return;
                }
                let (parsed, msg) = parse_json(&text);
                match parsed {
                    Some(words) => {
                        let summary = format!(
                            "{} u32 words parsed from JSON ({} bytes).",
                            words.len(),
                            words.len() * 4,
                        );
                        pending.set(Some(words));
                        status.set(summary);
                    }
                    None => {
                        status.set(msg);
                    }
                }
            }))
        })
    })
}

/// Parse a `.bin` byte slice into `u32` words. The contract says size
/// must be a multiple of 4.
fn parse_bin(bytes: &[u8]) -> (Option<Vec<u32>>, String) {
    if bytes.len() % 4 != 0 {
        return (
            None,
            format!(
                ".bin size {} is not a multiple of 4 — slots are u32 words.",
                bytes.len()
            ),
        );
    }
    let words: Vec<u32> = bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    (Some(words), String::new())
}

/// Parse a JSON array-of-numbers into `u32` words via `f32` bit-cast.
/// Nested arrays are flattened so authors can write
/// `[[x, y, w, h], …]` directly.
fn parse_json(text: &str) -> (Option<Vec<u32>>, String) {
    let value: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => return (None, format!("JSON parse error: {e}")),
    };
    let mut out: Vec<u32> = Vec::new();
    if let Err(e) = flatten_json_numbers(&value, &mut out) {
        return (None, e);
    }
    if out.is_empty() {
        return (None, "JSON parsed but produced no numbers".to_string());
    }
    (Some(out), String::new())
}

fn flatten_json_numbers(v: &serde_json::Value, out: &mut Vec<u32>) -> Result<(), String> {
    match v {
        serde_json::Value::Number(n) => {
            let f = n
                .as_f64()
                .ok_or_else(|| format!("non-finite number in JSON: {v:?}"))?;
            let bits = (f as f32).to_bits();
            out.push(bits);
            Ok(())
        }
        serde_json::Value::Array(items) => {
            for item in items {
                flatten_json_numbers(item, out)?;
            }
            Ok(())
        }
        other => Err(format!(
            "expected JSON numbers (or nested arrays of numbers); got {other:?}"
        )),
    }
}
