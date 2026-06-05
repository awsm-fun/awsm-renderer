//! Content Browser (content-browser.jsx) — the bottom Assets drawer.
//!
//! Collapsed: a 34px bar with a folder glyph + total asset count. Expanded: a
//! 218px drawer with category tabs (All/Materials/Textures/Meshes), a search
//! box, "+ Material" / "+ Texture" authoring buttons, and a card grid. Cards are
//! built from the project [`AssetTable`] plus the fixed built-in material family
//! palette (PBR/Unlit/Toon/Flipbook, decision 3) — built-ins carry a distinct
//! accent outline + family glyph. Every mutation dispatches an `EditorCommand`.
//!
//! The Asset Inspector right-rail (selecting a card) lives in `inspector.rs`.

use awsm_scene_schema::{
    AssetSource, MaterialAlphaMode, MaterialDef, MaterialShading, ProceduralTextureDef, TextureDef,
};

use crate::controller::ProceduralKind;
use crate::engine::scene::{AssetId, NodeKind};
use crate::prelude::*;

/// The category tabs across the drawer toolbar.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cat {
    All,
    Material,
    Texture,
    /// Imported glTF/glb model files.
    Model,
    /// Procedural meshes captured from the scene (the `Mesh` insert kind).
    Mesh,
}

/// One card in the grid — either a project asset (`id: Some`) or a built-in
/// material family from the fixed palette (`id: None`, `builtin: true`).
struct Card {
    cat: Cat,
    id: Option<AssetId>,
    name: String,
    swatch: String,
    badge: Option<(String, Tone)>,
    meta: String,
    builtin: bool,
    /// A custom WGSL material — clicking opens it in the Material-mode Studio
    /// rather than the Asset Inspector.
    custom: bool,
}

pub fn render() -> Dom {
    let ctrl = controller();
    html!("div", {
        .style("flex", "0 0 auto")
        .child_signal(ctrl.content_browser_open.signal().map(|open| {
            Some(if open { expanded() } else { collapsed() })
        }))
    })
}

fn collapsed() -> Dom {
    html!("div", {
        .style("flex", "0 0 auto")
        .style("display", "flex")
        .style("align-items", "center")
        .style("gap", "8px")
        .style("height", "34px")
        .style("padding", "0 10px")
        .style("border-top", "1px solid var(--line)")
        .style("background", "var(--bg-1)")
        .child(html!("button", {
            .class("t")
            .style("display", "flex").style("align-items", "center").style("gap", "7px")
            .style("background", "transparent").style("border-style", "none").style("cursor", "pointer")
            .style("color", "var(--text-1)").style("font-size", "12.5px").style("font-weight", "560")
            .child(Icon::new("folder").size(15.0).render())
            .child(html!("span", { .text("Content Browser") }))
            .child(html!("span", { .class("mono").style("font-size", "10.5px").style("color", "var(--text-3)")
                .text_signal(total_count_signal()) }))
            .event(|_: events::Click| set_open(true))
        }))
    })
}

fn expanded() -> Dom {
    let cat = Mutable::new(Cat::All);
    let query = Mutable::new(String::new());

    html!("div", {
        .style("flex", "0 0 auto")
        .style("height", "218px")
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("border-top", "1px solid var(--line)")
        .style("background", "var(--bg-1)")
        .child(toolbar(cat.clone(), query.clone()))
        .child(html!("div", {
            .style("flex", "1")
            .style("overflow-y", "auto")
            .style("padding", "10px")
            // Rebuild the grid on scene revision (assets), custom-material list
            // changes, tab, or query.
            .child_signal(map_ref! {
                let _rev = controller().scene.revision.signal(),
                let _cm = controller().custom_materials.signal_vec_cloned().len(),
                let c = cat.signal(),
                let q = query.signal_cloned() =>
                Some(grid(*c, q))
            })
        }))
    })
}

