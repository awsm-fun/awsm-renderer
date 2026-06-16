//! App shell: the top bar + mode router + global overlay hosts. Every action is
//! a dispatched [`EditorCommand`] through the [`controller`] — the UI never
//! mutates editor state directly.

use crate::controller::CameraAxis;
use crate::prelude::*;

const ACCENT_FG: &str = "oklch(0.18 0.02 255)";

/// A camera-axis snap button for the Settings → Camera grid.
fn cam_axis_btn(label: &str, axis: CameraAxis) -> Dom {
    Btn::new()
        .label(label)
        .variant(BtnVariant::Ghost)
        .size(BtnSize::Sm)
        .full(true)
        .on_click(move || {
            spawn_local(async move {
                let _ = controller()
                    .dispatch(EditorCommand::SnapCameraToAxis { axis })
                    .await;
            })
        })
        .render()
}

pub fn render() -> Dom {
    let ctrl = controller();

    // Overlays the root div (which hosts the live canvas + Modal/Toast). The
    // Scene viewport slot reparents the canvas into itself.
    html!("div", {
        .style("position", "absolute")
        .style("inset", "0")
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("font-size", "13px")
        .style("background-color", "var(--bg-0)")
        .style("color", "var(--text-0)")
        // ⌘K / Ctrl-K toggles the command palette from anywhere.
        // Bare Q/W/E/R/T switch the gizmo tool (Select/Move/Rotate/Scale/
        // Universal) — but only when not typing into a field.
        .global_event(|e: events::KeyDown| {
            use crate::engine::gizmo::{gizmo_mode, GizmoMode};
            // `ctrl_key()` here already covers ⌘ (it OR's meta_key).
            if e.key() == "k" && e.ctrl_key() {
                e.prevent_default();
                let o = controller().cmdk_open.clone();
                o.set_neq(!o.get());
                return;
            }
            // Don't hijack typing: ignore single-letter tool shortcuts while a
            // text field / editor / contenteditable holds focus, or with any
            // modifier held.
            if e.ctrl_key() || e.alt_key() || e.shift_key() || typing_in_field() {
                return;
            }
            // `5` toggles the editor view between perspective and orthographic
            // (Blender uses Numpad-5; dominator only exposes `key()`, so plain `5`
            // — which also works on numpad-less laptops).
            if e.key() == "5" {
                e.prevent_default();
                let ortho = controller().settings.editor_ortho.get();
                crate::scene_mode::viewport::set_editor_projection(!ortho);
                return;
            }
            let mode = match e.key().as_str() {
                "q" | "Q" => GizmoMode::Select,
                "w" | "W" => GizmoMode::Move,
                "e" | "E" => GizmoMode::Rotate,
                "r" | "R" => GizmoMode::Scale,
                "t" | "T" => GizmoMode::Universal,
                _ => return,
            };
            e.prevent_default();
            gizmo_mode().set_neq(mode);
        })
        .child(top_bar(&ctrl))
        .child(workspace(&ctrl))
        .child(stats_bar())
        .child(crate::command_palette::render())
        .child(busy_overlay())
        .child(agent_feed())
        .child_signal(ctrl.settings_open.signal().map(|open| if open { Some(settings_drawer()) } else { None }))
    })
}

/// Full-screen **blocking** busy overlay that surfaces in-progress background
/// work — model import / GPU upload, material + render-pipeline compilation,
/// scene-load phases (issues #1, #7). Driven by the `engine::activity` indicator
/// list; while *any* activity is in flight it covers the whole viewport with a
/// backdrop + spinner + the live activity label(s) and **blocks 100% of
/// interaction** (ribbon, viewport, etc.). It has no close button, isn't
/// dismissable by backdrop click or Esc, and auto-dismisses the instant all
/// activity clears.
///
/// Unlike the shared `Modal` singleton (used by real dialogs — including the
/// import dialog itself), this is a dedicated always-on-top overlay so the busy
/// state and a real dialog never fight over one singleton.
fn busy_overlay() -> Dom {
    use crate::engine::activity::activities;
    html!("div", {
        // The host is inert; only the gated child (below) blocks. When no
        // activity is running there is no element at all, so the editor is fully
        // interactive.
        .child_signal(activities().signal_ref(|acts| {
            if acts.is_empty() {
                return None;
            }
            // Dedupe identical labels (e.g. the same phase upserted) and keep
            // insertion order so the primary label reads first.
            let mut labels: Vec<String> = Vec::new();
            for (_, l) in acts.iter() {
                if !labels.contains(l) {
                    labels.push(l.clone());
                }
            }
            Some(busy_overlay_card(labels))
        }))
    })
}

/// The blocking backdrop + centered spinner card listing the active labels.
fn busy_overlay_card(labels: Vec<String>) -> Dom {
    html!("div", {
        .style("position", "fixed")
        .style("inset", "0")
        // Above the top bar (20), drawers, and the shared Modal (1001) so it
        // truly blocks everything behind it.
        .style("z-index", "5000")
        .style("display", "flex")
        .style("align-items", "center")
        .style("justify-content", "center")
        .style("background", "color-mix(in oklch, var(--bg-0) 70%, transparent)")
        .style("backdrop-filter", "blur(3px)")
        // Captures all pointer events so nothing behind the backdrop is clickable.
        .style("pointer-events", "auto")
        .style("cursor", "wait")
        .style("animation", "feed-in 0.18s ease-out")
        // Swallow interaction outright (belt-and-suspenders over the backdrop).
        .event(|e: events::Click| { e.stop_propagation(); })
        .event(|e: events::MouseDown| { e.stop_propagation(); })
        .child(html!("div", {
            .style("display", "flex")
            .style("flex-direction", "column")
            .style("align-items", "center")
            .style("gap", "16px")
            .style("padding", "26px 34px")
            .style("min-width", "240px")
            .style("max-width", "min(520px, calc(100vw - 2rem))")
            .style("background", "var(--bg-1)")
            .style("border", "1px solid var(--line)")
            .style("border-radius", "14px")
            .style("box-shadow", "var(--shadow-2)")
            // Reuse the global `boot-spin` keyframe (index.html).
            .child(html!("div", {
                .style("width", "30px")
                .style("height", "30px")
                .style("border", "3px solid var(--line)")
                .style("border-top-color", "var(--accent)")
                .style("border-radius", "50%")
                .style("animation", "boot-spin 0.85s linear infinite")
            }))
            .child(html!("div", {
                .style("display", "flex")
                .style("flex-direction", "column")
                .style("align-items", "center")
                .style("gap", "5px")
                .children(labels.into_iter().enumerate().map(|(i, label)| html!("span", {
                    .style("font-size", if i == 0 { "13.5px" } else { "12px" })
                    .style("font-weight", if i == 0 { "560" } else { "400" })
                    .style("color", if i == 0 { "var(--text-0)" } else { "var(--text-2)" })
                    .style("white-space", "nowrap")
                    .text(&label)
                })))
            }))
        }))
    })
}

