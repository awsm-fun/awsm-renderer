// ─────────────────────────────────────────────────────────────────────
// Inline-material editor — shared by every NodeKind variant that
// carries a `MaterialDef`. Renders the standard PBR knob set
// (base color / metallic / roughness / emissive / double-sided /
// vertex-colors-enabled / shading-model). Each row mutates the
// authored `MaterialDef` via a per-field setter closure so callers can
// route the writes back into whichever sub-field of `NodeKind` they
// live in (`Primitive::inline_material`, `SweepAlongCurve::inline_material`,
// etc.).
//
// First-class `AssetSource::Material(MaterialDef)` editing (the asset-
// table flow) hangs off the same rendering function: the asset inspector
// can read/write the `MaterialDef` directly out of the `AssetTable` by
// supplying its own get/set closures.

use crate::prelude::*;
use crate::properties::transform::number_input;
use crate::scene::{Node, NodeKind};
use crate::state::app_state;
use awsm_scene_schema::{AssetSource, MaterialDef, MaterialRef, MaterialShading};

use super::{field_row, section_header};

/// Render the editor that mutates whichever `MaterialDef` is currently
/// in play for the given node: when the kind's `Option<MaterialRef>` is
/// `Some`, edits route through the asset-table entry's `MaterialDef`;
/// when it's `None`, edits route through `inline_material`. The header
/// label flips accordingly so the user always knows what they're editing.
///
/// This satisfies the D-1c "first-class asset-table flow" — pick a
/// Material asset from the dropdown and the inspector swaps to editing
/// the shared `MaterialDef`. Switching back to `(inline material)`
/// flips it back to the per-node inline def.
pub fn render_material_for_node(
    node: Arc<Node>,
    read_ref: fn(&NodeKind) -> Option<MaterialRef>,
    extract_inline: fn(&NodeKind) -> Option<&MaterialDef>,
    apply_inline: fn(&mut NodeKind, MaterialDef),
) -> Dom {
    // We dedupe on the *Option<MaterialRef> tag* so flipping the picker
    // rebuilds the editor; tweaking knobs within the same target does
    // not (the inputs keep pointer capture mid-drag).
    let kind = node.kind.clone();
    let target_signal = kind.signal_ref(read_ref).dedupe();
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.5rem")
        .child_signal(target_signal.map(clone!(node => move |maybe_ref| {
            Some(match maybe_ref {
                Some(material_ref) => render_asset_material(material_ref),
                None => render_inline_material(node.clone(), extract_inline, apply_inline),
            })
        })))
    })
}

