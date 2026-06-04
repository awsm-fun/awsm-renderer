//! Material-mode **Studio** (material-mode.jsx + material-shell.jsx) — the
//! custom-WGSL authoring workspace (decision 3). A 3-column grid:
//! Library (the custom material list) · Definition (surface + declared uniforms/
//! textures/buffers) · main (a code pane + preview placeholder). The Material
//! Contract is a dismissible help drawer.
//!
//! The live 2nd-renderer preview + real GPU registration land in M10; this
//! milestone delivers the full authoring surface, a lightweight in-editor WGSL
//! check, and the register/draft lifecycle.

use std::sync::Arc;

use crate::controller::{AlphaMode, CustomMaterial, Slot};
use crate::engine::scene::AssetId;
use crate::prelude::*;

const UNIFORM_TYPES: &[&str] = &[
    "f32",
    "i32",
    "u32",
    "vec2<f32>",
    "vec3<f32>",
    "vec4<f32>",
    "mat3x3<f32>",
    "mat4x4<f32>",
];

pub fn render() -> Dom {
    let help = Mutable::new(false);
    html!("div", {
        .style("position", "absolute").style("inset", "0")
        .style("display", "flex").style("flex-direction", "column")
        .style("min-height", "0").style("background", "var(--bg-0)")
        .child(html!("div", {
            .style("position", "relative").style("flex", "1").style("min-height", "0")
            .style("display", "grid")
            .style("grid-template-columns", "222px 244px 1fr")
            .style("grid-template-rows", "minmax(0, 1fr)")
            .child(html!("div", {
                .style("border-right", "1px solid var(--line)").style("min-height", "0")
                .child(library())
            }))
            .child(html!("div", {
                .style("border-right", "1px solid var(--line)").style("min-height", "0")
                .child_signal(controller().current_material.signal().map(|id| Some(definition(id))))
            }))
            .child(html!("div", {
                .style("min-width", "0").style("min-height", "0").style("background", "var(--bg-0)")
                .child_signal(controller().current_material.signal().map(clone!(help => move |id| Some(main_pane(id, help.clone())))))
            }))
            .child_signal(help.signal().map(clone!(help => move |open| {
                if open { Some(contract_drawer(help.clone())) } else { None }
            })))
        }))
    })
}

// ── Library (material-mode.jsx MaterialLibrary) ───────────────────────────────

fn library() -> Dom {
    html!("div", {
        .style("display", "flex").style("flex-direction", "column").style("height", "100%").style("background", "var(--bg-1)")
        .child(panel_header("Material Assets", Some(
            IconBtn::new("plus").title("New material").size(15.0)
                .on_click(|| dispatch(EditorCommand::AddCustomMaterial)).render(),
        )))
        .child(html!("div", {
            .style("flex", "1").style("overflow-y", "auto").style("padding", "8px")
            .style("display", "flex").style("flex-direction", "column").style("gap", "5px")
            .children_signal_vec(controller().custom_materials.signal_vec_cloned().map(library_row))
            .child_signal(controller().custom_materials.signal_vec_cloned().len().map(|n| {
                if n == 0 {
                    Some(html!("div", {
                        .style("padding", "10px 4px").style("font-size", "12px").style("color", "var(--text-3)").style("line-height", "1.5")
                        .text("No custom materials yet. Create one to author WGSL.")
                    }))
                } else { None }
            }))
        }))
        .child(html!("div", {
            .style("padding", "10px").style("border-top", "1px solid var(--line-soft)")
            .child(Btn::new().label("New material").icon("plus").variant(BtnVariant::Solid).full(true)
                .on_click(|| dispatch(EditorCommand::AddCustomMaterial)).render())
        }))
    })
}