/// "Watch-it-work" agent-activity feed: a compact, auto-scrolling narration
/// strip pinned to the bottom-left, fed from the inbound MCP command stream (see
/// `engine::activity_feed`). Each entry reads "🤖 {phrase}" with a subtle
/// fade-in. Read-only/informational — it never mutates editor state, and
/// degrades silently (hidden) when no agent is connected or the feed is empty.
///
/// The newest entries render at the bottom (closest to the eye); only the last
/// handful are shown so the strip stays unobtrusive over the viewport while the
/// full ~50-entry history is retained in the model.
fn agent_feed() -> Dom {
    use crate::engine::activity_feed::feed;
    /// How many trailing entries the strip shows at once (the model keeps ~50).
    const VISIBLE: usize = 6;
    let max_height = format!("{}px", VISIBLE * 30);
    html!("div", {
        .style("position", "fixed")
        .style("left", "12px")
        .style("bottom", "40px")
        .style("z-index", "340")
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "5px")
        .style("max-width", "min(340px, 42vw)")
        .style("pointer-events", "none")
        .style("justify-content", "flex-end")
        // Hide the whole strip when the feed is empty (no agent activity yet) so
        // it degrades silently with no agent connected.
        .style_signal("display", feed().signal_vec_cloned().len().map(|n| if n == 0 { "none" } else { "flex" }))
        // A tiny "clear" affordance pinned above the rows (the column is
        // bottom-anchored, so the first child sits at top). Interactive, so it
        // opts back into pointer events the strip otherwise disables.
        .child(agent_feed_clear_btn())
        // Cap the *rows* to a trailing window via CSS: the inner column is
        // bottom-anchored, so older entries overflow + scroll off the top. The
        // model keeps the full ~50.
        .child(html!("div", {
            .style("display", "flex")
            .style("flex-direction", "column")
            .style("gap", "5px")
            .style("max-height", &max_height)
            .style("overflow", "hidden")
            .style("justify-content", "flex-end")
            .children_signal_vec(feed().signal_vec_cloned().map(|entry| agent_feed_row(&entry.phrase)))
        }))
    })
}

/// Small "✕ clear" chip above the feed rows — empties the narration strip. Mute
/// (stop narrating entirely) lives in Settings → "Follow agent activity".
fn agent_feed_clear_btn() -> Dom {
    html!("button", {
        .style("align-self", "flex-start")
        .style("pointer-events", "auto")
        .style("cursor", "pointer")
        .style("display", "inline-flex")
        .style("align-items", "center")
        .style("gap", "5px")
        .style("padding", "2px 8px")
        .style("margin-bottom", "1px")
        .style("background", "color-mix(in oklch, var(--bg-1) 80%, transparent)")
        .style("border", "1px solid var(--line-soft)")
        .style("border-radius", "999px")
        .style("font-size", "10.5px")
        .style("color", "var(--text-3)")
        .attr("title", "Clear the agent activity feed")
        .text("\u{2715} clear")
        .event(|_: events::Click| crate::engine::activity_feed::clear())
    })
}

/// One narration row: "🤖 {phrase}" in a small translucent pill that fades in.
fn agent_feed_row(phrase: &str) -> Dom {
    html!("div", {
        .style("display", "inline-flex")
        .style("align-items", "center")
        .style("gap", "7px")
        .style("align-self", "flex-start")
        .style("padding", "5px 11px 5px 9px")
        .style("background", "color-mix(in oklch, var(--bg-1) 88%, transparent)")
        .style("border", "1px solid var(--line-soft)")
        .style("border-radius", "999px")
        .style("box-shadow", "var(--shadow-2)")
        .style("font-size", "12px")
        .style("color", "var(--text-1)")
        .style("white-space", "nowrap")
        .style("max-width", "100%")
        .style("overflow", "hidden")
        .style("text-overflow", "ellipsis")
        .style("animation", "feed-in 0.28s ease-out")
        .child(html!("span", { .style("flex", "0 0 auto").text("\u{1F916}") }))
        .child(html!("span", {
            .style("overflow", "hidden")
            .style("text-overflow", "ellipsis")
            .text(phrase)
        }))
    })
}

/// Transient "agent acting" spotlight: while a panel is the active focus target
/// (set for ~1s when a matching command lands, see `engine::activity_feed`),
/// overlay a non-interactive pulsing accent ring on it so the human's eye lands
/// where the agent is working. Reuses the `mcp-pulse` keyframe (index.html).
/// Returns an `apply` closure adding the overlay child to a (positioned) panel.
fn panel_highlight(
    target: crate::engine::activity_feed::FocusTarget,
) -> impl FnOnce(
    dominator::DomBuilder<web_sys::HtmlElement>,
) -> dominator::DomBuilder<web_sys::HtmlElement> {
    use crate::engine::activity_feed::focus;
    move |d| {
        d.child(html!("div", {
            .style("position", "absolute")
            .style("inset", "0")
            .style("z-index", "300")
            .style("pointer-events", "none")
            .style("border-radius", "2px")
            .style("box-shadow", "inset 0 0 0 2px var(--accent-line)")
            .style("animation", "mcp-pulse 1.1s ease-in-out infinite")
            .style_signal("display", focus().signal().map(move |f| {
                if f == Some(target) { "block" } else { "none" }
            }))
        }))
    }
}

