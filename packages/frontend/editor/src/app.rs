//! App shell. M0: an empty themed shell (top bar + void body) that proves the
//! design-system tokens mount and the build renders in real Chrome. The mode
//! router, ribbon host, and global overlays (toasts / modals / ⌘K) land in M3
//! on top of the `EditorController`.

use awsm_web_shared::prelude::*;

const FONT_STACK: &str = "ui-sans-serif, system-ui, -apple-system, 'Segoe UI', Roboto, sans-serif";

pub fn render() -> Dom {
    html!("div", {
        .style("width", "100%")
        .style("height", "100%")
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("background-color", "var(--bg-0)")
        .style("color", "var(--text-1)")
        .style("font-family", FONT_STACK)
        .style("font-size", "13px")
        // Global overlay hosts — render nothing until used, but wired now so the
        // panic hook has somewhere to surface and forward milestones can push
        // toasts immediately.
        .child(Modal::render())
        .child(Toast::render())
        .child(top_bar())
        .child(body())
    })
}

fn top_bar() -> Dom {
    html!("div", {
        .style("flex", "0 0 auto")
        .style("height", "44px")
        .style("display", "flex")
        .style("align-items", "center")
        .style("gap", "10px")
        .style("padding", "0 12px")
        .style("background-color", "var(--bg-2)")
        .style("border-bottom", "1px solid var(--line)")
        .child(html!("div", {
            .style("display", "flex")
            .style("align-items", "center")
            .style("gap", "8px")
            .child(html!("div", {
                .style("width", "20px")
                .style("height", "20px")
                .style("border-radius", "5px")
                .style("background", "linear-gradient(135deg, var(--accent-bright), var(--accent))")
            }))
            .child(html!("span", {
                .style("font-weight", "600")
                .style("color", "var(--text-0)")
                .style("letter-spacing", "0.01em")
                .text("AwsmRenderer")
            }))
        }))
        .child(html!("div", { .style("flex", "1 1 0") }))
        .child(html!("span", {
            .style("font-family", "'JetBrains Mono', ui-monospace, monospace")
            .style("font-size", "11px")
            .style("color", "var(--text-3)")
            .style("letter-spacing", "0.04em")
            .text("v2 rebuild · M0")
        }))
    })
}

fn body() -> Dom {
    html!("div", {
        .style("flex", "1 1 0")
        .style("min-height", "0")
        .style("display", "flex")
        .style("align-items", "center")
        .style("justify-content", "center")
        .style("background-color", "var(--bg-0)")
        .child(html!("span", {
            .style("color", "var(--text-3)")
            .style("font-size", "12px")
            .style("letter-spacing", "0.04em")
            .text("empty app shell")
        }))
    })
}