fn toolbar(cat: Mutable<Cat>, query: Mutable<String>) -> Dom {
    html!("div", {
        .style("display", "flex").style("align-items", "center").style("gap", "10px")
        .style("height", "40px").style("padding", "0 10px")
        .style("border-bottom", "1px solid var(--line-soft)").style("flex", "0 0 auto")
        .child(html!("button", {
            .class("t")
            .style("display", "flex").style("align-items", "center").style("gap", "6px")
            .style("background", "transparent").style("border-style", "none").style("cursor", "pointer")
            .style("color", "var(--text-0)").style("font-size", "12.5px").style("font-weight", "620")
            .child(Icon::new("chevdown").size(14.0).color("var(--text-3)").render())
            .child(html!("span", { .text("Content Browser") }))
            .event(|_: events::Click| set_open(false))
        }))
        .child(tabs(cat))
        .child(html!("div", { .style("width", "180px")
            .child(TextInput::new(query.clone()).placeholder("Search assets\u{2026}").icon("search").render())
        }))
        .child(html!("div", { .style("flex", "1") }))
        .child(Btn::new().label("Material").icon("plus").variant(BtnVariant::Ghost).size(BtnSize::Sm)
            .on_click(|| {
                // "+ Material" authors a custom WGSL material in the Studio (decision 3).
                dispatch(EditorCommand::AddCustomMaterial);
                dispatch(EditorCommand::SwitchMode { mode: EditorMode::Material });
            }).render())
        .child(DropButton::new().label("Texture").icon("plus").variant(BtnVariant::Ghost).size(BtnSize::Sm)
            .items(|close| {
                let proc = |kind: ProceduralKind, label: &str, close: &Close| {
                    let close = close.clone();
                    MenuItem::new(label).icon("texture").on_click(move || {
                        dispatch(EditorCommand::AddTextureAsset { proc: kind });
                        (close.borrow_mut())();
                    }).render()
                };
                vec![
                    proc(ProceduralKind::Checker, "Procedural \u{00b7} Checker", &close),
                    proc(ProceduralKind::Gradient, "Procedural \u{00b7} Gradient", &close),
                    proc(ProceduralKind::Noise, "Procedural \u{00b7} Noise", &close),
                    MenuItem::new("Import image\u{2026}").icon("folder").on_click(|| {
                        Toast::info("Image import lands in M11");
                    }).render(),
                ]
            }).render())
    })
}

fn tabs(cat: Mutable<Cat>) -> Dom {
    let entries = [
        (Cat::All, "All", None),
        (Cat::Material, "Materials", Some(Cat::Material)),
        (Cat::Texture, "Textures", Some(Cat::Texture)),
        (Cat::Model, "Models", Some(Cat::Model)),
        (Cat::Mesh, "CapturedMeshes", Some(Cat::Mesh)),
    ];
    html!("div", {
        .style("display", "flex").style("gap", "2px")
        .style("background", "var(--bg-3)").style("border", "1px solid var(--line-soft)")
        .style("border-radius", "var(--r2)").style("padding", "2px")
        .children(entries.into_iter().map(move |(c, label, count_cat)| {
            let on_sig = cat.signal().map(move |sel| sel == c);
            let on_sig2 = cat.signal().map(move |sel| sel == c);
            let on_sig3 = cat.signal().map(move |sel| sel == c);
            html!("button", {
                .class("t")
                .style("display", "flex").style("align-items", "center").style("gap", "5px")
                .style("height", "22px").style("padding", "0 9px")
                .style("border-style", "none").style("border-radius", "var(--r1)").style("cursor", "pointer")
                .style("font-size", "11.5px")
                .style_signal("font-weight", on_sig.map(|on| if on { "600" } else { "520" }))
                .style_signal("background", on_sig2.map(|on| if on { "var(--bg-active)" } else { "transparent" }))
                .style_signal("color", on_sig3.map(|on| if on { "var(--text-0)" } else { "var(--text-2)" }))
                .child(html!("span", { .text(label) }))
                .apply(|b| match count_cat {
                    Some(cc) => b.child(html!("span", {
                        .class("mono").style("font-size", "9.5px").style("color", "var(--text-3)")
                        .text_signal(cat_count_signal(cc))
                    })),
                    None => b,
                })
                .event(clone!(cat => move |_: events::Click| cat.set_neq(c)))
            })
        }))
    })
}

fn grid(cat: Cat, query: &str) -> Dom {
    let cards = collect_cards(cat, query);
    if cards.is_empty() {
        return html!("div", {
            .style("height", "100%").style("display", "flex")
            .style("align-items", "center").style("justify-content", "center")
            .style("color", "var(--text-3)").style("font-size", "12.5px")
            .text("No assets match.")
        });
    }
    html!("div", {
        .style("display", "grid")
        .style("grid-template-columns", "repeat(auto-fill, minmax(116px, 1fr))")
        .style("gap", "10px")
        .children(cards.into_iter().map(card))
    })
}

