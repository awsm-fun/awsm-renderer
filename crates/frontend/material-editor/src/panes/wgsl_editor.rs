//! WGSL editor pane — center.
//!
//! Phase 8: read-only `<textarea>` showing the loaded material's WGSL.
//! Phase 9 wires Ctrl-S / blur to a debounced recompile. Phase 10
//! adds the auto-generated `struct MaterialData { ... }` preview
//! above the user's body. Phase 11 gives the textarea a stable DOM
//! id so the Errors pane can drive `setSelectionRange` for click-to-
//! position-cursor (see [`crate::panes::errors`]).

use dominator::{html, with_node, Dom};
use wasm_bindgen::JsCast;

use crate::state::EditState;

/// DOM id used by the Errors pane to look up the textarea and position
/// the caret at a parsed naga line/column. Stable so [`focus_at`] can
/// resolve it without threading a node handle through the UI.
pub const WGSL_EDITOR_TEXTAREA_ID: &str = "wgsl-editor-textarea";

pub fn render(state: &EditState) -> Dom {
    let wgsl = state.wgsl_source.clone();
    let wgsl_for_input = wgsl.clone();
    html!("div", {
        .style("padding", "12px")
        .style("border-right", "1px solid #333")
        .style("background", "#111")
        .style("color", "#ddd")
        .style("display", "flex")
        .style("flex-direction", "column")
        .child(html!("h3", { .text("shader.wgsl") }))
        .child(html!("textarea" => web_sys::HtmlTextAreaElement, {
            .attr("id", WGSL_EDITOR_TEXTAREA_ID)
            .style("flex", "1")
            .style("font-family", "monospace")
            .style("font-size", "12px")
            .style("background", "#0b0b0b")
            .style("color", "#cce")
            .style("border", "1px solid #333")
            .style("padding", "8px")
            .style("resize", "none")
            .prop_signal("value", wgsl.signal_cloned())
            .with_node!(_elem => {
                .event(move |e: dominator::events::Input| {
                    if let Some(target) = e.target() {
                        if let Ok(ta) = target.dyn_into::<web_sys::HtmlTextAreaElement>() {
                            wgsl_for_input.set(ta.value());
                        }
                    }
                })
            })
        }))
    })
}

/// Best-effort: focus the WGSL textarea and position the caret at
/// `(line, column)` (both 1-based; column is optional and defaults to
/// 1). Resolves the character offset by walking the textarea's current
/// `value` — naga reports diagnostics against the same source the
/// textarea holds, so the offsets line up. Silent no-op if either the
/// textarea isn't mounted yet or the line is past EOF.
pub fn focus_at(line: u32, column: Option<u32>) {
    let Some(document) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    let Some(elem) = document.get_element_by_id(WGSL_EDITOR_TEXTAREA_ID) else {
        return;
    };
    let Ok(ta) = elem.dyn_into::<web_sys::HtmlTextAreaElement>() else {
        return;
    };
    let value = ta.value();
    let target_line = line.saturating_sub(1) as usize;
    // Column is 1-based in naga diagnostics; clamp to the line's length
    // below so trailing-newline diagnostics don't blow past the line.
    let target_col = column.unwrap_or(1).saturating_sub(1) as usize;

    // Walk to the start of the target line. `lines()` strips trailing
    // newlines, so we accumulate the byte length of each preceding line
    // plus one (the consumed newline).
    let mut offset = 0usize;
    for (i, line_text) in value.split('\n').enumerate() {
        if i == target_line {
            let line_len = line_text.chars().count();
            let col = target_col.min(line_len);
            // Convert char index → UTF-16 code-unit index, which is
            // what HTMLTextAreaElement.setSelectionRange expects.
            let char_index = offset + col;
            let utf16_index = char_index_to_utf16(&value, char_index);
            let _ = ta.focus();
            let _ = ta.set_selection_range(utf16_index as u32, utf16_index as u32);
            return;
        }
        offset += line_text.chars().count() + 1; // +1 for the consumed '\n'
    }
}

/// Convert a char index (Unicode scalar position) within `s` to a
/// UTF-16 code-unit index — the unit `setSelectionRange` operates in.
/// Single BMP characters are 1 unit; supplementary plane (e.g. emoji)
/// are 2. Most WGSL source is ASCII so the loop is cheap.
fn char_index_to_utf16(s: &str, char_index: usize) -> usize {
    let mut units = 0usize;
    for (i, ch) in s.chars().enumerate() {
        if i == char_index {
            return units;
        }
        units += ch.len_utf16();
    }
    units
}
