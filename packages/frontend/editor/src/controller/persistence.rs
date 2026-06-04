//! Project (de)serialization — the TOML project format (decision 4 / §11).
//!
//! A project is a directory: a `project.toml` (carrying the scene tree,
//! environment, shadows, asset table, and custom-material refs) plus the asset
//! and material files it references. This module builds the serializable
//! [`EditorProject`] from the live editor state and rebuilds the live scene from
//! a loaded one. The source-abstracted load entry is `LoadProjectFromUrl` (it
//! fetches over HTTP, gesture-free — the external/MCP + scriptable-test path);
//! the directory-handle Save (FS Access) writes the same bytes back.

use std::path::PathBuf;
use std::sync::Arc;

use awsm_scene_schema::{CustomMaterialRef, EditorProject, StoredMaterial, StoredSlot};
use awsm_web_shared::prelude::Mutable;

use super::custom_material::{AlphaMode, CustomMaterial, Slot};
use super::node_spec::NodeSpec;
use super::EditorController;
use crate::engine::scene::node::Node;
use crate::error::{EditorError, EditorResult};

/// Snapshot a live custom material into its serializable form.
fn stored_from_material(m: &CustomMaterial) -> StoredMaterial {
    let slot = |s: &Slot| StoredSlot {
        name: s.name.clone(),
        ty: s.ty.clone(),
        val: s.val.clone(),
        debug: s.debug.clone(),
    };
    StoredMaterial {
        id: m.id,
        name: m.name.get_cloned(),
        builtin: m.builtin.get_cloned(),
        wgsl: m.wgsl.get_cloned(),
        alpha: m.alpha.get().key().to_string(),
        cutoff: m.cutoff.get() as f32,
        double_sided: m.double_sided.get(),
        color: m.color.get_cloned(),
        uniforms: m.uniforms.get_cloned().iter().map(slot).collect(),
        textures: m.textures.get_cloned().iter().map(slot).collect(),
        buffers: m.buffers.get_cloned().iter().map(slot).collect(),
        registered: m.registered.get(),
        shader_includes: m.shader_includes.get_cloned(),
        fragment_inputs: m.fragment_inputs.get_cloned(),
    }
}

/// Rebuild a live custom material from its serialized form (same id, so scene
/// nodes' material refs resolve).
fn material_from_stored(s: &StoredMaterial) -> Arc<CustomMaterial> {
    let slot = |x: &StoredSlot| Slot {
        name: x.name.clone(),
        ty: x.ty.clone(),
        val: x.val.clone(),
        debug: x.debug.clone(),
    };
    Arc::new(CustomMaterial {
        id: s.id,
        name: Mutable::new(s.name.clone()),
        builtin: Mutable::new(s.builtin.clone()),
        wgsl: Mutable::new(s.wgsl.clone()),
        alpha: Mutable::new(AlphaMode::from_key(&s.alpha)),
        cutoff: Mutable::new(s.cutoff as f64),
        double_sided: Mutable::new(s.double_sided),
        color: Mutable::new(if s.color.is_empty() {
            "#8aa0b8".to_string()
        } else {
            s.color.clone()
        }),
        uniforms: Mutable::new(s.uniforms.iter().map(slot).collect()),
        textures: Mutable::new(s.textures.iter().map(slot).collect()),
        buffers: Mutable::new(s.buffers.iter().map(slot).collect()),
        registered: Mutable::new(s.registered),
        shader_includes: Mutable::new(s.shader_includes.clone()),
        fragment_inputs: Mutable::new(s.fragment_inputs.clone()),
    })
}

/// Build the serializable project from the live editor state.
pub fn to_editor_project(ctrl: &EditorController) -> EditorProject {
    let nodes = ctrl
        .scene
        .nodes
        .lock_ref()
        .iter()
        .map(|n| NodeSpec::from_node(n).to_editor_node())
        .collect();

    let custom_materials = ctrl
        .custom_materials
        .lock_ref()
        .iter()
        .map(|m| {
            let name = m.name.get_cloned();
            let slug = slugify(&name);
            CustomMaterialRef {
                name,
                folder: PathBuf::from(format!("assets/materials/{slug}")),
            }
        })
        .collect();

    let editor_materials = ctrl
        .custom_materials
        .lock_ref()
        .iter()
        .map(|m| stored_from_material(m))
        .collect();

    EditorProject {
        name: ctrl.project_name.get_cloned(),
        environment: ctrl.scene.environment.get_cloned(),
        shadows: ctrl.scene.shadows.get_cloned(),
        assets: ctrl.scene.assets.lock().unwrap().clone(),
        custom_materials,
        editor_materials,
        nodes,
    }
}

/// Serialize the live project to a `project.toml` string.
pub fn project_to_toml(ctrl: &EditorController) -> EditorResult<String> {
    toml::to_string_pretty(&to_editor_project(ctrl))
        .map_err(|e| EditorError::Msg(format!("serialize project: {e}")))
}