fn card(c: Card) -> Dom {
    let id = c.id;
    let builtin = c.builtin;
    let custom = c.custom;
    let kind_label = match c.cat {
        Cat::Material => "material",
        Cat::Texture => "texture",
        Cat::Model => "model",
        Cat::Mesh => "mesh",
        Cat::All => "",
    };
    // Selected highlight binds to the live asset_selection.
    let sel_sig = controller()
        .asset_selection
        .signal()
        .map(move |sel| id.is_some() && sel == id);
    let sel_sig2 = controller()
        .asset_selection
        .signal()
        .map(move |sel| id.is_some() && sel == id);

    html!("button", {
        .class("t")
        .style("display", "flex").style("flex-direction", "column").style("padding", "0")
        .style("cursor", "pointer").style("text-align", "left").style("overflow", "hidden")
        .style("border-width", "1px").style("border-style", "solid")
        .style("border-radius", "var(--r2)").style("background", "var(--bg-2)")
        .style_signal("border-color", sel_sig.map(move |on| {
            if on { "var(--accent)" } else if builtin { "var(--accent-line)" } else { "var(--line-soft)" }
        }))
        .style_signal("box-shadow", sel_sig2.map(|on| if on { "0 0 0 2px var(--accent-ghost)" } else { "none" }))
        .child(html!("div", {
            .style("height", "64px").style("position", "relative")
            .style("background", &c.swatch)
            // Rendered material thumbnail (built-in materials) layers over the flat
            // swatch once it lands; falls back to the swatch when absent.
            .style("background-size", "cover")
            .style("background-position", "center")
            .apply(|b| match id {
                Some(mid) => b.style_signal("background-image",
                    crate::engine::thumbnail::thumbnails().signal_ref(move |m| m.get(&mid).cloned())
                        .map(|u| u.map(|u| format!("url({u})")).unwrap_or_else(|| "none".to_string()))),
                None => b,
            })
            .style("border-bottom", "1px solid var(--line-soft)")
            .apply(|b| match &c.badge {
                Some((label, tone)) => b.child(html!("span", {
                    .style("position", "absolute").style("top", "5px").style("left", "5px")
                    .child(badge(label, *tone))
                })),
                None => b,
            })
            .child(html!("span", {
                .style("position", "absolute").style("top", "5px").style("right", "5px")
                .style("padding", "1px 5px").style("font-size", "9px").style("font-weight", "600")
                .style("text-transform", "uppercase").style("letter-spacing", ".04em")
                .style("background", "oklch(0.16 0.006 255 / .7)").style("color", "var(--text-1)")
                .style("border-radius", "3px")
                .text(if builtin { "built-in" } else { kind_label })
            }))
        }))
        .child(html!("div", {
            .style("padding", "6px 8px")
            .child(html!("div", {
                .style("font-size", "11.5px").style("font-weight", "540").style("color", "var(--text-0)")
                .style("white-space", "nowrap").style("overflow", "hidden").style("text-overflow", "ellipsis")
                .text(&c.name)
            }))
            .child(html!("div", {
                .class("mono").style("font-size", "9.5px").style("color", "var(--text-3)").style("margin-top", "1px")
                .text(&c.meta)
            }))
        }))
        .event(move |_: events::Click| {
            match (id, custom) {
                // A custom WGSL material → open it in the Material-mode Studio.
                (Some(id), true) => {
                    dispatch(EditorCommand::SetCurrentMaterial { id: Some(id) });
                    dispatch(EditorCommand::SwitchMode { mode: EditorMode::Material });
                }
                // Any other project asset → route the right rail to the inspector.
                (Some(id), false) => dispatch(EditorCommand::SetAssetSelection { id: Some(id) }),
                // Built-in family palette: assigning to a mesh lands in M10.
                (None, _) => Toast::info("Drag a built-in family onto a mesh to assign (M10)"),
            }
        })
        .event(move |_: events::DoubleClick| {
            if id.is_some() && custom {
                dispatch(EditorCommand::SwitchMode { mode: EditorMode::Material });
            }
        })
    })
}

// ── data collection ───────────────────────────────────────────────────────────