fn library_row(mat: Arc<CustomMaterial>) -> Dom {
    let id = mat.id;
    let on_sig = controller()
        .current_material
        .signal()
        .map(move |c| c == Some(id));
    let on_sig2 = controller()
        .current_material
        .signal()
        .map(move |c| c == Some(id));
    html!("button", {
        .class("t")
        .style("display", "flex").style("align-items", "center").style("gap", "10px").style("padding", "8px")
        .style("border-radius", "var(--r2)").style("cursor", "pointer").style("text-align", "left")
        .style("border-width", "1px").style("border-style", "solid")
        .style_signal("border-color", on_sig.map(|on| if on { "var(--accent-line)" } else { "var(--line-soft)" }))
        .style_signal("background", on_sig2.map(|on| if on { "var(--accent-ghost)" } else { "var(--bg-2)" }))
        .child(html!("div", {
            .style("width", "38px").style("height", "38px").style("border-radius", "var(--r2)").style("flex", "0 0 auto")
            .style("border", "1px solid var(--line-strong)").style("box-shadow", "inset 0 0 0 1px oklch(1 0 0 / .08)")
            .style_signal("background", mat.color.signal_cloned())
        }))
        .child(html!("div", {
            .style("flex", "1").style("min-width", "0")
            .child(html!("div", {
                .style("font-size", "12.5px").style("font-weight", "560").style("color", "var(--text-0)")
                .style("white-space", "nowrap").style("overflow", "hidden").style("text-overflow", "ellipsis")
                .text_signal(mat.name.signal_cloned())
            }))
            .child(html!("div", {
                .style("margin-top", "3px")
                .child_signal(map_ref! {
                    let wgsl = mat.wgsl.signal_cloned(),
                    let reg = mat.registered.signal() =>
                    Some(status_badge(wgsl, *reg))
                })
            }))
        }))
        .event(move |_: events::Click| dispatch(EditorCommand::SetCurrentMaterial { id: Some(id) }))
    })
}

/// draft / ready / error pill (material-mode.jsx matBadge).
fn status_badge(wgsl: &str, registered: bool) -> Dom {
    let errs = crate::controller::compile_wgsl(wgsl);
    let (label, tone) = if !errs.is_empty() {
        ("error", Tone::Danger)
    } else if !registered {
        ("draft", Tone::Warn)
    } else {
        ("ready", Tone::Ok)
    };
    badge(label, tone)
}

// ── Definition rail (material-mode.jsx DefinitionPanel) ────────────────────────

fn definition(id: Option<AssetId>) -> Dom {
    let Some(mat) = id.and_then(|id| {
        crate::controller::custom_material::find_material(&controller().custom_materials, id)
    }) else {
        return html!("div", {
            .style("display", "flex").style("flex-direction", "column").style("height", "100%").style("background", "var(--bg-1)")
            .child(panel_header("Definition", None))
            .child(html!("div", { .style("padding", "16px").style("font-size", "12px").style("color", "var(--text-3)").style("line-height", "1.5")
                .text("Select or create a material in the Library to edit its definition.") }))
        });
    };

    html!("div", {
        .style("display", "flex").style("flex-direction", "column").style("height", "100%").style("background", "var(--bg-1)")
        .child(panel_header("Definition", None))
        .child(html!("div", {
            .style("flex", "1").style("overflow-y", "auto")
            .child(surface_section(&mat))
            .child(html!("div", {
                .style("margin", "11px 12px 2px").style("display", "flex").style("gap", "8px").style("align-items", "flex-start")
                .style("padding", "8px 10px").style("background", "oklch(0.80 0.13 85 / .08)")
                .style("border", "1px solid oklch(0.80 0.13 85 / .25)").style("border-radius", "var(--r2)")
                .child(Icon::new("warning").size(14.0).color("var(--warn)").render())
                .child(html!("span", { .style("font-size", "11px").style("color", "var(--text-1)").style("line-height", "1.45")
                    .text("Debug values drive the preview only. A mesh overrides them when this material is assigned.") }))
            }))
            .child(slot_list(&mat, SlotKind::Uniform))
            .child(slot_list(&mat, SlotKind::Texture))
            .child(slot_list(&mat, SlotKind::Buffer))
        }))
    })
}

