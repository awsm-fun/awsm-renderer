//! The Help modal — a guide to the editor, the renderer, the LOD/nanite
//! pipeline, building a player, and driving it all over MCP. Opened from the
//! top-bar Help button, and (deep-linked to the MCP tab) from the MCP connect
//! modal's Help button.
//!
//! Layout is a left **tab rail** (vertical, so labels never wrap) + a scrolling
//! content pane. Content is built from a small set of typography helpers
//! (`lede`/`h`/`p`/`bullets`/`command`/`code`) so every tab reads consistently;
//! `rich()` turns `\`backtick\`` spans into inline-code chips.

use crate::prelude::*;

/// One Help tab: label, rail icon, and the builder for its body.
type HelpTab = (&'static str, &'static str, fn() -> Dom);

/// Tab order. `open_help_mcp` deep-links to the MCP tab.
const TABS: &[HelpTab] = &[
    ("Overview", "grid", overview_section),
    ("Editor", "sliders", editor_section),
    ("Renderer", "cube", renderer_section),
    ("LOD & Nanite", "layers", lod_section),
    ("Player", "code", player_section),
    ("Using the MCP", "link", mcp_section),
];
/// Index of the MCP tab in [`TABS`] (deep-link target).
const MCP_TAB: usize = 5;

/// Open the Help modal on its Overview tab (the top-bar Help button).
pub fn open_help() {
    open_help_at(0);
}

/// Open the Help modal directly on the "Using the MCP" tab (the MCP connect
/// modal's Help button deep-links here).
pub fn open_help_mcp() {
    open_help_at(MCP_TAB);
}