fn collect_cards(cat: Cat, query: &str) -> Vec<Card> {
    let ql = query.trim().to_lowercase();
    let matches = |name: &str| ql.is_empty() || name.to_lowercase().contains(&ql);
    let mut cards: Vec<Card> = Vec::new();

    // No fixed "built-in family" palette — the Content Browser holds only materials
    // the user actually created (via the Material pane) + imported assets. A fresh
    // project starts empty.

    // Custom WGSL materials (decision 3) — shown in All + Materials.
    if matches!(cat, Cat::All | Cat::Material) {
        for mat in controller().custom_materials.lock_ref().iter() {
            // Queue a rendered thumbnail (built-in materials only; no-op once cached).
            crate::engine::thumbnail::request(mat.clone());
            let name = mat.name.get_cloned();
            if matches(&name) {
                let status = if mat.registered.get() {
                    "ready"
                } else {
                    "draft"
                };
                // Built-in materials show their shading kind; dynamic ones show "WGSL".
                let badge = match mat.builtin.get_cloned() {
                    Some(def) => shading_badge(&def),
                    None => ("WGSL".to_string(), Tone::Accent),
                };
                cards.push(Card {
                    cat: Cat::Material,
                    id: Some(mat.id),
                    name,
                    swatch: mat.color.get_cloned(),
                    badge: Some(badge),
                    meta: status.to_string(),
                    builtin: false,
                    // Clicking opens the Studio (its Definition rail shows the
                    // built-in variant panel, or the dynamic shader graph).
                    custom: true,
                });
            }
        }
    }

    // Project assets from the table.
    let ctrl = controller();
    let assets = ctrl.scene.assets.lock().unwrap();
    for (id, entry) in assets.entries.iter() {
        match &entry.source {
            AssetSource::Material(def) if matches!(cat, Cat::All | Cat::Material) => {
                let name = material_name(def);
                if matches(&name) {
                    cards.push(Card {
                        cat: Cat::Material,
                        id: Some(*id),
                        name,
                        swatch: rgb_css(def.base_color),
                        badge: Some(shading_badge(def)),
                        meta: material_meta(def),
                        builtin: false,
                        custom: false,
                    });
                }
            }
            AssetSource::Texture(def) if matches!(cat, Cat::All | Cat::Texture) => {
                let (name, meta, swatch) = texture_view(def);
                if matches(&name) {
                    cards.push(Card {
                        cat: Cat::Texture,
                        id: Some(*id),
                        name,
                        swatch,
                        badge: None,
                        meta,
                        builtin: false,
                        custom: false,
                    });
                }
            }
            AssetSource::Mesh(def) if matches!(cat, Cat::All | Cat::Mesh) => {
                let name = if def.label.is_empty() {
                    "Mesh".to_string()
                } else {
                    def.label.clone()
                };
                if matches(&name) {
                    cards.push(Card {
                        cat: Cat::Mesh,
                        id: Some(*id),
                        name,
                        swatch:
                            "linear-gradient(135deg, oklch(0.32 0.01 255), oklch(0.22 0.01 255))"
                                .to_string(),
                        badge: None,
                        meta: "captured mesh".to_string(),
                        builtin: false,
                        custom: false,
                    });
                }
            }
            // Imported glTF/glb model files — the deconstructed scene tree lives
            // in the Outliner; this is the browsable source-file asset.
            AssetSource::Filename(name) if matches!(cat, Cat::All | Cat::Model) => {
                if matches(name) {
                    cards.push(Card {
                        cat: Cat::Model,
                        id: Some(*id),
                        name: name.clone(),
                        swatch:
                            "linear-gradient(135deg, oklch(0.34 0.03 255), oklch(0.20 0.02 255))"
                                .to_string(),
                        badge: Some(("MODEL".to_string(), Tone::Accent)),
                        meta: "glTF/glb".to_string(),
                        builtin: false,
                        custom: false,
                    });
                }
            }
            _ => {}
        }
    }
    // Stable order: built-ins first (already pushed), then assets by name.
    cards
}

fn material_name(def: &MaterialDef) -> String {
    if def.label.is_empty() {
        "Material".to_string()
    } else {
        def.label.clone()
    }
}

fn material_meta(def: &MaterialDef) -> String {
    let alpha = match def.alpha_mode {
        MaterialAlphaMode::Opaque => "opaque",
        MaterialAlphaMode::Mask { .. } => "mask",
        MaterialAlphaMode::Blend => "blend",
    };
    format!("{} \u{00b7} {}", shading_badge(def).0.to_lowercase(), alpha)
}

