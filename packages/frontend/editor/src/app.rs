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
        // Bare Q/W/E/R/T switch the gizmo tool (Select/Move/Rotate/Scale/
        // Universal) — but only when not typing into a field.
        .global_event(|e: events::KeyDown| {
            use crate::engine::gizmo::{gizmo_mode, GizmoMode};
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
        // Pick FIRST: the directory picker needs a live user gesture, so nothing
        // async may run before it. Only once a directory is chosen do we raise the
        // busy overlay — the native picker is already modal to the page, so guarding
        // it with our own overlay just flashes it behind the OS dialog.
        match crate::fs::ProjectDir::pick().await {
            Ok(dir) => {
                // Now block ALL interaction across the write under one guard. The save
                // is an async loop over many side files (File System Access, to a real
                // disk); if the user triggers another op or navigates mid-write the
                // write is cut off at a variable point → a silent PARTIAL project (the
                // missing-meshes/textures bug). `begin_activity` raises the full-screen
                // `busy_overlay` (no close, swallows all input) and auto-clears on drop.
                let _activity =
                    crate::engine::activity::begin_activity(format!("Saving to {}/…", dir.name()));
                match crate::controller::persistence::save_to_dir(&controller(), &dir).await {
                    Ok(()) => {
                        controller().project_name.set(dir.name());
                        controller().dirty.set_neq(false);
                        Toast::info(format!("Saved to {}/", dir.name()));
                    }
                    Err(e) => Toast::error(format!("Save failed: {e}")),
                }
            }
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
fn purge_unused_assets() {
    // Delete every asset the live scene no longer references. Undoable in one step
    // (the command's inverse is a Batch of RestoreAsset); the handler toasts the
    // count purged, so no confirm dialog is needed.
    spawn_local(async {
        if let Err(e) = controller()
            .dispatch(awsm_renderer_editor_protocol::EditorCommand::PurgeUnusedAssets)
            .await
        {
            Toast::error(format!("Purge failed: {e}"));
        }
    });
}

fn export_scene_glb() {
    spawn_local(async {
        // Block ALL interaction while the GLB is assembled (async image
        // resolution + GPU readbacks) and downloaded. `begin_activity` raises the
        // full-screen `busy_overlay` (no close, swallows input) and auto-clears on
        // drop the instant the export resolves.
        let _activity = crate::engine::activity::begin_activity("Exporting scene…");
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

/// Assemble a player bundle (`scene.toml` + an `assets/` directory: geometry-only
/// glbs, custom-material wgsl folders, referenced textures, LOD/cluster files) and
/// write every file into a picked directory via the File System Access handle.
/// Reuses `bake_player_bundle`'s `assemble_bundle` layout so the editor and the
/// runtime/player loader never drift.
fn export_player_bundle() {
    spawn_local(async {
        // Pick FIRST: the directory picker needs a live user gesture, so nothing
        // async may precede it — baking before the picker consumed the gesture and
        // the picker then threw "must be in response to a user gesture". Once a
        // directory is chosen we raise the overlay, then bake + write under ONE
        // guard so the scene can't mutate underneath the bake and an interruption
        // mid-write can't leave a silent partial bundle. The guard auto-clears on
        // drop, so a bake error dismisses the overlay.
        match crate::fs::ProjectDir::pick().await {
            Ok(dir) => {
                let activity = crate::engine::activity::begin_activity(format!(
                    "Preparing player bundle for {}/…",
                    dir.name()
                ));
                let bundle =
                    match crate::controller::export::bake_player_bundle(&controller()).await {
                        Ok(bundle) => bundle,
                        Err(e) => {
                            Toast::error(format!("Export bundle failed: {e}"));
                            return;
                        }
                    };
                activity.set_label(format!("Exporting bundle to {}/…", dir.name()));
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
                .child(row("SMAA", toggle(s.smaa.clone())))
                .child(row("Shadow denoise", toggle(s.shadow_denoise.clone())))
                .child(row("Light heatmap", toggle(s.heatmap.clone())))
                .child(row(
                    "Agent activity overlay",
                    toggle(crate::engine::activity_feed::enabled()),
                ))
                .child(row(
                    "Follow agent workspace",
                    toggle(crate::engine::activity_feed::follow_enabled()),
                ))
                .child(row(
                    "Show MCP notifications",
                    toggle(crate::remote::show_notifications()),
                ))
                .render(),
        )
        .child(shadows_section())
        .child(post_processing_section())
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
                .child(row("Manual clip planes", toggle(s.cam_clip_manual.clone())))
                .child(row(
                    "Near (m)",
                    NumField::new(s.cam_clip_near.get())
                        .min(0.0001)
                        .step(0.01)
                        .on_change(|v| controller().settings.cam_clip_near.set_neq(v.max(0.0001)))
                        .render(),
                ))
                .child(row(
                    "Far (m)",
                    NumField::new(s.cam_clip_far.get())
                        .min(0.01)
                        .step(10.0)
                        .on_change(|v| controller().settings.cam_clip_far.set_neq(v.max(0.01)))
                        .render(),
                ))
                .child(html!("div", {
                    .style("font-size", "11px").style("color", "var(--text-3)")
                    .style("line-height", "1.4").style("padding", "2px 0 4px")
                    .text("Off = auto: the planes track the orbit distance, which can clip very \
                           large or very close geometry. On pins them for this session.")
                }))
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
            .text("Most editor settings affect the viewport and chrome only and aren't saved. The Shadows and Post-processing sections are part of the project \u{2014} they are saved into the file and the player bundle.")
        }))
        .render()
}

/// Global SSCS (screen-space contact shadows) controls. Unlike the ephemeral
/// Viewport toggles, these persist: every change dispatches `SetShadowsSscs`,
/// which merges into `scene.shadows` (serialized + bundled) and syncs live to
/// the renderer. Seeded from the current `scene.shadows` when the drawer opens
/// (the drawer is a transient popover, so seed-at-open is enough — no two-way
/// binding needed).
fn shadows_section() -> Dom {
    let sh = controller().scene.shadows.get_cloned();

    // `enabled` is compile-time (recompiles the shadow pipelines); route it
    // through a Mutable so the standard `toggle` widget drives the dispatch.
    let enabled = Mutable::new(sh.sscs_enabled);
    spawn_local(clone!(enabled => async move {
        let mut first = true;
        enabled
            .signal()
            .for_each(move |on| {
                let fire = !first;
                first = false;
                async move {
                    if fire {
                        dispatch_sscs(Some(on), None, None, None, None, None);
                    }
                }
            })
            .await;
    }));

    DrawerSection::new("Shadows")
        .right(settings_help_button(
            "Contact shadows (SSCS)",
            vec![
                (
                    "Contact shadows (SSCS)",
                    "Screen-space contact shadows: a short view-space ray-march that darkens \
                     the small contact gaps a shadow map misses (e.g. the lit gap right under \
                     a resting object). Subtle by design; toggling recompiles the shadow \
                     shaders. Global — saved into the project + player bundle.",
                ),
                (
                    "SSCS steps",
                    "How many samples the ray takes toward the light. More = longer reach and \
                     smoother result, at more cost. Compile-time (changing it recompiles).",
                ),
                (
                    "Step length (m)",
                    "World distance per step, in metres. Total reach = step length × steps. \
                     Small hugs the contact; large catches farther occluders.",
                ),
                (
                    "Thickness (m)",
                    "How thick an occluder can be and still count: a depth sample this far or \
                     less in front of the ray is treated as a blocker. Raise it to catch \
                     chunky objects (a ball needs ~0.3); too high over-darkens thin geometry.",
                ),
                (
                    "Directional darkening",
                    "Maximum darkening (0–1) SSCS applies under a directional (sun) light.",
                ),
                (
                    "Punctual darkening",
                    "Maximum darkening (0–1) under point/spot lights — usually higher than \
                     directional, since a cube shadow map leaves a wider lit contact gap.",
                ),
            ],
        ))
        .child(row("Contact shadows (SSCS)", toggle(enabled)))
        .child(row(
            "SSCS steps",
            NumField::new(sh.sscs_step_count as f64)
                .min(1.0)
                .step(1.0)
                .on_change(|v| dispatch_sscs(None, Some((v as u32).max(1)), None, None, None, None))
                .render(),
        ))
        .child(row(
            "Step length (m)",
            NumField::new(sh.sscs_step_world as f64)
                .min(0.0)
                .step(0.005)
                .on_change(|v| dispatch_sscs(None, None, Some(v as f32), None, None, None))
                .render(),
        ))
        .child(row(
            "Thickness (m)",
            NumField::new(sh.sscs_thickness as f64)
                .min(0.0)
                .step(0.01)
                .on_change(|v| dispatch_sscs(None, None, None, Some(v as f32), None, None))
                .render(),
        ))
        .child(row(
            "Directional darkening",
            NumField::new(sh.sscs_directional_darkening as f64)
                .min(0.0)
                .max(1.0)
                .step(0.05)
                .on_change(|v| dispatch_sscs(None, None, None, None, Some(v as f32), None))
                .render(),
        ))
        .child(row(
            "Punctual darkening",
            NumField::new(sh.sscs_punctual_darkening as f64)
                .min(0.0)
                .max(1.0)
                .step(0.05)
                .on_change(|v| dispatch_sscs(None, None, None, None, None, Some(v as f32)))
                .render(),
        ))
        .render()
}

/// Global post-processing controls (tonemapping / bloom / DoF / exposure).
/// Same model as [`shadows_section`]: these PERSIST — every change dispatches
/// `SetPostProcess`, which merges into `scene.post_process` (serialized +
/// bundled) and syncs live to the renderer via `settings_sync`. Seeded from the
/// current `scene.post_process` when the drawer opens.
fn post_processing_section() -> Dom {
    use awsm_renderer_editor_protocol::ToneMappingConfig;
    let pp = controller().scene.post_process.get_cloned();

    // Toggles + select ride Mutables so the standard widgets drive the
    // dispatch; skip each Mutable's initial (seed) emission.
    let bloom = Mutable::new(pp.bloom);
    spawn_local(clone!(bloom => async move {
        let mut first = true;
        bloom.signal().for_each(move |on| {
            let fire = !first;
            first = false;
            async move {
                if fire {
                    dispatch_post(None, Some(on), None, None, None, None, None, None);
                }
            }
        }).await;
    }));
    let dof = Mutable::new(pp.dof);
    spawn_local(clone!(dof => async move {
        let mut first = true;
        dof.signal().for_each(move |on| {
            let fire = !first;
            first = false;
            async move {
                if fire {
                    dispatch_post(None, None, Some(on), None, None, None, None, None);
                }
            }
        }).await;
    }));
    let tonemap = Mutable::new(
        match pp.tonemapping {
            ToneMappingConfig::None => "none",
            ToneMappingConfig::KhronosNeutralPbr => "khronos_neutral_pbr",
            ToneMappingConfig::Aces => "aces",
        }
        .to_string(),
    );
    spawn_local(clone!(tonemap => async move {
        let mut first = true;
        tonemap.signal_cloned().for_each(move |v| {
            let fire = !first;
            first = false;
            async move {
                if fire {
                    let t = match v.as_str() {
                        "none" => ToneMappingConfig::None,
                        "aces" => ToneMappingConfig::Aces,
                        _ => ToneMappingConfig::KhronosNeutralPbr,
                    };
                    dispatch_post(Some(t), None, None, None, None, None, None, None);
                }
            }
        }).await;
    }));

    // SSR toggles (structural — each recompiles / rebuilds the SSR pass) ride
    // Mutables like bloom/dof, skipping the seed emission. Scalar SSR knobs are
    // NumField blur-commit rows below (live uniforms). `half_res` is surfaced as
    // a toggle mapping on→0.5 / off→1.0 onto `resolution_scale`.
    let ssr_enabled = Mutable::new(pp.ssr.enabled);
    spawn_local(clone!(ssr_enabled => async move {
        let mut first = true;
        ssr_enabled.signal().for_each(move |on| {
            let fire = !first;
            first = false;
            async move {
                if fire {
                    dispatch_ssr(Some(on), None, None, None, None, None, None, None, None, None);
                }
            }
        }).await;
    }));
    let ssr_temporal = Mutable::new(pp.ssr.temporal);
    spawn_local(clone!(ssr_temporal => async move {
        let mut first = true;
        ssr_temporal.signal().for_each(move |on| {
            let fire = !first;
            first = false;
            async move {
                if fire {
                    dispatch_ssr(None, None, None, None, None, None, None, Some(on), None, None);
                }
            }
        }).await;
    }));
    let ssr_half_res = Mutable::new(pp.ssr.resolution_scale < 1.0);
    spawn_local(clone!(ssr_half_res => async move {
        let mut first = true;
        ssr_half_res.signal().for_each(move |on| {
            let fire = !first;
            first = false;
            async move {
                if fire {
                    let scale = if on { 0.5 } else { 1.0 };
                    dispatch_ssr(None, None, None, None, None, None, None, None, Some(scale), None);
                }
            }
        }).await;
    }));

    DrawerSection::new("Post-processing")
        .right(settings_help_button(
            "Post-processing",
            vec![
                (
                    "Tonemapping",
                    "Operator mapping the HDR scene to the display. Khronos PBR-neutral \
                     (default) preserves material colors; ACES is filmic (stronger highlight \
                     roll-off); None is linear pass-through (HDR values clip). Global — saved \
                     into the project + player bundle.",
                ),
                (
                    "Bloom",
                    "Bright areas bleed a soft glow. Toggling recompiles the effects \
                     pipelines.",
                ),
                (
                    "Depth of field",
                    "Blurs away from the active camera's focus distance (cameras default to \
                     focusing their orbit/look-at target at f/16). Toggling recompiles the \
                     effects pipelines.",
                ),
                (
                    "Exposure (EV)",
                    "Pre-tonemap scene exposure in stops: 0 = unity, +1 = twice as bright, \
                     -1 = half. Use it to pull photometric light intensities into the \
                     tonemapper's range.",
                ),
                (
                    "SSR",
                    "Screen-space reflections. Enabling recompiles the material + SSR passes \
                     and allocates the reflection targets (zero cost when off). Intensity / \
                     max distance / thickness / max steps / spread cutoff / edge fade are live \
                     uniforms (tune freely, no recompile). Half-res and Temporal are structural \
                     (they rebuild the SSR pass). Temporal accumulates across frames for static \
                     scenes but ghosts moving objects — leave off for gameplay cameras. \
                     Temporal weight = history kept per frame (0..1, higher = smoother \
                     but more ghosting).",
                ),
            ],
        ))
        .child(row(
            "Tonemapping",
            select(
                tonemap,
                vec![
                    ("khronos_neutral_pbr".to_string(), "Khronos PBR".to_string()),
                    ("aces".to_string(), "ACES".to_string()),
                    ("none".to_string(), "None (linear)".to_string()),
                ],
            ),
        ))
        .child(row("Bloom", toggle(bloom)))
        .child(row("Depth of field", toggle(dof)))
        .child(row(
            "Exposure (EV)",
            NumField::new(pp.exposure as f64)
                .step(0.25)
                .on_change(|v| {
                    dispatch_post(None, None, None, Some(v as f32), None, None, None, None)
                })
                .render(),
        ))
        .child(row(
            "Bloom threshold",
            NumField::new(pp.bloom_threshold as f64)
                .step(0.1)
                .on_change(|v| {
                    dispatch_post(None, None, None, None, Some(v as f32), None, None, None)
                })
                .render(),
        ))
        .child(row(
            "Bloom knee",
            NumField::new(pp.bloom_knee as f64)
                .step(0.05)
                .on_change(|v| {
                    dispatch_post(None, None, None, None, None, Some(v as f32), None, None)
                })
                .render(),
        ))
        .child(row(
            "Bloom intensity",
            NumField::new(pp.bloom_intensity as f64)
                .step(0.05)
                .on_change(|v| {
                    dispatch_post(None, None, None, None, None, None, Some(v as f32), None)
                })
                .render(),
        ))
        .child(row(
            "Bloom scatter",
            NumField::new(pp.bloom_scatter as f64)
                .step(0.1)
                .on_change(|v| {
                    dispatch_post(None, None, None, None, None, None, None, Some(v as f32))
                })
                .render(),
        ))
        // ── Screen-space reflections ──
        .child(row("SSR", toggle(ssr_enabled)))
        .child(row(
            "SSR intensity",
            NumField::new(pp.ssr.intensity as f64)
                .step(0.05)
                .on_change(|v| {
                    dispatch_ssr(
                        None,
                        Some(v as f32),
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                    )
                })
                .render(),
        ))
        .child(row(
            "SSR max distance",
            NumField::new(pp.ssr.max_distance as f64)
                .step(1.0)
                .on_change(|v| {
                    dispatch_ssr(
                        None,
                        None,
                        Some(v as f32),
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                    )
                })
                .render(),
        ))
        .child(row(
            "SSR thickness",
            NumField::new(pp.ssr.thickness as f64)
                .step(0.1)
                .on_change(|v| {
                    dispatch_ssr(
                        None,
                        None,
                        None,
                        Some(v as f32),
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                    )
                })
                .render(),
        ))
        .child(row(
            "SSR max steps",
            NumField::new(pp.ssr.max_steps as f64)
                .step(8.0)
                .on_change(|v| {
                    dispatch_ssr(
                        None,
                        None,
                        None,
                        None,
                        Some(v.max(1.0) as u32),
                        None,
                        None,
                        None,
                        None,
                        None,
                    )
                })
                .render(),
        ))
        .child(row(
            "SSR spread cutoff",
            NumField::new(pp.ssr.spread_cutoff as f64)
                .step(0.05)
                .on_change(|v| {
                    dispatch_ssr(
                        None,
                        None,
                        None,
                        None,
                        None,
                        Some(v as f32),
                        None,
                        None,
                        None,
                        None,
                    )
                })
                .render(),
        ))
        .child(row(
            "SSR edge fade",
            NumField::new(pp.ssr.edge_fade as f64)
                .step(0.02)
                .on_change(|v| {
                    dispatch_ssr(
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        Some(v as f32),
                        None,
                        None,
                        None,
                    )
                })
                .render(),
        ))
        .child(row("SSR half-res", toggle(ssr_half_res)))
        .child(row("SSR temporal", toggle(ssr_temporal)))
        .child(row(
            "SSR temporal weight",
            NumField::new(pp.ssr.temporal_weight as f64)
                .step(0.05)
                .on_change(|v| {
                    dispatch_ssr(
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        Some((v as f32).clamp(0.0, 1.0)),
                    )
                })
                .render(),
        ))
        .render()
}

/// Dispatch a post-processing patch — only the `Some` fields change.
/// Fire-and-forget; the `settings_sync` observer applies it live.
#[allow(clippy::too_many_arguments)]
fn dispatch_post(
    tonemapping: Option<awsm_renderer_editor_protocol::ToneMappingConfig>,
    bloom: Option<bool>,
    dof: Option<bool>,
    exposure: Option<f32>,
    bloom_threshold: Option<f32>,
    bloom_knee: Option<f32>,
    bloom_intensity: Option<f32>,
    bloom_scatter: Option<f32>,
) {
    spawn_local(async move {
        if let Err(e) = controller()
            .dispatch(EditorCommand::SetPostProcess {
                tonemapping,
                bloom,
                dof,
                exposure,
                bloom_threshold,
                bloom_knee,
                bloom_intensity,
                bloom_scatter,
                // SSR rides its own patch path (`dispatch_ssr`, the drawer's SSR
                // rows); leave every SSR field unchanged from this bloom/tonemap path.
                ssr_enabled: None,
                ssr_intensity: None,
                ssr_max_distance: None,
                ssr_thickness: None,
                ssr_max_steps: None,
                ssr_spread_cutoff: None,
                ssr_edge_fade: None,
                ssr_temporal: None,
                ssr_resolution_scale: None,
                ssr_temporal_weight: None,
                ssr_debug: None,
            })
            .await
        {
            tracing::error!("SetPostProcess: {e}");
        }
    });
}

/// Dispatch a single-field (or multi-field) SSR patch — only the `Some` fields
/// change; every non-SSR post-process field is left untouched. Sibling of
/// [`dispatch_post`] so the many bloom call sites don't grow SSR args. Structural
/// SSR axes (`enabled`, `temporal`, `resolution_scale`) trigger the pass
/// rebuild/recompile in `set_post_processing`; the rest are live uniforms.
#[allow(clippy::too_many_arguments)]
fn dispatch_ssr(
    ssr_enabled: Option<bool>,
    ssr_intensity: Option<f32>,
    ssr_max_distance: Option<f32>,
    ssr_thickness: Option<f32>,
    ssr_max_steps: Option<u32>,
    ssr_spread_cutoff: Option<f32>,
    ssr_edge_fade: Option<f32>,
    ssr_temporal: Option<bool>,
    ssr_resolution_scale: Option<f32>,
    ssr_temporal_weight: Option<f32>,
) {
    let ssr_debug: Option<u32> = None;
    spawn_local(async move {
        if let Err(e) = controller()
            .dispatch(EditorCommand::SetPostProcess {
                tonemapping: None,
                bloom: None,
                dof: None,
                exposure: None,
                bloom_threshold: None,
                bloom_knee: None,
                bloom_intensity: None,
                bloom_scatter: None,
                ssr_enabled,
                ssr_intensity,
                ssr_max_distance,
                ssr_thickness,
                ssr_max_steps,
                ssr_spread_cutoff,
                ssr_edge_fade,
                ssr_temporal,
                ssr_resolution_scale,
                ssr_temporal_weight,
                ssr_debug,
            })
            .await
        {
            tracing::error!("SetPostProcess (SSR): {e}");
        }
    });
}

/// Dispatch a single-field (or multi-field) SSCS patch — only the `Some` fields
/// change. Fire-and-forget; the `settings_sync` observer applies it live.
fn dispatch_sscs(
    enabled: Option<bool>,
    step_count: Option<u32>,
    step_world: Option<f32>,
    thickness: Option<f32>,
    directional_darkening: Option<f32>,
    punctual_darkening: Option<f32>,
) {
    spawn_local(async move {
        if let Err(e) = controller()
            .dispatch(EditorCommand::SetShadows {
                patch: awsm_renderer_editor_protocol::ShadowsPatch {
                    sscs_enabled: enabled,
                    sscs_step_count: step_count,
                    sscs_step_world: step_world,
                    sscs_thickness: thickness,
                    sscs_directional_darkening: directional_darkening,
                    sscs_punctual_darkening: punctual_darkening,
                    ..Default::default()
                },
            })
            .await
        {
            tracing::error!("SetShadows (SSCS): {e}");
        }
    });
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
    html!("a", {
        // Links home to awsm.fun — in a new tab so it never navigates away from
        // unsaved editor work. Mirrors the awsm-audio brand so the tools read as
        // one family.
        .attr("href", "https://awsm.fun")
        .attr("target", "_blank")
        .attr("rel", "noopener noreferrer")
        .attr("title", "awsm.fun")
        .style("text-decoration", "none")
        .style("color", "inherit")
        .style("cursor", "pointer")
        .style("user-select", "none")
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

/// Top-bar Help button — a full labelled `(?) Help` button (not a bare icon),
/// sitting next to the MCP button. Opens the Help modal on its Overview tab.
fn help_button() -> Dom {
    Btn::new()
        .icon("help")
        .label("Help")
        .variant(BtnVariant::Ghost)
        .size(BtnSize::Sm)
        .title("Open the help guide")
        .on_click(crate::help_modal::open_help)
        .render()
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

        html!("div", {
            .style("display", "flex")
            .style("flex-direction", "column")
            .style("gap", "14px")
            // Header: title on the left, Help on the right (easy to spot — the
            // guide is the first thing a new user wants). Right padding keeps the
            // Help button clear of the modal's absolutely-positioned close X.
            .child(html!("div", {
                .style("display", "flex")
                .style("align-items", "center")
                .style("justify-content", "space-between")
                .style("gap", "8px")
                .style("padding-right", "30px")
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
                .text("Run awsm-renderer-scene-mcp locally, then connect — the editor dials out to \
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
            // Live work display — the activity feed (narration + panel spotlight)
            // that lets you watch the agent build. Also under Settings, with the
            // separate "follow agent workspace" (mode-switching) toggle.
            .child(row("Agent activity overlay", toggle(crate::engine::activity_feed::enabled())))
            .child(row("Follow agent workspace", toggle(crate::engine::activity_feed::follow_enabled())))
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
        .child(help_button())
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
                MenuItem::new("Purge unused assets").icon("trash").on_click(clone!(close => move || { purge_unused_assets(); (close.borrow_mut())(); })).render(),
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
