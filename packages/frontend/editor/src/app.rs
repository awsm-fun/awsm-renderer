//! App shell: the top bar + mode router + global overlay hosts. Every action is
//! a dispatched [`EditorCommand`] through the [`controller`] — the UI never
//! mutates editor state directly. The ribbon + the real Scene/Material
//! workspaces land in M4+ (placeholders for now).

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
        .child(activity_indicator())
        .child_signal(ctrl.settings_open.signal().map(|open| if open { Some(settings_drawer()) } else { None }))
    })
}

/// Floating pill that surfaces in-progress background work — model import / GPU
/// upload, material + render-pipeline compilation (issue #7). Driven by the
/// `engine::activity` indicator list; hidden when nothing is running.
fn activity_indicator() -> Dom {
    use crate::engine::activity::activities;
    html!("div", {
        .style("position", "fixed")
        .style("top", "52px")
        .style("left", "50%")
        .style("transform", "translateX(-50%)")
        .style("z-index", "350")
        .style("pointer-events", "none")
        .child_signal(activities().signal_ref(|acts| {
            if acts.is_empty() {
                return None;
            }
            let first = acts[0].1.clone();
            let extra = acts.len().saturating_sub(1);
            let label = if extra > 0 {
                format!("{first}   (+{extra} more)")
            } else {
                first
            };
            Some(html!("div", {
                .style("display", "flex")
                .style("align-items", "center")
                .style("gap", "9px")
                .style("padding", "7px 15px 7px 12px")
                .style("background", "var(--bg-1)")
                .style("border", "1px solid var(--line)")
                .style("border-radius", "999px")
                .style("box-shadow", "var(--shadow-2)")
                .style("font-size", "12.5px")
                .style("color", "var(--text-1)")
                .style("white-space", "nowrap")
                // Reuse the global `boot-spin` keyframe (index.html).
                .child(html!("div", {
                    .style("width", "13px")
                    .style("height", "13px")
                    .style("border", "2px solid var(--line)")
                    .style("border-top-color", "var(--accent)")
                    .style("border-radius", "50%")
                    .style("animation", "boot-spin 0.85s linear infinite")
                }))
                .child(html!("span", { .text(&label) }))
            }))
        }))
    })
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
/// `project.toml` + the per-material side files (decision 4).
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
                .child(row("MSAA", toggle(s.msaa.clone())))
                .child(row("Light heatmap", toggle(s.heatmap.clone())))
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
            NodeKind::Primitive { .. } | NodeKind::Mesh { .. } | NodeKind::Model(_) => {
                c.meshes += 1
            }
            NodeKind::Light(_) => c.lights += 1,
            _ => {}
        }
        count_nodes(&node.children.lock_ref(), c);
    }
}

/// The bottom status bar (settings-overflow.jsx StatsBar): live scene + material
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
        ], false, false))
        .child(IconBtn::new("settings").title("Settings")
            .on_click(|| controller().settings_open.set_neq(true)).render())
        .child(cmdk_button())
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
            // M5: ribbon over [outliner · viewport]. Inspector (right) lands in M7.
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
                    .child(crate::scene_mode::outliner::render())
                }))
                .child(html!("div", {
                    .style("flex", "1")
                    .style("min-width", "0")
                    .style("min-height", "0")
                    .style("position", "relative")
                    .child(crate::scene_mode::viewport::render())
                }))
                .child(html!("div", {
                    .style("width", "288px")
                    .style("flex", "0 0 auto")
                    .style("border-left", "1px solid var(--line)")
                    .style("min-height", "0")
                    .child(crate::scene_mode::inspector::render())
                }))
            }))
            // M8: Content Browser bottom drawer (collapsed bar / expanded grid).
            .child(crate::scene_mode::content_browser::render())
        }))
        .child(html!("div", {
            .style("position", "absolute")
            .style("inset", "0")
            .style_signal("display", ctrl.mode.signal().map(|m| if m == EditorMode::Material { "block" } else { "none" }))
            .child(crate::material_mode::render())
        }))
    })
}

fn mode_to_str(m: EditorMode) -> String {
    match m {
        EditorMode::Scene => "scene".to_string(),
        EditorMode::Material => "material".to_string(),
    }
}
fn str_to_mode(s: &str) -> Option<EditorMode> {
    match s {
        "scene" => Some(EditorMode::Scene),
        "material" => Some(EditorMode::Material),
        _ => None,
    }
}
