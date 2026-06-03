//! Deep-link banner — top-of-screen overlay shown when the user lands
//! on the editor with a `?folder=<name>` query parameter.
//!
//! The browser's File System Access API (`show_directory_picker`)
//! requires a user-gesture click, so we can't auto-pop the picker from
//! a query-param at boot. Instead the banner offers a "Open <name> from
//! disk" button; clicking it opens the picker, reads `material.json` +
//! `shader.wgsl` out of the user-picked directory, and seeds
//! [`EditState`] accordingly. The deep-link state clears on successful
//! load (so the banner auto-dismisses).

use dominator::{clone, events, html, Dom};
use futures_signals::signal::SignalExt;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};

use crate::material::state::EditState;

pub fn render(state: &EditState) -> Dom {
    let folder = state.deep_link_folder.clone();
    let folder_for_signal = folder.clone();
    let state_for_button = state.clone();
    html!("div", {
        .child_signal(folder_for_signal.signal_cloned().map(clone!(state_for_button => move |slot| {
            slot.map(|folder_name| banner_body(folder_name, &state_for_button))
        })))
    })
}

fn banner_body(folder_name: String, state: &EditState) -> Dom {
    let state = state.clone();
    let folder_for_label = folder_name.clone();
    let error = state.deep_link_error.clone();
    html!("div", {
        .style("position", "fixed")
        .style("top", "0")
        .style("left", "0")
        .style("right", "0")
        .style("background", "#2a3a4a")
        .style("color", "#eef")
        .style("padding", "10px 16px")
        .style("display", "flex")
        .style("align-items", "center")
        .style("gap", "12px")
        .style("z-index", "1500")
        .style("border-bottom", "1px solid #46596d")
        .child(html!("span", {
            .style("flex", "1")
            .text(&format!(
                "Deep link: open material folder \"{folder_for_label}\" from disk. \
                 (Browser requires a click to grant access.)"
            ))
        }))
        .child(html!("button", {
            .style("background", "#3a5a7a")
            .style("color", "var(--text-0)")
            .style("border", "1px solid #4c7298")
            .style("padding", "4px 12px")
            .style("border-radius", "3px")
            .style("cursor", "pointer")
            .text(&format!("Open \"{folder_name}\"…"))
            .event(clone!(state, folder_name => move |_: events::Click| {
                let state = state.clone();
                let folder_name = folder_name.clone();
                spawn_local(async move {
                    if let Err(err) = load_material_via_picker(&state, &folder_name).await {
                        state.deep_link_error.set(Some(err));
                    }
                });
            }))
        }))
        .child(html!("button", {
            .style("background", "transparent")
            .style("color", "#abc")
            .style("border", "1px solid #557")
            .style("padding", "4px 10px")
            .style("border-radius", "3px")
            .style("cursor", "pointer")
            .text("Dismiss")
            .event(clone!(state => move |_: events::Click| {
                state.deep_link_folder.set(None);
                state.deep_link_error.set(None);
            }))
        }))
        .child_signal(error.signal_cloned().map(|err| {
            err.map(|msg| html!("span", {
                .style("color", "var(--danger-bright)")
                .style("font-size", "12px")
                .style("margin-left", "6px")
                .text(&msg)
            }))
        }))
    })
}

