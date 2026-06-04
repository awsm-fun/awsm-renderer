//! Procedural-texture asset inspector.
//!
//! Variant picker (Checker / Gradient / Noise) + per-variant param
//! rows. Every commit cascades through
//! `texture_cache::update_existing` so material bindings + sprite /
//! particle textures pick up the new pixels on the next frame.
//!
//! File-backed `TextureDef::Raster` entries are authored via the
//! Environment tab (skybox / IBL machinery) and aren't handled here —
//! the dispatcher in `super` routes them to a placeholder.

use crate::prelude::*;
use crate::properties::transform::number_input;
use crate::properties::{history_input, kind_editor};
use crate::scene::AssetId;
use crate::state::app_state;
use awsm_meshgen::{checker_rgba, gradient_rgba, noise_rgba};
use awsm_scene_schema::{AssetSource, ProceduralTextureDef, TextureDef};
use wasm_bindgen::JsCast;

pub(super) fn render(asset_id: AssetId) -> Dom {
    let scene = app_state().scene.clone();
    let revision = scene.revision.clone();
    let read_def = clone!(scene => move || -> Option<ProceduralTextureDef> {
        let table = scene.assets.lock().unwrap();
        match table.get(asset_id).map(|e| &e.source) {
            Some(AssetSource::Texture(TextureDef::Procedural(def))) => Some(def.clone()),
            _ => None,
        }
    });
    // Pure mutator — no history snapshot. Inputs that need coalesced
    // history (color via history_input, scalars via number_input)
    // handle their own FocusIn/FocusOut. Variant select wraps via
    // `write_def_committing` below (one click = one entry).
    let write_def = clone!(scene => move |new_def: ProceduralTextureDef| {
        {
            let mut table = scene.assets.lock().unwrap();
            if let Some(entry) = table.entries.get_mut(&asset_id) {
                if let AssetSource::Texture(t) = &mut entry.source {
                    *t = TextureDef::Procedural(new_def);
                }
            }
        }
        scene.bump_revision();
        // Cascade: every Material asset / inline material / Sprite /
        // Particle bound to this texture rebinds against the new bytes.
        wasm_bindgen_futures::spawn_local(async move {
            crate::renderer_bridge::texture_cache::update_existing(asset_id).await;
        });
    });
    // Snapshot+commit wrapper for single-event widgets (variant select,
    // horizontal checkbox). One entry per click.
    let write_def_committing = clone!(write_def => move |new_def: ProceduralTextureDef| {
        let state = app_state();
        let previous = state.snapshot_scene();
        write_def(new_def);
        state.commit_history(previous);
    });

    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.5rem")
        .child(kind_editor::section_header("Procedural Texture"))
        .child(kind_editor::field_row("Variant", render_variant_select(read_def.clone(), write_def_committing.clone(), revision.signal())))
        // Live preview: regenerates the texture bytes via awsm-meshgen
        // and paints into a <canvas> whenever the asset's `revision`
        // ticks. Tiny (128x128 displayed at 96x96) but enough for the
        // user to see colors / pattern / density at a glance.
        .child_signal(revision.signal().map(clone!(read_def => move |_rev| {
            Some(render_preview_swatch(read_def()?))
        })))
        .child_signal(revision.signal().map(clone!(read_def, write_def, write_def_committing => move |_rev| {
            let def = read_def()?;
            Some(render_params(def, read_def.clone(), write_def.clone(), write_def_committing.clone()))
        })))
    })
}

/// Render the procedural texture into a small `<canvas>` preview.
/// Calls the same `awsm-meshgen::*_rgba` functions the renderer-side
/// `texture_cache::get_or_upload` uses, so what the user sees is
/// pixel-for-pixel what the renderer will bind. ~96x96 display size
/// regardless of the authored Width/Height — large textures sample
/// down naturally via the browser canvas scaling.
fn render_preview_swatch(def: ProceduralTextureDef) -> Dom {
    const PREVIEW_PX: u32 = 96;
    let (rgba, src_w, src_h) = match def {
        ProceduralTextureDef::Checker {
            width,
            height,
            cells_x,
            cells_y,
            color_a,
            color_b,
        } => (
            checker_rgba(width, height, cells_x, cells_y, color_a, color_b),
            width,
            height,
        ),
        ProceduralTextureDef::Gradient {
            width,
            height,
            color_a,
            color_b,
            horizontal,
        } => (
            gradient_rgba(width, height, color_a, color_b, horizontal),
            width,
            height,
        ),
        ProceduralTextureDef::Noise {
            width,
            height,
            seed,
            scale,
        } => (noise_rgba(width, height, seed, scale), width, height),
    };

    // Use the source dims for the canvas backing store so put_image_data
    // can write the full-resolution bitmap; CSS shrinks the visible
    // surface to PREVIEW_PX. `image-rendering: pixelated` keeps sharp
    // edges (the user authored a checker — they should see crisp cells).
    let canvas = html!("canvas" => web_sys::HtmlCanvasElement, {
        .attr("width", &src_w.to_string())
        .attr("height", &src_h.to_string())
        .style("width", &format!("{PREVIEW_PX}px"))
        .style("height", &format!("{PREVIEW_PX}px"))
        .style("display", "block")
        .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .style("border-radius", "0.25rem")
        .style("image-rendering", "pixelated")
        .with_node!(canvas => {
            .apply(|builder| {
                paint_preview(&canvas, &rgba, src_w, src_h);
                builder
            })
        })
    });

    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.25rem")
        .style("padding", "0.25rem 0")
        .child(html!("div", {
            .style("font-size", "0.75rem")
            .style("color", ColorText::Byline.value())
            .text("Preview")
        }))
        .child(canvas)
    })
}