/// Edit the `MaterialDef` stored in the asset table behind a
/// `MaterialRef`. Bumps `scene.revision` on every edit so the bridge
/// re-reads the def (and the material_cache invalidates the cached
/// `MaterialKey` on the next materialize).
///
/// `pub(crate)` so the Assets panel (`properties::asset_editor`) can
/// reuse the same editor when the user picks a Material asset directly,
/// not via a node selection.
pub(crate) fn render_asset_material(material_ref: MaterialRef) -> Dom {
    let scene = app_state().scene.clone();
    // Header signal — show the picked asset id prefix so the user knows
    // they're editing the shared asset and not the inline material.
    let label = format!(
        "Material asset {} (shared)",
        &material_ref.0 .0.to_string()[..8]
    );
    let revision = scene.revision.clone();

    // Helper closures — read/write the asset's MaterialDef via the
    // scene's asset table. Edits invalidate the renderer-side material
    // cache so the next frame rebuilds the `MaterialKey`.
    let read_def = clone!(scene => move || -> MaterialDef {
        let table = scene.assets.lock().unwrap();
        match table.get(material_ref.0).map(|e| &e.source) {
            Some(AssetSource::Material(def)) => def.clone(),
            _ => MaterialDef::default(),
        }
    });
    // Pure mutator — no history snapshot. The input widgets that need
    // coalesced history (label / color via `history_input`, scalars
    // via `number_input`) handle their own FocusIn/FocusOut around it.
    // Single-event widgets (checkbox / select) call `write_def_committing`
    // below which wraps this in a snapshot+commit.
    let write_def = clone!(scene => move |def: MaterialDef| {
        {
            let mut table = scene.assets.lock().unwrap();
            if let Some(entry) = table.entries.get_mut(&material_ref.0) {
                if let AssetSource::Material(slot) = &mut entry.source {
                    *slot = def;
                }
            }
        }
        scene.bump_revision();
        // Push the new params into the renderer's existing MaterialKey
        // so meshes already bound to this shared material pick up the
        // edit on the next frame. Cheaper than re-materializing every
        // referencing node.
        wasm_bindgen_futures::spawn_local(async move {
            crate::context::with_renderer_mut(move |r| {
                let scene = app_state().scene.clone();
                crate::renderer_bridge::material_cache::update_existing(r, &scene, material_ref);
            }).await;
        });
    });
    // Snapshot+commit wrapper for inputs that fire a single event
    // per user action (checkbox, select). One history entry per click.
    let write_def_committing = clone!(write_def => move |def: MaterialDef| {
        let state = app_state();
        let previous = state.snapshot_scene();
        write_def(def);
        state.commit_history(previous);
    });

    // Each row is a thin signal-driven input bound to `revision` so
    // external edits (e.g. `Save → Load`) trigger a re-render. Within
    // a drag the read happens once and the write fires the revision
    // bump, so the input doesn't get torn down mid-drag.
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.5rem")
        .child(section_header(&label))
        .child(field_row("Label", asset_label_input(read_def.clone(), write_def.clone(), revision.signal())))
        .child(field_row("Base color", asset_color_input(read_def.clone(), write_def.clone(), revision.clone())))
        .child(field_row("Metallic", asset_scalar_input(read_def.clone(), write_def.clone(), revision.signal(), MaterialField::Metallic)))
        .child(field_row("Roughness", asset_scalar_input(read_def.clone(), write_def.clone(), revision.signal(), MaterialField::Roughness)))
        .child(field_row("Emissive R", asset_scalar_input(read_def.clone(), write_def.clone(), revision.signal(), MaterialField::EmissiveR)))
        .child(field_row("Emissive G", asset_scalar_input(read_def.clone(), write_def.clone(), revision.signal(), MaterialField::EmissiveG)))
        .child(field_row("Emissive B", asset_scalar_input(read_def.clone(), write_def.clone(), revision.signal(), MaterialField::EmissiveB)))
        .child(field_row("Double-sided", asset_bool_input(read_def.clone(), write_def_committing.clone(), revision.signal(), MaterialBool::DoubleSided)))
        .child(field_row("Vertex colors", asset_bool_input(read_def.clone(), write_def_committing.clone(), revision.signal(), MaterialBool::VertexColorsEnabled)))
        .child(field_row("Shading", asset_shading_select(read_def.clone(), write_def_committing, revision.signal())))
        // Toon-only param rows. Dedupe on the shading variant tag so
        // swapping pbr/unlit/toon rebuilds the rows but tweaking knobs
        // within Toon doesn't tear the inputs down mid-drag.
        .child(asset_toon_rows(read_def, write_def, revision.signal()))
    })
}

/// Render `Diffuse bands` + `Rim strength` rows when the current shading
/// is `Toon`; nothing otherwise. Dedupes on a "is-toon" bool so the
/// child rebuild only fires when the variant flips.
fn asset_toon_rows(
    read_def: impl Fn() -> MaterialDef + Clone + 'static,
    write_def: impl Fn(MaterialDef) + Clone + 'static,
    revision: impl futures_signals::signal::Signal<Item = u64> + 'static,
) -> Dom {
    use futures_signals::signal::SignalExt;
    let read_tag = read_def.clone();
    let is_toon = revision
        .map(move |_| matches!(read_tag().shading, MaterialShading::Toon { .. }))
        .dedupe();
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.5rem")
        .child_signal(is_toon.map(clone!(read_def, write_def => move |on| {
            if !on {
                return None;
            }
            // Inputs are revision-driven so external edits (Save/Load)
            // re-render the values, mirroring every other asset row.
            let revision = app_state().scene.revision.clone();
            Some(html!("div", {
                .style("display", "flex")
                .style("flex-direction", "column")
                .style("gap", "0.5rem")
                .child(field_row("Diffuse bands", asset_toon_bands_input(read_def.clone(), write_def.clone(), revision.signal())))
                .child(field_row("Rim strength", asset_toon_rim_input(read_def.clone(), write_def.clone(), revision.signal())))
            }))
        })))
    })
}

