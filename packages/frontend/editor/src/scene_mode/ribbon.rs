//! Scene-mode ribbon: a tab strip (Insert · Object ·
//! Environment + Assets toggle) over the active tab's action row. Every Insert
//! action dispatches an `EditorCommand::Insert` through the controller. Camera
//! ops live in the Settings drawer (a Camera node is inserted from Insert).

use awsm_renderer_editor_protocol::{LightKind, PrimitiveShape};

use crate::controller::InsertSpec;
use crate::engine::scene::{AssetId, AssetSource, EnvSlot};
use crate::prelude::*;

/// Dispatch an insert of `spec` at the scene root.
fn insert(spec: InsertSpec) {
    spawn_local(async move {
        if let Err(err) = controller()
            .dispatch(EditorCommand::Insert {
                id: awsm_renderer_editor_protocol::NodeId::new(),
                spec,
                parent: None,
            })
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
                _ => insert_row(),
            })))
        }))
    })
}

const TABS: &[&str] = &["Insert", "Object", "Environment"];

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

/// True if `path` names a pre-baked nanite / cluster-LOD asset (the
/// `awsm-renderer-lod-bake` CLI's `<id>.clusters.bin`). Strips any URL query /
/// fragment first so a served `…/foo.clusters.bin?v=2` still routes to the
/// nanite path; matches `cluster_mesh_filename`'s `.clusters.bin` suffix.
fn is_nanite_path(path: &str) -> bool {
    path.split(['?', '#'])
        .next()
        .unwrap_or(path)
        .to_lowercase()
        .ends_with(".clusters.bin")
}