fn shading_badge(def: &MaterialDef) -> (String, Tone) {
    match def.shading {
        MaterialShading::Pbr => ("PBR".to_string(), Tone::Accent),
        MaterialShading::Unlit => ("Unlit".to_string(), Tone::Warn),
        MaterialShading::Toon { .. } => ("Toon".to_string(), Tone::Ok),
    }
}

fn texture_view(def: &TextureDef) -> (String, String, String) {
    match def {
        TextureDef::Raster { display_name } => (
            display_name.clone(),
            "raster".to_string(),
            "linear-gradient(135deg, oklch(0.4 0.03 255), oklch(0.25 0.02 255))".to_string(),
        ),
        TextureDef::Procedural(p) => match p {
            ProceduralTextureDef::Checker {
                width,
                height,
                color_a,
                color_b,
                ..
            } => (
                "Checker".to_string(),
                format!("checker \u{00b7} {width}\u{00d7}{height}"),
                format!(
                    "repeating-conic-gradient({} 0% 25%, {} 0% 50%) 50% / 18px 18px",
                    rgb_css(*color_a),
                    rgb_css(*color_b)
                ),
            ),
            ProceduralTextureDef::Gradient {
                width,
                height,
                color_a,
                color_b,
                ..
            } => (
                "Gradient".to_string(),
                format!("gradient \u{00b7} {width}\u{00d7}{height}"),
                format!(
                    "linear-gradient(135deg, {}, {})",
                    rgb_css(*color_a),
                    rgb_css(*color_b)
                ),
            ),
            ProceduralTextureDef::Noise { width, height, .. } => (
                "Noise".to_string(),
                format!("noise \u{00b7} {width}\u{00d7}{height}"),
                "repeating-linear-gradient(45deg, oklch(0.5 0 0) 0 2px, oklch(0.3 0 0) 2px 4px)"
                    .to_string(),
            ),
        },
    }
}

fn rgb_css(c: [f32; 4]) -> String {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("rgb({}, {}, {})", b(c[0]), b(c[1]), b(c[2]))
}

// ── reactive counts ────────────────────────────────────────────────────────────

fn total_count_signal() -> impl Signal<Item = String> {
    controller().scene.revision.signal().map(|_| {
        let ctrl = controller();
        let assets = ctrl.scene.assets.lock().unwrap();
        let n = assets
            .entries
            .values()
            .filter(|e| {
                matches!(
                    e.source,
                    AssetSource::Material(_) | AssetSource::Texture(_) | AssetSource::Mesh(_)
                )
            })
            .count()
            + ctrl.custom_materials.lock_ref().len();
        n.to_string()
    })
}

fn cat_count_signal(cat: Cat) -> impl Signal<Item = String> {
    controller().scene.revision.signal().map(move |_| {
        let ctrl = controller();
        let assets = ctrl.scene.assets.lock().unwrap();
        let mut n = assets
            .entries
            .values()
            .filter(|e| {
                matches!(
                    (&e.source, cat),
                    (AssetSource::Material(_), Cat::Material)
                        | (AssetSource::Texture(_), Cat::Texture)
                        | (AssetSource::Mesh(_), Cat::Mesh)
                        | (AssetSource::Filename(_), Cat::Model)
                )
            })
            .count();
        // Custom (user-created) materials count toward Materials.
        if cat == Cat::Material {
            n += ctrl.custom_materials.lock_ref().len();
        }
        n.to_string()
    })
}

// ── helpers ────────────────────────────────────────────────────────────────────

fn set_open(open: bool) {
    controller().content_browser_open.set_neq(open);
}

fn dispatch(cmd: EditorCommand) {
    spawn_local(async move {
        let _ = controller().dispatch(cmd).await;
    });
}

/// Count of scene nodes that reference a material asset. Reserved for the Asset
/// Inspector's "Used by" row (M8 inspector); kept here next to the asset model.
#[allow(dead_code)]
fn material_users(_id: AssetId) -> usize {
    controller()
        .scene
        .nodes
        .lock_ref()
        .iter()
        .filter(|n| matches!(n.kind.get_cloned(), NodeKind::Primitive { .. }))
        .count()
}
