//! The ⌘K command palette (extras.jsx `CommandPalette`). A fuzzy-filtered
//! command list over the same `EditorCommand`s the panels dispatch — opened from
//! the top-bar search affordance or the ⌘K / Ctrl-K shortcut. Arrow keys move
//! the selection, Enter runs, Esc closes.

use std::rc::Rc;

use awsm_scene_schema::{LightKind, PrimitiveShape};

use crate::controller::animation::StepKind;
use crate::controller::InsertSpec;
use crate::prelude::*;

/// One palette entry.
struct Cmd {
    label: String,
    group: &'static str,
    icon: &'static str,
    run: Rc<dyn Fn()>,
}

pub fn render() -> Dom {
    html!("div", {
        .child_signal(controller().cmdk_open.signal().map(|open| if open { Some(palette()) } else { None }))
    })
}

/// Open / close the palette (also wired to the ⌘K shortcut + top-bar button).
pub fn set_open(open: bool) {
    controller().cmdk_open.set_neq(open);
}

fn close() {
    set_open(false);
}

fn palette() -> Dom {
    let cmds = Rc::new(commands());
    let query = Mutable::new(String::new());
    let idx = Mutable::new(0usize);

    html!("div", {
        // backdrop
        .child(html!("div", {
            .style("position", "fixed").style("inset", "0").style("background", "oklch(0 0 0 / 0.45)").style("z-index", "500")
            .event(|_: events::Click| close())
        }))
        // panel
        .child(html!("div", {
            .style("position", "fixed").style("left", "50%").style("top", "88px").style("transform", "translateX(-50%)")
            .style("width", "560px").style("max-width", "calc(100vw - 32px)").style("z-index", "501")
            .style("background", "var(--bg-1)").style("border", "1px solid var(--line)").style("border-radius", "var(--r4)")
            .style("box-shadow", "var(--shadow-3)").style("overflow", "hidden").style("display", "flex").style("flex-direction", "column")
            .child(search_row(cmds.clone(), query.clone(), idx.clone()))
            .child(html!("div", {
                .style("max-height", "380px").style("overflow-y", "auto").style("padding", "6px")
                .child_signal(clone!(cmds, idx => map_ref! {
                    let q = query.signal_cloned(),
                    let i = idx.signal() =>
                    Some(result_list(&cmds, q, *i, idx.clone()))
                }))
            }))
            .child(footer(cmds, query.clone()))
        }))
    })
}

fn search_row(cmds: Rc<Vec<Cmd>>, query: Mutable<String>, idx: Mutable<usize>) -> Dom {
    html!("div", {
        .style("display", "flex").style("align-items", "center").style("gap", "10px").style("padding", "12px 14px")
        .style("border-bottom", "1px solid var(--line-soft)")
        .child(Icon::new("search").size(17.0).color("var(--text-2)").render())
        .child(html!("input" => web_sys::HtmlInputElement, {
            .attr("placeholder", "Type a command or search\u{2026} (insert, switch, new)")
            .style("flex", "1").style("background", "transparent").style("border-style", "none").style("outline-style", "none")
            .style("color", "var(--text-0)").style("font-size", "14.5px")
            .after_inserted(|el| { let _ = el.focus(); })
            .with_node!(input => {
                .event(clone!(query, idx => move |_: events::Input| { query.set(input.value()); idx.set_neq(0); }))
                .event(clone!(cmds, query, idx => move |e: events::KeyDown| {
                    let n = filter(&cmds, &query.get_cloned()).len();
                    match e.key().as_str() {
                        "ArrowDown" => { e.prevent_default(); if n > 0 { idx.set_neq((idx.get() + 1).min(n - 1)); } }
                        "ArrowUp" => { e.prevent_default(); idx.set_neq(idx.get().saturating_sub(1)); }
                        "Enter" => {
                            e.prevent_default();
                            let f = filter(&cmds, &query.get_cloned());
                            if let Some(&ci) = f.get(idx.get()) {
                                let run = cmds[ci].run.clone();
                                close();
                                run();
                            }
                        }
                        "Escape" => { e.prevent_default(); close(); }
                        _ => {}
                    }
                }))
            })
        }))
        .child(html!("span", { .class("mono").style("font-size", "10px").style("color", "var(--text-3)")
            .style("border", "1px solid var(--line)").style("border-radius", "4px").style("padding", "2px 5px").text("ESC") }))
    })
}