/// Whether a text-entry element currently holds focus — used to suppress the
/// bare-letter gizmo shortcuts so typing into a field (name, WGSL editor,
/// numeric input, search box, …) doesn't switch tools.
fn typing_in_field() -> bool {
    web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.active_element())
        .map(|el| {
            let tag = el.tag_name().to_ascii_lowercase();
            tag == "input"
                || tag == "textarea"
                || tag == "select"
                || el
                    .get_attribute("contenteditable")
                    .map(|v| v != "false")
                    .unwrap_or(false)
        })
        .unwrap_or(false)
}

/// Save the live project into a picked directory (File System Access): writes
/// `project.toml` + the per-material side files.
fn save_project() {
    spawn_local(async {
        match crate::fs::ProjectDir::pick().await {
            Ok(dir) => match crate::controller::persistence::save_to_dir(&controller(), &dir).await
            {
                Ok(()) => {
                    controller().project_name.set(dir.name());
                    controller().dirty.set_neq(false);
                    Toast::info(format!("Saved to {}/", dir.name()));
                }
                Err(e) => Toast::error(format!("Save failed: {e}")),
            },
            Err(crate::fs::FsError::Cancelled) => {}
            Err(crate::fs::FsError::Unsupported) => {
                // No directory picker (e.g. Firefox/Safari): fall back to a download.
                if let Ok(toml) = crate::controller::persistence::project_to_toml(&controller()) {
                    download_text("project.toml", &toml);
                    Toast::info("Saved project.toml (download)");
                }
            }
            Err(e) => Toast::error(format!("Save: {e}")),
        }
    });
}

/// Open a project directory (File System Access) + load `project.toml`.
fn open_project() {
    spawn_local(async {
        match crate::fs::ProjectDir::pick().await {
            Ok(dir) => {
                match crate::controller::persistence::load_from_dir(&controller(), &dir).await {
                    Ok(()) => {
                        controller().project_name.set(dir.name());
                        Toast::info(format!("Opened {}/", dir.name()));
                    }
                    Err(e) => Toast::error(format!("Open failed: {e}")),
                }
            }
            Err(crate::fs::FsError::Cancelled) => {}
            Err(e) => Toast::error(format!("Open: {e}")),
        }
    });
}

/// Trigger a browser download of `content` as `filename`.
fn download_text(filename: &str, content: &str) {
    use wasm_bindgen::JsCast;
    let arr = js_sys::Array::new();
    arr.push(&wasm_bindgen::JsValue::from_str(content));
    let Ok(blob) = web_sys::Blob::new_with_str_sequence(&arr) else {
        return;
    };
    let Ok(url) = web_sys::Url::create_object_url_with_blob(&blob) else {
        return;
    };
    if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
        if let Ok(a) = doc.create_element("a") {
            if let Ok(a) = a.dyn_into::<web_sys::HtmlAnchorElement>() {
                a.set_href(&url);
                a.set_download(filename);
                a.click();
            }
        }
    }
    let _ = web_sys::Url::revoke_object_url(&url);
}

/// Trigger a browser download of raw `bytes` as `filename` (binary — e.g. a
/// `.glb`). Shared by the scene + per-node GLB export.
pub(crate) fn download_bytes(filename: &str, bytes: &[u8]) {
    use wasm_bindgen::JsCast;
    let u8arr = js_sys::Uint8Array::from(bytes);
    let parts = js_sys::Array::new();
    parts.push(&u8arr);
    let Ok(blob) = web_sys::Blob::new_with_u8_array_sequence(&parts) else {
        return;
    };
    let Ok(url) = web_sys::Url::create_object_url_with_blob(&blob) else {
        return;
    };
    if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
        if let Ok(a) = doc.create_element("a") {
            if let Ok(a) = a.dyn_into::<web_sys::HtmlAnchorElement>() {
                a.set_href(&url);
                a.set_download(filename);
                a.click();
            }
        }
    }
    let _ = web_sys::Url::revoke_object_url(&url);
}

/// Export the whole scene to a binary glTF and download it. The player/runtime
/// (or another DCC tool) re-imports the `.glb`; it's not auto-added back to the
/// project.
fn export_scene_glb() {
    spawn_local(async {
        match crate::controller::export::export_scene_glb(&controller()).await {
            Ok(bytes) => {
                let name = controller().project_name.get_cloned();
                let file = if name.is_empty() {
                    "scene.glb".to_string()
                } else {
                    format!("{name}.glb")
                };
                download_bytes(&file, &bytes);
                Toast::info(format!("Exported {file} ({} KB)", bytes.len() / 1024));
            }
            Err(e) => Toast::error(format!("Export failed: {e}")),
        }
    });
}

