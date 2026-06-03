//! Definition pane — left side.
//!
//! Phase 10 ships the full interactive table editor for uniforms +
//! a render-state controls section (alpha_mode + cutoff +
//! double_sided). Texture / Buffer tables are next-session work
//! (require file-picker plumbing).

use dominator::{clone, events, html, with_node, Dom};
use futures_signals::signal::{Mutable, SignalExt};
use futures_signals::signal_vec::SignalVecExt;
use wasm_bindgen::JsCast;

use awsm_scene_schema::dynamic_material::{
    BufferSlot, FieldType, MaterialDefinition, TextureSlot, UniformField, UniformValue,
};
use awsm_scene_schema::material::MaterialAlphaMode;

use crate::material::state::EditState;

pub fn render(state: &EditState) -> Dom {
    let definition = state.definition.clone();
    html!("div", {
        .style("padding", "12px")
        .style("border-right", "1px solid #333")
        .style("overflow", "auto")
        .style("background", "#1a1a1a")
        .style("color", "#ddd")
        .style("font-size", "12px")
        .child(html!("h3", { .text("Definition") }))
        .child(render_render_state(&definition))
        .child(html!("h4", { .text("Uniforms") }))
        .child(render_uniform_table(&definition))
        .child(html!("button", {
            .style("margin-top", "8px")
            .text("+ add uniform")
            .event(clone!(definition => move |_: events::Click| {
                let mut def = definition.lock_mut();
                let n = def.uniforms.len();
                def.uniforms.push(UniformField {
                    name: format!("field_{}", n),
                    ty: FieldType::F32,
                    default: UniformValue::F32(0.0),
                });
            }))
        }))
        .child(html!("h4", { .text("Textures") }))
        .child(render_texture_table(&definition))
        .child(html!("button", {
            .style("margin-top", "8px")
            .text("+ add texture slot")
            .event(clone!(definition => move |_: events::Click| {
                let mut def = definition.lock_mut();
                let n = def.textures.len();
                def.textures.push(TextureSlot {
                    name: format!("tex_{}", n),
                    default: None,
                });
            }))
        }))
        .child(html!("h4", { .text("Buffers") }))
        .child(render_buffer_table(state))
        .child(html!("button", {
            .style("margin-top", "8px")
            .text("+ add buffer slot")
            .event(clone!(definition => move |_: events::Click| {
                let mut def = definition.lock_mut();
                let n = def.buffers.len();
                def.buffers.push(BufferSlot {
                    name: format!("buf_{}", n),
                    default: None,
                });
            }))
        }))
    })
}

fn render_buffer_table(state: &EditState) -> Dom {
    let definition = state.definition.clone();
    let converter_open = state.converter_open_for_slot.clone();
    let buffer_defaults = state.buffer_defaults.clone();
    html!("div", {
        .child_signal(definition.signal_cloned().map(clone!(
            definition, converter_open, buffer_defaults => move |def| {
            Some(html!("ul", {
                .style("padding-left", "12px")
                .children(def.buffers.iter().enumerate().map(|(i, b)| {
                    let definition = definition.clone();
                    let converter_open = converter_open.clone();
                    let buffer_defaults = buffer_defaults.clone();
                    let name = b.name.clone();
                    let name_for_count = name.clone();
                    let name_for_button = name.clone();
                    let name_for_remove = name.clone();
                    html!("li", {
                        .style("display", "flex")
                        .style("align-items", "center")
                        .style("gap", "6px")
                        .style("margin-bottom", "2px")
                        .child(html!("span", {
                            .style("flex", "1")
                            .text(&name)
                        }))
                        .child(html!("span", {
                            .style("color", "#888")
                            .style("font-size", "11px")
                            .text_signal(buffer_defaults.signal_cloned().map(move |defs| {
                                defs.get(&name_for_count)
                                    .map(|v| format!("{} words", v.len()))
                                    .unwrap_or_else(|| "(empty)".to_string())
                            }))
                        }))
                        .child(html!("button", {
                            .text("Edit data…")
                            .event(clone!(converter_open => move |_: events::Click| {
                                converter_open.set(Some(name_for_button.clone()));
                            }))
                        }))
                        .child(html!("button", {
                            .text("×")
                            .event(clone!(definition, buffer_defaults => move |_: events::Click| {
                                let mut def = definition.lock_mut();
                                if i < def.buffers.len() {
                                    def.buffers.remove(i);
                                }
                                drop(def);
                                buffer_defaults.lock_mut().remove(&name_for_remove);
                            }))
                        }))
                    })
                }).collect::<Vec<_>>())
            }))
        })))
    })
}

