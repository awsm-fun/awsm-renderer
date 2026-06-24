//! The Help modal — a small tabbed guide. The "Using the MCP" tab is the
//! important one: how to install / run the `awsm-renderer-scene-mcp` server, attach this
//! editor, and point an agent at it. Opened from the top-bar Help button, and
//! (deep-linked to the MCP tab) from the MCP connect modal's Help button.

use crate::prelude::*;

/// Open the Help modal on its Overview tab (the top-bar Help button).
pub fn open_help() {
    open_help_at(0);
}

/// Open the Help modal directly on the "Using the MCP" tab (the MCP connect
/// modal's Help button deep-links here).
pub fn open_help_mcp() {
    open_help_at(1);
}

fn open_help_at(tab: usize) {
    Modal::open(move || {
        let active = Mutable::new(tab);
        ModalCard::new("Help")
            .width(660.0)
            .child(html!("div", {
                .style("display", "flex")
                .style("flex-direction", "column")
                .style("gap", "16px")
                // Tab bar (rebuilt on change so the active tab reads as Primary).
                .child_signal(active.signal().map(clone!(active => move |cur| Some(html!("div", {
                    .style("display", "flex")
                    .style("gap", "6px")
                    .child(tab_btn(&active, 0, "Overview", cur == 0))
                    .child(tab_btn(&active, 1, "Using the MCP", cur == 1))
                })))))
                // Body — the active tab's content.
                .child_signal(active.signal().map(|cur| Some(match cur {
                    1 => mcp_section(),
                    _ => overview_section(),
                })))
            }))
            .footer(
                Btn::new()
                    .label("Close")
                    .variant(BtnVariant::Primary)
                    .on_click(Modal::close)
                    .render(),
            )
            .render()
    });
}

fn tab_btn(active: &Mutable<usize>, idx: usize, label: &str, is_active: bool) -> Dom {
    let active = active.clone();
    Btn::new()
        .label(label)
        .variant(if is_active {
            BtnVariant::Primary
        } else {
            BtnVariant::Ghost
        })
        .size(BtnSize::Sm)
        .on_click(move || active.set_neq(idx))
        .render()
}

// ── content helpers ─────────────────────────────────────────────────────────

/// A section heading.
fn h(text: &str) -> Dom {
    html!("div", {
        .style("font-size", "13.5px")
        .style("font-weight", "650")
        .style("color", "var(--text-0)")
        .style("margin-top", "4px")
        .text(text)
    })
}

/// A body paragraph.
fn p(text: &str) -> Dom {
    html!("p", {
        .style("margin", "0")
        .style("font-size", "12.5px")
        .style("color", "var(--text-1)")
        .style("line-height", "1.55")
        .text(text)
    })
}

/// A monospace code / command block (multi-line, horizontally scrollable) with a
/// clipboard button in its top-right corner — the install/connect commands are
/// long and awkward to select by hand. The button flashes ✓ on a successful copy.
fn code(text: &'static str) -> Dom {
    let copied = Mutable::new(false);
    html!("div", {
        .style("position", "relative")
        .child(html!("div", {
            .class("mono")
            .style("font-size", "11.5px")
            .style("color", "var(--text-1)")
            .style("background", "var(--bg-3)")
            .style("border", "1px solid var(--line-soft)")
            .style("border-radius", "var(--r1)")
            // Extra right padding so long lines don't run under the copy button.
            .style("padding", "7px 34px 7px 9px")
            .style("overflow-x", "auto")
            .style("white-space", "pre")
            .text(text)
        }))
        .child(html!("button", {
            .style("position", "absolute")
            .style("top", "5px")
            .style("right", "5px")
            .style("display", "inline-flex")
            .style("align-items", "center")
            .style("justify-content", "center")
            .style("width", "22px")
            .style("height", "22px")
            .style("padding", "0")
            .style("cursor", "pointer")
            .style("border-radius", "var(--r1)")
            .style("background", "var(--bg-2)")
            .style("border", "1px solid var(--line-soft)")
            .style("font-size", "12px")
            .style("line-height", "1")
            .style_signal("color", copied.signal().map(|c| {
                if c { "var(--ok)" } else { "var(--text-2)" }
            }))
            .attr("title", "Copy to clipboard")
            .text_signal(copied.signal().map(|c| if c { "\u{2713}" } else { "\u{1F4CB}" }))
            .event(clone!(copied => move |_: events::Click| {
                copy_to_clipboard(text);
                copied.set(true);
                spawn_local(clone!(copied => async move {
                    gloo_timers::future::TimeoutFuture::new(1200).await;
                    copied.set(false);
                }));
            }))
        }))
    })
}

/// A captioned command: a muted caption line (e.g. the platform a command is
/// for) above the copyable command box. The caption stays OUTSIDE the box so the
/// copy button grabs only the command itself — no `#` comment to strip.
fn command(caption: &str, cmd: &'static str) -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "4px")
        .child(html!("div", {
            .style("font-size", "11px")
            .style("color", "var(--text-3)")
            .text(caption)
        }))
        .child(code(cmd))
    })
}