/// Assemble a player bundle (scene.glb + custom-material side-files + referenced
/// custom-material textures + env.json + bundle.json index) and write every file
/// into a picked directory via the File System Access handle. Reuses the native
/// `assemble_bundle` layout so the editor and the tested layout never drift.
fn export_player_bundle() {
    spawn_local(async {
        let name = {
            let n = controller().project_name.get_cloned();
            if n.is_empty() {
                "bundle".to_string()
            } else {
                n
            }
        };
        let _ = &name; // bundle name is the picked directory's; kept for the toast.
        let bundle = match crate::controller::export::bake_player_bundle(&controller()).await {
            Ok(bundle) => bundle,
            Err(e) => {
                Toast::error(format!("Export bundle failed: {e}"));
                return;
            }
        };
        match crate::fs::ProjectDir::pick().await {
            Ok(dir) => {
                let count = bundle.len();
                for file in &bundle {
                    if let Err(e) = dir.write_bytes(&file.path, &file.bytes).await {
                        Toast::error(format!("Export bundle failed ({}): {e}", file.path));
                        return;
                    }
                }
                Toast::info(format!("Wrote {count} files to {}/", dir.name()));
            }
            Err(crate::fs::FsError::Cancelled) => {}
            Err(crate::fs::FsError::Unsupported) => {
                Toast::error("Export bundle needs a directory picker (Chromium-only)");
            }
            Err(e) => Toast::error(format!("Export bundle: {e}")),
        }
    });
}

fn settings_drawer() -> Dom {
    let s = controller().settings.clone();
    RightDrawer::new("Settings")
        .icon("settings")
        .width(344.0)
        .on_close(|| controller().settings_open.set_neq(false))
        .child(
            DrawerSection::new("Viewport")
                .child(row("Show grid", toggle(s.grid.clone())))
                .child(row("Show gizmo", toggle(s.gizmo.clone())))
                .child(row("Light gizmos", toggle(s.light_gizmos.clone())))
                .child(row("Skeleton overlay", toggle(s.skeleton_viz.clone())))
                .child(row("MSAA", toggle(s.msaa.clone())))
                .child(row("Light heatmap", toggle(s.heatmap.clone())))
                .child(row(
                    "Follow agent activity",
                    toggle(crate::engine::activity_feed::enabled()),
                ))
                .child(row(
                    "Show MCP notifications",
                    toggle(crate::remote::show_notifications()),
                ))
                .render(),
        )
        .child(
            DrawerSection::new("Camera")
                .child(html!("div", {
                    .style("display", "grid")
                    .style("grid-template-columns", "repeat(3, 1fr)")
                    .style("gap", "6px")
                    .style("padding", "2px 0 8px")
                    .child(cam_axis_btn("Top", CameraAxis::PosY))
                    .child(cam_axis_btn("Front", CameraAxis::PosZ))
                    .child(cam_axis_btn("Right", CameraAxis::PosX))
                    .child(cam_axis_btn("Bottom", CameraAxis::NegY))
                    .child(cam_axis_btn("Back", CameraAxis::NegZ))
                    .child(cam_axis_btn("Left", CameraAxis::NegX))
                }))
                .child(
                    Btn::new()
                        .label("Reset View")
                        .icon("reset")
                        .variant(BtnVariant::Ghost)
                        .size(BtnSize::Sm)
                        .full(true)
                        .on_click(|| {
                            spawn_local(async {
                                let _ = controller().dispatch(EditorCommand::ResetCamera).await;
                            })
                        })
                        .render(),
                )
                .render(),
        )
        .child(
            DrawerSection::new("Units & snapping")
                .child(row(
                    "Units",
                    select(
                        s.units.clone(),
                        ["meters", "centimeters", "feet"]
                            .iter()
                            .map(|u| (u.to_string(), u.to_string()))
                            .collect(),
                    ),
                ))
                .child(row("Snap to grid", toggle(s.snap.clone())))
                .render(),
        )
        .child(html!("div", {
            .style("padding", "16px").style("font-size", "11px").style("color", "var(--text-3)").style("line-height", "1.5")
            .text("Editor settings affect the viewport and chrome only \u{2014} they are not saved into the project file.")
        }))
        .render()
}

fn open_about() {
    Modal::open(|| {
        ModalCard::new("About AwsmRenderer")
            .width(560.0)
            .child(html!("div", {
                .style("display", "flex").style("flex-direction", "column").style("gap", "12px")
                .style("font-size", "13px").style("color", "var(--text-1)").style("line-height", "1.55")
                .child(html!("p", { .style("margin", "0").text("A WebGPU scene & material editor that runs entirely in your browser. It needs two Chromium-only features, so it works in Chrome, Edge, Arc, or Brave.") }))
                .child(html!("div", { .style("display", "flex").style("flex-direction", "column").style("gap", "7px")
                    .child(html!("div", { .child(html!("strong", { .style("color", "var(--text-0)").text("WebGPU") })).child(html!("span", { .text(" \u{2014} renders the 3D scene. Not yet in stable Firefox or Safari.") })) }))
                    .child(html!("div", { .child(html!("strong", { .style("color", "var(--text-0)").text("File System Access API") })).child(html!("span", { .text(" \u{2014} Load opens a project directory and Save writes the project back alongside your assets.") })) }))
                }))
                .child(html!("p", { .style("margin", "0").text("A project is a directory containing one project.toml plus the asset files it references. Nothing is uploaded.") }))
            }))
            .footer(Btn::new().label("Close").variant(BtnVariant::Primary).on_click(Modal::close).render())
            .render()
    });
}

fn open_clear_all() {
    Modal::open(|| {
        ModalCard::new("Clear scene?")
            .width(360.0)
            .child(html!("p", {
                .style("margin", "0 0 8px").style("font-size", "13px").style("color", "var(--text-1)").style("line-height", "1.5")
                .text("This removes every node in the scene. You can undo it.")
            }))
            .footer(html!("div", {
                .style("display", "flex").style("gap", "8px")
                .child(Btn::new().label("Cancel").variant(BtnVariant::Ghost).on_click(Modal::close).render())
                .child(Btn::new().label("Clear All").variant(BtnVariant::Primary).on_click(|| {
                    spawn_local(async {
                        let ids: Vec<_> = controller().scene.nodes.lock_ref().iter().map(|n| n.id).collect();
                        for id in ids {
                            let _ = controller().dispatch(EditorCommand::Delete { id }).await;
                        }
                    });
                    Modal::close();
                }).render())
            }))
            .render()
    });
}