fn surface_section(mat: &Arc<CustomMaterial>) -> Dom {
    // Alpha mode segmented.
    let alpha = Mutable::new(mat.alpha.get().key().to_string());
    spawn_local(clone!(alpha, mat => async move {
        let mut first = true;
        alpha.signal_cloned().for_each(move |k| {
            let fire = !first; first = false;
            clone!(mat => async move { if fire { mat.alpha.set_neq(AlphaMode::from_key(&k)); draft(&mat); } })
        }).await;
    }));

    let mut sec = Section::new("Surface").dense(true).child(html!("div", {
        .style("display", "flex").style("flex-direction", "column").style("gap", "5px")
        .child(html!("span", { .style("font-size", "12px").style("color", "var(--text-1)").text("Alpha mode") }))
        .child(segmented(alpha, vec![
            SegOption::new("opaque", "Opaque"),
            SegOption::new("mask", "Mask"),
            SegOption::new("blend", "Blend"),
        ], true, true))
    }));

    // Cutoff (mask only) — rebuild on alpha.
    sec = sec.child(html!("div", {
        .child_signal(mat.alpha.signal().map(clone!(mat => move |a| {
            if a == AlphaMode::Mask {
                let m = mat.clone();
                Some(row("Cutoff", NumField::new(mat.cutoff.get()).min(0.0).max(1.0).step(0.01)
                    .on_change(move |v| { m.cutoff.set_neq(v); draft(&m); }).render()))
            } else { None }
        })))
    }));

    // Double-sided.
    let ds = Mutable::new(mat.double_sided.get());
    spawn_local(clone!(ds, mat => async move {
        let mut first = true;
        ds.signal().for_each(move |on| {
            let fire = !first; first = false;
            clone!(mat => async move { if fire { mat.double_sided.set_neq(on); draft(&mat); } })
        }).await;
    }));
    sec = sec.child(row("Double-sided", toggle(ds)));

    // Base color (debug).
    let col = Mutable::new(mat.color.get_cloned());
    spawn_local(clone!(col, mat => async move {
        let mut first = true;
        col.signal_cloned().for_each(move |hex| {
            let fire = !first; first = false;
            clone!(mat => async move { if fire { mat.color.set_neq(hex); draft(&mat); } })
        }).await;
    }));
    sec = sec.child(row("Base color", html!("div", {
        .style("display", "flex").style("align-items", "center").style("gap", "8px")
        .child(swatch(col.clone(), 22.0))
        .child(html!("span", { .class("mono").style("font-size", "11px").style("color", "var(--text-2)")
            .text_signal(col.signal_cloned()) }))
    })));

    sec.render()
}

#[derive(Clone, Copy, PartialEq)]
enum SlotKind {
    Uniform,
    Texture,
    Buffer,
}

fn slot_list(mat: &Arc<CustomMaterial>, kind: SlotKind) -> Dom {
    let (title, _icon, add_label) = match kind {
        SlotKind::Uniform => ("Uniforms", "sliders", "add uniform"),
        SlotKind::Texture => ("Textures", "texture", "add texture slot"),
        SlotKind::Buffer => ("Buffers", "buffer", "add buffer slot"),
    };
    let field = slot_field(mat, kind);

    let mat_add = mat.clone();
    let add_btn = html!("button", {
        .class("t")
        .style("display", "flex").style("align-items", "center").style("justify-content", "center").style("gap", "6px")
        .style("width", "100%").style("margin-top", "6px").style("height", "28px")
        .style("border", "1px dashed var(--line)").style("border-radius", "var(--r1)")
        .style("background", "transparent").style("color", "var(--text-2)").style("cursor", "pointer").style("font-size", "11.5px")
        .child(Icon::new("plus").size(13.0).render())
        .child(html!("span", { .text(add_label) }))
        .event(move |_: events::Click| {
            let f = slot_field_of(&mat_add, kind);
            let mut v = f.get_cloned();
            let n = v.len() + 1;
            v.push(match kind {
                SlotKind::Uniform => Slot::uniform(format!("value{n}"), "f32", "0.0"),
                SlotKind::Texture => Slot::named(format!("tex{n}"), "texture_2d<f32>"),
                SlotKind::Buffer => Slot::named(format!("buf{n}"), "array<vec4<f32>>"),
            });
            f.set(v);
            draft(&mat_add);
        })
    });

    Section::new(title)
        .dense(true)
        .right(html!("span", { .class("mono").style("font-size", "10px").style("color", "var(--text-3)")
            .text_signal(slot_field_of(mat, kind).signal_cloned().map(|v| v.len().to_string())) }))
        .child(html!("div", {
            .style("display", "flex").style("flex-direction", "column").style("gap", "6px")
            .child_signal(field)
        }))
        .child(add_btn)
        .render()
}

