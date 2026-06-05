//! Real glTF/glb model import + **deconstruction**. Fetches the document and
//! `populate_gltf`s it into the renderer (which builds the full transform tree,
//! meshes, and skinning), then snapshots that into an
//! [`AssetTemplate`](super::asset_template::AssetTemplate). The caller
//! (`EditorController::finish_model_import`) mirrors the template as editor
//! `Group`/`Model` nodes so the import appears as an editable hierarchy in the
//! Outliner; each `Model` node duplicates the template meshes under its own
//! transform (see `node_sync::materialize_model`). The template's own meshes are
//! hidden so they don't double-render.

use awsm_renderer_gltf::data::GltfData;
use awsm_renderer_gltf::loader::{get_type_from_filename, GltfFileType};
use awsm_renderer_gltf::{loader::GltfLoader, AwsmRendererGltfExt};
use awsm_scene_schema::MaterialDef;

use super::asset_template::{self, AssetTemplate};
use crate::engine::context::renderer_handle;

/// The result of importing one glTF/glb: a display name, the node template to
/// deconstruct into the editor scene tree, and the materials + texture names the
/// file brought in (surfaced in the Content Browser — #6.3).
pub struct GltfImport {
    pub display_name: String,
    pub template: AssetTemplate,
    /// One editable [`MaterialDef`] per glTF material (in glTF material-index
    /// order). The mesh keeps rendering with the renderer-baked material; these
    /// are the browsable/editable extractions.
    pub materials: Vec<MaterialDef>,
    /// Display name per glTF image (in image-index order).
    pub image_names: Vec<String>,
}

/// Load + populate a glTF/glb from `url`; display name derived from the URL.
/// File type is inferred from the URL extension (`.glb`/`.gltf`).
pub async fn import(url: &str) -> Result<GltfImport, String> {
    import_typed(url, None, None).await
}

/// Load + populate a glTF/glb from a URL with an explicit file type + display
/// name. Used by the **file picker**: the picked file becomes a `blob:` object
/// URL (which has no extension, so the type can't be inferred), and we want the
/// real filename for the Outliner label rather than the opaque blob id.
pub async fn import_file(name: &str, url: &str) -> Result<GltfImport, String> {
    let file_type = get_type_from_filename(name);
    import_typed(url, file_type, Some(name)).await
}

async fn import_typed(
    url: &str,
    file_type: Option<GltfFileType>,
    name: Option<&str>,
) -> Result<GltfImport, String> {
    let loader = GltfLoader::load(url, file_type)
        .await
        .map_err(|e| format!("load: {e}"))?;
    let data = loader.into_data(None).map_err(|e| format!("decode: {e}"))?;
    // Extract browsable materials + texture names before `data` is moved into
    // `populate_gltf` (#6.3).
    let materials = extract_materials(&data);
    let image_names = extract_image_names(&data);
    let template = {
        // Hold the renderer lock across the async populate + the synchronous
        // template snapshot, so nothing mutates the freshly-built tree first.
        let handle = renderer_handle();
        let mut r = handle.lock().await;
        let ctx = r
            .populate_gltf(data, None)
            .await
            .map_err(|e| format!("populate: {e}"))?;
        let template = asset_template::build_from_context(&r, &ctx);
        // The renderer already rendered these meshes directly; hide them so the
        // editor's user-movable Model-node duplicates are the only visible copy.
        asset_template::hide_template_meshes(&mut r, &template);
        template
    };
    Ok(GltfImport {
        display_name: name.map(str::to_owned).unwrap_or_else(|| model_name(url)),
        template,
        materials,
        image_names,
    })
}

/// One editable [`MaterialDef`] per glTF material, carrying its PBR factors.
/// (Textures aren't wired through — these are browsable extractions; the mesh
/// still renders from the renderer-baked material so textured models stay
/// textured.)
fn extract_materials(data: &GltfData) -> Vec<MaterialDef> {
    data.doc
        .materials()
        .map(|m| {
            let pbr = m.pbr_metallic_roughness();
            let idx = m.index().unwrap_or(0);
            MaterialDef {
                label: m
                    .name()
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("Material {idx}")),
                base_color: pbr.base_color_factor(),
                metallic: pbr.metallic_factor(),
                roughness: pbr.roughness_factor(),
                emissive: m.emissive_factor(),
                double_sided: m.double_sided(),
                ..MaterialDef::default()
            }
        })
        .collect()
}

/// Display name per glTF image (used to seed browsable Texture asset entries).
fn extract_image_names(data: &GltfData) -> Vec<String> {
    data.doc
        .images()
        .enumerate()
        .map(|(i, img)| {
            img.name()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("image {i}"))
        })
        .collect()
}

fn model_name(url: &str) -> String {
    url.rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("model")
        .to_string()
}