/// Counts derived from the scene + material list, recomputed on each revision.
#[derive(Default, Clone, Copy)]
struct Counts {
    nodes: usize,
    meshes: usize,
    lights: usize,
}

fn count_nodes(nodes: &[std::sync::Arc<crate::engine::scene::Node>], c: &mut Counts) {
    use crate::engine::scene::NodeKind;
    for node in nodes {
        c.nodes += 1;
        match node.kind.get_cloned() {
            NodeKind::Mesh { .. } | NodeKind::SkinnedMesh { .. } => c.meshes += 1,
            NodeKind::Light(_) => c.lights += 1,
            _ => {}
        }
        count_nodes(&node.children.lock_ref(), c);
    }
}

/// The bottom status bar: live scene + material
/// counts. A thin always-on strip below the workspace.
fn stats_bar() -> Dom {
    html!("div", {
        .style("display", "flex").style("align-items", "center").style("height", "30px").style("padding", "0 12px")
        .style("flex", "0 0 auto").style("border-top", "1px solid var(--line-soft)").style("background", "var(--bg-3)")
        .child(html!("div", {
            .class("mono").style("font-size", "11px").style("color", "var(--text-2)").style("display", "flex").style("gap", "14px")
            .child_signal(stats_signal())
        }))
    })
}

fn stats_signal() -> impl Signal<Item = Option<Dom>> {
    let ctrl = controller();
    map_ref! {
        let _rev = ctrl.scene.revision.signal(),
        let _cm = ctrl.custom_materials.signal_vec_cloned().len() => {
            let ctrl = controller();
            let mut c = Counts::default();
            count_nodes(&ctrl.scene.nodes.lock_ref(), &mut c);
            let materials = ctrl.custom_materials.lock_ref().len();
            let buckets = ctrl.custom_materials.lock_ref().iter().filter(|m| m.registered.get()).count();
            let tris = c.meshes * 1200; // estimate until the renderer reports exact counts
            let tris_label = if tris >= 1000 { format!("{:.1}k", tris as f64 / 1000.0) } else { tris.to_string() };
            let span = |t: String| html!("span", { .text(&t) });
            Some(html!("div", {
                .style("display", "flex").style("gap", "14px")
                .child(span(format!("{} nodes", c.nodes)))
                .child(span(format!("{} meshes", c.meshes)))
                .child(span(format!("{} lights", c.lights)))
                .child(span(format!("{tris_label} tris")))
                .child(span(format!("{materials} materials \u{00b7} {buckets} buckets")))
            }))
        }
    }
}

fn vdivider() -> Dom {
    html!("div", {
        .style("width", "1px")
        .style("height", "22px")
        .style("background", "var(--line)")
        .style("flex", "0 0 auto")
    })
}

fn brand() -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("align-items", "center")
        .style("gap", "9px")
        .child(html!("div", {
            .style("width", "26px")
            .style("height", "26px")
            .style("border-radius", "7px")
            .style("position", "relative")
            .style("flex", "0 0 auto")
            .style("background", "linear-gradient(145deg, var(--accent-bright), var(--accent-dim))")
            .style("box-shadow", "inset 0 1px 0 oklch(1 0 0 / .25), var(--shadow-1)")
            .child(html!("div", {
                .style("position", "absolute")
                .style("inset", "0")
                .style("display", "flex")
                .style("align-items", "center")
                .style("justify-content", "center")
                .child(Icon::new("sphere").size(16.0).stroke_width(1.8).color(ACCENT_FG).render())
            }))
        }))
        .child(html!("span", {
            .style("font-size", "13px")
            .style("font-weight", "680")
            .style("letter-spacing", "-0.01em")
            .text("Awsm")
            .child(html!("span", {
                .style("color", "var(--text-2)")
                .style("font-weight", "500")
                .text("Renderer")
            }))
        }))
    })
}

fn cmdk_button() -> Dom {
    html!("button", {
        .class("t")
        .attr("title", "Command palette")
        .style("display", "flex")
        .style("align-items", "center")
        .style("gap", "8px")
        .style("height", "28px")
        .style("padding", "0 9px 0 11px")
        .style("margin-left", "4px")
        .style("cursor", "pointer")
        .style("border", "1px solid var(--line-soft)")
        .style("border-radius", "var(--r2)")
        .style("background", "var(--bg-3)")
        .style("color", "var(--text-2)")
        .style("font-size", "12px")
        .event(|_: events::Click| crate::command_palette::set_open(true))
        .child(Icon::new("search").size(14.0).render())
        .child(html!("span", { .style("min-width", "60px").style("text-align", "left").text("Search\u{2026}") }))
        .child(html!("span", {
            .class("mono")
            .style("font-size", "10px")
            .style("color", "var(--text-3)")
            .style("border", "1px solid var(--line)")
            .style("border-radius", "4px")
            .style("padding", "1px 5px")
            .text("\u{2318}K")
        }))
    })
}

/// Top-bar MCP cluster: a `MCP` / `MCP…` / `MCP ✓` status button (opens the
/// connect modal, or disconnects when connected) plus — while connected — a
/// same-sized 🤖 activity chip that pulses whenever the agent is mid-request.
///
/// The chip is informational only: the editor stays fully interactive while the
/// agent works (every edit is command-sourced + undoable), matching the
/// awsm-audio convention — it tells the human "changes are landing live; wait
/// for idle before editing / exporting" without locking input.
///
/// [`status`]: crate::remote::status
fn mcp_button() -> Dom {
    use crate::remote::RemoteStatus;
    html!("div", {
        .style("display", "flex")
        .style("align-items", "center")
        .style("gap", "6px")
        .child_signal(crate::remote::status().signal().map(|st| Some(mcp_status_button(st))))
        .child_signal(map_ref! {
            let status = crate::remote::status().signal(),
            let working = crate::remote::working().signal() =>
            (*status == RemoteStatus::Connected).then(|| mcp_activity_chip(*working))
        })
    })
}

