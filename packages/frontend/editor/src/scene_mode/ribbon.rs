//! Scene-mode ribbon (ribbon-rows.jsx): a tab strip (Insert · Object ·
//! Environment · Camera + Assets toggle) over the active tab's action row.
//! Every Insert action dispatches an `EditorCommand::Insert` through the
//! controller. Object/Environment/Camera rows wire what the engine exposes
//! today; the deeper env/camera wiring fills in with M6.

use awsm_scene_schema::{LightKind, PrimitiveShape};

use crate::controller::InsertSpec;
use crate::prelude::*;

/// Dispatch an insert of `spec` at the scene root.
fn insert(spec: InsertSpec) {
    spawn_local(async move {
        if let Err(err) = controller()
            .dispatch(EditorCommand::Insert { spec, parent: None })
            .await
        {
            tracing::error!("ribbon: Insert failed: {err}");
        }
    });
}

pub fn render() -> Dom {
    let tab = Mutable::new("Insert".to_string());

    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("background", "var(--bg-1)")
        .style("border-bottom", "1px solid var(--line)")
        .style("flex", "0 0 auto")
        .child(tab_strip(&tab))
        .child(html!("div", {
            .style("display", "flex")
            .style("align-items", "center")
            .style("gap", "8px")
            .style("min-height", "44px")
            .style("padding", "6px 12px")
            .style("overflow-x", "auto")
            .child_signal(tab.signal_cloned().map(|t| Some(match t.as_str() {
                "Insert" => insert_row(),
                "Object" => object_row(),
                "Environment" => environment_row(),
                "Camera" => camera_row(),
                _ => insert_row(),
            })))
        }))
    })
}

const TABS: &[&str] = &["Insert", "Object", "Environment", "Camera"];

fn tab_strip(tab: &Mutable<String>) -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("align-items", "center")
        .style("height", "38px")
        .style("padding", "0 10px")
        .style("border-bottom", "1px solid var(--line-soft)")
        .child(html!("div", {
            .style("display", "flex")
            .style("align-items", "center")
            .style("gap", "2px")
            .children(TABS.iter().map(|&t| {
                let on_sig = tab.signal_cloned().map(move |cur| cur == t);
                let on_sig2 = tab.signal_cloned().map(move |cur| cur == t);
                html!("button", {
                    .class("t")
                    .style("position", "relative")
                    .style("height", "38px")
                    .style("padding", "0 13px")
                    .style("border-style", "none")
                    .style("background", "transparent")
                    .style("cursor", "pointer")
                    .style("font-size", "12.5px")
                    .style_signal("font-weight", on_sig.map(|on| if on { "600" } else { "500" }))
                    .style_signal("color", tab.signal_cloned().map(move |cur| if cur == t { "var(--text-0)" } else { "var(--text-2)" }))
                    .event(clone!(tab => move |_: events::Click| tab.set_neq(t.to_string())))
                    .text(t)
                    .child(html!("span", {
                        .style("position", "absolute")
                        .style("left", "10px")
                        .style("right", "10px")
                        .style("bottom", "0")
                        .style("height", "2px")
                        .style("border-radius", "2px")
                        .style_signal("background", on_sig2.map(|on| if on { "var(--accent)" } else { "transparent" }))
                    }))
                })
            }))
        }))
        .child(html!("div", { .style("flex", "1") }))
        .child(Btn::new().label("Assets").icon("folder").variant(BtnVariant::Ghost).size(BtnSize::Sm)
            .on_click(|| {
                let open = controller().content_browser_open.clone();
                open.set_neq(!open.get());
            }).render())
    })
}