fn result_list(cmds: &Rc<Vec<Cmd>>, query: &str, sel: usize, idx: Mutable<usize>) -> Dom {
    let f = filter(cmds, query);
    if f.is_empty() {
        return html!("div", {
            .style("padding", "14px 12px").style("font-size", "12.5px").style("color", "var(--text-3)")
            .text(&format!("No commands match \u{201c}{query}\u{201d}."))
        });
    }
    html!("div", {
        .children(f.iter().enumerate().map(|(row, &ci)| {
            let on = row == sel;
            let run = cmds[ci].run.clone();
            let (label, group, icon) = (cmds[ci].label.clone(), cmds[ci].group, cmds[ci].icon);
            html!("button", {
                .style("display", "flex").style("align-items", "center").style("gap", "11px").style("width", "100%")
                .style("padding", "8px 11px").style("border-style", "none").style("border-radius", "var(--r2)").style("cursor", "pointer").style("text-align", "left")
                .style("background", if on { "var(--accent-ghost)" } else { "transparent" }).style("color", "var(--text-0)")
                .child(Icon::new(icon).size(16.0).color(if on { "var(--accent-bright)" } else { "var(--text-2)" }).render())
                .child(html!("span", { .style("flex", "1").style("font-size", "13px").text(&label) }))
                .child(html!("span", { .class("kicker").style("font-size", "9.5px").style("text-transform", "uppercase").style("letter-spacing", ".06em")
                    .style("color", if on { "var(--accent-bright)" } else { "var(--text-3)" }).text(group) }))
                .event(clone!(idx => move |_: events::MouseEnter| idx.set_neq(row)))
                .event(move |_: events::Click| { close(); run(); })
            })
        }))
    })
}

fn footer(cmds: Rc<Vec<Cmd>>, query: Mutable<String>) -> Dom {
    html!("div", {
        .style("display", "flex").style("align-items", "center").style("gap", "12px").style("padding", "7px 14px")
        .style("border-top", "1px solid var(--line-soft)").style("font-size", "10.5px").style("color", "var(--text-3)")
        .child(html!("span", { .class("mono").text("\u{2191}\u{2193} navigate") }))
        .child(html!("span", { .class("mono").text("\u{21b5} run") }))
        .child(html!("span", { .class("mono").style("margin-left", "auto")
            .text_signal(query.signal_cloned().map(move |q| format!("{} results", filter(&cmds, &q).len()))) }))
    })
}

/// Indices of `cmds` whose "label group" matches `query` as a subsequence.
fn filter(cmds: &[Cmd], query: &str) -> Vec<usize> {
    cmds.iter()
        .enumerate()
        .filter(|(_, c)| subseq(&format!("{} {}", c.label, c.group), query))
        .map(|(i, _)| i)
        .take(40)
        .collect()
}

fn subseq(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let hay = haystack.to_lowercase();
    let mut it = hay.chars();
    'outer: for nc in needle.to_lowercase().chars() {
        if nc == ' ' {
            continue;
        }
        for hc in it.by_ref() {
            if hc == nc {
                continue 'outer;
            }
        }
        return false;
    }
    true
}

fn dispatch(cmd: EditorCommand) {
    spawn_local(async move {
        let _ = controller().dispatch(cmd).await;
    });
}

fn insert(spec: InsertSpec) -> Rc<dyn Fn()> {
    Rc::new(move || {
        dispatch(EditorCommand::Insert {
            spec: spec.clone(),
            parent: None,
        })
    })
}