fn paint_preview(canvas: &web_sys::HtmlCanvasElement, rgba: &[u8], src_w: u32, src_h: u32) {
    let Some(ctx_obj) = canvas.get_context("2d").ok().flatten() else {
        return;
    };
    let Ok(ctx) = ctx_obj.dyn_into::<web_sys::CanvasRenderingContext2d>() else {
        return;
    };
    let Ok(image_data) = web_sys::ImageData::new_with_u8_clamped_array_and_sh(
        wasm_bindgen::Clamped(rgba),
        src_w,
        src_h,
    ) else {
        return;
    };
    let _ = ctx.put_image_data(&image_data, 0, 0);
}

fn render_variant_select(
    read_def: impl Fn() -> Option<ProceduralTextureDef> + Clone + 'static,
    write_def: impl Fn(ProceduralTextureDef) + Clone + 'static,
    revision: impl futures_signals::signal::Signal<Item = u64> + 'static,
) -> Dom {
    use futures_signals::signal::SignalExt;
    let read_for_signal = read_def.clone();
    html!("select" => web_sys::HtmlSelectElement, {
        .style("padding", "0.3rem 0.45rem")
        .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .style("border-radius", "0.25rem")
        .style("background-color", ColorRaw::Darkest.value())
        .style("color", ColorText::SidebarHeader.value())
        .style("font-size", "0.85rem")
        .style("cursor", "pointer")
        .child(html!("option", { .attr("value", "checker").text("Checker") }))
        .child(html!("option", { .attr("value", "gradient").text("Gradient") }))
        .child(html!("option", { .attr("value", "noise").text("Noise") }))
        .with_node!(select => {
            .future(clone!(select => {
                revision.for_each(move |_| {
                    let v = match read_for_signal() {
                        Some(ProceduralTextureDef::Checker { .. }) => "checker",
                        Some(ProceduralTextureDef::Gradient { .. }) => "gradient",
                        Some(ProceduralTextureDef::Noise { .. }) => "noise",
                        None => "checker",
                    };
                    if select.value() != v {
                        select.set_value(v);
                    }
                    async {}
                })
            }))
            .event(clone!(select => move |_: events::Change| {
                // Switching variants resets to that variant's defaults.
                // Width / Height carry over so a user-tuned size isn't lost.
                let (w, h) = match read_def() {
                    Some(ProceduralTextureDef::Checker { width, height, .. })
                    | Some(ProceduralTextureDef::Gradient { width, height, .. })
                    | Some(ProceduralTextureDef::Noise { width, height, .. }) => (width, height),
                    None => (256, 256),
                };
                let new_def = match select.value().as_str() {
                    "gradient" => ProceduralTextureDef::Gradient {
                        width: w,
                        height: h,
                        color_a: [0.1, 0.1, 0.4, 1.0],
                        color_b: [0.9, 0.6, 0.2, 1.0],
                        horizontal: false,
                    },
                    "noise" => ProceduralTextureDef::Noise {
                        width: w,
                        height: h,
                        seed: 1,
                        scale: 4.0,
                    },
                    _ => ProceduralTextureDef::Checker {
                        width: w,
                        height: h,
                        cells_x: 8,
                        cells_y: 8,
                        color_a: [0.1, 0.1, 0.1, 1.0],
                        color_b: [0.9, 0.9, 0.9, 1.0],
                    },
                };
                write_def(new_def);
            }))
        })
    })
}

