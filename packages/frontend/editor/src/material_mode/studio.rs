use std::sync::Arc;

use awsm_editor_protocol::{CustomAlphaMode, SlotSpec};

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

/// Menu rows for "New material" — pick a dynamic WGSL material or a built-in
/// (PBR / Unlit / Toon) shading type. Built-ins carry shared variant settings;
/// their uniform values are set per-mesh.
fn new_material_items(close: Close) -> Vec<Dom> {
    use awsm_scene_schema::MaterialShading;
    let mk = |label: &str, shading: Option<MaterialShading>, close: Close| {
        MenuItem::new(label)
            .on_click(move || {
                match shading {
                    Some(s) => dispatch(EditorCommand::AddBuiltinMaterial {
                        id: awsm_scene_schema::AssetId::new(),
                        shading: s,
                    }),
                    None => dispatch(EditorCommand::AddCustomMaterial {
                        id: awsm_scene_schema::AssetId::new(),
                    }),
                }
                (close.borrow_mut())();
            })
            .render()
    };
    vec![
        mk("PBR", Some(MaterialShading::Pbr), close.clone()),
        mk("Unlit", Some(MaterialShading::Unlit), close.clone()),
        mk(
            "Toon",
            Some(MaterialShading::Toon {
                diffuse_bands: 3,
                rim_strength: 0.4,
                specular_steps: 2,
                shininess: 32.0,
                rim_power: 2.0,
            }),
            close.clone(),
        ),
        mk("Dynamic (WGSL)", None, close),
    ]
}