fn asset_toon_bands_input(
    read_def: impl Fn() -> MaterialDef + Clone + 'static,
    write_def: impl Fn(MaterialDef) + Clone + 'static,
    revision: impl futures_signals::signal::Signal<Item = u64> + 'static,
) -> Dom {
    use futures_signals::signal::SignalExt;
    let read_for_signal = read_def.clone();
    let value_signal = revision.map(move |_| match read_for_signal().shading {
        MaterialShading::Toon { diffuse_bands, .. } => diffuse_bands as f32,
        _ => 4.0,
    });
    number_input(value_signal, move |new_value| {
        let mut def = read_def();
        let bands = new_value.round().clamp(1.0, 16.0) as u32;
        let rim = match def.shading {
            MaterialShading::Toon { rim_strength, .. } => rim_strength,
            _ => 0.6,
        };
        def.shading = MaterialShading::Toon {
            diffuse_bands: bands,
            rim_strength: rim,
        };
        write_def(def);
    })
}

fn asset_toon_rim_input(
    read_def: impl Fn() -> MaterialDef + Clone + 'static,
    write_def: impl Fn(MaterialDef) + Clone + 'static,
    revision: impl futures_signals::signal::Signal<Item = u64> + 'static,
) -> Dom {
    use futures_signals::signal::SignalExt;
    let read_for_signal = read_def.clone();
    let value_signal = revision.map(move |_| match read_for_signal().shading {
        MaterialShading::Toon { rim_strength, .. } => rim_strength,
        _ => 0.6,
    });
    number_input(value_signal, move |new_value| {
        let mut def = read_def();
        let bands = match def.shading {
            MaterialShading::Toon { diffuse_bands, .. } => diffuse_bands,
            _ => 4,
        };
        def.shading = MaterialShading::Toon {
            diffuse_bands: bands,
            rim_strength: new_value.clamp(0.0, 4.0),
        };
        write_def(def);
    })
}

fn asset_label_input(
    read_def: impl Fn() -> MaterialDef + Clone + 'static,
    write_def: impl Fn(MaterialDef) + Clone + 'static,
    revision: impl futures_signals::signal::Signal<Item = u64> + 'static,
) -> Dom {
    let read_for_text = read_def.clone();
    let read_label = move || read_for_text().label.clone();
    let write_label = move |new_label: String| {
        let mut def = read_def();
        def.label = new_label;
        write_def(def);
    };
    crate::properties::history_input::text_input(read_label, write_label, revision)
}

fn asset_scalar_input(
    read_def: impl Fn() -> MaterialDef + Clone + 'static,
    write_def: impl Fn(MaterialDef) + Clone + 'static,
    revision: impl futures_signals::signal::Signal<Item = u64> + 'static,
    field: MaterialField,
) -> Dom {
    use futures_signals::signal::SignalExt;
    let read_for_signal = read_def.clone();
    let value_signal = revision.map(move |_| {
        let def = read_for_signal();
        match field {
            MaterialField::Metallic => def.metallic,
            MaterialField::Roughness => def.roughness,
            MaterialField::EmissiveR => def.emissive[0],
            MaterialField::EmissiveG => def.emissive[1],
            MaterialField::EmissiveB => def.emissive[2],
        }
    });
    number_input(value_signal, move |new_value| {
        let mut def = read_def();
        match field {
            MaterialField::Metallic => def.metallic = new_value.clamp(0.0, 1.0),
            MaterialField::Roughness => def.roughness = new_value.clamp(0.0, 1.0),
            MaterialField::EmissiveR => def.emissive[0] = new_value.max(0.0),
            MaterialField::EmissiveG => def.emissive[1] = new_value.max(0.0),
            MaterialField::EmissiveB => def.emissive[2] = new_value.max(0.0),
        }
        write_def(def);
    })
}

fn asset_bool_input(
    read_def: impl Fn() -> MaterialDef + Clone + 'static,
    write_def: impl Fn(MaterialDef) + Clone + 'static,
    revision: impl futures_signals::signal::Signal<Item = u64> + 'static,
    field: MaterialBool,
) -> Dom {
    use futures_signals::signal::SignalExt;
    let read_for_signal = read_def.clone();
    html!("input" => web_sys::HtmlInputElement, {
        .attr("type", "checkbox")
        .style("cursor", "pointer")
        .with_node!(input => {
            .future(clone!(input => {
                revision.for_each(move |_| {
                    let def = read_for_signal();
                    let value = match field {
                        MaterialBool::DoubleSided => def.double_sided,
                        MaterialBool::VertexColorsEnabled => def.vertex_colors_enabled,
                    };
                    if input.checked() != value {
                        input.set_checked(value);
                    }
                    async {}
                })
            }))
            .event(clone!(input => move |_: events::Change| {
                let mut def = read_def();
                match field {
                    MaterialBool::DoubleSided => def.double_sided = input.checked(),
                    MaterialBool::VertexColorsEnabled => def.vertex_colors_enabled = input.checked(),
                }
                write_def(def);
            }))
        })
    })
}