fn render_render_state(definition: &std::sync::Arc<Mutable<MaterialDefinition>>) -> Dom {
    let definition = definition.clone();
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "6px")
        .style("margin-bottom", "12px")
        .child(html!("label", {
            .style("display", "flex")
            .style("align-items", "center")
            .style("gap", "6px")
            .child(html!("span", { .text("alpha_mode:") }))
            .child(html!("select" => web_sys::HtmlSelectElement, {
                .child(html!("option", { .attr("value", "opaque").text("opaque") }))
                .child(html!("option", { .attr("value", "mask").text("mask") }))
                .child(html!("option", { .attr("value", "blend").text("blend") }))
                .prop_signal("value", definition.signal_cloned().map(|d| match d.alpha_mode {
                    MaterialAlphaMode::Opaque => "opaque".to_string(),
                    MaterialAlphaMode::Mask { .. } => "mask".to_string(),
                    MaterialAlphaMode::Blend => "blend".to_string(),
                }))
                .with_node!(_elem => {
                    .event(clone!(definition => move |e: events::Change| {
                        if let Some(target) = e.target() {
                            if let Ok(sel) = target.dyn_into::<web_sys::HtmlSelectElement>() {
                                let mut def = definition.lock_mut();
                                def.alpha_mode = match sel.value().as_str() {
                                    "mask" => MaterialAlphaMode::Mask { cutoff: 0.5 },
                                    "blend" => MaterialAlphaMode::Blend,
                                    _ => MaterialAlphaMode::Opaque,
                                };
                            }
                        }
                    }))
                })
            }))
        }))
        .child(html!("label", {
            .style("display", "flex")
            .style("align-items", "center")
            .style("gap", "6px")
            .child(html!("input" => web_sys::HtmlInputElement, {
                .attr("type", "checkbox")
                .prop_signal("checked", definition.signal_cloned().map(|d| d.double_sided))
                .with_node!(_elem => {
                    .event(clone!(definition => move |e: events::Change| {
                        if let Some(target) = e.target() {
                            if let Ok(inp) = target.dyn_into::<web_sys::HtmlInputElement>() {
                                let mut def = definition.lock_mut();
                                def.double_sided = inp.checked();
                            }
                        }
                    }))
                })
            }))
            .child(html!("span", { .text("double sided") }))
        }))
    })
}

fn render_uniform_table(definition: &std::sync::Arc<Mutable<MaterialDefinition>>) -> Dom {
    let definition = definition.clone();
    html!("div", {
        .child_signal(definition.signal_cloned().map(clone!(definition => move |def| {
            Some(html!("table", {
                .style("border-collapse", "collapse")
                .style("width", "100%")
                .child(html!("thead", {
                    .child(html!("tr", {
                        .children(&mut [
                            header_cell("name"),
                            header_cell("type"),
                            header_cell(""),
                        ])
                    }))
                }))
                .child(html!("tbody", {
                    .children(def.uniforms.iter().enumerate().map(|(i, field)| {
                        render_uniform_row(definition.clone(), i, field)
                    }).collect::<Vec<_>>())
                }))
            }))
        })))
    })
}

fn render_uniform_row(
    definition: std::sync::Arc<Mutable<MaterialDefinition>>,
    index: usize,
    field: &UniformField,
) -> Dom {
    let current_name = field.name.clone();
    let current_ty = field.ty;
    html!("tr", {
        .children(&mut [
            html!("td", {
                .style("padding", "2px")
                .child(html!("input" => web_sys::HtmlInputElement, {
                    .attr("type", "text")
                    .attr("value", &current_name)
                    .style("width", "100%")
                    .style("background", "#0b0b0b")
                    .style("color", "#cce")
                    .style("border", "1px solid #333")
                    .with_node!(_elem => {
                        .event(clone!(definition => move |e: events::Change| {
                            if let Some(target) = e.target() {
                                if let Ok(inp) = target.dyn_into::<web_sys::HtmlInputElement>() {
                                    let mut def = definition.lock_mut();
                                    if let Some(f) = def.uniforms.get_mut(index) {
                                        f.name = inp.value();
                                    }
                                }
                            }
                        }))
                    })
                }))
            }),
            html!("td", {
                .style("padding", "2px")
                .child(html!("select" => web_sys::HtmlSelectElement, {
                    .children(field_type_options(current_ty))
                    .with_node!(_elem => {
                        .event(clone!(definition => move |e: events::Change| {
                            if let Some(target) = e.target() {
                                if let Ok(sel) = target.dyn_into::<web_sys::HtmlSelectElement>() {
                                    let new_ty = parse_field_type(&sel.value());
                                    let mut def = definition.lock_mut();
                                    if let Some(f) = def.uniforms.get_mut(index) {
                                        f.ty = new_ty;
                                        f.default = default_value_for(new_ty);
                                    }
                                }
                            }
                        }))
                    })
                }))
            }),
            html!("td", {
                .style("padding", "2px")
                .child(html!("button", {
                    .text("×")
                    .event(clone!(definition => move |_: events::Click| {
                        let mut def = definition.lock_mut();
                        if index < def.uniforms.len() {
                            def.uniforms.remove(index);
                        }
                    }))
                }))
            }),
        ])
    })
}