fn open_help_at(tab: usize) {
    // Wide host: the two-pane (rail + content) layout needs the room, and it
    // keeps every tab label on one line.
    Modal::open_sized(ModalSize::Wide, move || {
        let active = Mutable::new(tab);
        ModalCard::new("Help")
            .width(880.0)
            .child(html!("div", {
                .style("display", "flex")
                .style("gap", "20px")
                .style("align-items", "stretch")
                // ── tab rail (vertical) ──────────────────────────────────────
                .child(html!("div", {
                    .style("display", "flex")
                    .style("flex-direction", "column")
                    .style("gap", "2px")
                    .style("flex", "0 0 auto")
                    .style("width", "176px")
                    .style("border-right", "1px solid var(--line-soft)")
                    .style("padding-right", "12px")
                    .children(TABS.iter().enumerate().map(|(i, (label, icon, _))| {
                        rail_btn(&active, i, label, icon)
                    }).collect::<Vec<_>>())
                }))
                // ── content pane (scrolls) ───────────────────────────────────
                .child(html!("div", {
                    .style("flex", "1")
                    .style("min-width", "0")
                    .child_signal(active.signal().map(|cur| {
                        let (label, _icon, render) = *TABS.get(cur).unwrap_or(&TABS[0]);
                        Some(html!("div", {
                            .style("max-height", "62vh")
                            .style("overflow-y", "auto")
                            .style("padding-right", "10px")
                            .style("display", "flex")
                            .style("flex-direction", "column")
                            .style("gap", "11px")
                            // The active tab's title, as a content header.
                            .child(html!("div", {
                                .style("font-size", "15px")
                                .style("font-weight", "650")
                                .style("color", "var(--text-0)")
                                .style("padding-bottom", "10px")
                                .style("border-bottom", "1px solid var(--line-soft)")
                                .text(label)
                            }))
                            .child(render())
                        }))
                    }))
                }))
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

/// One vertical rail entry. Reactive to `active` (highlight) and local hover, so
/// the rail never rebuilds on selection.
fn rail_btn(active: &Mutable<usize>, idx: usize, label: &str, icon: &str) -> Dom {
    let active = active.clone();
    let hover = Mutable::new(false);
    let label = label.to_string();
    let icon = icon.to_string();
    html!("button", {
        .class("t")
        .style("display", "flex")
        .style("align-items", "center")
        .style("gap", "9px")
        .style("width", "100%")
        .style("text-align", "left")
        .style("cursor", "pointer")
        .style("padding", "7px 9px")
        .style("border-radius", "var(--r1)")
        .style("border-style", "none")
        .style("border-left-style", "solid")
        .style("border-left-width", "2px")
        .style("font-size", "12.5px")
        .style_signal("border-left-color", active.signal().map(move |a| {
            if a == idx { "var(--accent-bright)" } else { "transparent" }
        }))
        .style_signal("font-weight", active.signal().map(move |a| {
            if a == idx { "600" } else { "500" }
        }))
        .style_signal("color", map_ref! {
            let a = active.signal(), let h = hover.signal() =>
            if *a == idx { "var(--text-0)" } else if *h { "var(--text-1)" } else { "var(--text-2)" }
        })
        .style_signal("background", map_ref! {
            let a = active.signal(), let h = hover.signal() =>
            if *a == idx { "var(--accent-ghost)" } else if *h { "var(--bg-3)" } else { "transparent" }
        })
        .event(clone!(hover => move |_: events::MouseEnter| hover.set_neq(true)))
        .event(clone!(hover => move |_: events::MouseLeave| hover.set_neq(false)))
        .event(clone!(active => move |_: events::Click| active.set_neq(idx)))
        .child(Icon::new(icon).size(15.0).render())
        .child(html!("span", { .text(&label) }))
    })
}

// ── content helpers ─────────────────────────────────────────────────────────

/// Split `text` into inline runs, rendering `\`backtick\`` spans as inline-code
/// chips and everything else as plain text. Returns the run nodes for use as the
/// children of a paragraph / list item.
fn rich(text: &str) -> Vec<Dom> {
    text.split('`')
        .enumerate()
        .filter(|(_, seg)| !seg.is_empty())
        .map(|(i, seg)| {
            if i % 2 == 1 {
                html!("code", {
                    .class("mono")
                    .style("font-size", "0.88em")
                    .style("background", "var(--bg-3)")
                    .style("border", "1px solid var(--line-soft)")
                    .style("border-radius", "4px")
                    .style("padding", "0.5px 4px")
                    .style("color", "var(--text-0)")
                    .style("white-space", "nowrap")
                    .text(seg)
                })
            } else {
                html!("span", { .text(seg) })
            }
        })
        .collect()
}

/// The lead paragraph of a tab — slightly larger, higher-contrast.
fn lede(text: &str) -> Dom {
    html!("p", {
        .style("margin", "0")
        .style("font-size", "13.5px")
        .style("color", "var(--text-0)")
        .style("line-height", "1.6")
        .children(rich(text))
    })
}

/// A section heading — a compact uppercase accent kicker that cleanly separates
/// the dense body blocks.
fn h(text: &str) -> Dom {
    html!("div", {
        .style("font-size", "11px")
        .style("font-weight", "700")
        .style("letter-spacing", "0.07em")
        .style("text-transform", "uppercase")
        .style("color", "var(--accent-bright)")
        .style("margin-top", "8px")
        .text(text)
    })
}

/// A body paragraph (with inline-code support).
fn p(text: &str) -> Dom {
    html!("p", {
        .style("margin", "0")
        .style("font-size", "13px")
        .style("color", "var(--text-1)")
        .style("line-height", "1.65")
        .children(rich(text))
    })
}

/// A bulleted list — proper hanging indents instead of `•`-prefixed run-on text.
fn bullets(items: Vec<&str>) -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "7px")
        .children(items.into_iter().map(|t| html!("div", {
            .style("display", "flex")
            .style("gap", "9px")
            .style("align-items", "baseline")
            .style("font-size", "13px")
            .style("color", "var(--text-1)")
            .style("line-height", "1.6")
            .child(html!("span", {
                .style("flex", "0 0 auto")
                .style("color", "var(--accent-bright)")
                .text("\u{2022}")
            }))
            .child(html!("span", { .style("flex", "1").style("min-width", "0").children(rich(t)) }))
        })).collect::<Vec<_>>())
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
            .style("font-size", "12px")
            .style("color", "var(--text-1)")
            .style("background", "var(--bg-3)")
            .style("border", "1px solid var(--line-soft)")
            .style("border-radius", "var(--r1)")
            // Extra right padding so long lines don't run under the copy button.
            .style("padding", "8px 34px 8px 10px")
            .style("overflow-x", "auto")
            .style("white-space", "pre")
            .style("line-height", "1.5")
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
        .style("gap", "5px")
        .child(html!("div", {
            .style("font-size", "11px")
            .style("font-weight", "600")
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
        .style("gap", "11px")
        .children(children)
    })
}

// ── tabs ────────────────────────────────────────────────────────────────────

fn overview_section() -> Dom {
    stack(vec![
        lede(
            "AwsmRenderer is a WebGPU renderer with a built-in scene, material & animation \
           editor that runs entirely in your browser (Chrome, Edge, Arc, or Brave — it needs \
           two Chromium-only features). You author here, export a compact bundle, and load \
           it in your own game/app with a few lines of Rust.",
        ),
        h("How the pieces fit"),
        bullets(vec![
            "Editor — author scenes, meshes, materials, animation (this app).",
            "Renderer — the WebGPU engine the editor and your player both run.",
            "Player — your shipped app: load an exported scene with `populate_awsm_scene`.",
            "MCP — let an AI agent drive the editor through typed tool calls, via a local MCP server CLI.",
            "LOD / Nanite — bounded draw + VRAM for heavy meshes, with an offline pre-bake CLI.",
        ]),
        p("Each has its own tab in the sidebar. Start with \u{201c}Editor\u{201d} to author, \u{201c}Player\u{201d} to ship."),
        h("The basics"),
        p(
            "Switch between Scene, Material, and Animation with the segmented control in \
           the top bar. Load opens a project directory; Save writes it back. The gear \
           (Settings) controls the viewport + chrome only — those are not saved into the \
           project file.",
        ),
    ])
}

fn editor_section() -> Dom {
    stack(vec![
        h("Three modes"),
        p(
            "The top-bar segmented control switches between Scene, Material, and Animation. \
           Each has its own left panel (tree / library / clips) and inspector.",
        ),
        h("Scene mode"),
        p(
            "Insert primitives, lights, cameras, curves and visual nodes from the Insert bar. \
           The Outliner (left) is the node tree; select a node to edit it in the Properties \
           inspector (right). The viewport toolbar gives Move / Rotate / Scale gizmos and \
           perspective/ortho toggles. Meshes are non-destructive: each carries a modifier \
           stack (subdivide, twist, bend, taper, lathe, sweep, displace …) you can reorder \
           and tweak live, plus per-vertex selection + paint ops.",
        ),
        h("Material mode"),
        p(
            "Build PBR materials, or write custom WGSL against the material contract (opaque / \
           transparent / vertex hooks). The Studio previews on a sphere; assign materials to \
           nodes back in Scene mode.",
        ),
        h("Animation mode"),
        p(
            "Author clips with tracks + keyframes (transform, morph weights, material params), \
           scrub the playhead, and blend via mixer layers.",
        ),
        h("Load / Save / Export"),
        p(
            "Load opens a project directory; Save writes it back. Export produces a player \
           bundle (a `scene.toml` + an `assets/` folder of glbs/textures) — that's what your \
           player loads. Settings (the gear) only affects the viewport + chrome and is NOT \
           saved into the project.",
        ),
    ])
}

fn renderer_section() -> Dom {
    stack(vec![
        h("What it is"),
        p(
            "A modern WebGPU renderer. The geometry pass is a thin VISIBILITY-BUFFER pass: it \
           writes only triangle IDs, and all shading is deferred and computed per-pixel — so \
           cost scales with screen pixels, not scene complexity.",
        ),
        h("Features"),
        bullets(vec![
            "PBR materials + custom WGSL materials (with a typed contract).",
            "GPU-driven culling with hierarchical-Z occlusion.",
            "Shadows (cascaded sun + point), image-based lighting / environment.",
            "MSAA with an edge-resolve pass; decals; GPU particles.",
            "Skinning + morph targets; instancing.",
            "LOD: discrete level chains + cluster \u{201c}virtual geometry\u{201d} (nanite-style).",
        ]),
        h("Bounded by the screen, not the asset"),
        p(
            "With cluster LOD, the drawn triangle count tracks screen resolution + a pixel-error \
           budget — roughly a few hundred thousand to ~2M triangles for typical resolutions — \
           whether the source mesh is 1M or 500M triangles. A fixed-capacity page pool keeps \
           VRAM bounded too (see the LOD & Nanite tab).",
        ),
        h("Profiles"),
        p(
            "Pick a `RendererProfile` (Desktop / Mobile / Cinema) at build time to set \
           quality + feature defaults; individual features are also togglable via \
           `RendererFeatures` on the builder.",
        ),
    ])
}

fn lod_section() -> Dom {
    stack(vec![
        h("Two LOD systems"),
        bullets(vec![
            "Discrete chains — simplified level meshes selected per-instance by screen-space \
             error (great for skinned/deforming and mid-size meshes).",
            "Cluster \u{201c}virtual geometry\u{201d} (nanite-style) — a per-cluster GPU cut over a \
             baked DAG, with streaming residency so multi-million-triangle static meshes render \
             with BOUNDED draw + BOUNDED VRAM.",
        ]),
        h("Per-mesh toggle"),
        p(
            "Every mesh has a LOD toggle (inspector → LOD → Enabled), on by default. When on, \
           the export bake generates the simplified levels and — for dense static meshes — a \
           cluster DAG (`<id>.clusters.bin`). Turn it off to ship a mesh at full detail.",
        ),
        h("Pre-bake offline (the CLI)"),
        p(
            "Baking a huge mesh in the browser is slow. The `awsm-renderer-lod-bake` CLI converts \
           a glTF/GLB into nanite-ready assets OFFLINE — a base glb, the discrete levels + \
           manifest, and the cluster DAG — so you import pre-baked instead of converting \
           in-editor.",
        ),
        h("Install the CLI"),
        p("Prebuilt binaries — no toolchain needed (installs the `awsm-renderer-lod-bake` command):"),
        command("macOS / Linux", "curl --proto '=https' --tlsv1.2 -LsSf https://github.com/awsm-fun/awsm-renderer/releases/latest/download/awsm-renderer-lod-bake-cli-installer.sh | sh"),
        command("Windows (PowerShell)", "powershell -ExecutionPolicy Bypass -c \"irm https://github.com/awsm-fun/awsm-renderer/releases/latest/download/awsm-renderer-lod-bake-cli-installer.ps1 | iex\""),
        command("From source (needs Rust)", "cargo install --git https://github.com/awsm-fun/awsm-renderer awsm-renderer-lod-bake-cli"),
        h("Run it"),
        command(
            "Pre-bake a model (writes the base glb + levels + cluster DAG into ./assets)",
            "awsm-renderer-lod-bake my-model.glb --out ./assets",
        ),
        p(
            "It reuses the exact bake the editor uses, so the output is identical to an \
           in-editor bake — just done once, up front.",
        ),
        h("Zero cost when you don't use it"),
        p(
            "LOD is a Cargo feature (default-on). Build your player with \
           `default-features = false` (no `lod`) and ALL LOD code is compiled out. Even with \
           it on, a scene whose meshes don't use LOD pays nothing per frame — the cluster cut \
           and paging early-out when no cluster mesh is resident.",
        ),
    ])
}

fn player_section() -> Dom {
    stack(vec![
        h("Ship what you authored"),
        p(
            "A player is your own Rust app that builds an `AwsmRenderer`, loads the bundle the \
           editor exported (`scene.toml` + `assets/`), and drives a render loop. The editor \
           imports arbitrary glTF and refactors it into this compact format; your player loads \
           that fast path with `populate_awsm_scene`.",
        ),
        h("Minimal load + loop"),
        code(
            "use awsm_renderer::AwsmRendererBuilder;\n\
             use awsm_renderer_scene_loader::populate_awsm_scene;\n\
             use std::collections::HashMap;\n\
             \n\
             // 1. Build the renderer onto a <canvas> (GPU setup: see the Player Guide).\n\
             let mut renderer = AwsmRendererBuilder::new(gpu_builder).build().await?;\n\
             \n\
             // 2. Parse the exported scene.toml.\n\
             let scene = awsm_renderer_scene::scene_from_toml(&scene_toml_text)?;\n\
             \n\
             // 3. Pre-fetch every asset the scene references (path -> bytes).\n\
             //    e.g. \"assets/<id>.glb\", \"assets/<id>.clusters.bin\", textures…\n\
             let mut assets: HashMap<String, Vec<u8>> = HashMap::new();\n\
             // assets.insert(\"assets/<id>.glb\".into(), fetch(\"assets/<id>.glb\").await?);\n\
             \n\
             // 4. Load it (commits internally — do NOT call commit_load after).\n\
             let _loaded = populate_awsm_scene(&mut renderer, &scene, &assets, |_p| {}).await?;\n\
             \n\
             // 5. Each frame, in order:\n\
             renderer.update_animations(dt_ms)?;\n\
             renderer.update_camera(camera_matrices)?;\n\
             renderer.update_transforms();\n\
             renderer.render(None)?;",
        ),
        h("Notes"),
        bullets(vec![
            "`assets` is a map of bundle-relative path → already-fetched bytes — the loader \
             never fetches for you. A primitive-only scene uses an empty map.",
            "The renderer is matrices-only: you supply the camera matrices and drive the loop \
             from requestAnimationFrame.",
            "Full setup (canvas, GPU device, camera) is in docs/PLAYER-GUIDE.md.",
        ]),
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
        command("macOS / Linux", "curl --proto '=https' --tlsv1.2 -LsSf https://github.com/awsm-fun/awsm-renderer/releases/latest/download/awsm-renderer-scene-mcp-installer.sh | sh"),
        command("Windows (PowerShell)", "powershell -ExecutionPolicy Bypass -c \"irm https://github.com/awsm-fun/awsm-renderer/releases/latest/download/awsm-renderer-scene-mcp-installer.ps1 | iex\""),
        command("From source (needs Rust)", "cargo install --git https://github.com/awsm-fun/awsm-renderer awsm-renderer-scene-mcp"),

        h("2 · Run it in a terminal"),
        p("Start the server and leave it running (it defaults to port 9086):"),
        code("awsm-renderer-scene-mcp"),
        p("It listens on http://127.0.0.1:9086 — /mcp for agents, plus a WebSocket this \
           editor dials out to. Pass --port to change it."),

        h("3 · Connect this editor"),
        p("Two ways to attach this tab to a running server:"),
        bullets(vec![
            "Click the MCP button in the top bar, check the address, then Connect.",
            "Or open the editor with a `?mcp=` parameter to auto-connect:",
        ]),
        code("http://localhost:9085/?mcp=http://127.0.0.1:9086"),
        p("For a TLS-terminated remote server, tick \u{201c}Use TLS\u{201d} in the connect modal. \
           Each server serves a single editor tab — if a newer tab attaches to the same \
           server, the older one detaches (to run two sessions at once, start a second \
           server on another port). Attached, the MCP button shows \u{201c}MCP \u{2713}\u{201d} and a 🤖 \
           chip tells you when the agent is editing."),

        h("4 · Point your agent at it"),
        p("It's a streamable-HTTP MCP server, so every MCP client connects to the same \
           URL:"),
        code("http://127.0.0.1:9086/mcp"),
        p("A ready-to-use .mcp.json sits in the repo root. Register the server the way \
           your agent does — for example:"),
        command("Claude Code", "claude mcp add --transport http awsm-renderer-scene http://127.0.0.1:9086/mcp"),
        command(".mcp.json (Claude Code / Cursor / others)", "{ \"mcpServers\": { \"awsm-renderer-scene\": { \"type\": \"http\", \"url\": \"http://127.0.0.1:9086/mcp\" } } }"),
        p("Then just ask: \u{201c}add a tessellated sphere with a brushed-metal material\u{201d}, or \
           \u{201c}rough up that mesh and screenshot it\u{201d}. The agent discovers every node and \
           command from the server's typed schema — no guesswork. If it reports \u{201c}no \
           editor tab is attached\u{201d}, make sure this editor is connected (MCP \u{2713})."),

        h("5 · Watch it work"),
        p("While the agent drives, the 🤖 chip pulses and the \u{201c}Agent activity feed\u{201d} \
           narrates each edit and briefly spotlights the panel it touched. Toggle it \
           under Settings → Agent activity feed."),
    ])
}