fn slot_field_of(mat: &Arc<CustomMaterial>, kind: SlotKind) -> Mutable<Vec<Slot>> {
    match kind {
        SlotKind::Uniform => mat.uniforms.clone(),
        SlotKind::Texture => mat.textures.clone(),
        SlotKind::Buffer => mat.buffers.clone(),
    }
}

/// The reactive list of slot rows for one kind, rebuilt when the vec changes.
fn slot_field(mat: &Arc<CustomMaterial>, kind: SlotKind) -> impl Signal<Item = Option<Dom>> {
    let field = slot_field_of(mat, kind);
    let mat = mat.clone();
    field.signal_cloned().map(move |slots| {
        if slots.is_empty() {
            return Some(html!("div", { .style("font-size", "11.5px").style("color", "var(--text-3)").style("padding", "4px 2px").text("None yet.") }));
        }
        let rows: Vec<Dom> = slots
            .iter()
            .enumerate()
            .map(|(i, s)| slot_row(&mat, kind, i, s))
            .collect();
        Some(html!("div", { .style("display", "flex").style("flex-direction", "column").style("gap", "6px").children(rows) }))
    })
}

fn slot_row(mat: &Arc<CustomMaterial>, kind: SlotKind, i: usize, slot: &Slot) -> Dom {
    let field = slot_field_of(mat, kind);
    // Name input.
    let name = Mutable::new(slot.name.clone());
    let f_name = field.clone();
    let m_name = mat.clone();
    spawn_local(clone!(name => async move {
        let mut first = true;
        name.signal_cloned().for_each(move |v| {
            let fire = !first; first = false;
            clone!(f_name, m_name => async move {
                if fire {
                    let mut arr = f_name.get_cloned();
                    if let Some(s) = arr.get_mut(i) { s.name = v; f_name.set(arr); draft(&m_name); }
                }
            })
        }).await;
    }));

    let type_label = match kind {
        SlotKind::Uniform => None,
        SlotKind::Texture => Some("2D".to_string()),
        SlotKind::Buffer => None,
    };

    let f_rm = field.clone();
    let m_rm = mat.clone();
    html!("div", {
        .style("background", "var(--bg-2)").style("border", "1px solid var(--line-soft)").style("border-radius", "var(--r1)").style("overflow", "hidden")
        .child(html!("div", {
            .style("display", "flex").style("align-items", "center").style("gap", "7px").style("padding", "5px 6px 5px 8px")
            .child(Icon::new(match kind { SlotKind::Uniform => "sliders", SlotKind::Texture => "texture", SlotKind::Buffer => "buffer" }).size(14.0).color("var(--text-2)").render())
            .child(html!("div", { .style("flex", "1").style("min-width", "0").child(TextInput::new(name).render()) }))
            .apply(|b| match (kind, type_label) {
                (SlotKind::Uniform, _) => b.child(uniform_type_select(mat, i)),
                (_, Some(lbl)) => b.child(html!("span", { .class("mono").style("font-size", "10px").style("color", "var(--text-3)").text(&lbl) })),
                _ => b,
            })
            .child(html!("button", {
                .class("t").style("background", "transparent").style("border-style", "none").style("cursor", "pointer")
                .style("color", "var(--text-3)").style("display", "flex").style("padding", "2px")
                .attr("title", "Remove")
                .child(Icon::new("trash").size(13.0).render())
                .event(move |_: events::Click| {
                    let mut arr = f_rm.get_cloned();
                    if i < arr.len() { arr.remove(i); f_rm.set(arr); draft(&m_rm); }
                })
            }))
        }))
    })
}

fn uniform_type_select(mat: &Arc<CustomMaterial>, i: usize) -> Dom {
    let field = mat.uniforms.clone();
    let cur = field
        .get_cloned()
        .get(i)
        .map(|s| s.ty.clone())
        .unwrap_or_else(|| "f32".to_string());
    let sel = Mutable::new(cur);
    let f = field.clone();
    let m = mat.clone();
    spawn_local(clone!(sel => async move {
        let mut first = true;
        sel.signal_cloned().for_each(move |ty| {
            let fire = !first; first = false;
            clone!(f, m => async move {
                if fire {
                    let mut arr = f.get_cloned();
                    if let Some(s) = arr.get_mut(i) { s.ty = ty; f.set(arr); draft(&m); }
                }
            })
        }).await;
    }));
    select(
        sel,
        UNIFORM_TYPES
            .iter()
            .map(|t| (t.to_string(), t.to_string()))
            .collect(),
    )
}

