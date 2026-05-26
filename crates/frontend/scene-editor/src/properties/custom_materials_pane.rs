//! Materials pane — surfaces the project's `custom_materials` list
//! and the Import Material button.
//!
//! The Import button opens the File System Access API directory
//! picker, reads `material.json` + `shader.wgsl` (+ any
//! `assets/*.png` / `*.bin` referenced by the layout's defaults),
//! and dispatches to
//! [`crate::renderer_bridge::dynamic_material_bridge::register_loaded_folder`]
//! to plumb the result into the renderer. Per-mesh assignment uses
//! the Custom picker exposed by [`list_custom_material_names`] —
//! the per-mesh property panel surfaces it as a "Custom" submenu in
//! the material-type dropdown.

use std::rc::Rc;
use std::sync::Arc;

use dominator::{clone, events, html, Dom};
use futures_signals::signal::{Mutable, SignalExt};
use futures_signals::signal_vec::{MutableVec, SignalVecExt};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};

use awsm_scene_schema::dynamic_material::{
    CustomMaterialRef, LoadedMaterialFolder, MaterialDefinition,
};

/// Render the Materials sub-pane given the project's current
/// `custom_materials` list (reactive). The Import button appends to
/// the same `MutableVec` so the listing updates in real time.
pub fn render(
    custom_materials: Rc<MutableVec<CustomMaterialRef>>,
    status: Arc<Mutable<ImportStatus>>,
) -> Dom {
    html!("div", {
        .style("padding", "8px 12px")
        .style("border-top", "1px solid #333")
        .child(html!("h4", { .text("Custom Materials") }))
        .child(render_import_button(custom_materials.clone(), status.clone()))
        .child(render_status(status.clone()))
        .child(html!("div", {
            .children_signal_vec(custom_materials.signal_vec_cloned().map(render_row))
        }))
        .child_signal({
            let custom_materials = custom_materials.clone();
            custom_materials.signal_vec_cloned().to_signal_cloned().map(|list| {
                if list.is_empty() {
                    Some(html!("p", {
                        .style("font-size", "11px")
                        .style("color", "#888")
                        .text("None imported. Click Import Material… to bring in a folder.")
                    }))
                } else {
                    None
                }
            })
        })
    })
}

fn render_import_button(
    custom_materials: Rc<MutableVec<CustomMaterialRef>>,
    status: Arc<Mutable<ImportStatus>>,
) -> Dom {
    html!("button", {
        .style("margin", "4px 0")
        .text("Import Material…")
        .event(clone!(custom_materials, status => move |_: events::Click| {
            let custom_materials = custom_materials.clone();
            let status = status.clone();
            spawn_local(async move {
                status.set(ImportStatus::Picking);
                let loaded = match import_material_via_picker().await {
                    Ok(l) => l,
                    Err(ImportError::Cancelled) => {
                        status.set(ImportStatus::Idle);
                        return;
                    }
                    Err(e) => {
                        status.set(ImportStatus::Failed(e.to_string()));
                        return;
                    }
                };
                status.set(ImportStatus::Reading);

                // Register against the live renderer via the
                // dynamic_material_bridge converter. Holds the
                // renderer lock briefly — the registration is
                // synchronous; pipeline compile fires async via
                // prewarm_pipelines below.
                let renderer = crate::context::renderer_handle();
                let mut renderer = renderer.lock().await;
                let mut map = crate::renderer_bridge::dynamic_material_bridge::CustomMaterialRegistryMap::new();
                match crate::renderer_bridge::dynamic_material_bridge::register_loaded_folder(
                    &mut renderer,
                    &mut map,
                    &loaded,
                ) {
                    Ok(_id) => {
                        let name = loaded.definition.name.clone();
                        let folder = std::path::PathBuf::from(format!(
                            "assets/materials/{}",
                            name
                        ));
                        custom_materials.lock_mut().push_cloned(CustomMaterialRef {
                            name: name.clone(),
                            folder,
                        });
                        status.set(ImportStatus::Done(name));
                    }
                    Err(e) => {
                        status.set(ImportStatus::Failed(format!("{e}")));
                    }
                }
            });
        }))
    })
}

fn render_status(status: Arc<Mutable<ImportStatus>>) -> Dom {
    html!("div", {
        .style("font-size", "11px")
        .style("color", "#888")
        .child_signal(status.signal_cloned().map(|s| Some(html!("span", {
            .text(&match s {
                ImportStatus::Idle => String::new(),
                ImportStatus::Picking => "Picking folder…".to_string(),
                ImportStatus::Reading => "Reading material files…".to_string(),
                ImportStatus::Done(name) => format!("Imported '{name}'."),
                ImportStatus::Failed(msg) => format!("Import failed: {msg}"),
            })
        }))))
    })
}

fn render_row(custom: CustomMaterialRef) -> Dom {
    let name = custom.name.clone();
    let folder = custom.folder.display().to_string();
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("margin-bottom", "6px")
        .style("padding", "4px 8px")
        .style("border-left", "2px solid #444")
        .child(html!("strong", { .text(&name) }))
        .child(html!("small", {
            .style("color", "#888")
            .text(&folder)
        }))
        .child(html!("a", {
            .attr("href", &format!(
                "http://localhost:9084/?folder={}",
                urlencode(&folder)
            ))
            .attr("target", "_blank")
            .style("font-size", "11px")
            .style("color", "#88f")
            .text("Open in material-editor")
        }))
    })
}