/// Color (RGB) picker + a sibling alpha `number_input`. The HTML
/// `<input type="color">` is RGB-only, so alpha lives in its own input
/// — clamped to 0..=1 on commit and preserved across color edits.
///
/// Takes the `revision` Mutable directly (instead of one signal) so it
/// can spawn two independent `signal()`s — one for the picker, one for
/// the alpha input. `MutableSignal` itself isn't `Clone`.
fn asset_color_input(
    read_def: impl Fn() -> MaterialDef + Clone + 'static,
    write_def: impl Fn(MaterialDef) + Clone + 'static,
    revision: Mutable<u64>,
) -> Dom {
    use futures_signals::signal::SignalExt;
    // Picker: focus-aware history via `history_input::color_input`.
    // One drag (mouse-down → release) commits one history entry.
    let read_hex_def = read_def.clone();
    let read_hex = move || {
        let rgba = read_hex_def().base_color;
        format!(
            "#{:02x}{:02x}{:02x}",
            (rgba[0] * 255.0).round() as u8,
            (rgba[1] * 255.0).round() as u8,
            (rgba[2] * 255.0).round() as u8,
        )
    };
    let write_hex_read = read_def.clone();
    let write_hex_write = write_def.clone();
    let write_hex = move |hex: String| {
        let parse = |s: &str| u8::from_str_radix(s, 16).ok().map(|v| v as f32 / 255.0);
        let (Some(r), Some(g), Some(b)) = (
            parse(hex.get(1..3).unwrap_or("ff")),
            parse(hex.get(3..5).unwrap_or("ff")),
            parse(hex.get(5..7).unwrap_or("ff")),
        ) else {
            return;
        };
        let mut def = write_hex_read();
        let a = def.base_color[3];
        def.base_color = [r, g, b, a];
        write_hex_write(def);
    };
    let picker =
        crate::properties::history_input::color_input(read_hex, write_hex, revision.signal());

    // Alpha: number_input already handles its own FocusIn/FocusOut
    // history snapshot, so the `write_def` closure is the pure mutator.
    let read_for_alpha = read_def.clone();
    let alpha_signal = revision
        .signal()
        .map(move |_| read_for_alpha().base_color[3]);
    let alpha = html!("div", {
        .style("flex", "1")
        .style("min-width", "0")
        .child(number_input(alpha_signal, move |new_value| {
            let mut def = read_def();
            def.base_color[3] = new_value.clamp(0.0, 1.0);
            write_def(def);
        }))
    });

    html!("div", {
        .style("display", "flex")
        .style("align-items", "center")
        .style("gap", "0.5rem")
        .style("width", "100%")
        .child(picker)
        .child(alpha)
    })
}

fn asset_shading_select(
    read_def: impl Fn() -> MaterialDef + Clone + 'static,
    write_def: impl Fn(MaterialDef) + Clone + 'static,
    revision: impl futures_signals::signal::Signal<Item = u64> + 'static,
) -> Dom {
    use futures_signals::signal::SignalExt;
    let read_for_signal = read_def.clone();
    html!("select" => web_sys::HtmlSelectElement, {
        .style("background-color", ColorRaw::Darkest.value())
        .style("color", ColorText::SidebarHeader.value())
        .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .style("border-radius", "0.25rem")
        .style("padding", "0.25rem 0.4rem")
        .style("cursor", "pointer")
        .child(html!("option", { .attr("value", "pbr").text("PBR") }))
        .child(html!("option", { .attr("value", "unlit").text("Unlit") }))
        .child(html!("option", { .attr("value", "toon").text("Toon") }))
        .with_node!(select => {
            .future(clone!(select => {
                revision.for_each(move |_| {
                    let cur = read_for_signal().shading;
                    let v = match cur {
                        MaterialShading::Pbr => "pbr",
                        MaterialShading::Unlit => "unlit",
                        MaterialShading::Toon { .. } => "toon",
                    };
                    if select.value() != v {
                        select.set_value(v);
                    }
                    async {}
                })
            }))
            .event(clone!(select => move |_: events::Change| {
                let new_shading = match select.value().as_str() {
                    "unlit" => MaterialShading::Unlit,
                    "toon" => MaterialShading::Toon { diffuse_bands: 4, rim_strength: 0.6 },
                    _ => MaterialShading::Pbr,
                };
                let mut def = read_def();
                def.shading = new_shading;
                write_def(def);
            }))
        })
    })
}