// ── Main pane: code + preview (material-shell.jsx CodePane) ────────────────────

fn main_pane(id: Option<AssetId>, help: Mutable<bool>) -> Dom {
    let Some(mat) = id.and_then(|id| {
        crate::controller::custom_material::find_material(&controller().custom_materials, id)
    }) else {
        return html!("div", {
            .style("height", "100%").style("display", "flex").style("align-items", "center").style("justify-content", "center")
            .style("color", "var(--text-3)").style("font-size", "13px")
            .text("Create a material to start authoring.")
        });
    };
    html!("div", {
        .style("display", "flex").style("flex-direction", "column").style("height", "100%").style("min-width", "0").style("min-height", "0")
        .child(html!("div", { .style("flex", "1 1 56%").style("min-height", "0").style("border-bottom", "1px solid var(--line)").child(preview_pane(&mat)) }))
        .child(html!("div", { .style("flex", "1 1 44%").style("min-height", "0").child(code_pane(&mat, help)) }))
    })
}

fn preview_pane(mat: &Arc<CustomMaterial>) -> Dom {
    html!("div", {
        .style("position", "relative").style("height", "100%").style("display", "flex")
        .style("align-items", "center").style("justify-content", "center").style("overflow", "hidden")
        .style("background", "radial-gradient(120% 120% at 50% 30%, oklch(0.26 0.01 255), oklch(0.16 0.008 255))")
        // A debug-colored sphere stand-in; the live 2nd-renderer preview lands in M10.
        .child(html!("div", {
            .style("width", "120px").style("height", "120px").style("border-radius", "50%")
            .style("box-shadow", "inset -18px -22px 40px oklch(0 0 0 / .55), inset 10px 12px 26px oklch(1 0 0 / .12)")
            .style_signal("background", mat.color.signal_cloned().map(|c| format!("radial-gradient(circle at 36% 30%, oklch(1 0 0 / .35), {c} 60%)")))
        }))
        .child(html!("div", {
            .style("position", "absolute").style("left", "12px").style("bottom", "10px")
            .class("mono").style("font-size", "10.5px").style("color", "var(--text-3)")
            .text("preview · live render lands in M10")
        }))
    })
}

fn code_pane(mat: &Arc<CustomMaterial>, help: Mutable<bool>) -> Dom {
    html!("div", {
        .style("display", "flex").style("flex-direction", "column").style("height", "100%").style("min-height", "0")
        .style("background", "var(--bg-3)").style("overflow", "hidden")
        // Header.
        .child(html!("div", {
            .style("display", "flex").style("align-items", "center").style("gap", "8px").style("height", "38px").style("padding", "0 8px 0 12px")
            .style("background", "var(--bg-2)").style("border-bottom", "1px solid var(--line-soft)").style("flex", "0 0 auto")
            .child(Icon::new("code").size(15.0).color("var(--accent-bright)").render())
            .child(html!("span", { .class("mono").style("font-size", "12px").style("color", "var(--text-0)").style("font-weight", "500").text("shader.wgsl") }))
            .child(html!("span", { .style("width", "1px").style("height", "16px").style("background", "var(--line)").style("margin", "0 2px") }))
            .child(html!("span", { .class("mono").style("font-size", "11px").style("color", "var(--text-2)").text_signal(mat.name.signal_cloned()) }))
            .child(html!("div", {
                .style("margin-left", "auto").style("display", "flex").style("align-items", "center").style("gap", "6px")
                .child(html!("span", {
                    .style("display", "flex").style("align-items", "center").style("gap", "5px").style("font-size", "11px")
                    .child_signal(mat.wgsl.signal_cloned().map(|w| {
                        let errs = crate::controller::compile_wgsl(&w);
                        Some(if errs.is_empty() {
                            html!("span", { .style("color", "var(--ok)").text("\u{25cf} compiled") })
                        } else {
                            html!("span", { .style("color", "var(--danger)").text(&format!("\u{25cf} {} error{}", errs.len(), if errs.len() > 1 { "s" } else { "" })) })
                        })
                    }))
                }))
                .child(IconBtn::new("help").title("Contract & reference").size(15.0)
                    .on_click(clone!(help => move || help.set_neq(true))).render())
            }))
        }))
        // Editor (line gutter + textarea).
        .child(code_editor(mat))
        // Problems strip.
        .child(html!("div", {
            .style("flex", "0 0 auto").style("border-top", "1px solid var(--line-soft)").style("background", "var(--bg-2)").style("max-height", "120px").style("overflow-y", "auto")
            .child_signal(mat.wgsl.signal_cloned().map(|w| Some(problems(&w))))
        }))
        // Register / draft footer.
        .child(register_bar(mat))
    })
}