/// The three-state MCP status button.
fn mcp_status_button(status: crate::remote::RemoteStatus) -> Dom {
    use crate::remote::RemoteStatus;
    match status {
        RemoteStatus::Disconnected => Btn::new()
            .label("MCP")
            .variant(BtnVariant::Ghost)
            .size(BtnSize::Sm)
            .title("Connect to an MCP server")
            .on_click(open_mcp_modal)
            .render(),
        RemoteStatus::Connecting => Btn::new()
            .label("MCP\u{2026}")
            .variant(BtnVariant::Ghost)
            .size(BtnSize::Sm)
            .title("Connecting\u{2026}")
            .disabled(true)
            .render(),
        RemoteStatus::Connected => Btn::new()
            .label("MCP \u{2713}")
            .variant(BtnVariant::Primary)
            .size(BtnSize::Sm)
            .title("Connected \u{2014} click to manage the MCP connection")
            .on_click(open_mcp_modal)
            .render(),
    }
}

/// The 🤖 agent-activity chip shown next to the MCP button while connected.
/// Sized to match the `BtnSize::Sm` button (26px height) so the two read as one
/// cluster. Pulses (via the `mcp-pulse` keyframe in `index.html`) while working.
fn mcp_activity_chip(working: bool) -> Dom {
    html!("div", {
        .style("display", "inline-flex")
        .style("align-items", "center")
        .style("gap", "5px")
        .style("height", "26px")
        .style("box-sizing", "border-box")
        .style("padding", "0 11px")
        .style("border-radius", "var(--r2)")
        .style("border-style", "solid")
        .style("border-width", "1px")
        .style("font-size", "12.5px")
        .style("font-weight", "550")
        .style("white-space", "nowrap")
        .style("user-select", "none")
        .apply(|d| if working {
            d.style("color", "var(--accent-bright)")
                .style("background", "var(--accent-ghost)")
                .style("border-color", "var(--accent-line)")
                .style("animation", "mcp-pulse 1.1s ease-in-out infinite")
                .attr(
                    "title",
                    "Agent is working \u{2014} changes are landing live; wait for idle before editing or exporting.",
                )
        } else {
            d.style("color", "var(--text-3)")
                .style("background", "transparent")
                .style("border-color", "var(--line)")
                .attr("title", "Agent idle \u{2014} safe to edit / export.")
        })
        .child(html!("span", { .text("\u{1F916}") }))
        // Name the current action ("added a box") instead of a bare "working…",
        // pulled from the live narration; falls back to "working…" before the
        // first command (or when the feed/follow toggle is off).
        .child(html!("span", {
            .style("max-width", "240px")
            .style("overflow", "hidden")
            .style("text-overflow", "ellipsis")
            .text_signal(crate::engine::activity_feed::current_action().signal_cloned().map(move |action| {
                if working {
                    action.unwrap_or_else(|| "working\u{2026}".to_string())
                } else {
                    "idle".to_string()
                }
            }))
        }))
    })
}