/// Opens the FS Access API directory picker, reads `material.json` and
/// `shader.wgsl` from the picked directory, parses them, and writes
/// the result into the EditState. Clears the deep-link state on
/// success; populates `deep_link_error` and leaves the banner up on
/// failure.
///
/// The `expected_name` is the value from the `?folder=` param — used
/// only for the banner label and the success log line; we don't
/// enforce that the user picks a folder with that name (the user
/// might have renamed it on disk). The actual material's name comes
/// from the parsed `material.json`.
async fn load_material_via_picker(state: &EditState, expected_name: &str) -> Result<(), String> {
    use awsm_scene_schema::dynamic_material::{validate_layout_names, MaterialDefinition};

    let window = web_sys::window().ok_or("no window")?;
    let options = web_sys::DirectoryPickerOptions::new();
    options.set_mode(web_sys::FileSystemPermissionMode::Read);
    let picker_promise = window
        .show_directory_picker_with_options(&options)
        .map_err(|_| {
            "FS Access API unavailable (requires Chrome / Edge + secure context)".to_string()
        })?;
    let handle_value = JsFuture::from(picker_promise).await.map_err(|err| {
        // AbortError = user cancelled; surface a quieter message.
        let s = err
            .as_string()
            .or_else(|| {
                js_sys::Reflect::get(&err, &"name".into())
                    .ok()
                    .and_then(|n| n.as_string())
            })
            .unwrap_or_default();
        if s.contains("Abort") {
            "cancelled".to_string()
        } else {
            format!("picker: {s}")
        }
    })?;
    let dir: web_sys::FileSystemDirectoryHandle = handle_value
        .dyn_into()
        .map_err(|_| "picker returned non-directory handle".to_string())?;

    let material_json = read_text_file(&dir, "material.json")
        .await
        .map_err(|e| format!("read material.json: {e}"))?;
    let definition: MaterialDefinition =
        serde_json::from_str(&material_json).map_err(|e| format!("parse material.json: {e}"))?;
    validate_layout_names(&definition).map_err(|e| format!("validate layout: {e}"))?;

    let wgsl = read_text_file(&dir, "shader.wgsl")
        .await
        .map_err(|e| format!("read shader.wgsl: {e}"))?;

    // Seed the live EditState. Mutating the existing Mutables (instead
    // of swapping the whole struct) preserves every pane's signal
    // subscriptions, mirroring how `EditState::reset_to` operates.
    state.definition.set(definition);
    state.wgsl_source.set(wgsl);
    state.errors.set(Vec::new());
    state.compile_last_error.set(None);
    // Clearing buffer_defaults on deep-link load matches the schema:
    // disk-side `BufferSlot.default` PathBufs aren't currently read
    // here (a future polish item — see remaining.md). For now the
    // author drops bytes through the Buffer Converter modal after the
    // material loads.
    state.buffer_defaults.set(std::collections::HashMap::new());
    state.deep_link_folder.set(None);
    state.deep_link_error.set(None);

    tracing::info!(
        "[material-editor] deep-link loaded material from picked folder (expected name: \"{expected_name}\")"
    );
    Ok(())
}

async fn read_text_file(
    dir: &web_sys::FileSystemDirectoryHandle,
    name: &str,
) -> Result<String, String> {
    let handle_promise = dir.get_file_handle(name);
    let handle_value = JsFuture::from(handle_promise)
        .await
        .map_err(|e| format!("get_file_handle({name}): {e:?}"))?;
    let handle: web_sys::FileSystemFileHandle = handle_value
        .dyn_into()
        .map_err(|_| format!("{name} is not a file handle"))?;
    let file_value = JsFuture::from(handle.get_file())
        .await
        .map_err(|e| format!("{name}.get_file(): {e:?}"))?;
    let file: web_sys::File = file_value
        .dyn_into()
        .map_err(|_| format!("{name}.get_file() returned non-File"))?;
    let text_value = JsFuture::from(file.text())
        .await
        .map_err(|e| format!("{name}.text(): {e:?}"))?;
    text_value
        .as_string()
        .ok_or_else(|| format!("{name}.text() returned non-string"))
}

/// Parse `?folder=<name>` out of the current `window.location.search`.
/// Returns `None` when the param is absent or empty.
pub fn read_folder_query_param() -> Option<String> {
    let location = web_sys::window()?.location();
    let search = location.search().ok()?;
    let params = web_sys::UrlSearchParams::new_with_str(&search).ok()?;
    let folder = params.get("folder")?;
    if folder.is_empty() {
        None
    } else {
        Some(folder)
    }
}