fn commands() -> Vec<Cmd> {
    let mut v: Vec<Cmd> = Vec::new();
    let mut go = |label: &'static str, icon, run: Rc<dyn Fn()>| {
        v.push(Cmd {
            label: label.to_string(),
            group: "Go",
            icon,
            run,
        })
    };
    go(
        "Switch to Scene mode",
        "layers",
        Rc::new(|| {
            dispatch(EditorCommand::SwitchMode {
                mode: EditorMode::Scene,
            })
        }),
    );
    go(
        "Switch to Material mode",
        "material",
        Rc::new(|| {
            dispatch(EditorCommand::SwitchMode {
                mode: EditorMode::Material,
            })
        }),
    );
    go(
        "Toggle Content Browser",
        "folder",
        Rc::new(|| {
            let o = controller().content_browser_open.clone();
            o.set_neq(!o.get());
        }),
    );
    go(
        "New project",
        "file",
        Rc::new(|| dispatch(EditorCommand::NewProject)),
    );

    let mut edit = |label: &'static str, icon, run: Rc<dyn Fn()>| {
        v.push(Cmd {
            label: label.to_string(),
            group: "Edit",
            icon,
            run,
        })
    };
    edit(
        "Undo",
        "undo",
        Rc::new(|| {
            spawn_local(async {
                controller().undo().await;
            })
        }),
    );
    edit(
        "Redo",
        "redo",
        Rc::new(|| {
            spawn_local(async {
                controller().redo().await;
            })
        }),
    );

    let mut ins = |label: &'static str, icon, run: Rc<dyn Fn()>| {
        v.push(Cmd {
            label: label.to_string(),
            group: "Insert",
            icon,
            run,
        })
    };
    ins("Insert Empty", "empty", insert(InsertSpec::Empty));
    ins(
        "Insert Sphere",
        "sphere",
        insert(InsertSpec::Primitive(PrimitiveShape::default_sphere())),
    );
    ins(
        "Insert Box",
        "cube",
        insert(InsertSpec::Primitive(PrimitiveShape::default_box())),
    );
    ins(
        "Insert Plane",
        "sphere",
        insert(InsertSpec::Primitive(PrimitiveShape::default_plane())),
    );
    ins(
        "Insert Cylinder",
        "sphere",
        insert(InsertSpec::Primitive(PrimitiveShape::default_cylinder())),
    );
    ins(
        "Insert Cone",
        "sphere",
        insert(InsertSpec::Primitive(PrimitiveShape::default_cone())),
    );
    ins(
        "Insert Torus",
        "sphere",
        insert(InsertSpec::Primitive(PrimitiveShape::default_torus())),
    );
    ins(
        "Insert Directional Light",
        "light",
        insert(InsertSpec::Light(LightKind::Directional)),
    );
    ins(
        "Insert Point Light",
        "light",
        insert(InsertSpec::Light(LightKind::Point)),
    );
    ins(
        "Insert Spot Light",
        "light",
        insert(InsertSpec::Light(LightKind::Spot)),
    );
    ins("Insert Camera", "camera", insert(InsertSpec::Camera));
    ins(
        "Insert Box Collider",
        "collision",
        insert(InsertSpec::CollisionBox),
    );

    let mut mat = |label: &'static str, icon, run: Rc<dyn Fn()>| {
        v.push(Cmd {
            label: label.to_string(),
            group: "Material",
            icon,
            run,
        })
    };
    mat(
        "New custom material",
        "material",
        Rc::new(|| {
            dispatch(EditorCommand::AddCustomMaterial);
            dispatch(EditorCommand::SwitchMode {
                mode: EditorMode::Material,
            });
        }),
    );

    let mut anim = |label: &'static str, icon, run: Rc<dyn Fn()>| {
        v.push(Cmd {
            label: label.to_string(),
            group: "Animation",
            icon,
            run,
        })
    };
    anim(
        "Switch to Animation mode",
        "curve",
        Rc::new(|| {
            dispatch(EditorCommand::SwitchMode {
                mode: EditorMode::Animation,
            })
        }),
    );
    anim(
        "Animation: New clip",
        "plus",
        Rc::new(|| dispatch(EditorCommand::AddClip)),
    );
    anim(
        "Animation: Play / Pause",
        "curve",
        Rc::new(|| {
            let on = !controller().playing.get();
            dispatch(EditorCommand::SetPlaying { on });
        }),
    );
    anim(
        "Animation: To start",
        "reset",
        Rc::new(|| {
            dispatch(EditorCommand::StepPlayhead {
                kind: StepKind::Home,
            })
        }),
    );

    // Per-clip "select" entries — switch to Animation mode + select that clip.
    // Built directly (owned labels per clip).
    for clip in controller().custom_animations.lock_ref().iter() {
        let id = clip.id;
        let label = format!("Animation: Select clip \u{2014} {}", clip.name.get_cloned());
        v.push(Cmd {
            label,
            group: "Animation",
            icon: "curve",
            run: Rc::new(move || {
                dispatch(EditorCommand::SwitchMode {
                    mode: EditorMode::Animation,
                });
                dispatch(EditorCommand::SetCurrentClip { id: Some(id) });
            }),
        });
    }

    v
}