/// Open the MCP connect modal: an editable server address + a connect/disconnect
/// action that reflects the live [`status`](crate::remote::status).
fn open_mcp_modal() {
    Modal::open(|| {
        use crate::remote::RemoteStatus;
        // Seeded once per open from the current/last-used origin (the `?mcp=` value
        // or the build default); edits feed straight into `connect`.
        let addr = Mutable::new(crate::remote::origin().get_cloned());
        // Pair-code input — revealed only when the server asked for one.
        let pair_code = Mutable::new(crate::remote::pair().get_cloned());

        html!("div", {
            .style("display", "flex")
            .style("flex-direction", "column")
            .style("gap", "14px")
            // Header: title on the left, Help on the right (easy to spot — the
            // guide is the first thing a new user wants).
            .child(html!("div", {
                .style("display", "flex")
                .style("align-items", "center")
                .style("justify-content", "space-between")
                .style("gap", "8px")
                .child(html!("div", {
                    .style("font-size", "15px")
                    .style("font-weight", "600")
                    .style("color", "var(--text-0)")
                    .text("MCP server")
                }))
                .child(Btn::new()
                    .label("Help")
                    .icon("help")
                    .variant(BtnVariant::Ghost)
                    .size(BtnSize::Sm)
                    .title("How the MCP works — open the guide")
                    .on_click(|| { Modal::close(); crate::help_modal::open_help_mcp(); })
                    .render())
            }))
            .child(html!("div", {
                .style("font-size", "12.5px")
                .style("color", "var(--text-2)")
                .style("line-height", "1.5")
                .text("Run awsm-scene-mcp locally, then connect — the editor dials out to \
                       this address. An MCP agent (Claude, Codex, \u{2026}) drives the editor \
                       through that same server.")
            }))
            // Live status line.
            .child(html!("div", {
                .style("display", "flex")
                .style("align-items", "center")
                .style("gap", "8px")
                .style("font-size", "12.5px")
                .child(html!("span", {
                    .style("width", "8px")
                    .style("height", "8px")
                    .style("border-radius", "50%")
                    .style("flex", "0 0 auto")
                    .style_signal("background", crate::remote::status().signal().map(|s| match s {
                        RemoteStatus::Connected => "var(--ok)",
                        RemoteStatus::Connecting => "var(--warn)",
                        RemoteStatus::Disconnected => "var(--text-3)",
                    }))
                }))
                .child(html!("span", {
                    .style("color", "var(--text-1)")
                    .text_signal(crate::remote::status().signal().map(|s| match s {
                        RemoteStatus::Connected => "Connected",
                        RemoteStatus::Connecting => "Connecting\u{2026}",
                        RemoteStatus::Disconnected => "Not connected",
                    }))
                }))
            }))
            .child(html!("label", {
                .style("font-size", "11px")
                .style("color", "var(--text-3)")
                .style("text-transform", "uppercase")
                .style("letter-spacing", "0.04em")
                .text("Server address")
            }))
            .child(TextInput::new(addr.clone())
                .placeholder(crate::remote::default_origin())
                .mono(true)
                .render())
            // TLS toggle — for a remote server behind https/wss (off for the
            // usual local server).
            .child(row("Use TLS (wss / https)", toggle(crate::remote::tls())))
            // Pairing code — ALWAYS shown (optional). Blank = auto-pair, which
            // works when exactly one tab + one agent are connected. When more than
            // one is connected the server needs a code: the agent prints it (its
            // `pairing_status` tool); type it here + Pair to claim this tab.
            .child(html!("label", {
                .style("font-size", "11px")
                .style("color", "var(--text-3)")
                .style("text-transform", "uppercase")
                .style("letter-spacing", "0.04em")
                .text("Pairing code (optional)")
            }))
            .child(html!("div", {
                .style("display", "flex")
                .style("gap", "8px")
                .style("align-items", "center")
                .child(html!("div", {
                    .style("flex", "1 1 auto")
                    .style("min-width", "0")
                    .child(TextInput::new(pair_code.clone())
                        .placeholder("auto-pairs if blank \u{2014} e.g. 3K9J")
                        .mono(true)
                        .render())
                }))
                .child(Btn::new()
                    .label("Pair")
                    .variant(BtnVariant::Ghost)
                    .size(BtnSize::Md)
                    .title("Claim this tab for the agent holding this code")
                    .on_click(clone!(pair_code => move || {
                        crate::remote::submit_pair_code(pair_code.get_cloned());
                    }))
                    .render())
            }))
            // Highlighted prompt when the server actually asked for a code.
            .child_signal(crate::remote::pairing_needed().signal().map(|needed| needed.then(|| html!("div", {
                .style("font-size", "11.5px")
                .style("color", "var(--warn)")
                .style("line-height", "1.4")
                .text("This server has multiple tabs/agents connected \u{2014} enter the pairing \
                       code your agent printed (its pairing_status) to bind it to this tab.")
            }))))
            // Live work display — the activity feed (narration + panel spotlight)
            // that lets you watch the agent build. Also under Settings.
            .child(row("Follow agent activity", toggle(crate::engine::activity_feed::enabled())))
            // Action: Connect / Connecting… / Disconnect, by live status. (Help
            // lives in the header now.)
            .child(html!("div", {
                .style("display", "flex")
                .style("justify-content", "flex-end")
                .style("align-items", "center")
                .style("margin-top", "4px")
                .child_signal(crate::remote::status().signal().map(clone!(addr => move |st| {
                    Some(match st {
                        RemoteStatus::Connected => Btn::new()
                            .label("Disconnect")
                            .variant(BtnVariant::Ghost)
                            .size(BtnSize::Md)
                            .on_click(|| { crate::remote::disconnect(); Modal::close(); })
                            .render(),
                        RemoteStatus::Connecting => Btn::new()
                            .label("Connecting\u{2026}")
                            .variant(BtnVariant::Ghost)
                            .size(BtnSize::Md)
                            .disabled(true)
                            .render(),
                        RemoteStatus::Disconnected => Btn::new()
                            .label("Connect")
                            .variant(BtnVariant::Primary)
                            .size(BtnSize::Md)
                            .on_click(clone!(addr => move || {
                                crate::remote::connect(addr.get_cloned());
                                Modal::close();
                            }))
                            .render(),
                    })
                })))
            }))
        })
    });
}

fn project_label(ctrl: &EditorController) -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("align-items", "center")
        .style("gap", "7px")
        .style("padding", "0 4px")
        .child(html!("span", {
            .style("width", "7px")
            .style("height", "7px")
            .style("border-radius", "50%")
            .style_signal("background", ctrl.dirty.signal().map(|d| if d { "var(--warn)" } else { "var(--ok)" }))
        }))
        .child(html!("span", {
            .style("font-size", "12.5px")
            .style("color", "var(--text-1)")
            .style("font-weight", "500")
            .text_signal(ctrl.project_name.signal_cloned())
        }))
        .child(html!("span", {
            .class("mono")
            .style("font-size", "10.5px")
            .style("color", "var(--text-3)")
            .text_signal(ctrl.dirty.signal().map(|d| if d { "unsaved" } else { "saved" }))
        }))
    })
}

fn top_bar(ctrl: &EditorController) -> Dom {
    // Local view-mirror of the canonical mode (controller.mode). The segmented
    // sets this; we translate the change into a dispatched SwitchMode and
    // reflect external mode changes back. The router reads the canonical
    // controller.mode, not this mirror.
    let mode_str = Mutable::new(mode_to_str(ctrl.mode.get()));

    // mirror -> dispatch (skip the initial value)
    spawn_local(clone!(mode_str => async move {
        let mut first = true;
        mode_str.signal_cloned().for_each(move |s| {
            let fire = !first;
            first = false;
            async move {
                if fire {
                    if let Some(mode) = str_to_mode(&s) {
                        let _ = controller().dispatch(EditorCommand::SwitchMode { mode }).await;
                    }
                }
            }
        }).await;
    }));
    // canonical -> mirror
    spawn_local(clone!(ctrl, mode_str => async move {
        ctrl.mode.signal().for_each(move |m| {
            mode_str.set_neq(mode_to_str(m));
            async {}
        }).await;
    }));

    html!("div", {
        .style("display", "flex")
        .style("align-items", "center")
        .style("gap", "12px")
        .style("height", "48px")
        .style("padding", "0 12px")
        .style("background", "var(--bg-2)")
        .style("border-bottom", "1px solid var(--line)")
        .style("flex", "0 0 auto")
        .style("position", "relative")
        .style("z-index", "20")
        .child(brand())
        .child(vdivider())
        .child(segmented(mode_str, vec![
            SegOption::new("scene", "Scene").icon("layers"),
            SegOption::new("material", "Material").icon("material"),
            SegOption::new("animation", "Animation").icon("curve"),
        ], false, false))
        .child(IconBtn::new("settings").title("Settings")
            .on_click(|| controller().settings_open.set_neq(true)).render())
        .child(IconBtn::new("help").title("Help")
            .on_click(crate::help_modal::open_help).render())
        .child(cmdk_button())
        .child(mcp_button())
        .child(html!("div", { .style("flex", "1") }))
        .child(project_label(ctrl))
        .child(vdivider())
        .child(html!("div", {
            .style("display", "flex")
            .style("align-items", "center")
            .style("gap", "2px")
            .child(IconBtn::new("folder").title("Open project directory")
                .on_click(open_project).render())
            .child(IconBtn::new("save").title("Save project to directory")
                .on_click(save_project).render())
            .child(IconBtn::new("undo").title("Undo")
                .on_click(|| spawn_local(async { controller().undo().await; })).render())
            .child(IconBtn::new("redo").title("Redo")
                .on_click(|| spawn_local(async { controller().redo().await; })).render())
            .child(overflow_button(ctrl))
        }))
    })
}

