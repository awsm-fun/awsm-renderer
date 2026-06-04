//! App shell: the top bar + mode router + global overlay hosts. Every action is
//! a dispatched [`EditorCommand`] through the [`controller`] — the UI never
//! mutates editor state directly. The ribbon + the real Scene/Material
//! workspaces land in M4+ (placeholders for now).

use crate::prelude::*;

const ACCENT_FG: &str = "oklch(0.18 0.02 255)";

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
        .child(top_bar(&ctrl))
        .child(workspace(&ctrl))
    })
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
        .event(|_: events::Click| Toast::info("Command palette — lands in M11"))
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
            .on_click(|| Toast::info("Settings — lands in M11")).render())
        .child(cmdk_button())
        .child(html!("div", { .style("flex", "1") }))
        .child(project_label(ctrl))
        .child(vdivider())
        .child(html!("div", {
            .style("display", "flex")
            .style("align-items", "center")
            .style("gap", "2px")
            .child(IconBtn::new("folder").title("New")
                .on_click(|| spawn_local(async { let _ = controller().dispatch(EditorCommand::NewProject).await; })).render())
            .child(IconBtn::new("save").title("Save")
                .on_click(|| Toast::info("Save — lands in M11")).render())
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
        .child(IconBtn::new("more").title("More")
            .on_click(|| Toast::info("More \u{2014} overflow menu lands in M11")).render())
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
        }))
        .child(html!("div", {
            .style("position", "absolute")
            .style("inset", "0")
            .style_signal("display", ctrl.mode.signal().map(|m| if m == EditorMode::Material { "block" } else { "none" }))
            .child(placeholder("Material workspace", "the Studio lands in M9\u{2013}M10"))
        }))
    })
}

fn placeholder(title: &str, sub: &str) -> Dom {
    html!("div", {
        .style("position", "absolute")
        .style("inset", "0")
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("align-items", "center")
        .style("justify-content", "center")
        .style("gap", "8px")
        .style("background", "var(--bg-0)")
        .child(html!("span", {
            .style("color", "var(--text-2)").style("font-size", "14px").style("font-weight", "600").text(title)
        }))
        .child(html!("span", {
            .style("color", "var(--text-3)").style("font-size", "12px").text(sub)
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