fn code_editor(mat: &Arc<CustomMaterial>) -> Dom {
    let mat = mat.clone();
    let initial = mat.wgsl.get_cloned();
    html!("div", {
        .style("position", "relative").style("flex", "1").style("min-height", "0").style("display", "flex").style("background", "var(--bg-3)").style("overflow", "hidden")
        .child(html!("textarea" => web_sys::HtmlTextAreaElement, {
            .class("mono")
            .attr("spellcheck", "false").attr("wrap", "off")
            .prop("value", &initial)
            .style("flex", "1").style("min-width", "0").style("margin", "0").style("padding", "12px 14px")
            .style("background", "var(--bg-3)").style("border-style", "none").style("outline-style", "none").style("resize", "none")
            .style("color", "var(--text-0)").style("font-size", "12.5px").style("line-height", "19px").style("white-space", "pre").style("tab-size", "4")
            .with_node!(ta => {
                .event(clone!(ta, mat => move |_: events::Input| mat.wgsl.set(ta.value())))
            })
        }))
    })
}

fn problems(wgsl: &str) -> Dom {
    let errs = crate::controller::compile_wgsl(wgsl);
    html!("div", {
        .child(html!("div", {
            .style("display", "flex").style("align-items", "center").style("gap", "8px").style("padding", "6px 12px")
            .child(html!("span", { .class("kicker").style("font-size", "10px").style("color", "var(--text-3)").style("text-transform", "uppercase").style("letter-spacing", ".06em").text("Problems") }))
            .apply(|b| if errs.is_empty() {
                b.child(html!("span", { .style("font-size", "11px").style("color", "var(--text-3)").text("no compile errors") }))
            } else {
                b.child(badge(errs.len().to_string(), Tone::Danger))
            })
        }))
        .children(errs.into_iter().map(|(line, msg)| html!("div", {
            .style("display", "flex").style("align-items", "center").style("gap", "9px").style("padding", "5px 12px").style("border-top", "1px solid var(--line-soft)")
            .child(Icon::new("help").size(13.0).color("var(--danger)").render())
            .child(html!("span", { .class("mono").style("font-size", "11px").style("color", "var(--danger)").text(&format!("L{line}")) }))
            .child(html!("span", { .style("font-size", "11.5px").style("color", "var(--text-1)").text(&msg) }))
        })))
    })
}

fn register_bar(mat: &Arc<CustomMaterial>) -> Dom {
    let id = mat.id;
    html!("div", {
        .style("flex", "0 0 auto").style("display", "flex").style("align-items", "center").style("gap", "8px")
        .style("padding", "8px 12px").style("border-top", "1px solid var(--line-soft)").style("background", "var(--bg-2)")
        .child(html!("span", {
            .style("font-size", "11px")
            .child_signal(mat.registered.signal().map(|r| Some(if r {
                html!("span", { .style("color", "var(--ok)").text("registered") })
            } else {
                html!("span", { .style("color", "var(--warn)").text("draft \u{2014} not registered") })
            })))
        }))
        .child(html!("div", { .style("flex", "1") }))
        .child(Btn::new().label("Register").icon("check").variant(BtnVariant::Primary).size(BtnSize::Sm)
            .on_click(move || dispatch(EditorCommand::RegisterMaterial { id })).render())
    })
}

// ── Contract drawer (material-shell.jsx HelpDrawer) ────────────────────────────