fn render_params(
    def: ProceduralTextureDef,
    read_def: impl Fn() -> Option<ProceduralTextureDef> + Clone + 'static,
    write_def: impl Fn(ProceduralTextureDef) + Clone + 'static,
    write_def_committing: impl Fn(ProceduralTextureDef) + Clone + 'static,
) -> Dom {
    let mut rows: Vec<Dom> = Vec::new();
    // Width / Height shared by every variant.
    rows.push(kind_editor::field_row(
        "Width",
        uint_input(read_def.clone(), write_def.clone(), Field::Width),
    ));
    rows.push(kind_editor::field_row(
        "Height",
        uint_input(read_def.clone(), write_def.clone(), Field::Height),
    ));
    match def {
        ProceduralTextureDef::Checker { .. } => {
            rows.push(kind_editor::field_row(
                "Cells X",
                uint_input(read_def.clone(), write_def.clone(), Field::CellsX),
            ));
            rows.push(kind_editor::field_row(
                "Cells Y",
                uint_input(read_def.clone(), write_def.clone(), Field::CellsY),
            ));
            rows.push(kind_editor::field_row(
                "Color A",
                color_input(read_def.clone(), write_def.clone(), Channel::A),
            ));
            rows.push(kind_editor::field_row(
                "Color B",
                color_input(read_def.clone(), write_def.clone(), Channel::B),
            ));
        }
        ProceduralTextureDef::Gradient { .. } => {
            rows.push(kind_editor::field_row(
                "Color A",
                color_input(read_def.clone(), write_def.clone(), Channel::A),
            ));
            rows.push(kind_editor::field_row(
                "Color B",
                color_input(read_def.clone(), write_def.clone(), Channel::B),
            ));
            rows.push(kind_editor::field_row(
                "Horizontal",
                bool_input(read_def.clone(), write_def_committing.clone()),
            ));
        }
        ProceduralTextureDef::Noise { .. } => {
            rows.push(kind_editor::field_row(
                "Seed",
                uint_input(read_def.clone(), write_def.clone(), Field::Seed),
            ));
            rows.push(kind_editor::field_row(
                "Scale",
                scale_input(read_def, write_def),
            ));
        }
    }
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.5rem")
        .children(rows)
    })
}

#[derive(Clone, Copy)]
enum Field {
    Width,
    Height,
    CellsX,
    CellsY,
    Seed,
}

#[derive(Clone, Copy)]
enum Channel {
    A,
    B,
}

fn uint_input(
    read_def: impl Fn() -> Option<ProceduralTextureDef> + Clone + 'static,
    write_def: impl Fn(ProceduralTextureDef) + Clone + 'static,
    field: Field,
) -> Dom {
    let revision = app_state().scene.revision.clone();
    let read_for_signal = read_def.clone();
    let value_signal = revision
        .signal()
        .map(move |_| match (field, read_for_signal()) {
            (Field::Width, Some(def)) => dims(&def).0 as f32,
            (Field::Height, Some(def)) => dims(&def).1 as f32,
            (Field::CellsX, Some(ProceduralTextureDef::Checker { cells_x, .. })) => cells_x as f32,
            (Field::CellsY, Some(ProceduralTextureDef::Checker { cells_y, .. })) => cells_y as f32,
            (Field::Seed, Some(ProceduralTextureDef::Noise { seed, .. })) => seed as f32,
            _ => 0.0,
        });
    number_input(value_signal, move |new_value| {
        let Some(mut def) = read_def() else {
            return;
        };
        let v = new_value.round().max(1.0) as u32;
        match (field, &mut def) {
            (Field::Width, ProceduralTextureDef::Checker { width, .. })
            | (Field::Width, ProceduralTextureDef::Gradient { width, .. })
            | (Field::Width, ProceduralTextureDef::Noise { width, .. }) => *width = v,
            (Field::Height, ProceduralTextureDef::Checker { height, .. })
            | (Field::Height, ProceduralTextureDef::Gradient { height, .. })
            | (Field::Height, ProceduralTextureDef::Noise { height, .. }) => *height = v,
            (Field::CellsX, ProceduralTextureDef::Checker { cells_x, .. }) => *cells_x = v,
            (Field::CellsY, ProceduralTextureDef::Checker { cells_y, .. }) => *cells_y = v,
            (Field::Seed, ProceduralTextureDef::Noise { seed, .. }) => *seed = v,
            _ => {}
        }
        write_def(def);
    })
}

