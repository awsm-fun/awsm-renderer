//! Real glTF/glb model import + **deconstruction**. Fetches the document and
//! `populate_gltf`s it into the renderer (which builds the full transform tree,
//! meshes, and skinning), then snapshots that into an
//! [`AssetTemplate`](super::asset_template::AssetTemplate). The caller
//! (`EditorController::finish_model_import`) mirrors the template as editor
//! `Group`/`Model` nodes so the import appears as an editable hierarchy in the
//! Outliner; each `Model` node duplicates the template meshes under its own
//! transform (see `node_sync::materialize_model`). The template's own meshes are
//! hidden so they don't double-render.

use awsm_renderer::textures::TextureKey;
use awsm_renderer_gltf::data::GltfData;
use awsm_renderer_gltf::loader::{get_type_from_filename, GltfFileType};
use awsm_renderer_gltf::populate::GltfPopulateContext;
use awsm_renderer_gltf::{loader::GltfLoader, AwsmRendererGltfExt};
use awsm_scene_schema::MaterialDef;

use super::asset_template::{self, AssetTemplate};
use crate::engine::context::renderer_handle;

/// The result of importing one glTF/glb: a display name, the node template to
/// deconstruct into the editor scene tree, and the materials + texture names the
/// file brought in (surfaced in the Content Browser + wired onto the meshes).
pub struct GltfImport {
    pub display_name: String,
    pub template: AssetTemplate,
    /// One editable material per glTF material (in glTF material-index order),
    /// with its factors + the renderer textures `populate_gltf` already baked.
    pub materials: Vec<ExtractedMaterial>,
}

/// A glTF material extracted into an editable [`MaterialDef`] (factors only;
/// the controller fills the texture refs once it has minted texture-asset ids)
/// plus the renderer [`TextureKey`]s the populate pass already uploaded, so they
/// can be **reused** (not re-decoded) when this material renders.
pub struct ExtractedMaterial {
    pub def: MaterialDef,
    pub textures: MaterialTextureKeys,
}

/// Baked renderer textures for a material's PBR slots (reused from populate).
#[derive(Default)]
pub struct MaterialTextureKeys {
    pub base_color: Option<TextureKey>,
    pub metallic_roughness: Option<TextureKey>,
    pub normal: Option<TextureKey>,
    pub occlusion: Option<TextureKey>,
    pub emissive: Option<TextureKey>,
}

/// glTF texture indices for a material's PBR slots (resolved to keys post-populate).
#[derive(Default)]
struct MaterialTextureIndices {
    base_color: Option<usize>,
    metallic_roughness: Option<usize>,
    normal: Option<usize>,
    occlusion: Option<usize>,
    emissive: Option<usize>,
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
    // Read material factors + texture indices from the document before it's moved
    // into `populate_gltf`; the indices are resolved to baked texture keys after.
    let mat_specs = extract_material_specs(&data);
    let (template, materials) = {
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
        let materials = resolve_materials(&ctx, mat_specs);
        (template, materials)
    };
    Ok(GltfImport {
        display_name: name.map(str::to_owned).unwrap_or_else(|| model_name(url)),
        template,
        materials,
    })
}

/// Read each glTF material's editable factors + its slot texture indices.
fn extract_material_specs(data: &GltfData) -> Vec<(MaterialDef, MaterialTextureIndices)> {
    data.doc
        .materials()
        .map(|m| {
            let pbr = m.pbr_metallic_roughness();
            let idx = m.index().unwrap_or(0);
            let def = MaterialDef {
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
            };
            let ix = MaterialTextureIndices {
                base_color: pbr.base_color_texture().map(|t| t.texture().index()),
                metallic_roughness: pbr.metallic_roughness_texture().map(|t| t.texture().index()),
                normal: m.normal_texture().map(|t| t.texture().index()),
                occlusion: m.occlusion_texture().map(|t| t.texture().index()),
                emissive: m.emissive_texture().map(|t| t.texture().index()),
            };
            (def, ix)
        })
        .collect()
}

/// Resolve each material's slot texture indices to the renderer [`TextureKey`]s
/// the populate pass uploaded (matched by glTF texture index — a texture maps to
/// one baked key regardless of the colour-space variant used in the lookup key).
fn resolve_materials(
    ctx: &GltfPopulateContext,
    specs: Vec<(MaterialDef, MaterialTextureIndices)>,
) -> Vec<ExtractedMaterial> {
    let textures = ctx.textures.lock().unwrap();
    let find = |idx: Option<usize>| -> Option<TextureKey> {
        let i = idx?;
        textures
            .iter()
            .find(|(k, _)| k.index == i)
            .map(|(_, v)| *v)
    };
    specs
        .into_iter()
        .map(|(def, ix)| ExtractedMaterial {
            def,
            textures: MaterialTextureKeys {
                base_color: find(ix.base_color),
                metallic_roughness: find(ix.metallic_roughness),
                normal: find(ix.normal),
                occlusion: find(ix.occlusion),
                emissive: find(ix.emissive),
            },
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