/// Per-custom-material side files (`material-<slug>.toml` + `.wgsl`) — the body
/// the Studio authored. Returned alongside `project.toml` for the directory-handle
/// (FS Access) Save writer, which lands as the follow-on; the single-file Save
/// downloads only `project.toml` today.
#[allow(dead_code)]
pub fn material_files(ctrl: &EditorController) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for m in ctrl.custom_materials.lock_ref().iter() {
        let slug = slugify(&m.name.get_cloned());
        out.push((format!("material-{slug}.wgsl"), m.wgsl.get_cloned()));
        // A compact TOML sidecar of the surface + declared slots.
        let meta = format!(
            "name = \"{}\"\nalpha = \"{}\"\ndouble_sided = {}\nregistered = {}\n",
            m.name.get_cloned(),
            m.alpha.get().key(),
            m.double_sided.get(),
            m.registered.get(),
        );
        out.push((format!("material-{slug}.toml"), meta));
    }
    out
}

/// Rebuild the live scene from a loaded project (replaces the current scene).
pub fn apply_project(ctrl: &EditorController, project: EditorProject) {
    ctrl.scene.environment.set(project.environment);
    ctrl.scene.shadows.set(project.shadows);
    *ctrl.scene.assets.lock().unwrap() = project.assets;
    if !project.name.is_empty() {
        ctrl.project_name.set(project.name);
    }

    // Restore the custom-material library FIRST (built-in variant defs + dynamic
    // WGSL), keyed by stable id, so the nodes that reference them resolve when
    // they materialize below. Re-arm each material's lifecycle: dynamics compile
    // (auto-register); built-ins re-sync assigned meshes on later variant edits.
    let mats: Vec<Arc<CustomMaterial>> = project
        .editor_materials
        .iter()
        .map(material_from_stored)
        .collect();
    ctrl.custom_materials
        .lock_mut()
        .replace_cloned(mats.clone());
    for m in mats {
        if m.is_builtin() {
            super::spawn_builtin_resync(m);
        } else {
            super::spawn_auto_register(m);
        }
    }

    let new_nodes: Vec<Arc<Node>> = project
        .nodes
        .iter()
        .map(|n| NodeSpec::from_editor_node(n).to_node())
        .collect();
    ctrl.scene.nodes.lock_mut().replace_cloned(new_nodes);
    ctrl.selected.set(Vec::new());
    ctrl.scene.bump_revision();
}

/// Save the project to a picked directory (File System Access): writes
/// `project.toml` + each custom material's `material-<slug>.{toml,wgsl}` side
/// files. The directory layout is decision 4's flat project directory.
pub async fn save_to_dir(ctrl: &EditorController, dir: &crate::fs::ProjectDir) -> EditorResult<()> {
    dir.write_text("project.toml", &project_to_toml(ctrl)?)
        .await
        .map_err(|e| EditorError::Msg(e.to_string()))?;
    for (name, content) in material_files(ctrl) {
        dir.write_text(&name, &content)
            .await
            .map_err(|e| EditorError::Msg(e.to_string()))?;
    }
    Ok(())
}

/// Load a project from a picked directory: reads `project.toml` + rebuilds the
/// live scene. (Reloading custom-material bodies into the Studio is the follow-on.)
pub async fn load_from_dir(
    ctrl: &EditorController,
    dir: &crate::fs::ProjectDir,
) -> EditorResult<()> {
    let body = dir
        .read_text("project.toml")
        .await
        .map_err(|e| EditorError::Msg(e.to_string()))?;
    let project: EditorProject =
        toml::from_str(&body).map_err(|e| EditorError::Msg(format!("parse project.toml: {e}")))?;
    apply_project(ctrl, project);
    ctrl.reset_history();
    ctrl.dirty.set_neq(false);
    Ok(())
}

/// Fetch + parse a `project.toml` from `<base_url>/project.toml`.
pub async fn load_project_from_url(ctrl: &EditorController, base_url: &str) -> EditorResult<()> {
    let base = base_url.trim_end_matches('/');
    let url = format!("{base}/project.toml");
    let resp = gloo_net::http::Request::get(&url)
        .send()
        .await
        .map_err(|e| EditorError::Msg(format!("fetch {url}: {e}")))?;
    if !resp.ok() {
        return Err(EditorError::Msg(format!(
            "fetch {url}: HTTP {}",
            resp.status()
        )));
    }
    let body = resp
        .text()
        .await
        .map_err(|e| EditorError::Msg(format!("read {url}: {e}")))?;
    let project: EditorProject =
        toml::from_str(&body).map_err(|e| EditorError::Msg(format!("parse {url}: {e}")))?;
    apply_project(ctrl, project);
    Ok(())
}

/// A filesystem-safe slug for a material name (`"Holo Grid"` → `holo-grid`).
fn slugify(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let s = s.trim_matches('-').to_string();
    if s.is_empty() {
        "material".to_string()
    } else {
        s
    }
}