/// Render the inline-material knob set for a node that owns a
/// `MaterialDef`. `extract` reads the current `MaterialDef` out of the
/// node's `NodeKind`; `apply` writes a mutated copy back into the
/// node's kind. Both closures return / accept references so the helper
/// stays variant-agnostic.
pub fn render_inline_material(
    node: Arc<Node>,
    extract: fn(&NodeKind) -> Option<&MaterialDef>,
    apply: fn(&mut NodeKind, MaterialDef),
) -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.5rem")
        .child(section_header("Material"))
        .child(field_row("Base color", color_input(node.clone(), extract, apply)))
        .child(field_row("Metallic", scalar_input(node.clone(), extract, apply, MaterialField::Metallic)))
        .child(field_row("Roughness", scalar_input(node.clone(), extract, apply, MaterialField::Roughness)))
        .child(field_row("Emissive R", scalar_input(node.clone(), extract, apply, MaterialField::EmissiveR)))
        .child(field_row("Emissive G", scalar_input(node.clone(), extract, apply, MaterialField::EmissiveG)))
        .child(field_row("Emissive B", scalar_input(node.clone(), extract, apply, MaterialField::EmissiveB)))
        .child(field_row("Double-sided", bool_input(node.clone(), extract, apply, MaterialBool::DoubleSided)))
        .child(field_row("Vertex colors", bool_input(node.clone(), extract, apply, MaterialBool::VertexColorsEnabled)))
        .child(field_row("Shading", shading_select(node.clone(), extract, apply)))
        // Toon-only param rows. Dedupe on a `is-toon` bool so flipping
        // pbr/unlit/toon rebuilds the rows but a drag inside Toon's
        // params doesn't tear the inputs down mid-edit.
        .child(inline_toon_rows(node, extract, apply))
    })
}

fn inline_toon_rows(
    node: Arc<Node>,
    extract: fn(&NodeKind) -> Option<&MaterialDef>,
    apply: fn(&mut NodeKind, MaterialDef),
) -> Dom {
    let kind = node.kind.clone();
    let is_toon = kind
        .signal_ref(move |k| {
            extract(k)
                .map(|m| matches!(m.shading, MaterialShading::Toon { .. }))
                .unwrap_or(false)
        })
        .dedupe();
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.5rem")
        .child_signal(is_toon.map(clone!(node => move |on| {
            if !on {
                return None;
            }
            Some(html!("div", {
                .style("display", "flex")
                .style("flex-direction", "column")
                .style("gap", "0.5rem")
                .child(field_row("Diffuse bands", inline_toon_bands_input(node.clone(), extract, apply)))
                .child(field_row("Rim strength", inline_toon_rim_input(node.clone(), extract, apply)))
            }))
        })))
    })
}

fn inline_toon_bands_input(
    node: Arc<Node>,
    extract: fn(&NodeKind) -> Option<&MaterialDef>,
    apply: fn(&mut NodeKind, MaterialDef),
) -> Dom {
    let kind = node.kind.clone();
    let value_signal = kind.signal_cloned().map(move |k| {
        extract(&k)
            .map(|m| match m.shading {
                MaterialShading::Toon { diffuse_bands, .. } => diffuse_bands as f32,
                _ => 4.0,
            })
            .unwrap_or(4.0)
    });
    number_input(value_signal, move |new_value| {
        let mut k = kind.get_cloned();
        let mut def = extract(&k).cloned().unwrap_or_default();
        let bands = new_value.round().clamp(1.0, 16.0) as u32;
        let rim = match def.shading {
            MaterialShading::Toon { rim_strength, .. } => rim_strength,
            _ => 0.6,
        };
        def.shading = MaterialShading::Toon {
            diffuse_bands: bands,
            rim_strength: rim,
        };
        apply(&mut k, def);
        kind.set(k);
    })
}