// Returns a reusable `Fn` (the dropdown rebuilds its rows on each open). Clones
// the entries each call; InsertSpec isn't Copy (Primitive carries a struct
// variant), so the per-item closure also clones its spec on each click.
/// Open the glTF import modal — a URL field + Import action that dispatches
/// `ImportModelFromUrl` (the gesture-free, source-abstracted path; a File picker
/// variant is the follow-on).
fn open_import_model() {
    Modal::open(|| {
        let url = Mutable::new(String::new());
        ModalCard::new("Import glTF model")
            .width(520.0)
            .child(html!("div", {
                .style("display", "flex").style("flex-direction", "column").style("gap", "8px")
                .child(html!("span", { .style("font-size", "12.5px").style("color", "var(--text-2)").style("line-height", "1.5")
                    .text("Paste a URL to a .glb / .gltf model. It loads into the scene and renders.") }))
                .child(TextInput::new(url.clone()).placeholder("https://\u{2026}/model.glb").render())
            }))
            .footer(html!("div", {
                .style("display", "flex").style("gap", "8px")
                .child(Btn::new().label("Cancel").variant(BtnVariant::Ghost).on_click(Modal::close).render())
                .child(Btn::new().label("Import").icon("cube").variant(BtnVariant::Primary)
                    .on_click(clone!(url => move || {
                        let u = url.get_cloned();
                        if u.trim().is_empty() { return; }
                        spawn_local(async move {
                            let _ = controller().dispatch(EditorCommand::ImportModelFromUrl { url: u }).await;
                        });
                        Modal::close();
                    })).render())
            }))
            .render()
    });
}

fn drop_items(entries: Vec<(&'static str, InsertSpec)>) -> impl Fn(Close) -> Vec<Dom> + 'static {
    move |close| {
        entries
            .iter()
            .cloned()
            .map(|(label, spec)| {
                let close = close.clone();
                MenuItem::new(label)
                    .on_click(move || {
                        // Dispatch (spawned, queued) before closing the popup.
                        insert(spec.clone());
                        (close.borrow_mut())();
                    })
                    .render()
            })
            .collect()
    }
}

fn insert_row() -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("align-items", "center")
        .style("gap", "7px")
        .child(Btn::new().label("Empty").icon("empty").variant(BtnVariant::Ghost).size(BtnSize::Sm)
            .on_click(|| insert(InsertSpec::Empty)).render())
        .child(Btn::new().label("Model\u{2026}").icon("cube").variant(BtnVariant::Ghost).size(BtnSize::Sm)
            .on_click(open_import_model).render())
        .child(DropButton::new().label("Light\u{2026}").icon("light").size(BtnSize::Sm)
            .items(drop_items(vec![
                ("Directional", InsertSpec::Light(LightKind::Directional)),
                ("Point", InsertSpec::Light(LightKind::Point)),
                ("Spot", InsertSpec::Light(LightKind::Spot)),
            ])).render())
        .child(DropButton::new().label("Collision\u{2026}").icon("collision").size(BtnSize::Sm)
            .items(drop_items(vec![
                ("Box", InsertSpec::CollisionBox),
                ("Sphere", InsertSpec::CollisionSphere),
                ("Capsule", InsertSpec::CollisionCapsule),
                ("Cylinder", InsertSpec::CollisionCylinder),
                ("Cone", InsertSpec::CollisionCone),
                ("Ellipsoid", InsertSpec::CollisionEllipsoid),
            ])).render())
        .child(Btn::new().label("Camera").icon("camera").variant(BtnVariant::Ghost).size(BtnSize::Sm)
            .on_click(|| insert(InsertSpec::Camera)).render())
        .child(DropButton::new().label("Primitive\u{2026}").icon("sphere").size(BtnSize::Sm)
            .items(drop_items(vec![
                ("Plane", InsertSpec::Primitive(PrimitiveShape::default_plane())),
                ("Box", InsertSpec::Primitive(PrimitiveShape::default_box())),
                ("Sphere", InsertSpec::Primitive(PrimitiveShape::default_sphere())),
                ("Cylinder", InsertSpec::Primitive(PrimitiveShape::default_cylinder())),
                ("Cone", InsertSpec::Primitive(PrimitiveShape::default_cone())),
                ("Torus", InsertSpec::Primitive(PrimitiveShape::default_torus())),
            ])).render())
        .child(DropButton::new().label("Curve\u{2026}").icon("curve").size(BtnSize::Sm)
            .items(drop_items(vec![
                ("Curve", InsertSpec::Curve),
                ("Sweep along curve", InsertSpec::Sweep),
                ("Instances along curve", InsertSpec::Instances),
            ])).render())
        .child(DropButton::new().label("Visual\u{2026}").icon("sprite").size(BtnSize::Sm)
            .items(drop_items(vec![
                ("Line", InsertSpec::Line),
                ("Sprite", InsertSpec::Sprite),
                ("Particle Emitter", InsertSpec::Particle),
                ("Decal", InsertSpec::Decal),
                ("Shared Mesh", InsertSpec::Mesh),
            ])).render())
        .child(Btn::new().label("+ Material Asset").icon("material").variant(BtnVariant::Ghost).size(BtnSize::Sm)
            .on_click(|| Toast::info("Custom materials are authored in Material mode (M9)")).render())
    })
}