fn overflow_button(ctrl: &EditorController) -> Dom {
    html!("span", {
        .style("position", "relative")
        .style("display", "inline-flex")
        .child(DropButton::new().icon("more").variant(BtnVariant::Quiet).chevron(false)
            .items(|close| vec![
                MenuItem::new("Export scene as GLB\u{2026}").icon("mesh").on_click(clone!(close => move || { export_scene_glb(); (close.borrow_mut())(); })).render(),
                MenuItem::new("Export player bundle\u{2026}").icon("mesh").on_click(clone!(close => move || { export_player_bundle(); (close.borrow_mut())(); })).render(),
                MenuItem::new("Settings\u{2026}").icon("settings").on_click(clone!(close => move || { controller().settings_open.set_neq(true); (close.borrow_mut())(); })).render(),
                MenuItem::new("About AwsmRenderer\u{2026}").icon("help").on_click(clone!(close => move || { open_about(); (close.borrow_mut())(); })).render(),
                MenuItem::new("Clear scene\u{2026}").icon("trash").danger(true).on_click(clone!(close => move || { open_clear_all(); (close.borrow_mut())(); })).render(),
            ]).render())
        // Red dot when there are missing assets.
        .child_signal(ctrl.missing_assets.signal_ref(|m| !m.is_empty()).map(|has| if has {
            Some(html!("span", {
                .style("position", "absolute")
                .style("top", "4px")
                .style("right", "4px")
                .style("width", "7px")
                .style("height", "7px")
                .style("border-radius", "50%")
                .style("background", "var(--danger)")
                .style("box-shadow", "0 0 0 1.5px var(--bg-2)")
                .style("pointer-events", "none")
            }))
        } else {
            None
        }))
    })
}

fn workspace(ctrl: &EditorController) -> Dom {
    use crate::engine::activity_feed::FocusTarget;
    // Both workspaces stay mounted and are display-toggled by mode, so the
    // WebGPU canvas (reparented into the Scene viewport slot) is never torn out
    // of the DOM on a mode switch — the render loop keeps ticking.
    html!("div", {
        .style("flex", "1")
        .style("min-height", "0")
        .style("position", "relative")
        .child(html!("div", {
            .style("position", "absolute")
            .style("inset", "0")
            .style("display", "flex")
            .style("flex-direction", "column")
            .style_signal("display", ctrl.mode.signal().map(|m| if m == EditorMode::Scene { "flex" } else { "none" }))
            // Ribbon over [outliner · viewport · inspector].
            .child(crate::scene_mode::ribbon::render())
            .child(html!("div", {
                .style("flex", "1")
                .style("min-height", "0")
                .style("display", "flex")
                .style("flex-direction", "row")
                .child(html!("div", {
                    .style("width", "240px")
                    .style("flex", "0 0 auto")
                    .style("border-right", "1px solid var(--line)")
                    .style("min-height", "0")
                    .style("position", "relative")
                    .apply(panel_highlight(FocusTarget::Outliner))
                    .child(crate::scene_mode::outliner::render())
                }))
                .child(html!("div", {
                    .style("flex", "1")
                    .style("min-width", "0")
                    .style("min-height", "0")
                    .style("position", "relative")
                    .apply(panel_highlight(FocusTarget::Viewport))
                    .child(crate::scene_mode::viewport::render())
                }))
                .child(html!("div", {
                    .style("width", "288px")
                    .style("flex", "0 0 auto")
                    .style("border-left", "1px solid var(--line)")
                    .style("min-height", "0")
                    .style("position", "relative")
                    .apply(panel_highlight(FocusTarget::Inspector))
                    .child(crate::scene_mode::inspector::render())
                }))
            }))
            // Content Browser bottom drawer (collapsed bar / expanded grid).
            .child(crate::scene_mode::content_browser::render())
        }))
        .child(html!("div", {
            .style("position", "absolute")
            .style("inset", "0")
            .style_signal("display", ctrl.mode.signal().map(|m| if m == EditorMode::Material { "block" } else { "none" }))
            .child(crate::material_mode::render())
        }))
        .child(html!("div", {
            .style("position", "absolute")
            .style("inset", "0")
            .style_signal("display", ctrl.mode.signal().map(|m| if m == EditorMode::Animation { "block" } else { "none" }))
            .child(crate::animation_mode::render())
        }))
    })
}

fn mode_to_str(m: EditorMode) -> String {
    match m {
        EditorMode::Scene => "scene".to_string(),
        EditorMode::Material => "material".to_string(),
        EditorMode::Animation => "animation".to_string(),
    }
}
fn str_to_mode(s: &str) -> Option<EditorMode> {
    match s {
        "scene" => Some(EditorMode::Scene),
        "material" => Some(EditorMode::Material),
        "animation" => Some(EditorMode::Animation),
        _ => None,
    }
}