fn inline_toon_rim_input(
    node: Arc<Node>,
    extract: fn(&NodeKind) -> Option<&MaterialDef>,
    apply: fn(&mut NodeKind, MaterialDef),
) -> Dom {
    let kind = node.kind.clone();
    let value_signal = kind.signal_cloned().map(move |k| {
        extract(&k)
            .map(|m| match m.shading {
                MaterialShading::Toon { rim_strength, .. } => rim_strength,
                _ => 0.6,
            })
            .unwrap_or(0.6)
    });
    number_input(value_signal, move |new_value| {
        let mut k = kind.get_cloned();
        let mut def = extract(&k).cloned().unwrap_or_default();
        let bands = match def.shading {
            MaterialShading::Toon { diffuse_bands, .. } => diffuse_bands,
            _ => 4,
        };
        def.shading = MaterialShading::Toon {
            diffuse_bands: bands,
            rim_strength: new_value.clamp(0.0, 4.0),
        };
        apply(&mut k, def);
        kind.set(k);
    })
}

#[derive(Clone, Copy)]
enum MaterialField {
    Metallic,
    Roughness,
    EmissiveR,
    EmissiveG,
    EmissiveB,
}

#[derive(Clone, Copy)]
enum MaterialBool {
    DoubleSided,
    VertexColorsEnabled,
}

fn scalar_input(
    node: Arc<Node>,
    extract: fn(&NodeKind) -> Option<&MaterialDef>,
    apply: fn(&mut NodeKind, MaterialDef),
    field: MaterialField,
) -> Dom {
    let kind = node.kind.clone();
    let value_signal = kind.signal_cloned().map(move |k| {
        extract(&k)
            .map(|m| match field {
                MaterialField::Metallic => m.metallic,
                MaterialField::Roughness => m.roughness,
                MaterialField::EmissiveR => m.emissive[0],
                MaterialField::EmissiveG => m.emissive[1],
                MaterialField::EmissiveB => m.emissive[2],
            })
            .unwrap_or(0.0)
    });
    number_input(value_signal, move |new_value| {
        let mut k = kind.get_cloned();
        let mut def = extract(&k).cloned().unwrap_or_default();
        match field {
            MaterialField::Metallic => def.metallic = new_value.clamp(0.0, 1.0),
            MaterialField::Roughness => def.roughness = new_value.clamp(0.0, 1.0),
            MaterialField::EmissiveR => def.emissive[0] = new_value.max(0.0),
            MaterialField::EmissiveG => def.emissive[1] = new_value.max(0.0),
            MaterialField::EmissiveB => def.emissive[2] = new_value.max(0.0),
        }
        apply(&mut k, def);
        kind.set(k);
    })
}

fn bool_input(
    node: Arc<Node>,
    extract: fn(&NodeKind) -> Option<&MaterialDef>,
    apply: fn(&mut NodeKind, MaterialDef),
    field: MaterialBool,
) -> Dom {
    let kind = node.kind.clone();
    html!("input" => web_sys::HtmlInputElement, {
        .attr("type", "checkbox")
        .style("cursor", "pointer")
        .with_node!(input => {
            .future(clone!(kind, input => {
                kind.signal_cloned().for_each(move |k| {
                    let value = extract(&k).map(|m| match field {
                        MaterialBool::DoubleSided => m.double_sided,
                        MaterialBool::VertexColorsEnabled => m.vertex_colors_enabled,
                    }).unwrap_or(false);
                    if input.checked() != value {
                        input.set_checked(value);
                    }
                    async {}
                })
            }))
            .event(clone!(kind, input => move |_: events::Change| {
                let mut k = kind.get_cloned();
                let mut def = extract(&k).cloned().unwrap_or_default();
                match field {
                    MaterialBool::DoubleSided => def.double_sided = input.checked(),
                    MaterialBool::VertexColorsEnabled => def.vertex_colors_enabled = input.checked(),
                }
                apply(&mut k, def);
                kind.set(k);
            }))
        })
    })
}