fn object_row() -> Dom {
    // Object actions operate on the selection, which lands in M5. For now the
    // buttons are present (prototype-faithful) and toast until selection exists.
    html!("div", {
        .style("display", "flex").style("align-items", "center").style("gap", "7px")
        .child(Btn::new().label("Duplicate").icon("copy").variant(BtnVariant::Solid).size(BtnSize::Sm)
            .on_click(|| Toast::info("Selection-driven object actions land in M5")).render())
        .child(Btn::new().label("Split").icon("layers").variant(BtnVariant::Ghost).size(BtnSize::Sm)
            .on_click(|| Toast::info("Split lands in M5")).render())
        .child(Btn::new().label("Deselect").variant(BtnVariant::Ghost).size(BtnSize::Sm)
            .on_click(|| Toast::info("Selection lands in M5")).render())
        .child(Btn::new().label("Delete").icon("trash").variant(BtnVariant::Ghost).size(BtnSize::Sm)
            .on_click(|| Toast::info("Selection-driven delete lands in M5")).render())
    })
}

fn environment_row() -> Dom {
    use crate::engine::environment::{self, EnvPreset};
    html!("div", {
        .style("display", "flex").style("align-items", "center").style("gap", "8px")
        .child(DropButton::new().label("Environment\u{2026}").icon("env").size(BtnSize::Sm)
            .items(move |close: Close| {
                let presets: Vec<(&'static str, EnvPreset)> = vec![
                    ("Simple Sky", EnvPreset::SimpleSky),
                    ("Studio White", EnvPreset::StudioWhite),
                    ("Neutral Grey", EnvPreset::NeutralGrey),
                ];
                presets.into_iter().map(|(label, preset)| {
                    let close = close.clone();
                    MenuItem::new(label)
                        .on_click(move || {
                            environment::apply(preset.clone());
                            (close.borrow_mut())();
                        })
                        .render()
                }).collect()
            }).render())
        .child(Btn::new().label("HDR set\u{2026}").icon("sphere").variant(BtnVariant::Ghost).size(BtnSize::Sm)
            .on_click(open_hdr_modal).render())
    })
}

/// Photoreal HDR environment: a base-URL field → loads
/// `<base>/{env,irradiance,skybox}.ktx2` into the IBL + skybox.
fn open_hdr_modal() {
    use crate::engine::environment::{self, EnvPreset};
    Modal::open(|| {
        let url = Mutable::new(String::new());
        ModalCard::new("Load HDR environment")
            .width(520.0)
            .child(html!("div", {
                .style("display", "flex").style("flex-direction", "column").style("gap", "8px")
                .child(html!("span", { .style("font-size", "12.5px").style("color", "var(--text-2)").style("line-height", "1.5")
                    .text("Base URL of a cubemap set served as env.ktx2 / irradiance.ktx2 / skybox.ktx2.") }))
                .child(TextInput::new(url.clone()).placeholder("https://\u{2026}/my_env").render())
            }))
            .footer(html!("div", {
                .style("display", "flex").style("gap", "8px")
                .child(Btn::new().label("Cancel").variant(BtnVariant::Ghost).on_click(Modal::close).render())
                .child(Btn::new().label("Load").icon("sphere").variant(BtnVariant::Primary)
                    .on_click(clone!(url => move || {
                        let u = url.get_cloned();
                        if u.trim().is_empty() { return; }
                        environment::apply(EnvPreset::Hdr { base_url: u });
                        Modal::close();
                    })).render())
            }))
            .render()
    });
}

fn camera_row() -> Dom {
    html!("div", {
        .style("display", "flex").style("align-items", "center").style("gap", "10px")
        .child(Btn::new().label("Reset View").icon("reset").variant(BtnVariant::Ghost).size(BtnSize::Sm)
            .on_click(|| Toast::info("Camera ops land in M6")).render())
    })
}