fn library() -> Dom {
    html!("div", {
        .style("display", "flex").style("flex-direction", "column").style("height", "100%").style("background", "var(--bg-1)")
        .child(panel_header("Material Assets", Some(
            DropButton::new().icon("plus").variant(BtnVariant::Quiet).chevron(false)
                .items(new_material_items).render(),
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
            .child(DropButton::new().label("New material").icon("plus").variant(BtnVariant::Solid)
                .items(new_material_items).render())
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

/// Mutate the inner `MaterialDef` of a built-in library material + flag dirty.
/// The `spawn_builtin_resync` observer re-materializes assigned meshes.
fn edit_builtin(mat: &Arc<CustomMaterial>, f: impl FnOnce(&mut awsm_scene_schema::MaterialDef)) {
    let mut def = mat.builtin.get_cloned().unwrap_or_default();
    f(&mut def);
    mat.builtin.set(Some(def));
    // The variant changed → refresh its card thumbnail.
    crate::engine::thumbnail::invalidate(mat.id);
    crate::engine::thumbnail::request(mat.clone());
    controller().dirty.set_neq(true);
}

/// A toggle row bound to a built-in material's variant `MaterialDef`.
fn builtin_toggle_row(
    mat: &Arc<CustomMaterial>,
    label: &str,
    value: bool,
    set: impl Fn(&mut awsm_scene_schema::MaterialDef, bool) + 'static,
) -> Dom {
    use futures_signals::signal::SignalExt;
    let state = Mutable::new(value);
    let mat = mat.clone();
    let set = std::rc::Rc::new(set);
    spawn_local({
        let state = state.clone();
        async move {
            let mut first = true;
            state
                .signal()
                .for_each(move |on| {
                    let fire = !first;
                    first = false;
                    let mat = mat.clone();
                    let set = set.clone();
                    async move {
                        if fire {
                            edit_builtin(&mat, |d| set(d, on));
                        }
                    }
                })
                .await;
        }
    });
    row(label, toggle(state))
}

/// A `NumField` row bound to a built-in material's variant `MaterialDef` (factor
/// edits — no recompile, just a uniform change once the per-mesh path exists; for
/// now they live on the shared variant material).
fn builtin_num_row(
    mat: &Arc<CustomMaterial>,
    label: &str,
    value: f64,
    min: f64,
    max: f64,
    step: f64,
    set: impl Fn(&mut awsm_scene_schema::MaterialDef, f64) + 'static,
) -> Dom {
    let mat = mat.clone();
    row(
        label,
        NumField::new(value)
            .min(min)
            .max(max)
            .step(step)
            .on_change(move |v| edit_builtin(&mat, |d| set(d, v)))
            .render(),
    )
}

/// A texture-slot picker bound to a built-in material's variant `MaterialDef`.
/// Enabling a slot (None↔Some) is a VARIANT change (assigned meshes recompile to a
/// distinct shader); the texture itself is part of the material (not per-mesh) under
/// the current model. "— none —" disables the slot.
fn builtin_texture_row(
    mat: &Arc<CustomMaterial>,
    label: &str,
    current: Option<awsm_scene_schema::TextureRef>,
    set: impl Fn(&mut awsm_scene_schema::MaterialDef, Option<awsm_scene_schema::TextureRef>) + 'static,
) -> Dom {
    use awsm_scene_schema::TextureRef;
    use futures_signals::signal::SignalExt;
    let textures = crate::scene_mode::inspector::collect_texture_assets();
    let mut options: Vec<(String, String)> = vec![("__none__".into(), "— none —".into())];
    options.extend(
        textures
            .iter()
            .map(|(id, name)| (id.to_string(), name.clone())),
    );
    let lookup: Vec<(String, AssetId)> = textures
        .iter()
        .map(|(id, _)| (id.to_string(), *id))
        .collect();
    let sel = Mutable::new(
        current
            .map(|t| t.asset.0.to_string())
            .unwrap_or_else(|| "__none__".into()),
    );
    let mat = mat.clone();
    let set = std::rc::Rc::new(set);
    spawn_local(clone!(sel => async move {
        let mut first = true;
        sel.signal_cloned().for_each(move |val| {
            let fire = !first; first = false;
            let picked = lookup.iter().find(|(s, _)| *s == val).map(|(_, id)| TextureRef::new(*id));
            let mat = mat.clone(); let set = set.clone();
            async move { if fire { edit_builtin(&mat, |d| set(d, picked)); } }
        }).await;
    }));
    row(label, select(sel, options))
}

/// The material's texture slots (variant: enabling one recompiles assigned meshes).
/// Base-color + emissive maps apply to every shading model; metallic/roughness,
/// normal and occlusion maps are PBR-only.
fn textures_section(mat: &Arc<CustomMaterial>, def: &awsm_scene_schema::MaterialDef) -> Dom {
    use awsm_scene_schema::MaterialShading;
    let mut sec = Section::new("Textures");
    sec = sec.child(builtin_texture_row(
        mat,
        "Base color map",
        def.base_color_texture,
        |d, t| {
            d.base_color_texture = t;
        },
    ));
    if matches!(def.shading, MaterialShading::Pbr) {
        sec = sec.child(builtin_texture_row(
            mat,
            "Metal/rough map",
            def.metallic_roughness_texture,
            |d, t| {
                d.metallic_roughness_texture = t;
            },
        ));
        sec = sec.child(builtin_texture_row(
            mat,
            "Normal map",
            def.normal_texture,
            |d, t| {
                d.normal_texture = t;
            },
        ));
        sec = sec.child(builtin_texture_row(
            mat,
            "Occlusion map",
            def.occlusion_texture,
            |d, t| {
                d.occlusion_texture = t;
            },
        ));
    }
    sec = sec.child(builtin_texture_row(
        mat,
        "Emissive map",
        def.emissive_texture,
        |d, t| {
            d.emissive_texture = t;
        },
    ));
    sec.render()
}

/// The Toon knobs (uniforms structurally carried on the Toon shading variant, so
/// they live on the material). Only shown for Toon materials.
fn toon_section(mat: &Arc<CustomMaterial>, def: &awsm_scene_schema::MaterialDef) -> Dom {
    use awsm_scene_schema::MaterialShading;
    let MaterialShading::Toon {
        diffuse_bands,
        rim_strength,
        specular_steps,
        shininess,
        rim_power,
    } = def.shading
    else {
        return html!("div", {});
    };
    let knob = |label: &str,
                value: f64,
                min: f64,
                max: f64,
                step: f64,
                mutate: fn(&mut MaterialShading, f64)| {
        builtin_num_row(mat, label, value, min, max, step, move |d, v| {
            mutate(&mut d.shading, v)
        })
    };
    Section::new("Toon")
        .child(knob(
            "Diffuse bands",
            diffuse_bands as f64,
            1.0,
            16.0,
            1.0,
            |s, v| {
                if let MaterialShading::Toon { diffuse_bands, .. } = s {
                    *diffuse_bands = (v.round() as u32).max(1);
                }
            },
        ))
        .child(knob(
            "Specular steps",
            specular_steps as f64,
            1.0,
            8.0,
            1.0,
            |s, v| {
                if let MaterialShading::Toon { specular_steps, .. } = s {
                    *specular_steps = (v.round() as u32).max(1);
                }
            },
        ))
        .child(knob(
            "Shininess",
            shininess as f64,
            1.0,
            256.0,
            1.0,
            |s, v| {
                if let MaterialShading::Toon { shininess, .. } = s {
                    *shininess = v as f32;
                }
            },
        ))
        .child(knob(
            "Rim strength",
            rim_strength as f64,
            0.0,
            2.0,
            0.05,
            |s, v| {
                if let MaterialShading::Toon { rim_strength, .. } = s {
                    *rim_strength = v as f32;
                }
            },
        ))
        .child(knob(
            "Rim power",
            rim_power as f64,
            0.0,
            8.0,
            0.1,
            |s, v| {
                if let MaterialShading::Toon { rim_power, .. } = s {
                    *rim_power = v as f32;
                }
            },
        ))
        .render()
}

/// The KHR-extensions panel for a built-in **PBR** material. Each extension has
/// an enable toggle (flipping it is a *variant* change → assigned meshes recompile
/// to a distinct shader) and, when enabled, its scalar factor knob(s). Reactive on
/// the material's `builtin` signal so toggling reveals/hides the factor rows.
/// Color factors default to white and are edited once the texture/color picker
/// pass lands; the primary scalar of every extension is authorable here now.
fn extensions_section(mat: &Arc<CustomMaterial>) -> Dom {
    use awsm_scene_schema::MaterialShading;
    use futures_signals::signal::SignalExt;
    let mat = mat.clone();
    html!("div", {
        .child_signal(mat.builtin.signal_cloned().map(clone!(mat => move |b| {
            let def = b.unwrap_or_default();
            // Extensions only affect the PBR path.
            if !matches!(def.shading, MaterialShading::Pbr) {
                return None;
            }
            let e = def.extensions;
            let mut sec = Section::new("PBR extensions");

            // (enable toggle, [optional factor rows]) for each of the 11.
            macro_rules! toggle {
                ($label:literal, $is:expr, $set:expr) => {
                    sec = sec.child(builtin_toggle_row(&mat, $label, $is, $set));
                };
            }
            toggle!("Emissive strength", e.emissive_strength.is_some(),
                |d, on| d.extensions.emissive_strength = on.then(<_>::default));
            if let Some(x) = e.emissive_strength {
                sec = sec.child(builtin_num_row(&mat, "  Strength", x.strength as f64, 0.0, 100.0, 0.1,
                    |d, v| { if let Some(ref mut a) = d.extensions.emissive_strength { a.strength = v as f32; } }));
            }
            toggle!("IOR", e.ior.is_some(), |d, on| d.extensions.ior = on.then(<_>::default));
            if let Some(x) = e.ior {
                sec = sec.child(builtin_num_row(&mat, "  Index", x.ior as f64, 1.0, 3.0, 0.01,
                    |d, v| { if let Some(ref mut a) = d.extensions.ior { a.ior = v as f32; } }));
            }
            toggle!("Specular", e.specular.is_some(), |d, on| d.extensions.specular = on.then(<_>::default));
            if let Some(x) = e.specular {
                sec = sec.child(builtin_num_row(&mat, "  Factor", x.factor as f64, 0.0, 1.0, 0.01,
                    |d, v| { if let Some(ref mut a) = d.extensions.specular { a.factor = v as f32; } }));
            }
            toggle!("Transmission", e.transmission.is_some(), |d, on| d.extensions.transmission = on.then(<_>::default));
            if let Some(x) = e.transmission {
                sec = sec.child(builtin_num_row(&mat, "  Factor", x.factor as f64, 0.0, 1.0, 0.01,
                    |d, v| { if let Some(ref mut a) = d.extensions.transmission { a.factor = v as f32; } }));
            }
            toggle!("Diffuse transmission", e.diffuse_transmission.is_some(), |d, on| d.extensions.diffuse_transmission = on.then(<_>::default));
            if let Some(x) = e.diffuse_transmission {
                sec = sec.child(builtin_num_row(&mat, "  Factor", x.factor as f64, 0.0, 1.0, 0.01,
                    |d, v| { if let Some(ref mut a) = d.extensions.diffuse_transmission { a.factor = v as f32; } }));
            }
            toggle!("Volume", e.volume.is_some(), |d, on| d.extensions.volume = on.then(<_>::default));
            if let Some(x) = e.volume {
                sec = sec.child(builtin_num_row(&mat, "  Thickness", x.thickness_factor as f64, 0.0, 10.0, 0.05,
                    |d, v| { if let Some(ref mut a) = d.extensions.volume { a.thickness_factor = v as f32; } }));
                sec = sec.child(builtin_num_row(&mat, "  Attenuation dist", x.attenuation_distance as f64, 0.0, 100.0, 0.1,
                    |d, v| { if let Some(ref mut a) = d.extensions.volume { a.attenuation_distance = v as f32; } }));
            }
            toggle!("Clearcoat", e.clearcoat.is_some(), |d, on| d.extensions.clearcoat = on.then(<_>::default));
            if let Some(x) = e.clearcoat {
                sec = sec.child(builtin_num_row(&mat, "  Factor", x.factor as f64, 0.0, 1.0, 0.01,
                    |d, v| { if let Some(ref mut a) = d.extensions.clearcoat { a.factor = v as f32; } }));
                sec = sec.child(builtin_num_row(&mat, "  Roughness", x.roughness_factor as f64, 0.0, 1.0, 0.01,
                    |d, v| { if let Some(ref mut a) = d.extensions.clearcoat { a.roughness_factor = v as f32; } }));
            }
            toggle!("Sheen", e.sheen.is_some(), |d, on| d.extensions.sheen = on.then(<_>::default));
            if let Some(x) = e.sheen {
                sec = sec.child(builtin_num_row(&mat, "  Roughness", x.roughness_factor as f64, 0.0, 1.0, 0.01,
                    |d, v| { if let Some(ref mut a) = d.extensions.sheen { a.roughness_factor = v as f32; } }));
            }
            toggle!("Dispersion", e.dispersion.is_some(), |d, on| d.extensions.dispersion = on.then(<_>::default));
            if let Some(x) = e.dispersion {
                sec = sec.child(builtin_num_row(&mat, "  Amount", x.dispersion as f64, 0.0, 1.0, 0.005,
                    |d, v| { if let Some(ref mut a) = d.extensions.dispersion { a.dispersion = v as f32; } }));
            }
            toggle!("Anisotropy", e.anisotropy.is_some(), |d, on| d.extensions.anisotropy = on.then(<_>::default));
            if let Some(x) = e.anisotropy {
                sec = sec.child(builtin_num_row(&mat, "  Strength", x.strength as f64, 0.0, 1.0, 0.01,
                    |d, v| { if let Some(ref mut a) = d.extensions.anisotropy { a.strength = v as f32; } }));
                sec = sec.child(builtin_num_row(&mat, "  Rotation", x.rotation as f64, 0.0, std::f64::consts::TAU, 0.01,
                    |d, v| { if let Some(ref mut a) = d.extensions.anisotropy { a.rotation = v as f32; } }));
            }
            toggle!("Iridescence", e.iridescence.is_some(), |d, on| d.extensions.iridescence = on.then(<_>::default));
            if let Some(x) = e.iridescence {
                sec = sec.child(builtin_num_row(&mat, "  Factor", x.factor as f64, 0.0, 1.0, 0.01,
                    |d, v| { if let Some(ref mut a) = d.extensions.iridescence { a.factor = v as f32; } }));
                sec = sec.child(builtin_num_row(&mat, "  IOR", x.ior as f64, 1.0, 3.0, 0.01,
                    |d, v| { if let Some(ref mut a) = d.extensions.iridescence { a.ior = v as f32; } }));
            }
            Some(sec.render())
        })))
    })
}

/// The Definition rail for a **built-in** material: its shared variant settings
/// (shading type + alpha / double-sided / vertex-colors). Uniform values + texture
/// bindings are set per-mesh, so they don't appear here.
fn builtin_definition(mat: &Arc<CustomMaterial>) -> Dom {
    use awsm_scene_schema::{MaterialAlphaMode, MaterialShading};
    let def = mat.builtin.get_cloned().unwrap_or_default();
    let shading_label = match def.shading {
        MaterialShading::Pbr => "PBR (physically based)",
        MaterialShading::Unlit => "Unlit (emissive only)",
        MaterialShading::Toon { .. } => "Toon (cel-shaded)",
    };
    // Alpha mode select.
    let alpha = Mutable::new(
        match def.alpha_mode {
            MaterialAlphaMode::Opaque => "opaque",
            MaterialAlphaMode::Mask { .. } => "mask",
            MaterialAlphaMode::Blend => "blend",
        }
        .to_string(),
    );
    spawn_local({
        use futures_signals::signal::SignalExt;
        let alpha = alpha.clone();
        let mat = mat.clone();
        async move {
            let mut first = true;
            alpha
                .signal_cloned()
                .for_each(move |v| {
                    let fire = !first;
                    first = false;
                    let mat = mat.clone();
                    async move {
                        if fire {
                            edit_builtin(&mat, |d| {
                                d.alpha_mode = match v.as_str() {
                                    "mask" => MaterialAlphaMode::Mask { cutoff: 0.5 },
                                    "blend" => MaterialAlphaMode::Blend,
                                    _ => MaterialAlphaMode::Opaque,
                                };
                            });
                        }
                    }
                })
                .await;
        }
    });

    html!("div", {
        .style("display", "flex").style("flex-direction", "column").style("height", "100%").style("background", "var(--bg-1)")
        .child(panel_header("Definition", None))
        .child(html!("div", {
            .style("flex", "1").style("overflow-y", "auto")
            .child(name_section(mat))
            .child(Section::new("Variant")
                .child(row("Shading", html!("span", {
                    .style("font-size", "12.5px").style("color", "var(--text-1)").text(shading_label)
                })))
                .child(row("Alpha", select(alpha, vec![
                    ("opaque".into(), "Opaque".into()),
                    ("mask".into(), "Mask".into()),
                    ("blend".into(), "Blend".into()),
                ])))
                .child(builtin_toggle_row(mat, "Double-sided", def.double_sided, |d, on| d.double_sided = on))
                .child(builtin_toggle_row(mat, "Vertex colors", def.vertex_colors_enabled, |d, on| d.vertex_colors_enabled = on))
                .render())
            .child(textures_section(mat, &def))
            .child(toon_section(mat, &def))
            .child(extensions_section(mat))
            .child(html!("div", {
                .style("margin", "11px 12px").style("padding", "8px 10px")
                .style("background", "var(--bg-2)").style("border", "1px solid var(--line-soft)").style("border-radius", "var(--r2)")
                .style("font-size", "11px").style("color", "var(--text-2)").style("line-height", "1.5")
                .text("This material's VARIANT (shading, alpha, double-sided, vertex colours, textures, extensions) is edited here. The per-mesh UNIFORM values (base color, metallic, roughness, emissive\u{2026}) are set in the scene inspector when this material is assigned.")
            }))
        }))
    })
}

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

    if mat.is_builtin() {
        return builtin_definition(&mat);
    }

    html!("div", {
        .style("display", "flex").style("flex-direction", "column").style("height", "100%").style("background", "var(--bg-1)")
        .child(panel_header("Definition", None))
        .child(html!("div", {
            .style("flex", "1").style("overflow-y", "auto")
            .child(name_section(&mat))
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
            .child(pass_deps_section(&mat))
        }))
    })
}

/// Editable material name. Rename is cosmetic — the renderer registry and mesh
/// assignments are keyed by the material's stable id, so renaming never breaks
/// an assigned mesh or requires re-registration.
fn name_section(mat: &Arc<CustomMaterial>) -> Dom {
    Section::new("Name")
        .dense(true)
        .child(
            TextInput::new(mat.name.clone())
                .placeholder("Material name")
                .on_change(|_| controller().dirty.set_neq(true))
                .render(),
        )
        .render()
}

/// Pass Dependencies (the v1 "skinny materials" win): declare which
/// `ShaderIncludes` + `FragmentInputs` this material's WGSL actually needs, so
/// registration compiles a leaner bucket. Default is everything (behavior-
/// preserving); unchecking pares the emitted shader down.
/// Which declared-dependency list a `dep_group` edits.
#[derive(Clone, Copy, PartialEq)]
enum DepKind {
    Includes,
    Inputs,
}

fn pass_deps_section(mat: &Arc<CustomMaterial>) -> Dom {
    use crate::controller::custom_material::{FRAGMENT_INPUT_KEYS, SHADER_INCLUDE_KEYS};
    Section::new("Pass Dependencies")
        .dense(true)
        .child(html!("div", {
            .style("font-size", "11px").style("color", "var(--text-3)").style("line-height", "1.45").style("margin-bottom", "8px")
            .text("Which shader includes + interpolants the WGSL needs. Fewer = leaner bucket.")
        }))
        .child(dep_group("Shader includes", SHADER_INCLUDE_KEYS, DepKind::Includes, mat))
        .child(html!("div", { .style("height", "8px") }))
        .child(dep_group("Fragment inputs", FRAGMENT_INPUT_KEYS, DepKind::Inputs, mat))
        .render()
}

/// Build a `SetCustomMaterial{ShaderIncludes,FragmentInputs}` command from the
/// currently-checked dep states.
fn dep_command(
    mat_id: AssetId,
    kind: DepKind,
    keys: &[&'static str],
    states: &[Mutable<bool>],
) -> EditorCommand {
    let list: Vec<String> = keys
        .iter()
        .zip(states)
        .filter(|(_, on)| on.get())
        .map(|(&k, _)| k.to_string())
        .collect();
    match kind {
        DepKind::Includes => EditorCommand::SetCustomMaterialShaderIncludes {
            id: mat_id,
            includes: list,
        },
        DepKind::Inputs => EditorCommand::SetCustomMaterialFragmentInputs {
            id: mat_id,
            inputs: list,
        },
    }
}

fn dep_group(
    title: &str,
    keys: &'static [&'static str],
    kind: DepKind,
    mat: &Arc<CustomMaterial>,
) -> Dom {
    // One bool per key (the checkbox + All/None drive these); any change rebuilds
    // the full list and dispatches it through the controller (undo + cross-tab +
    // MCP). Seeded from the material's current declared set.
    let current = match kind {
        DepKind::Includes => mat.shader_includes.get_cloned(),
        DepKind::Inputs => mat.fragment_inputs.get_cloned(),
    };
    let states: Vec<Mutable<bool>> = keys
        .iter()
        .map(|&k| Mutable::new(current.iter().any(|x| x == k)))
        .collect();
    let all = states.clone();
    let none = states.clone();
    html!("div", {
        .child(html!("div", {
            .style("display", "flex").style("align-items", "center").style("justify-content", "space-between").style("margin-bottom", "5px")
            .child(html!("div", { .class("kicker").style("font-size", "9.5px").style("text-transform", "uppercase").style("letter-spacing", ".06em").style("color", "var(--text-3)").text(title) }))
            .child(html!("div", {
                .style("display", "flex").style("gap", "10px")
                .child(dep_bulk_btn("All", move || { for s in all.iter() { s.set_neq(true); } }))
                .child(dep_bulk_btn("None", move || { for s in none.iter() { s.set_neq(false); } }))
            }))
        }))
        .children(keys.iter().zip(states.iter().cloned()).map(clone!(mat, states => move |(&key, on)| dep_row(key, on, keys, states.clone(), kind, mat.id))))
    })
}

/// A tiny inline text button ("All" / "None") for the dep-group header.
fn dep_bulk_btn(label: &str, on_click: impl Fn() + 'static) -> Dom {
    html!("button", {
        .class("focusring")
        .style("font-size", "10px").style("color", "var(--text-2)").style("cursor", "pointer")
        .style("background", "transparent").style("border-style", "none").style("padding", "0")
        .text(label)
        .event(move |_: events::Click| on_click())
    })
}

fn dep_row(
    key: &'static str,
    on: Mutable<bool>,
    keys: &'static [&'static str],
    states: Vec<Mutable<bool>>,
    kind: DepKind,
    mat_id: AssetId,
) -> Dom {
    let _ = key;
    spawn_local(clone!(on => async move {
        let mut first = true;
        on.signal().for_each(move |_checked| {
            let fire = !first; first = false;
            clone!(states => async move {
                if fire {
                    dispatch(dep_command(mat_id, kind, keys, &states));
                }
            })
        }).await;
    }));
    html!("label", {
        .style("display", "flex").style("align-items", "center").style("gap", "8px").style("padding", "2px 0").style("cursor", "pointer")
        .child(check(on))
        .child(html!("span", { .class("mono").style("font-size", "11px").style("color", "var(--text-1)").text(key) }))
    })
}

fn surface_section(mat: &Arc<CustomMaterial>) -> Dom {
    // Alpha mode segmented. Routes through the controller (undo + cross-tab + MCP).
    let alpha = Mutable::new(mat.alpha.get().key().to_string());
    spawn_local(clone!(alpha, mat => async move {
        let mut first = true;
        alpha.signal_cloned().for_each(move |k| {
            let fire = !first; first = false;
            clone!(mat => async move {
                if fire {
                    let mode = match k.as_str() {
                        "mask" => CustomAlphaMode::Mask { cutoff: mat.cutoff.get() },
                        "blend" => CustomAlphaMode::Blend,
                        _ => CustomAlphaMode::Opaque,
                    };
                    dispatch(EditorCommand::SetCustomMaterialAlphaMode { id: mat.id, mode });
                }
            })
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
                    .on_change(move |v| dispatch(EditorCommand::SetCustomMaterialAlphaMode {
                        id: m.id,
                        mode: CustomAlphaMode::Mask { cutoff: v },
                    })).render()))
            } else { None }
        })))
    }));

    // Double-sided.
    let ds = Mutable::new(mat.double_sided.get());
    spawn_local(clone!(ds, mat => async move {
        let mut first = true;
        ds.signal().for_each(move |on| {
            let fire = !first; first = false;
            clone!(mat => async move { if fire { dispatch(EditorCommand::SetCustomMaterialDoubleSided { id: mat.id, double_sided: on }); } })
        }).await;
    }));
    sec = sec.child(row("Double-sided", toggle(ds)));

    // Base color (debug).
    let col = Mutable::new(mat.color.get_cloned());
    spawn_local(clone!(col, mat => async move {
        let mut first = true;
        col.signal_cloned().for_each(move |hex| {
            let fire = !first; first = false;
            clone!(mat => async move { if fire { dispatch(EditorCommand::SetCustomMaterialDebugColor { id: mat.id, hex }); } })
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
            let mut v = slot_field_of(&mat_add, kind).get_cloned();
            let n = v.len() + 1;
            v.push(match kind {
                SlotKind::Uniform => Slot::uniform(format!("value{n}"), "f32", "0.0"),
                SlotKind::Texture => Slot::named(format!("tex{n}"), "texture_2d<f32>"),
                SlotKind::Buffer => Slot::named(format!("buf{n}"), "array<vec4<f32>>"),
            });
            dispatch_layout(&mat_add, kind, v);
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

fn to_specs(slots: &[Slot]) -> Vec<SlotSpec> {
    slots
        .iter()
        .map(|s| SlotSpec {
            name: s.name.clone(),
            ty: s.ty.clone(),
            val: s.val.clone(),
            debug: s.debug.clone(),
        })
        .collect()
}

/// Dispatch a layout edit through the controller: takes the new `slots` for one
/// kind and reads the other two off the live material, sending the full layout.
fn dispatch_layout(mat: &Arc<CustomMaterial>, kind: SlotKind, slots: Vec<Slot>) {
    let uniforms = if kind == SlotKind::Uniform {
        to_specs(&slots)
    } else {
        to_specs(&mat.uniforms.get_cloned())
    };
    let textures = if kind == SlotKind::Texture {
        to_specs(&slots)
    } else {
        to_specs(&mat.textures.get_cloned())
    };
    let buffers = if kind == SlotKind::Buffer {
        to_specs(&slots)
    } else {
        to_specs(&mat.buffers.get_cloned())
    };
    dispatch(EditorCommand::SetCustomMaterialLayout {
        id: mat.id,
        uniforms,
        textures,
        buffers,
    });
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
                    if let Some(s) = arr.get_mut(i) { s.name = v; dispatch_layout(&m_name, kind, arr); }
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
                    if i < arr.len() { arr.remove(i); dispatch_layout(&m_rm, kind, arr); }
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
                    if let Some(s) = arr.get_mut(i) { s.ty = ty; dispatch_layout(&m, SlotKind::Uniform, arr); }
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
    // Built-in materials have no shader graph — the whole authoring area is an
    // informational panel; their look is set per-mesh + via the variant rail.
    if mat.is_builtin() {
        return html!("div", {
            .style("height", "100%").style("display", "flex").style("flex-direction", "column")
            .style("align-items", "center").style("justify-content", "center").style("gap", "12px").style("padding", "24px")
            .style("text-align", "center")
            .child(Icon::new("material").size(40.0).color("var(--text-3)").render())
            .child(html!("div", {
                .style("font-size", "15px").style("font-weight", "600").style("color", "var(--text-1)")
                .text("Built-in materials can't be edited here")
            }))
            .child(html!("div", {
                .style("font-size", "12.5px").style("color", "var(--text-3)").style("line-height", "1.6").style("max-width", "420px")
                .text("Built-ins have no shader code or texture slots to author. Change their shared variant settings in the Definition rail on the left; set per-mesh colors and values in the scene inspector when this material is assigned. Rename, duplicate, and assign work just like dynamic materials.")
            }))
        });
    }
    html!("div", {
        .style("display", "flex").style("flex-direction", "column").style("height", "100%").style("min-width", "0").style("min-height", "0")
        .child(html!("div", { .style("flex", "1 1 56%").style("min-height", "0").style("border-bottom", "1px solid var(--line)").child(preview_pane(&mat)) }))
        .child(html!("div", { .style("flex", "1 1 44%").style("min-height", "0").child(code_pane(&mat, help)) }))
    })
}

fn preview_pane(mat: &Arc<CustomMaterial>) -> Dom {
    // Re-shade the live preview when this material's body changes — debounced
    // (250ms idle) so a half-typed shader never reaches the GPU compiler
    // mid-keystroke, and only when the WGSL is well-formed.
    let m = mat.clone();
    spawn_local(clone!(m => async move {
        let generation = std::rc::Rc::new(std::cell::Cell::new(0u64));
        let mut first = true;
        m.wgsl.signal_cloned().for_each(clone!(m, generation => move |wgsl| {
            let skip = first; first = false;
            let g = generation.get().wrapping_add(1);
            generation.set(g);
            clone!(m, generation => {
                if !skip {
                    spawn_local(async move {
                        gloo_timers::future::TimeoutFuture::new(250).await;
                        // Only the latest edit fires, and only if it compiles.
                        if generation.get() == g && crate::controller::compile_wgsl(&wgsl).is_empty() {
                            crate::engine::preview::set_material(m.clone());
                        }
                    });
                }
                async {}
            })
        })).await;
    }));

    html!("div", {
        .style("position", "relative").style("height", "100%").style("overflow", "hidden")
        .style("background", "radial-gradient(120% 120% at 50% 30%, oklch(0.26 0.01 255), oklch(0.16 0.008 255))")
        // The live 2nd-renderer preview canvas (its own device-scoped renderer).
        .child(html!("canvas" => web_sys::HtmlCanvasElement, {
            .style("width", "100%").style("height", "100%").style("display", "block")
            .after_inserted(crate::engine::preview::mount)
            .after_removed(|_| crate::engine::preview::unmount())
        }))
        .child(html!("div", {
            .style("position", "absolute").style("left", "12px").style("bottom", "10px")
            .class("mono").style("font-size", "10.5px").style("color", "var(--text-3)")
            .text("preview \u{00b7} live")
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
                // Route through the controller (coalesced into one undo step,
                // cross-tab broadcast, MCP-reachable) rather than writing the
                // reactive model directly.
                .event(clone!(ta, mat => move |_: events::Input| dispatch(EditorCommand::SetCustomMaterialWgsl { id: mat.id, wgsl: ta.value() })))
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
            // Auto-registered: compiles on edit (debounced). Shows live / errors.
            .child_signal(mat.registered.signal().map(|r| Some(if r {
                html!("span", { .style("color", "var(--ok)").text("\u{25cf} live \u{2014} compiles on edit") })
            } else {
                html!("span", { .style("color", "var(--warn)").text("\u{25cf} fix errors to go live") })
            })))
        }))
        .child(html!("div", { .style("flex", "1") }))
        .child(Btn::new().label("Assign to selection").icon("link").variant(BtnVariant::Ghost).size(BtnSize::Sm)
            .on_click(move || assign_to_selection(id)).render())
    })
}

/// Assign `material` to the primary scene selection if it's a mesh primitive.
fn assign_to_selection(material: AssetId) {
    spawn_local(async move {
        let ctrl = controller();
        let Some(node) = ctrl.selected.get_cloned().last().copied() else {
            Toast::warning("Select a mesh in the Scene to assign this material to.");
            return;
        };
        let is_mesh = crate::engine::scene::mutate::find_by_id(&ctrl.scene, node)
            .map(|n| {
                matches!(
                    n.kind.get_cloned(),
                    crate::engine::scene::NodeKind::Mesh { .. }
                )
            })
            .unwrap_or(false);
        if !is_mesh {
            Toast::warning("Select a mesh to assign this material.");
            return;
        }
        let registered =
            crate::controller::custom_material::find_material(&ctrl.custom_materials, material)
                .map(|m| m.registered.get())
                .unwrap_or(false);
        if !registered {
            Toast::warning("Register the material before assigning it.");
            return;
        }
        let _ = ctrl
            .dispatch(EditorCommand::AssignMaterial {
                node,
                material: Some(material),
            })
            .await;
        Toast::info("Material assigned to selection.");
    });
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

fn dispatch(cmd: EditorCommand) {
    spawn_local(async move {
        let _ = controller().dispatch(cmd).await;
    });
}