// Returns a reusable `Fn` (the dropdown rebuilds its rows on each open). Clones
// the entries each call; InsertSpec isn't Copy (Primitive carries a struct
// variant), so the per-item closure also clones its spec on each click.
/// Open the model import modal — either paste a URL (gesture-free, source-
/// abstracted) **or** pick a local file, routed by extension:
/// - `.glb` / `.gltf` → `ImportModelFromFile` / `ImportModelFromUrl` (the
///   normal editable-mesh path).
/// - `.clusters.bin`  → `ImportNaniteAsset` (the pre-baked nanite / cluster-LOD
///   path — a view-only `ClusterMesh` drawn through the bounded cluster
///   pipeline). For a local pick we mint a `blob:` URL; `fetch_cluster_mesh`
///   GETs it like any URL.
fn open_import_model() {
    Modal::open(|| {
        let url = Mutable::new(String::new());
        let picked: Mutable<Option<web_sys::File>> = Mutable::new(None);
        let pick_err: Mutable<Option<String>> = Mutable::new(None);

        // When the user picks a local file, mint a blob: object URL from it and
        // import straight away (no extra click) — then dismiss the modal.
        spawn_local(clone!(picked => async move {
            let mut first = true;
            picked.signal_cloned().for_each(move |maybe| {
                let fire = !first;
                first = false;
                async move {
                    if !fire { return; }
                    if let Some(file) = maybe {
                        let name = file.name();
                        if let Ok(obj_url) = web_sys::Url::create_object_url_with_blob(&file) {
                            spawn_local(async move {
                                if is_nanite_path(&name) {
                                    // View-only nanite import. We do NOT revoke the blob:
                                    // the URL is stored as the asset source and redo
                                    // re-dispatches this command, which re-GETs it — a
                                    // revoked blob would break redo. It lives for the
                                    // session (same session-local lifetime as the cache).
                                    let _ = controller()
                                        .dispatch(EditorCommand::ImportNaniteAsset { clusters_url: obj_url })
                                        .await;
                                } else {
                                    // The glb loader copies geometry into GPU templates,
                                    // so the controller revokes this blob once done.
                                    let _ = controller()
                                        .dispatch(EditorCommand::ImportModelFromFile { name, url: obj_url })
                                        .await;
                                }
                            });
                            Modal::close();
                        }
                    }
                }
            }).await;
        }));

        ModalCard::new("Import model")
            .width(520.0)
            .child(html!("div", {
                .style("display", "flex").style("flex-direction", "column").style("gap", "10px")
                .child(html!("span", { .style("font-size", "12.5px").style("color", "var(--text-2)").style("line-height", "1.5")
                    .text("Load a model into the scene — paste a URL, or pick a local file below. \
                           A .glb / .gltf imports as an editable mesh; a pre-baked .clusters.bin \
                           (awsm-renderer-lod-bake output) imports as a view-only nanite mesh.") }))
                .child(TextInput::new(url.clone()).placeholder("https://\u{2026}/model.glb").render())
                .child(FilePicker::new()
                    .with_accept(".glb,.gltf,.clusters.bin")
                    .with_placeholder("Drag & drop a .glb / .gltf / .clusters.bin, or click to browse")
                    .render(picked.clone(), pick_err.clone()))
                .child_signal(pick_err.signal_cloned().map(|e| e.map(|msg| html!("span", {
                    .style("font-size", "12px").style("color", "var(--danger, #e06c75)")
                    .text(&msg)
                }))))
            }))
            .footer(html!("div", {
                .style("display", "flex").style("gap", "8px")
                .child(Btn::new().label("Cancel").variant(BtnVariant::Ghost).on_click(Modal::close).render())
                .child(Btn::new().label("Import URL").icon("cube").variant(BtnVariant::Primary)
                    .on_click(clone!(url => move || {
                        let u = url.get_cloned();
                        if u.trim().is_empty() { return; }
                        spawn_local(async move {
                            let cmd = if is_nanite_path(&u) {
                                EditorCommand::ImportNaniteAsset { clusters_url: u }
                            } else {
                                EditorCommand::ImportModelFromUrl { url: u }
                            };
                            let _ = controller().dispatch(cmd).await;
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
    })
}

fn object_row() -> Dom {
    // Object actions operate on the selection. For now the buttons are present
    // and toast until selection exists.
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

/// The three independent environment slots. Each renders as its own picker
/// showing the current assignment (see [`env_slot_picker`]).
#[derive(Clone, Copy, PartialEq)]
enum Slot {
    Skybox,
    Specular,
    Irradiance,
}

impl Slot {
    fn title(self) -> &'static str {
        match self {
            Slot::Skybox => "Skybox",
            Slot::Specular => "IBL Specular",
            Slot::Irradiance => "IBL Irradiance",
        }
    }
    /// This slot's current value out of an environment config.
    fn get(self, env: &crate::engine::scene::EnvironmentConfig) -> EnvSlot {
        match self {
            Slot::Skybox => env.skybox,
            Slot::Specular => env.specular,
            Slot::Irradiance => env.irradiance,
        }
    }
    /// A `PatchEnvironment` that changes ONLY this slot (the others stay `None` →
    /// preserved), so assigning one slot never resets the other two.
    fn patch(self, value: EnvSlot) -> EditorCommand {
        let (skybox, specular, irradiance) = match self {
            Slot::Skybox => (Some(value), None, None),
            Slot::Specular => (None, Some(value), None),
            Slot::Irradiance => (None, None, Some(value)),
        };
        EditorCommand::PatchEnvironment {
            skybox,
            specular,
            irradiance,
        }
    }
}

fn environment_row() -> Dom {
    html!("div", {
        .style("display", "flex").style("align-items", "center").style("gap", "8px")
        .child(env_slot_picker(Slot::Skybox))
        .child(env_slot_picker(Slot::Specular))
        .child(env_slot_picker(Slot::Irradiance))
    })
}

/// A per-slot picker. The trigger label reflects the CURRENT assignment (reacts
/// to every write path — this picker, MCP `set_environment`, project load), so
/// what's set is always visible. The menu offers the built-in default sky, every
/// imported env cubemap asset, and an import action.
fn env_slot_picker(slot: Slot) -> Dom {
    html!("span", {
        .child_signal(controller().scene.environment.signal_cloned().map(move |env| {
            Some(env_slot_button(slot, slot.get(&env)))
        }))
    })
}

fn env_slot_button(slot: Slot, current: EnvSlot) -> Dom {
    let label = format!("{}: {}", slot.title(), env_slot_label(&current));
    DropButton::new()
        .label(label)
        .icon("env")
        .size(BtnSize::Sm)
        .items(move |close: Close| {
            let mut rows = vec![MenuItem::new("Default sky")
                .checked(matches!(current, EnvSlot::BuiltInDefault))
                .on_click(clone!(close => move || {
                    patch_env_slot(slot, EnvSlot::BuiltInDefault);
                    (close.borrow_mut())();
                }))
                .render()];
            // Preserve (and mark) an agent-authored sky gradient if that's what
            // this slot currently holds — the UI can't author one, but it must
            // not hide it or misreport the slot as "Default sky".
            if matches!(current, EnvSlot::SkyGradient { .. }) {
                rows.push(
                    MenuItem::new("Sky gradient (set via MCP)")
                        .checked(true)
                        .disabled(true)
                        .render(),
                );
            }
            let assets = collect_env_assets();
            if !assets.is_empty() {
                rows.push(menu_sep());
                for (id, name) in assets {
                    let is_current = matches!(current, EnvSlot::Ktx { asset_id } if asset_id == id);
                    rows.push(
                        MenuItem::new(name)
                            .checked(is_current)
                            .on_click(clone!(close => move || {
                                patch_env_slot(slot, EnvSlot::Ktx { asset_id: id });
                                (close.borrow_mut())();
                            }))
                            .render(),
                    );
                }
            }
            rows.push(menu_sep());
            rows.push(
                MenuItem::new("Import .ktx2\u{2026}")
                    .icon("sphere")
                    .on_click(clone!(close => move || {
                        import_env_ktx(slot);
                        (close.borrow_mut())();
                    }))
                    .render(),
            );
            rows
        })
        .render()
}

/// Short display label for a slot's current value.
fn env_slot_label(slot: &EnvSlot) -> String {
    match slot {
        EnvSlot::BuiltInDefault => "Default sky".to_string(),
        EnvSlot::SkyGradient { .. } => "sky gradient".to_string(),
        EnvSlot::Ktx { asset_id } => env_asset_label(*asset_id),
    }
}

/// Display label for a KTX environment asset — its `Filename`/`Url` leaf name,
/// or a short id when the asset entry is gone.
fn env_asset_label(id: AssetId) -> String {
    let name = controller()
        .scene
        .assets
        .lock()
        .unwrap()
        .entries
        .get(&id)
        .and_then(|e| match &e.source {
            AssetSource::Filename(name) => Some(name.clone()),
            AssetSource::Url(url) => url.rsplit('/').next().map(|s| s.to_string()),
            _ => None,
        });
    name.unwrap_or_else(|| format!("{:.8}", id.0.to_string()))
}

/// Every env-cubemap asset (`.ktx2`/`.ktx`, from an import here or an MCP URL
/// import) that a slot can reference, sorted by name. Shares `is_env_cubemap`
/// with the Content Browser so both agree on what's an environment map.
fn collect_env_assets() -> Vec<(AssetId, String)> {
    use crate::scene_mode::content_browser::is_env_cubemap;
    let ctrl = controller();
    let assets = ctrl.scene.assets.lock().unwrap();
    let mut out: Vec<(AssetId, String)> = assets
        .entries
        .iter()
        .filter_map(|(id, e)| match &e.source {
            AssetSource::Filename(name) if is_env_cubemap(name) => Some((*id, name.clone())),
            AssetSource::Url(url) if is_env_cubemap(url) => {
                Some((*id, url.rsplit('/').next().unwrap_or(url).to_string()))
            }
            _ => None,
        })
        .collect();
    out.sort_by_key(|(_, name)| name.to_lowercase());
    out
}

/// Assign `value` to `slot` via a partial `PatchEnvironment` (undoable,
/// serialized, MCP-consistent).
fn patch_env_slot(slot: Slot, value: EnvSlot) {
    spawn_local(async move {
        if let Err(err) = controller().dispatch(slot.patch(value)).await {
            tracing::error!("ribbon: PatchEnvironment failed: {err}");
        }
    });
}

async fn read_file_bytes(file: &web_sys::File) -> Result<Vec<u8>, String> {
    let buf = wasm_bindgen_futures::JsFuture::from(file.array_buffer())
        .await
        .map_err(|e| format!("read {}: {e:?}", file.name()))?;
    Ok(js_sys::Uint8Array::new(&buf).to_vec())
}

/// Import a single `.ktx2` cubemap and assign it to `slot`. One file picker,
/// then a partial patch — the other two slots are untouched.
fn import_env_ktx(slot: Slot) {
    Modal::open(move || {
        let file: Mutable<Option<web_sys::File>> = Mutable::new(None);
        ModalCard::new(format!("Import .ktx2 \u{2192} {}", slot.title()))
            .width(480.0)
            .child(html!("div", {
                .style("display", "flex").style("flex-direction", "column").style("gap", "10px")
                .child(html!("span", { .style("font-size", "12.5px").style("color", "var(--text-2)").style("line-height", "1.5")
                    .text("Pick a .ktx2 cubemap to assign to this slot. It joins the project assets and can be reused by the other slots.") }))
                .child(env_file_row(".ktx2 cubemap", file.clone()))
            }))
            .footer(html!("div", {
                .style("display", "flex").style("gap", "8px")
                .child(Btn::new().label("Cancel").variant(BtnVariant::Ghost).on_click(Modal::close).render())
                .child(Btn::new().label("Import").icon("sphere").variant(BtnVariant::Primary)
                    .on_click(clone!(file => move || {
                        let Some(f) = file.get_cloned() else {
                            Toast::error("Pick a .ktx2 file.");
                            return;
                        };
                        spawn_local(async move {
                            match import_env_file(f).await {
                                Ok(id) => {
                                    patch_env_slot(slot, EnvSlot::Ktx { asset_id: id });
                                    Modal::close();
                                }
                                Err(e) => Toast::error(format!("Import failed: {e}")),
                            }
                        });
                    })).render())
            }))
            .render()
    });
}

fn env_file_row(label: &str, slot: Mutable<Option<web_sys::File>>) -> Dom {
    html!("div", {
        .style("display", "flex").style("align-items", "center").style("gap", "8px")
        .child(html!("span", { .style("font-size", "12.5px").style("color", "var(--text-2)").style("min-width", "120px").text(label) }))
        .child(html!("input" => web_sys::HtmlInputElement, {
            .attr("type", "file").attr("accept", ".ktx2,.ktx")
            .with_node!(el => {
                .event(clone!(slot => move |_: events::Change| {
                    slot.set(el.files().and_then(|f| f.get(0)));
                }))
            })
        }))
    })
}

/// Read a picked KTX file, register it as a project asset + stash its bytes, and
/// return the new asset id (to reference from a slot). Mirrors the MCP
/// `ImportKtxEnvFromUrl` path (a `Filename` asset entry + `env_sync` byte stash).
async fn import_env_file(file: web_sys::File) -> Result<AssetId, String> {
    use crate::engine::bridge::env_sync::stash_ktx;
    use awsm_renderer_editor_protocol::AssetEntry;

    let bytes = read_file_bytes(&file).await?;
    let id = AssetId::new();
    controller()
        .scene
        .assets
        .lock()
        .unwrap()
        .entries
        .insert(id, AssetEntry::new(AssetSource::Filename(file.name())));
    stash_ktx(id, bytes);
    // Surface the new asset in the Content Browser / other pickers immediately.
    controller().scene.bump_revision();
    Ok(id)
}
