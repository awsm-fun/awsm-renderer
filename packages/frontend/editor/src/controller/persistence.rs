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

use awsm_scene_schema::{CustomMaterialRef, EditorProject};

use super::node_spec::NodeSpec;
use super::EditorController;
use crate::engine::scene::node::Node;
use crate::error::{EditorError, EditorResult};

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

    EditorProject {
        name: ctrl.project_name.get_cloned(),
        environment: ctrl.scene.environment.get_cloned(),
        shadows: ctrl.scene.shadows.get_cloned(),
        assets: ctrl.scene.assets.lock().unwrap().clone(),
        custom_materials,
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

    let new_nodes: Vec<Arc<Node>> = project
        .nodes
        .iter()
        .map(|n| NodeSpec::from_editor_node(n).to_node())
        .collect();
    ctrl.scene.nodes.lock_mut().replace_cloned(new_nodes);
    ctrl.selected.set(Vec::new());
    ctrl.scene.bump_revision();
    // NOTE: custom-material *bodies* live in their side files; loading those back
    // into `custom_materials` (so they reappear in the Studio) is the follow-on —
    // the scene tree + assets + env round-trip here.
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