/// Color (RGB) picker + a sibling alpha `number_input`. The HTML
/// `<input type="color">` is RGB-only, so alpha lives in its own input
/// — clamped to 0..=1 on commit and preserved across color edits.
fn color_input(
    node: Arc<Node>,
    extract: fn(&NodeKind) -> Option<&MaterialDef>,
    apply: fn(&mut NodeKind, MaterialDef),
) -> Dom {
    let kind = node.kind.clone();
    let kind_for_picker = kind.clone();
    let picker = html!("input" => web_sys::HtmlInputElement, {
        .attr("type", "color")
        .style("cursor", "pointer")
        .style("width", "3rem")
        .with_node!(input => {
            .future(clone!(kind_for_picker, input => {
                kind_for_picker.signal_cloned().for_each(move |k| {
                    let rgba = extract(&k).map(|m| m.base_color).unwrap_or([1.0; 4]);
                    let hex = format!(
                        "#{:02x}{:02x}{:02x}",
                        (rgba[0] * 255.0).round() as u8,
                        (rgba[1] * 255.0).round() as u8,
                        (rgba[2] * 255.0).round() as u8,
                    );
                    if input.value() != hex {
                        input.set_value(&hex);
                    }
                    async {}
                })
            }))
            .event(clone!(kind_for_picker, input => move |_: events::Input| {
                let hex = input.value();
                let parse = |s: &str| u8::from_str_radix(s, 16).ok().map(|v| v as f32 / 255.0);
                let (Some(r), Some(g), Some(b)) = (
                    parse(hex.get(1..3).unwrap_or("ff")),
                    parse(hex.get(3..5).unwrap_or("ff")),
                    parse(hex.get(5..7).unwrap_or("ff")),
                ) else { return; };
                let mut k = kind_for_picker.get_cloned();
                let mut def = extract(&k).cloned().unwrap_or_default();
                let a = def.base_color[3];
                def.base_color = [r, g, b, a];
                apply(&mut k, def);
                kind_for_picker.set(k);
            }))
        })
    });

    let alpha_signal = kind
        .signal_cloned()
        .map(move |k| extract(&k).map(|m| m.base_color[3]).unwrap_or(1.0));
    let kind_for_alpha = kind.clone();
    let alpha = html!("div", {
        .style("flex", "1")
        .style("min-width", "0")
        .child(number_input(alpha_signal, move |new_value| {
            let mut k = kind_for_alpha.get_cloned();
            let mut def = extract(&k).cloned().unwrap_or_default();
            def.base_color[3] = new_value.clamp(0.0, 1.0);
            apply(&mut k, def);
            kind_for_alpha.set(k);
        }))
    });

    html!("div", {
        .style("display", "flex")
        .style("align-items", "center")
        .style("gap", "0.5rem")
        .style("width", "100%")
        .child(picker)
        .child(alpha)
    })
}

fn shading_select(
    node: Arc<Node>,
    extract: fn(&NodeKind) -> Option<&MaterialDef>,
    apply: fn(&mut NodeKind, MaterialDef),
) -> Dom {
    let kind = node.kind.clone();
    html!("select" => web_sys::HtmlSelectElement, {
        .style("background-color", ColorRaw::Darkest.value())
        .style("color", ColorText::SidebarHeader.value())
        .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .style("border-radius", "0.25rem")
        .style("padding", "0.25rem 0.4rem")
        .style("cursor", "pointer")
        .child(html!("option", { .attr("value", "pbr").text("PBR") }))
        .child(html!("option", { .attr("value", "unlit").text("Unlit") }))
        .child(html!("option", { .attr("value", "toon").text("Toon") }))
        .with_node!(select => {
            .future(clone!(kind, select => {
                kind.signal_cloned().for_each(move |k| {
                    let cur = extract(&k).map(|m| m.shading).unwrap_or(MaterialShading::Pbr);
                    let v = match cur {
                        MaterialShading::Pbr => "pbr",
                        MaterialShading::Unlit => "unlit",
                        MaterialShading::Toon { .. } => "toon",
                    };
                    if select.value() != v {
                        select.set_value(v);
                    }
                    async {}
                })
            }))
            .event(clone!(kind, select => move |_: events::Change| {
                let new_shading = match select.value().as_str() {
                    "unlit" => MaterialShading::Unlit,
                    "toon" => MaterialShading::Toon { diffuse_bands: 4, rim_strength: 0.6 },
                    _ => MaterialShading::Pbr,
                };
                let mut k = kind.get_cloned();
                let mut def = extract(&k).cloned().unwrap_or_default();
                def.shading = new_shading;
                apply(&mut k, def);
                kind.set(k);
            }))
        })
    })
}