fn contract_drawer(help: Mutable<bool>) -> Dom {
    let mat = controller().current_material.get().and_then(|id| {
        crate::controller::custom_material::find_material(&controller().custom_materials, id)
    });
    let alpha = mat
        .as_ref()
        .map(|m| m.alpha.get())
        .unwrap_or(AlphaMode::Opaque);

    html!("div", {
        .child(html!("div", {
            .style("position", "fixed").style("inset", "0").style("background", "oklch(0 0 0 / 0.4)").style("z-index", "200")
            .event(clone!(help => move |_: events::Click| help.set_neq(false)))
        }))
        .child(html!("div", {
            .style("position", "fixed").style("top", "0").style("right", "0").style("bottom", "0").style("width", "380px")
            .style("background", "var(--bg-1)").style("border-left", "1px solid var(--line)").style("box-shadow", "var(--shadow-3)")
            .style("z-index", "201").style("display", "flex").style("flex-direction", "column")
            .child(html!("div", {
                .style("display", "flex").style("align-items", "center").style("height", "44px").style("padding", "0 10px 0 16px").style("border-bottom", "1px solid var(--line-soft)")
                .child(Icon::new("help").size(16.0).color("var(--accent-bright)").render())
                .child(html!("span", { .style("font-size", "13px").style("font-weight", "620").style("margin-left", "8px").text("Material Contract") }))
                .child(html!("div", { .style("margin-left", "auto")
                    .child(IconBtn::new("minus").title("Close").on_click(clone!(help => move || help.set_neq(false))).render()) }))
            }))
            .child(html!("div", {
                .style("flex", "1").style("overflow-y", "auto").style("padding", "16px").style("display", "flex").style("flex-direction", "column").style("gap", "16px")
                .child(doc_block(&format!("Return type \u{00b7} {}", alpha.key()), Some(alpha.ret_sig()), alpha.ret_note(), true))
                .child(doc_block("How your fragment is injected", None,
                    "Your shader.wgsl body is wrapped at emit time. You have `in` (interpolants), `camera`, `globals`, plus every uniform, texture and buffer you declare in the Definition rail \u{2014} referenced by name.", false))
                .child(doc_block("Specialize-only \u{00b7} bucket cap", None,
                    "Each registered material compiles to its own pipeline (a \u{201c}bucket\u{201d}) keyed by shader_id. The renderer caps total buckets at MAX_BUCKET_ENTRIES. Registration is transactional \u{2014} if any entry in a batch is invalid, the whole batch is rejected.", false))
            }))
        }))
    })
}

fn doc_block(title: &str, code: Option<&str>, body: &str, accent: bool) -> Dom {
    html!("div", {
        .style("padding", "13px").style("background", "var(--bg-2)").style("border-radius", "var(--r2)")
        .style("border", &format!("1px solid {}", if accent { "var(--accent-line)" } else { "var(--line-soft)" }))
        .child(html!("div", {
            .class("kicker").style("margin-bottom", "9px").style("font-size", "10px").style("text-transform", "uppercase").style("letter-spacing", ".06em")
            .style("color", if accent { "var(--accent-bright)" } else { "var(--text-2)" })
            .text(title)
        }))
        .apply(|b| match code {
            Some(c) => b.child(html!("code", { .class("mono").style("font-size", "11.5px").style("color", "var(--tk-fn)").style("display", "block").style("margin-bottom", "8px").text(c) })),
            None => b,
        })
        .child(html!("p", { .style("margin", "0").style("font-size", "12px").style("color", "var(--text-1)").style("line-height", "1.55").text(body) }))
    })
}

// ── helpers ────────────────────────────────────────────────────────────────────

fn panel_header(title: &str, right: Option<Dom>) -> Dom {
    html!("div", {
        .style("display", "flex").style("align-items", "center").style("height", "38px").style("padding", "0 8px 0 14px")
        .style("border-bottom", "1px solid var(--line-soft)").style("flex", "0 0 auto")
        .child(html!("span", { .style("font-size", "12.5px").style("font-weight", "620").style("color", "var(--text-0)").text(title) }))
        .child(html!("div", { .style("margin-left", "auto").apply(|b| match right { Some(r) => b.child(r), None => b }) }))
    })
}

/// Mark a material a draft (un-registered) after any content edit.
fn draft(mat: &Arc<CustomMaterial>) {
    mat.registered.set_neq(false);
    controller().dirty.set_neq(true);
}

fn dispatch(cmd: EditorCommand) {
    spawn_local(async move {
        let _ = controller().dispatch(cmd).await;
    });
}