// ─────────────────────────────────────────────────────────────────────
// Import flow
// ─────────────────────────────────────────────────────────────────────

/// Status of the in-flight import. Drives the pane's inline status
/// line.
#[derive(Clone, Debug)]
pub enum ImportStatus {
    /// No import in progress.
    Idle,
    /// User is in the directory-picker modal.
    Picking,
    /// Reading + parsing the folder's files.
    #[allow(dead_code)]
    Reading,
    /// Most recent import succeeded for the given name.
    Done(String),
    /// Most recent import failed with the given message.
    Failed(String),
}

#[derive(thiserror::Error, Debug)]
enum ImportError {
    #[error("user cancelled")]
    Cancelled,
    #[error("file system API unavailable")]
    Unsupported,
    #[error("missing material.json in folder")]
    MissingMaterialJson,
    #[error("missing shader.wgsl in folder")]
    MissingShaderWgsl,
    #[error("material.json parse error: {0}")]
    Parse(String),
    #[error("{0}")]
    Js(String),
}

/// Open the directory picker, read material.json + shader.wgsl,
/// validate the layout, and return a [`LoadedMaterialFolder`] the
/// caller can hand to the renderer-bridge. Texture / buffer
/// default-asset reads are a thin extension of the same handle
/// walk; for now they're empty maps (Phase 19 polish lands the asset
/// reads alongside the actual texture-pool / extras-pool plumbing
/// for those slots).
async fn import_material_via_picker() -> Result<LoadedMaterialFolder, ImportError> {
    use awsm_scene_schema::dynamic_material::validate_layout_names;
    use web_sys::{FileSystemDirectoryHandle, FileSystemPermissionMode};

    let window = web_sys::window().ok_or(ImportError::Unsupported)?;

    let options = web_sys::DirectoryPickerOptions::new();
    options.set_mode(FileSystemPermissionMode::Read);
    let picker_promise = window
        .show_directory_picker_with_options(&options)
        .map_err(|_| ImportError::Unsupported)?;
    let handle_value = match JsFuture::from(picker_promise).await {
        Ok(v) => v,
        Err(err) => {
            let s = err
                .as_string()
                .or_else(|| {
                    js_sys::Reflect::get(&err, &"name".into())
                        .ok()
                        .and_then(|n| n.as_string())
                })
                .unwrap_or_default();
            if s.contains("AbortError") || s.contains("Abort") {
                return Err(ImportError::Cancelled);
            }
            return Err(ImportError::Js(s));
        }
    };
    let dir: FileSystemDirectoryHandle = handle_value
        .dyn_into()
        .map_err(|_| ImportError::Js("picker returned non-directory handle".into()))?;

    let material_json = read_text_file(&dir, "material.json")
        .await
        .map_err(|_| ImportError::MissingMaterialJson)?;
    let definition: MaterialDefinition =
        serde_json::from_str(&material_json).map_err(|e| ImportError::Parse(e.to_string()))?;
    validate_layout_names(&definition).map_err(|e| ImportError::Parse(e.to_string()))?;

    let shader = read_text_file(&dir, "shader.wgsl")
        .await
        .map_err(|_| ImportError::MissingShaderWgsl)?;

    Ok(LoadedMaterialFolder {
        definition,
        wgsl_source: shader,
        texture_data: std::collections::HashMap::new(),
        buffer_data: std::collections::HashMap::new(),
    })
}

async fn read_text_file(
    dir: &web_sys::FileSystemDirectoryHandle,
    name: &str,
) -> Result<String, ImportError> {
    let handle_promise = dir.get_file_handle(name);
    let handle_value = JsFuture::from(handle_promise)
        .await
        .map_err(|e| ImportError::Js(format!("get_file_handle({name}) await: {e:?}")))?;
    let handle: web_sys::FileSystemFileHandle = handle_value
        .dyn_into()
        .map_err(|_| ImportError::Js(format!("{name} is not a file handle")))?;
    let file_value = JsFuture::from(handle.get_file())
        .await
        .map_err(|e| ImportError::Js(format!("{name}.get_file(): {e:?}")))?;
    let file: web_sys::File = file_value
        .dyn_into()
        .map_err(|_| ImportError::Js(format!("{name}.get_file() returned non-File")))?;
    let text_value = JsFuture::from(file.text())
        .await
        .map_err(|e| ImportError::Js(format!("{name}.text(): {e:?}")))?;
    text_value
        .as_string()
        .ok_or_else(|| ImportError::Js(format!("{name}.text() returned non-string")))
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '~' | '/' => out.push(c),
            ' ' => out.push_str("%20"),
            _ => {
                for b in c.to_string().bytes() {
                    out.push_str(&format!("%{:02X}", b));
                }
            }
        }
    }
    out
}

// ─────────────────────────────────────────────────────────────────────
// Per-mesh Custom picker — exposed for the per-mesh property panel
// to consume.
// ─────────────────────────────────────────────────────────────────────

/// Return the names of all imported custom materials. The per-mesh
/// material picker uses this list to populate its "Custom" submenu.
/// Selecting a name from that menu sets the mesh's
/// `NodeKind::Primitive::custom_material` to
/// `Some(CustomMaterialInstance { material: name, … })`.
///
/// Exposed for the per-mesh property panel to consume.
#[allow(dead_code)]
pub fn list_custom_material_names(custom_materials: &MutableVec<CustomMaterialRef>) -> Vec<String> {
    custom_materials
        .lock_ref()
        .iter()
        .map(|c| c.name.clone())
        .collect()
}