fn scale_input(
    read_def: impl Fn() -> Option<ProceduralTextureDef> + Clone + 'static,
    write_def: impl Fn(ProceduralTextureDef) + Clone + 'static,
) -> Dom {
    let revision = app_state().scene.revision.clone();
    let read_for_signal = read_def.clone();
    let value_signal = revision.signal().map(move |_| match read_for_signal() {
        Some(ProceduralTextureDef::Noise { scale, .. }) => scale,
        _ => 4.0,
    });
    number_input(value_signal, move |new_value| {
        let Some(mut def) = read_def() else {
            return;
        };
        if let ProceduralTextureDef::Noise { scale, .. } = &mut def {
            *scale = new_value.max(0.01);
        }
        write_def(def);
    })
}

fn color_input(
    read_def: impl Fn() -> Option<ProceduralTextureDef> + Clone + 'static,
    write_def: impl Fn(ProceduralTextureDef) + Clone + 'static,
    which: Channel,
) -> Dom {
    use futures_signals::signal::SignalExt;
    let revision = app_state().scene.revision.clone();
    // Picker — focus-aware history via `history_input::color_input`.
    let read_def_hex = read_def.clone();
    let read_hex = move || {
        let rgba = read_def_hex()
            .and_then(|d| color(&d, which))
            .unwrap_or([1.0; 4]);
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
        let Some(mut def) = write_hex_read() else {
            return;
        };
        let alpha = color(&def, which).map(|c| c[3]).unwrap_or(1.0);
        set_color(&mut def, which, [r, g, b, alpha]);
        write_hex_write(def);
    };
    let picker = history_input::color_input(read_hex, write_hex, revision.signal());

    let revision_for_alpha = app_state().scene.revision.clone();
    let read_for_alpha = read_def.clone();
    let alpha_signal = revision_for_alpha.signal().map(move |_| {
        read_for_alpha()
            .and_then(|d| color(&d, which))
            .map(|c| c[3])
            .unwrap_or(1.0)
    });
    let alpha = html!("div", {
        .style("flex", "1")
        .style("min-width", "0")
        .child(number_input(alpha_signal, move |new_value| {
            let Some(mut def) = read_def() else { return; };
            let rgb = color(&def, which).unwrap_or([1.0; 4]);
            set_color(&mut def, which, [rgb[0], rgb[1], rgb[2], new_value.clamp(0.0, 1.0)]);
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

fn bool_input(
    read_def: impl Fn() -> Option<ProceduralTextureDef> + Clone + 'static,
    write_def: impl Fn(ProceduralTextureDef) + Clone + 'static,
) -> Dom {
    use futures_signals::signal::SignalExt;
    let revision = app_state().scene.revision.clone();
    let read_for_signal = read_def.clone();
    html!("input" => web_sys::HtmlInputElement, {
        .attr("type", "checkbox")
        .style("cursor", "pointer")
        .with_node!(input => {
            .future(clone!(input => {
                revision.signal().for_each(move |_| {
                    let v = matches!(
                        read_for_signal(),
                        Some(ProceduralTextureDef::Gradient { horizontal: true, .. })
                    );
                    if input.checked() != v {
                        input.set_checked(v);
                    }
                    async {}
                })
            }))
            .event(clone!(read_def, write_def, input => move |_: events::Change| {
                let Some(mut def) = read_def() else { return; };
                if let ProceduralTextureDef::Gradient { horizontal, .. } = &mut def {
                    *horizontal = input.checked();
                    write_def(def);
                }
            }))
        })
    })
}

fn dims(def: &ProceduralTextureDef) -> (u32, u32) {
    match def {
        ProceduralTextureDef::Checker { width, height, .. }
        | ProceduralTextureDef::Gradient { width, height, .. }
        | ProceduralTextureDef::Noise { width, height, .. } => (*width, *height),
    }
}

fn color(def: &ProceduralTextureDef, which: Channel) -> Option<[f32; 4]> {
    match (def, which) {
        (ProceduralTextureDef::Checker { color_a, .. }, Channel::A)
        | (ProceduralTextureDef::Gradient { color_a, .. }, Channel::A) => Some(*color_a),
        (ProceduralTextureDef::Checker { color_b, .. }, Channel::B)
        | (ProceduralTextureDef::Gradient { color_b, .. }, Channel::B) => Some(*color_b),
        _ => None,
    }
}

fn set_color(def: &mut ProceduralTextureDef, which: Channel, rgba: [f32; 4]) {
    match (def, which) {
        (ProceduralTextureDef::Checker { color_a, .. }, Channel::A)
        | (ProceduralTextureDef::Gradient { color_a, .. }, Channel::A) => *color_a = rgba,
        (ProceduralTextureDef::Checker { color_b, .. }, Channel::B)
        | (ProceduralTextureDef::Gradient { color_b, .. }, Channel::B) => *color_b = rgba,
        _ => {}
    }
}