fn render_texture_table(definition: &std::sync::Arc<Mutable<MaterialDefinition>>) -> Dom {
    let definition = definition.clone();
    html!("div", {
        .child_signal(definition.signal_cloned().map(clone!(definition => move |def| {
            Some(html!("ul", {
                .style("padding-left", "12px")
                .children(def.textures.iter().enumerate().map(|(i, t)| {
                    let definition = definition.clone();
                    let name = t.name.clone();
                    html!("li", {
                        .style("display", "flex")
                        .style("gap", "6px")
                        .child(html!("span", { .text(&name) }))
                        .child(html!("button", {
                            .text("×")
                            .event(move |_: events::Click| {
                                let mut def = definition.lock_mut();
                                if i < def.textures.len() {
                                    def.textures.remove(i);
                                }
                            })
                        }))
                    })
                }).collect::<Vec<_>>())
            }))
        })))
    })
}

fn header_cell(label: &str) -> Dom {
    html!("th", {
        .style("text-align", "left")
        .style("padding", "2px 4px")
        .style("border-bottom", "1px solid #333")
        .text(label)
    })
}

fn field_type_options(current: FieldType) -> Vec<Dom> {
    let all = [
        ("f32", FieldType::F32),
        ("vec2", FieldType::Vec2),
        ("vec3", FieldType::Vec3),
        ("vec4", FieldType::Vec4),
        ("u32", FieldType::U32),
        ("ivec2", FieldType::IVec2),
        ("ivec3", FieldType::IVec3),
        ("ivec4", FieldType::IVec4),
        ("mat3", FieldType::Mat3),
        ("mat4", FieldType::Mat4),
        ("color3", FieldType::Color3),
        ("color4", FieldType::Color4),
        ("bool", FieldType::Bool),
    ];
    all.into_iter()
        .map(|(label, ty)| {
            html!("option", {
                .attr("value", label)
                .apply_if(ty == current, |b| b.attr("selected", ""))
                .text(label)
            })
        })
        .collect()
}

fn parse_field_type(s: &str) -> FieldType {
    match s {
        "vec2" => FieldType::Vec2,
        "vec3" => FieldType::Vec3,
        "vec4" => FieldType::Vec4,
        "u32" => FieldType::U32,
        "ivec2" => FieldType::IVec2,
        "ivec3" => FieldType::IVec3,
        "ivec4" => FieldType::IVec4,
        "mat3" => FieldType::Mat3,
        "mat4" => FieldType::Mat4,
        "color3" => FieldType::Color3,
        "color4" => FieldType::Color4,
        "bool" => FieldType::Bool,
        _ => FieldType::F32,
    }
}

fn default_value_for(ty: FieldType) -> UniformValue {
    match ty {
        FieldType::F32 => UniformValue::F32(0.0),
        FieldType::Vec2 => UniformValue::Vec2([0.0; 2]),
        FieldType::Vec3 => UniformValue::Vec3([0.0; 3]),
        FieldType::Vec4 => UniformValue::Vec4([0.0; 4]),
        FieldType::U32 => UniformValue::U32(0),
        FieldType::IVec2 => UniformValue::IVec2([0; 2]),
        FieldType::IVec3 => UniformValue::IVec3([0; 3]),
        FieldType::IVec4 => UniformValue::IVec4([0; 4]),
        FieldType::Mat3 => UniformValue::Mat3([0.0; 9]),
        FieldType::Mat4 => UniformValue::Mat4([0.0; 16]),
        FieldType::Color3 => UniformValue::Color3([1.0, 1.0, 1.0]),
        FieldType::Color4 => UniformValue::Color4([1.0, 1.0, 1.0, 1.0]),
        FieldType::Bool => UniformValue::Bool(false),
    }
}

// SignalVecExt is used inline through child_signal — keep the
// trait in scope so the editor compiles cleanly when the build
// surface adds direct usage.
#[allow(dead_code)]
fn _ensure_signalvecext_in_scope() {
    let _ = std::marker::PhantomData::<dyn SignalVecExt<Item = ()>>;
}