/// Write `text` to the OS clipboard (fire-and-forget; the promise is driven to
/// completion so the browser doesn't log an unhandled rejection).
fn copy_to_clipboard(text: &str) {
    if let Some(win) = web_sys::window() {
        let promise = win.navigator().clipboard().write_text(text);
        spawn_local(async move {
            let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
        });
    }
}

/// A vertical stack wrapping one tab's content.
fn stack(children: Vec<Dom>) -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "10px")
        .children(children)
    })
}

// ── tabs ────────────────────────────────────────────────────────────────────

fn overview_section() -> Dom {
    stack(vec![
        p(
            "A WebGPU scene, material & animation editor that runs entirely in your \
           browser (Chrome, Edge, Arc, or Brave — it needs two Chromium-only features).",
        ),
        h("The basics"),
        p(
            "Switch between Scene, Material, and Animation with the segmented control in \
           the top bar. Load opens a project directory; Save writes it back. The gear \
           (Settings) controls the viewport + chrome only — those are not saved into the \
           project file.",
        ),
        h("Drive it with an AI agent"),
        p(
            "The editor exposes an MCP server, so an AI agent (Claude, Codex, Cursor, …) \
           can build the scene for you through typed tool calls — on the same canvas \
           you're looking at. See the “Using the MCP” tab to set it up.",
        ),
    ])
}

fn mcp_section() -> Dom {
    stack(vec![
        h("What it is"),
        p("This editor ships an MCP server: it lets an AI agent (or any MCP client) \
           drive the editor — add nodes, edit meshes, author materials and animation, \
           and screenshot the result — entirely through typed tool calls. The agent \
           works the same canvas you do; you watch it build in real time."),
        p("The loop has three pieces, all required: the MCP server, an attached editor \
           tab (this page — the scene truth), and your agent. Set them up in that order."),

        h("1 · Install the server"),
        p("Prebuilt binaries — no toolchain needed:"),
        command("macOS / Linux", "curl --proto '=https' --tlsv1.2 -LsSf https://github.com/awsm-fun/awsm-renderer/releases/latest/download/awsm-scene-mcp-installer.sh | sh"),
        command("Windows (PowerShell)", "powershell -ExecutionPolicy Bypass -c \"irm https://github.com/awsm-fun/awsm-renderer/releases/latest/download/awsm-scene-mcp-installer.ps1 | iex\""),
        command("From source (needs Rust)", "cargo install --git https://github.com/awsm-fun/awsm-renderer awsm-renderer-scene-mcp"),

        h("2 · Run it in a terminal"),
        p("Start the server and leave it running (it defaults to port 9086):"),
        code("awsm-renderer-scene-mcp"),
        p("It listens on http://127.0.0.1:9086 — /mcp for agents, plus a WebSocket this \
           editor dials out to. Pass --port to change it."),

        h("3 · Connect this editor"),
        p("Two ways to attach this tab to a running server:"),
        p("• Click the MCP button in the top bar, check the address, then Connect."),
        p("• Or open the editor with a ?mcp= parameter to auto-connect:"),
        code("http://localhost:9085/?mcp=http://127.0.0.1:9086"),
        p("For a TLS-terminated remote server, tick “Use TLS” in the connect modal. \
           When the server has more than one tab/agent it asks for a pairing code — the \
           agent prints it; enter it in the modal or append &pair=<code> to the URL. \
           Attached, the MCP button shows “MCP ✓” and a 🤖 chip tells you when the agent \
           is editing."),

        h("4 · Point your agent at it"),
        p("It's a streamable-HTTP MCP server, so every MCP client connects to the same \
           URL:"),
        code("http://127.0.0.1:9086/mcp"),
        p("A ready-to-use .mcp.json sits in the repo root. Register the server the way \
           your agent does — for example:"),
        command("Claude Code", "claude mcp add --transport http awsm-renderer-scene http://127.0.0.1:9086/mcp"),
        command(".mcp.json (Claude Code / Cursor / others)", "{ \"mcpServers\": { \"awsm-renderer-scene\": { \"type\": \"http\", \"url\": \"http://127.0.0.1:9086/mcp\" } } }"),
        p("Then just ask: “add a tessellated sphere with a brushed-metal material”, or \
           “rough up that mesh and screenshot it”. The agent discovers every node and \
           command from the server's typed schema — no guesswork. If it reports “no \
           editor is paired”, have it call its pairing_status tool to get the code."),

        h("5 · Watch it work"),
        p("While the agent drives, the 🤖 chip pulses and the “Agent activity feed” \
           narrates each edit and briefly spotlights the panel it touched. Toggle it \
           under Settings → Agent activity feed."),
    ])
}
