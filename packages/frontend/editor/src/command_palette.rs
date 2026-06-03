//! ⌘K command palette. Fuzzy-filterable list of commands — switch mode, open
//! Settings, insert nodes. Opened by ⌘K (see `keys.rs`) or the top-bar search
//! button. A skeleton here; the full command set (select any object, open any
//! material) is filled in at M9.

use crate::{actions, prelude::*, state, state::EditorMode};

struct Cmd {
    label: &'static str,
    group: &'static str,
    run: Box<dyn Fn()>,
}

fn insert_cmd(label: &'static str, f: fn()) -> Cmd {
    Cmd {
        label,
        group: "Insert",
        run: Box::new(move || {
            // Inserts land in the scene — make sure we're showing it.
            state::app_state().mode.set_neq(EditorMode::Scene);
            f();
        }),
    }
}

fn commands() -> Vec<Cmd> {
    vec![
        Cmd {
            label: "Switch to Scene mode",
            group: "Go",
            run: Box::new(|| state::app_state().mode.set_neq(EditorMode::Scene)),
        },
        Cmd {
            label: "Switch to Material mode",
            group: "Go",
            run: Box::new(|| state::app_state().mode.set_neq(EditorMode::Material)),
        },
        Cmd {
            label: "Open Settings",
            group: "Go",
            run: Box::new(|| state::app_state().settings_open.set_neq(true)),
        },
        insert_cmd("Insert Empty", actions::insert::empty),
        insert_cmd("Insert Sphere", actions::insert::primitive_sphere),
        insert_cmd("Insert Box", actions::insert::primitive_box),
        insert_cmd("Insert Plane", actions::insert::primitive_plane),
        insert_cmd("Insert Cylinder", actions::insert::primitive_cylinder),
        insert_cmd("Insert Cone", actions::insert::primitive_cone),
        insert_cmd("Insert Torus", actions::insert::primitive_torus),
        insert_cmd("Insert Camera", actions::insert::camera),
    ]
}

fn matches(cmd: &Cmd, q: &str) -> bool {
    if q.is_empty() {
        return true;
    }
    let q = q.to_ascii_lowercase();
    cmd.label.to_ascii_lowercase().contains(&q) || cmd.group.to_ascii_lowercase().contains(&q)
}

/// Mounts the palette (renders nothing until `cmdk_open` is true).
pub fn render() -> Dom {
    let open = state::app_state().cmdk_open.clone();
    html!("div", {
        .child_signal(open.signal().map(clone!(open => move |is_open| {
            if is_open { Some(render_palette(open.clone())) } else { None }
        })))
    })
}

fn render_palette(open: Mutable<bool>) -> Dom {
    let query = Mutable::new(String::new());

    let run_first = clone!(open, query => move || {
        let q = query.get_cloned();
        if let Some(cmd) = commands().into_iter().find(|c| matches(c, &q)) {
            (cmd.run)();
            open.set_neq(false);
        }
    });

    html!("div", {
        // backdrop
        .child(html!("div", {
            .style("position", "fixed")
            .style("inset", "0")
            .style("background", "oklch(0 0 0 / 0.45)")
            .style("z-index", "300")
            .event(clone!(open => move |_: events::Click| open.set_neq(false)))
        }))
        // panel
        .child(html!("div", {
            .style("position", "fixed")
            .style("top", "84px")
            .style("left", "50%")
            .style("transform", "translateX(-50%)")
            .style("width", "min(560px, 92vw)")
            .style("max-height", "60vh")
            .style("display", "flex")
            .style("flex-direction", "column")
            .style("background", "var(--bg-2)")
            .style("border", "1px solid var(--line)")
            .style("border-radius", "var(--r3)")
            .style("box-shadow", "var(--shadow-3)")
            .style("z-index", "301")
            .style("overflow", "hidden")
            // input row
            .child(html!("div", {
                .style("display", "flex")
                .style("align-items", "center")
                .style("gap", "9px")
                .style("padding", "12px 14px")
                .style("border-bottom", "1px solid var(--line-soft)")
                .child(html!("span", {
                    .style("color", "var(--text-3)")
                    .style("font-size", "14px")
                    .text("⌕")
                }))
                .child(html!("input" => web_sys::HtmlInputElement, {
                    .style("flex", "1 1 0")
                    .style("min-width", "0")
                    .style("background", "transparent")
                    .style("border", "0")
                    .style("outline", "none")
                    .style("color", "var(--text-0)")
                    .style("font-size", "14px")
                    .attr("placeholder", "Type a command… (insert, select, switch)")
                    .after_inserted(|el| { let _ = el.focus(); })
                    .with_node!(input => {
                        .event(clone!(query => move |_: events::Input| {
                            query.set_neq(input.value());
                        }))
                    })
                    .event(clone!(open, run_first => move |e: events::KeyDown| {
                        match e.key().as_str() {
                            "Escape" => open.set_neq(false),
                            "Enter" => run_first(),
                            _ => {}
                        }
                    }))
                }))
                .child(html!("span", {
                    .class("mono")
                    .style("font-size", "10px")
                    .style("color", "var(--text-3)")
                    .style("border", "1px solid var(--line)")
                    .style("border-radius", "4px")
                    .style("padding", "1px 5px")
                    .text("ESC")
                }))
            }))
            // results
            .child(html!("div", {
                .style("max-height", "44vh")
                .style("overflow-y", "auto")
                .style("padding", "4px")
                .child_signal(query.signal_cloned().map(clone!(open => move |q| {
                    Some(render_results(&q, open.clone()))
                })))
            }))
        }))
    })
}

fn render_results(q: &str, open: Mutable<bool>) -> Dom {
    let filtered: Vec<Cmd> = commands().into_iter().filter(|c| matches(c, q)).collect();
    if filtered.is_empty() {
        return html!("div", {
            .style("padding", "12px 14px")
            .style("font-size", "12.5px")
            .style("color", "var(--text-3)")
            .text("No commands match.")
        });
    }
    html!("div", {
        .children(filtered.into_iter().map(clone!(open => move |cmd| {
            let run = cmd.run;
            html!("button", {
                .class(["t", &*ROW])
                .child(html!("span", {
                    .style("flex", "1 1 0")
                    .style("text-align", "left")
                    .text(cmd.label)
                }))
                .child(html!("span", {
                    .class("kicker")
                    .style("font-size", "9.5px")
                    .text(cmd.group)
                }))
                .event(clone!(open => move |_: events::Click| {
                    run();
                    open.set_neq(false);
                }))
            })
        })))
    })
}

static ROW: LazyLock<String> = LazyLock::new(|| {
    class! {
        .style("display", "flex")
        .style("align-items", "center")
        .style("gap", "10px")
        .style("width", "100%")
        .style("padding", "8px 10px")
        .style("border-radius", "var(--r1)")
        .style("cursor", "pointer")
        .style("color", "var(--text-1)")
        .style("font-size", "13px")
        .pseudo!(":hover", {
            .style("background", "var(--bg-hover)")
            .style("color", "var(--text-0)")
        })
    }
});
