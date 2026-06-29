//! Real glTF/glb model import + **deconstruction**. Fetches the document and
//! `populate_gltf`s it into the renderer (which builds the full transform tree,
//! meshes, and skinning), then snapshots that into an
//! [`AssetTemplate`](super::asset_template::AssetTemplate). The caller
//! (`EditorController::finish_model_import`) mirrors the template as editor
//! `Group`/`Mesh` nodes so the import appears as an editable hierarchy in the
//! Outliner.
//!
//! **Geometry is baked at import** (not retained as a hidden renderer copy): each
//! mesh-bearing glTF node's geometry is read CPU-side from the document's
//! accessors via [`awsm_renderer_glb_export::extract_node_mesh`] and carried on
//! [`GltfImport::node_meshes`]. The controller mints a captured `MeshDef` asset
//! per node and builds a `NodeKind::Mesh { mesh: Captured(..), .. }` node — so an
//! imported model is the same unified Mesh node as every other geometry kind, with
//! no `NodeKind::Model` and no retained source bytes. `populate_gltf` is still run
//! purely to extract materials + textures (and the transform/material-index
//! template); its meshes are hidden so they don't render.

use std::collections::HashMap;

use awsm_renderer::textures::TextureKey;
use awsm_renderer_editor_protocol::MaterialDef;
use awsm_renderer_glb_export::{ExportImage, MeshData};
use awsm_renderer_gltf::data::GltfData;
use awsm_renderer_gltf::extract::{extract_animations, ExtractedAnimation};
use awsm_renderer_gltf::loader::{get_type_from_filename, GltfFileType};
use awsm_renderer_gltf::populate::GltfPopulateContext;
use awsm_renderer_gltf::{loader::GltfLoader, AwsmRendererGltfExt};

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
    /// Parsed animation clips (keyed per channel by glTF node index). The
    /// controller maps each channel's node index to its minted `NodeId` and
    /// lowers the sampler into authored keyframes. Empty when the file has no
    /// animations (or extraction failed — logged, never fatal).
    pub animations: Vec<ExtractedAnimation>,
    /// Baked geometry for every mesh-bearing glTF node, read CPU-side from the
    /// document accessors at import. Keyed by `(node_index, primitive_index)`:
    /// the `None` primitive key is the whole node (every primitive merged), and
    /// each `Some(i)` key is one primitive in isolation — the controller uses the
    /// merged entry for single-material nodes and the per-primitive entries when
    /// it destructures a multi-material node. Positions are the node's *raw* local
    /// accessor values; the editor node carries the glTF node's local transform,
    /// so the geometry is used as-is (no extra matrix). Skinned meshes bake to
    /// their bind pose (JOINTS/WEIGHTS are not read).
    pub node_meshes: HashMap<(u32, Option<u32>), (MeshData, Option<Vec<[f32; 4]>>)>,
    /// `Some` when the import carries skins: the whole rig (geometry + skeleton +
    /// joints/weights + morph) re-exported through our writer into a clean glb
    /// (materials/animations dropped). This is what the player bundle ships for
    /// the import's `SkinnedMesh` nodes (`assets/<source-id>.glb`); cached under
    /// the source-file `AssetId` at `finish_model_import`. `None` for unskinned
    /// imports (those go through the captured-mesh path).
    pub skinned_glb: Option<Vec<u8>>,
    /// Source glTF node index → its node index in the clean re-export
    /// (`skinned_glb`), the depth-first flatten the player's loader sees. Used at
    /// `finish_model_import` to record each skin joint's bone `NodeId` → clean-glb
    /// index on `SkinnedMeshRef::joints`. Empty for unskinned imports.
    pub node_flat_indices: HashMap<u32, u32>,
    /// The ENCODED image bytes (original PNG/JPEG) of every imported texture,
    /// keyed by the renderer [`TextureKey`] `populate_gltf` uploaded it to. The
    /// controller stashes these (by minted texture-asset id) so imported textures
    /// persist across Save → reload (the renderer keeps only decoded pixels).
    /// Built by pairing `extract_texture_images` (glTF-texture-index → bytes) with
    /// the populate context's `GltfTextureKey.index → TextureKey` map.
    pub texture_images: HashMap<TextureKey, ExportImage>,
}

/// A glTF material extracted into an editable [`MaterialDef`] (factors only;
/// the controller fills the texture refs once it has minted texture-asset ids)
/// plus the renderer [`TextureKey`]s the populate pass already uploaded, so they
/// can be **reused** (not re-decoded) when this material renders.
pub struct ExtractedMaterial {
    pub def: MaterialDef,
    pub textures: MaterialTextureKeys,
    /// Resolved KHR-extension texture slots, keyed by `"<ext>.<field>"` (e.g.
    /// `"clearcoat.normal_tex"`). The controller turns each into a `TextureRef`
    /// on the matching `def.extensions` field once it has minted asset ids.
    pub ext_textures: Vec<(&'static str, (TextureKey, TexBinding))>,
}

/// The per-binding sampling metadata for one texture slot: which UV set (glTF
/// `texCoord`) and an optional `KHR_texture_transform`. Travels with the texture
/// key/index so it can be written onto the `TextureRef` at import.
#[derive(Clone, Copy, Default)]
pub struct TexBinding {
    pub uv_index: u32,
    pub transform: Option<awsm_renderer_editor_protocol::TextureTransform>,
    pub sampler: Option<awsm_renderer_editor_protocol::TextureSampler>,
}

/// Map a glTF texture sampler → the editor's [`TextureSampler`]. Returns `None`
/// when it's the glTF default (repeat + linear), to keep refs compact.
fn gltf_sampler(
    s: gltf::texture::Sampler,
) -> Option<awsm_renderer_editor_protocol::TextureSampler> {
    use awsm_renderer_editor_protocol::{TextureFilter, TextureSampler, TextureWrap};
    let wrap = |w: gltf::texture::WrappingMode| match w {
        gltf::texture::WrappingMode::ClampToEdge => TextureWrap::ClampToEdge,
        gltf::texture::WrappingMode::MirroredRepeat => TextureWrap::MirroredRepeat,
        gltf::texture::WrappingMode::Repeat => TextureWrap::Repeat,
    };
    let mag = match s.mag_filter() {
        Some(gltf::texture::MagFilter::Nearest) => TextureFilter::Nearest,
        _ => TextureFilter::Linear,
    };
    // glTF's minFilter packs the min-filter AND mipmap behaviour into one enum.
    let (min, mip) = match s.min_filter() {
        Some(gltf::texture::MinFilter::Nearest) => (TextureFilter::Nearest, TextureFilter::Linear),
        Some(gltf::texture::MinFilter::Linear) => (TextureFilter::Linear, TextureFilter::Linear),
        Some(gltf::texture::MinFilter::NearestMipmapNearest) => {
            (TextureFilter::Nearest, TextureFilter::Nearest)
        }
        Some(gltf::texture::MinFilter::LinearMipmapNearest) => {
            (TextureFilter::Linear, TextureFilter::Nearest)
        }
        Some(gltf::texture::MinFilter::NearestMipmapLinear) => {
            (TextureFilter::Nearest, TextureFilter::Linear)
        }
        Some(gltf::texture::MinFilter::LinearMipmapLinear) | None => {
            (TextureFilter::Linear, TextureFilter::Linear)
        }
    };
    let sampler = TextureSampler {
        wrap_u: wrap(s.wrap_s()),
        wrap_v: wrap(s.wrap_t()),
        mag_filter: mag,
        min_filter: min,
        mipmap_filter: mip,
    };
    (sampler != TextureSampler::default()).then_some(sampler)
}

/// Baked renderer textures for a material's PBR slots (reused from populate),
/// each with its glTF binding metadata (UV set + transform).
#[derive(Default)]
pub struct MaterialTextureKeys {
    pub base_color: Option<(TextureKey, TexBinding)>,
    pub metallic_roughness: Option<(TextureKey, TexBinding)>,
    pub normal: Option<(TextureKey, TexBinding)>,
    pub occlusion: Option<(TextureKey, TexBinding)>,
    pub emissive: Option<(TextureKey, TexBinding)>,
}

/// glTF texture indices for a material's PBR slots (resolved to keys
/// post-populate), each with its binding metadata.
#[derive(Default)]
struct MaterialTextureIndices {
    base_color: Option<(usize, TexBinding)>,
    metallic_roughness: Option<(usize, TexBinding)>,
    normal: Option<(usize, TexBinding)>,
    occlusion: Option<(usize, TexBinding)>,
    emissive: Option<(usize, TexBinding)>,
}

/// Read a texture slot's UV set + `KHR_texture_transform` from its glTF info.
/// (Works for any of `gltf::texture::Info` / `NormalTexture` / `OcclusionTexture`
/// — they all expose `tex_coord()` + `texture_transform()`.) The transform may
/// override the texCoord per glTF spec.
fn tex_binding(
    tex_coord: u32,
    xform: Option<gltf::texture::TextureTransform>,
    sampler: Option<awsm_renderer_editor_protocol::TextureSampler>,
) -> TexBinding {
    let uv_index = xform
        .as_ref()
        .and_then(|x| x.tex_coord())
        .unwrap_or(tex_coord);
    let transform = xform.map(|x| awsm_renderer_editor_protocol::TextureTransform {
        offset: x.offset(),
        rotation: x.rotation(),
        scale: x.scale(),
    });
    TexBinding {
        uv_index,
        transform,
        sampler,
    }
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

/// Read CPU-side geometry for every mesh-bearing glTF node out of an
/// already-decoded document, into a map keyed by `(node_index, primitive_index)`.
/// For each such node we store the whole-node merge (`primitive_index = None`) and
/// one entry per individual primitive (`Some(i)`), so the controller can build a
/// single Mesh node or destructure a multi-material node per-primitive — exactly
/// the cases the old `Model` path covered. Positions are raw local accessor values
/// (see [`awsm_renderer_glb_export::extract_node_mesh`] on the no-double-transform rule).
/// Per-node primary geometry keyed by `(node_index, primitive_index)`. ALL UV sets
/// (incl. `TEXCOORD_1`) ride `MeshData.uvs` now — no separate parallel map.
type NodeMeshMaps = HashMap<(u32, Option<u32>), (MeshData, Option<Vec<[f32; 4]>>)>;

fn extract_node_meshes(data: &GltfData) -> NodeMeshMaps {
    let buffers = &data.buffers.raw;
    let mut out = HashMap::new();
    for node in data.doc.nodes() {
        let Some(mesh) = node.mesh() else { continue };
        let node_index = node.index() as u32;
        // Whole-node merge (the common, single-material case). `ex.tangents` rides
        // alongside the geometry so the captured mesh preserves the authored basis.
        if let Some(ex) =
            awsm_renderer_glb_export::extract_node_mesh(&data.doc, buffers, node_index, None)
        {
            out.insert((node_index, None), (ex.mesh, ex.tangents));
        }
        // Per-primitive (used when a node's primitives carry different materials
        // and the controller destructures it into one Mesh child per primitive).
        let prim_count = mesh.primitives().count();
        if prim_count > 1 {
            for i in 0..prim_count as u32 {
                if let Some(ex) = awsm_renderer_glb_export::extract_node_mesh(
                    &data.doc,
                    buffers,
                    node_index,
                    Some(i),
                ) {
                    out.insert((node_index, Some(i)), (ex.mesh, ex.tangents));
                }
            }
        }
    }
    out
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
    // Bake every mesh-bearing node's geometry CPU-side from the accessors before
    // `data` is moved into `populate_gltf` — this is what becomes each editor
    // node's captured Mesh asset (the renderer's own meshes are only kept for
    // material/texture extraction, then hidden).
    let node_meshes = extract_node_meshes(&data);
    // Read material factors + texture indices from the document before it's moved
    // into `populate_gltf`; the indices are resolved to baked texture keys after.
    let mat_specs = extract_material_specs(&data);
    // Grab the ENCODED texture bytes (PNG/JPEG) off the document before populate
    // consumes it — the renderer keeps only decoded pixels, so these are what we
    // persist (paired with the baked TextureKeys below). Keyed by glTF tex index.
    // Resolve embedded AND external-URI images: the loader re-fetched external-file
    // image bytes into `data.encoded_images`, so persistence captures them too
    // (else an external-URI texture drops on save→reload — P0-A).
    let tex_images_by_index = awsm_renderer_glb_export::extract_texture_images_with_external(
        &data.doc,
        &data.buffers.raw,
        &data.encoded_images,
    );
    // Parse animations off the document before `data` is moved into populate.
    // A parse error must not abort the whole import — log it + import zero clips.
    let animations = match extract_animations(&data.doc, &data.buffers.raw) {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!("glTF animation extraction failed (importing 0 clips): {e}");
            Vec::new()
        }
    };
    // If the import carries skins OR morph targets, ingest the whole rig
    // (geometry + skeleton +
    // joints/weights + morph) into OUR clean glb — re-exported through our writer
    // (materials/anims/cruft dropped), the same pipeline static meshes use. This
    // is what the player bundle ships for skinned content (the source bytes never
    // need retaining — we have our own re-export). Static imports go through
    // `node_meshes`/`mesh_cache` instead, so we only pay this for skinned files.
    let has_morphs = data
        .doc
        .meshes()
        .any(|m| m.primitives().any(|p| p.morph_targets().next().is_some()));
    let skinned_glb = if data.doc.skins().next().is_some() || has_morphs {
        awsm_renderer_glb_export::reexport_clean_scene(&data.doc, &data.buffers.raw)
            .map(|scene| awsm_renderer_glb_export::write_glb(&scene))
    } else {
        None
    };
    // The source→clean node-index map (same DFS flatten the clean glb uses), so
    // `finish_model_import` can bind each skin joint's bone `NodeId` to the index
    // the player's loader will assign that joint. Only meaningful when skinned.
    let node_flat_indices: HashMap<u32, u32> = if skinned_glb.is_some() {
        awsm_renderer_glb_export::scene_node_flat_indices(&data.doc)
            .into_iter()
            .map(|(src, clean)| (src as u32, clean as u32))
            .collect()
    } else {
        HashMap::new()
    };
    let (template, materials, texture_images) = {
        // Hold the renderer lock across the async populate + the synchronous
        // template snapshot, so nothing mutates the freshly-built tree first.
        let handle = renderer_handle();
        let mut r = handle.lock().await;
        let ctx = r
            .populate_gltf(data, None)
            .await
            .map_err(|e| format!("populate: {e}"))?;
        // `populate_gltf` is a pure deferred ADD (load-transaction model): it stages
        // geometry but resolves NOTHING until `commit_load`. The template snapshot
        // below reads each node's renderer mesh keys (`keys_by_transform_key` →
        // `transform_to_meshes`) + per-mesh skin/morph classification (`mesh_is_skinned`
        // / `geometry_morph_key_for_mesh` → the mesh RESOURCE), ALL of which are
        // populated only at resolve (commit). Without committing first the snapshot
        // sees zero mesh keys, so `build_editor_subtree` makes every node an empty
        // Group (no geometry, no skinned/morph detection). Commit here so the meshes
        // resolve before we snapshot — and so they're synced into the spatial index
        // before the lock releases (no bound-but-unresolved render-frame window).
        r.commit_load(crate::engine::activity::commit_phase_handler())
            .await
            .map_err(|e| format!("commit: {e}"))?;
        let template = asset_template::build_from_context(&r, &ctx);
        // The renderer already rendered these meshes directly; hide them so the
        // editor's user-movable Model-node duplicates are the only visible copy.
        asset_template::hide_template_meshes(&mut r, &template);
        // Remove the populate-baked lights: each KHR light is re-materialized as
        // an editable `NodeKind::Light` bound to its editor node's transform (so
        // it follows animation + gets the inspector). Drop the populate copies so
        // they don't double up (and so the frozen populate-bound copy is gone).
        asset_template::remove_template_lights(&mut r, &ctx);
        let materials = resolve_materials(&ctx, mat_specs);
        // Pair each baked TextureKey with its encoded source bytes: the populate
        // ctx maps GltfTextureKey{ index } → TextureKey, and `tex_images_by_index`
        // maps that glTF texture index → encoded image. (Same key may map from
        // several GltfTextureKeys that differ only by color info — fine, identical
        // bytes; content-hash dedups at persist.)
        let texture_images: HashMap<TextureKey, ExportImage> = ctx
            .textures
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(gk, tk)| {
                tex_images_by_index
                    .get(&gk.index)
                    .map(|img| (*tk, img.clone()))
            })
            .collect();
        (template, materials, texture_images)
    };
    Ok(GltfImport {
        display_name: name.map(str::to_owned).unwrap_or_else(|| model_name(url)),
        template,
        materials,
        animations,
        node_meshes,
        skinned_glb,
        node_flat_indices,
        texture_images,
    })
}

/// Rebuild an imported skinned source's renderer **template** from its persisted
/// clean rig glb (slice-3 persistence). A cold project reload has no template
/// (session-local), so its `SkinnedMesh` nodes render empty + log "no import
/// template cached". Re-running `populate_gltf` on the rig + snapshotting the
/// template (then hiding non-skinned copies, as import does) makes
/// `materialize_skinned_mesh` resolve them again. Call BEFORE the SkinnedMesh
/// nodes materialize (i.e. before `apply_project` sets the scene). The
/// reclaim-guard's scene check (`node_sync::scene_has_skinned_from`) keeps this
/// template alive through the reload's old-node teardown.
pub async fn rebuild_skinned_template(
    source: awsm_renderer_editor_protocol::AssetId,
    rig_bytes: Vec<u8>,
) -> Result<(), String> {
    let loader = GltfLoader::from_glb_bytes(&rig_bytes)
        .await
        .map_err(|e| format!("rig load: {e}"))?;
    let data = loader
        .into_data(None)
        .map_err(|e| format!("rig decode: {e}"))?;
    let template = {
        let handle = renderer_handle();
        let mut r = handle.lock().await;
        let ctx = r
            .populate_gltf(data, None)
            .await
            .map_err(|e| format!("rig populate: {e}"))?;
        // Resolve the staged geometry before snapshotting — see the commit note in
        // `import_typed`. Without it the rig template snapshots zero mesh keys and the
        // reloaded SkinnedMesh nodes can't resolve their drawable.
        r.commit_load(crate::engine::activity::commit_phase_handler())
            .await
            .map_err(|e| format!("rig commit: {e}"))?;
        let template = asset_template::build_from_context(&r, &ctx);
        asset_template::hide_template_meshes(&mut r, &template);
        template
    };
    super::bridge().insert_template(source, std::sync::Arc::new(template));
    Ok(())
}

/// Read each glTF material's editable factors + its slot texture indices.
type MatSpec = (
    MaterialDef,
    MaterialTextureIndices,
    Vec<(&'static str, (usize, TexBinding))>,
);

fn extract_material_specs(data: &GltfData) -> Vec<MatSpec> {
    data.doc
        .materials()
        .map(|m| {
            let pbr = m.pbr_metallic_roughness();
            let idx = m.index().unwrap_or(0);
            let mut ext_textures = Vec::new();
            let extensions = extract_extensions(&m, &mut ext_textures);
            let def = MaterialDef {
                label: m
                    .name()
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("Material {idx}")),
                base_color: pbr.base_color_factor(),
                metallic: pbr.metallic_factor(),
                roughness: pbr.roughness_factor(),
                emissive: m.emissive_factor(),
                normal_scale: m.normal_texture().map(|t| t.scale()).unwrap_or(1.0),
                occlusion_strength: m.occlusion_texture().map(|t| t.strength()).unwrap_or(1.0),
                double_sided: m.double_sided(),
                alpha_mode: extract_alpha_mode(&m),
                // KHR_materials_unlit → the editor's flat/unlit shading model.
                shading: if m.unlit() {
                    awsm_renderer_editor_protocol::MaterialShading::Unlit
                } else {
                    awsm_renderer_editor_protocol::MaterialShading::Pbr
                },
                extensions,
                ..MaterialDef::default()
            };
            let ix = MaterialTextureIndices {
                base_color: pbr.base_color_texture().map(|t| {
                    (
                        t.texture().index(),
                        tex_binding(
                            t.tex_coord(),
                            t.texture_transform(),
                            gltf_sampler(t.texture().sampler()),
                        ),
                    )
                }),
                metallic_roughness: pbr.metallic_roughness_texture().map(|t| {
                    (
                        t.texture().index(),
                        tex_binding(
                            t.tex_coord(),
                            t.texture_transform(),
                            gltf_sampler(t.texture().sampler()),
                        ),
                    )
                }),
                // NormalTexture / OcclusionTexture don't expose the typed
                // texture_transform() accessor (only the base `Info` does), so
                // they carry their UV set + sampler; a transform on a normal/
                // occlusion map is rare and left off.
                normal: m.normal_texture().map(|t| {
                    (
                        t.texture().index(),
                        TexBinding {
                            uv_index: t.tex_coord(),
                            transform: None,
                            sampler: gltf_sampler(t.texture().sampler()),
                        },
                    )
                }),
                occlusion: m.occlusion_texture().map(|t| {
                    (
                        t.texture().index(),
                        TexBinding {
                            uv_index: t.tex_coord(),
                            transform: None,
                            sampler: gltf_sampler(t.texture().sampler()),
                        },
                    )
                }),
                emissive: m.emissive_texture().map(|t| {
                    (
                        t.texture().index(),
                        tex_binding(
                            t.tex_coord(),
                            t.texture_transform(),
                            gltf_sampler(t.texture().sampler()),
                        ),
                    )
                }),
            };
            // Patch each extension texture's sampler from the glTF texture (the
            // raw-JSON ext_tex path only had the textureInfo, not the sampler).
            for (_, (idx, b)) in ext_textures.iter_mut() {
                b.sampler = data
                    .doc
                    .textures()
                    .nth(*idx)
                    .and_then(|t| gltf_sampler(t.sampler()));
            }
            (def, ix, ext_textures)
        })
        .collect()
}

/// glTF `material.alphaMode` (+ cutoff) → the editor's [`MaterialAlphaMode`].
fn extract_alpha_mode(m: &gltf::Material) -> awsm_renderer_editor_protocol::MaterialAlphaMode {
    use awsm_renderer_editor_protocol::MaterialAlphaMode;
    match m.alpha_mode() {
        gltf::material::AlphaMode::Opaque => MaterialAlphaMode::Opaque,
        gltf::material::AlphaMode::Mask => MaterialAlphaMode::Mask {
            cutoff: m.alpha_cutoff().unwrap_or(0.5),
        },
        gltf::material::AlphaMode::Blend => MaterialAlphaMode::Blend,
    }
}

/// Read a scalar field off a raw glTF extension JSON object.
fn ext_f32(v: &gltf::json::Value, key: &str, default: f32) -> f32 {
    v.get(key)
        .and_then(|x| x.as_f64())
        .map(|x| x as f32)
        .unwrap_or(default)
}

/// Read a 3-component colour/vector field off a raw glTF extension JSON object.
fn ext_color3(v: &gltf::json::Value, key: &str, default: [f32; 3]) -> [f32; 3] {
    v.get(key)
        .and_then(|x| x.as_array())
        .map(|a| {
            let c = |i: usize| {
                a.get(i)
                    .and_then(|x| x.as_f64())
                    .unwrap_or(default[i] as f64) as f32
            };
            [c(0), c(1), c(2)]
        })
        .unwrap_or(default)
}

/// Extract every KHR material extension the editor models into per-mesh
/// uniforms. Read straight off the raw extensions JSON (uniform across all 11,
/// and independent of which typed accessors the `gltf` crate version exposes) —
/// only the *factors* matter here (the editor's `MaterialDef` carries no
/// extension texture slots). An enabled extension becomes a variant bit on the
/// imported material; its parameters become the per-mesh overrides this mesh
/// seeds from.
fn extract_extensions(
    m: &gltf::Material,
    ext_textures: &mut Vec<(&'static str, (usize, TexBinding))>,
) -> awsm_renderer_editor_protocol::material::PbrExtensions {
    use awsm_renderer_editor_protocol::material::*;
    let mut e = PbrExtensions::default();

    // KHR_materials_{emissive_strength, ior, specular, transmission, volume} are
    // parsed NATIVELY by the `gltf` crate into typed accessors. Reading them via
    // `extension_value` returns `None` (the crate already consumed them out of the
    // raw extensions map) — which silently DROPPED transmission/volume/ior/specular
    // on import (e.g. a glass model imported opaque, not translucent). Use the typed
    // accessors, exactly as `populate_gltf` (renderer-gltf) does. The remaining
    // extensions below are NOT in the crate's typed API, so they read raw JSON.
    if let Some(strength) = m.emissive_strength() {
        e.emissive_strength = Some(EmissiveStrengthExt { strength });
    }
    if let Some(ior) = m.ior() {
        e.ior = Some(IorExt { ior });
    }
    if let Some(s) = m.specular() {
        e.specular = Some(SpecularExt {
            factor: s.specular_factor(),
            color_factor: s.specular_color_factor(),
            ..Default::default()
        });
        if let Some(i) = s.specular_texture() {
            ext_textures.push(("specular.tex", info_to_ext(i)));
        }
        if let Some(i) = s.specular_color_texture() {
            ext_textures.push(("specular.color_tex", info_to_ext(i)));
        }
    }
    if let Some(t) = m.transmission() {
        e.transmission = Some(TransmissionExt {
            factor: t.transmission_factor(),
            ..Default::default()
        });
        if let Some(i) = t.transmission_texture() {
            ext_textures.push(("transmission.tex", info_to_ext(i)));
        }
    }
    if let Some(vol) = m.volume() {
        // attenuation_distance defaults to +inf ("no absorption") in glTF; clamp
        // non-finite to a large finite value so the MaterialDef stays
        // JSON/TOML-serializable (the bundle round-trip) without changing the look.
        let attenuation_distance = {
            let d = vol.attenuation_distance();
            if d.is_finite() {
                d
            } else {
                f32::MAX
            }
        };
        e.volume = Some(VolumeExt {
            thickness_factor: vol.thickness_factor(),
            attenuation_distance,
            attenuation_color: vol.attenuation_color(),
            ..Default::default()
        });
        if let Some(i) = vol.thickness_texture() {
            ext_textures.push(("volume.thickness_tex", info_to_ext(i)));
        }
    }

    // Capture an extension texture slot (a glTF `textureInfo` JSON object) by name.
    let mut grab = |slot: &'static str, v: &gltf::json::Value, json_key: &str| {
        if let Some(t) = ext_tex(v, json_key) {
            ext_textures.push((slot, t));
        }
    };
    if let Some(v) = m.extension_value("KHR_materials_diffuse_transmission") {
        e.diffuse_transmission = Some(DiffuseTransmissionExt {
            factor: ext_f32(v, "diffuseTransmissionFactor", 0.0),
            color_factor: ext_color3(v, "diffuseTransmissionColorFactor", [1.0, 1.0, 1.0]),
            ..Default::default()
        });
        grab("diffuse_transmission.tex", v, "diffuseTransmissionTexture");
        grab(
            "diffuse_transmission.color_tex",
            v,
            "diffuseTransmissionColorTexture",
        );
    }
    if let Some(v) = m.extension_value("KHR_materials_clearcoat") {
        e.clearcoat = Some(ClearcoatExt {
            factor: ext_f32(v, "clearcoatFactor", 0.0),
            roughness_factor: ext_f32(v, "clearcoatRoughnessFactor", 0.0),
            normal_scale: v
                .get("clearcoatNormalTexture")
                .map(|t| ext_f32(t, "scale", 1.0))
                .unwrap_or(1.0),
            ..Default::default()
        });
        grab("clearcoat.tex", v, "clearcoatTexture");
        grab("clearcoat.roughness_tex", v, "clearcoatRoughnessTexture");
        grab("clearcoat.normal_tex", v, "clearcoatNormalTexture");
    }
    if let Some(v) = m.extension_value("KHR_materials_sheen") {
        e.sheen = Some(SheenExt {
            roughness_factor: ext_f32(v, "sheenRoughnessFactor", 0.0),
            color_factor: ext_color3(v, "sheenColorFactor", [0.0, 0.0, 0.0]),
            ..Default::default()
        });
        grab("sheen.color_tex", v, "sheenColorTexture");
        grab("sheen.roughness_tex", v, "sheenRoughnessTexture");
    }
    if let Some(v) = m.extension_value("KHR_materials_dispersion") {
        e.dispersion = Some(DispersionExt {
            dispersion: ext_f32(v, "dispersion", 0.0),
        });
    }
    if let Some(v) = m.extension_value("KHR_materials_anisotropy") {
        e.anisotropy = Some(AnisotropyExt {
            strength: ext_f32(v, "anisotropyStrength", 0.0),
            rotation: ext_f32(v, "anisotropyRotation", 0.0),
            ..Default::default()
        });
        grab("anisotropy.tex", v, "anisotropyTexture");
    }
    if let Some(v) = m.extension_value("KHR_materials_iridescence") {
        e.iridescence = Some(IridescenceExt {
            factor: ext_f32(v, "iridescenceFactor", 0.0),
            ior: ext_f32(v, "iridescenceIor", 1.3),
            thickness_min: ext_f32(v, "iridescenceThicknessMinimum", 100.0),
            thickness_max: ext_f32(v, "iridescenceThicknessMaximum", 400.0),
            ..Default::default()
        });
        grab("iridescence.tex", v, "iridescenceTexture");
        grab(
            "iridescence.thickness_tex",
            v,
            "iridescenceThicknessTexture",
        );
    }
    e
}

/// A typed glTF extension `textureInfo` (from the crate's native accessors —
/// e.g. `specular.specular_texture()`, `transmission.transmission_texture()`) →
/// (glTF texture index, binding). The typed-accessor counterpart of [`ext_tex`]
/// (which parses crate-unknown extensions' textures from raw JSON); mirrors the
/// standard-slot path in `extract_material_specs`.
fn info_to_ext(info: gltf::texture::Info) -> (usize, TexBinding) {
    let texture = info.texture();
    let index = texture.index();
    let sampler = gltf_sampler(texture.sampler());
    (
        index,
        tex_binding(info.tex_coord(), info.texture_transform(), sampler),
    )
}

/// Read an extension `textureInfo` JSON object → (glTF texture index, binding).
/// Honors the slot's own `texCoord` + an inline `KHR_texture_transform`.
fn ext_tex(v: &gltf::json::Value, key: &str) -> Option<(usize, TexBinding)> {
    let info = v.get(key)?;
    let index = info.get("index").and_then(|x| x.as_u64())? as usize;
    let tex_coord = info.get("texCoord").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
    let xform = info
        .get("extensions")
        .and_then(|e| e.get("KHR_texture_transform"));
    let (uv_index, transform) = match xform {
        Some(t) => {
            let uv = t
                .get("texCoord")
                .and_then(|x| x.as_u64())
                .map(|x| x as u32)
                .unwrap_or(tex_coord);
            let transform = awsm_renderer_editor_protocol::TextureTransform {
                offset: read_vec2(t, "offset", [0.0, 0.0]),
                rotation: ext_f32(t, "rotation", 0.0),
                scale: read_vec2(t, "scale", [1.0, 1.0]),
            };
            (uv, Some(transform))
        }
        None => (tex_coord, None),
    };
    Some((
        index,
        TexBinding {
            uv_index,
            transform,
            // Patched from the glTF texture's sampler in extract_material_specs
            // (this raw-JSON path only sees the textureInfo).
            sampler: None,
        },
    ))
}

/// Read a 2-component float field off a raw glTF JSON object.
fn read_vec2(v: &gltf::json::Value, key: &str, default: [f32; 2]) -> [f32; 2] {
    v.get(key)
        .and_then(|x| x.as_array())
        .map(|a| {
            let c = |i: usize| {
                a.get(i)
                    .and_then(|x| x.as_f64())
                    .unwrap_or(default[i] as f64) as f32
            };
            [c(0), c(1)]
        })
        .unwrap_or(default)
}

/// Resolve each material's slot texture indices to the renderer [`TextureKey`]s
/// the populate pass uploaded (matched by glTF texture index — a texture maps to
/// one baked key regardless of the colour-space variant used in the lookup key).
fn resolve_materials(ctx: &GltfPopulateContext, specs: Vec<MatSpec>) -> Vec<ExtractedMaterial> {
    let textures = ctx.textures.lock().unwrap();
    // Resolve a (glTF texture index, binding) → (baked TextureKey, binding).
    let find = |slot: Option<(usize, TexBinding)>| -> Option<(TextureKey, TexBinding)> {
        let (i, binding) = slot?;
        textures
            .iter()
            .find(|(k, _)| k.index == i)
            .map(|(_, v)| (*v, binding))
    };
    specs
        .into_iter()
        .map(|(def, ix, ext_idx)| {
            let ext_textures = ext_idx
                .into_iter()
                .filter_map(|(slot, (i, b))| find(Some((i, b))).map(|kb| (slot, kb)))
                .collect();
            ExtractedMaterial {
                def,
                textures: MaterialTextureKeys {
                    base_color: find(ix.base_color),
                    metallic_roughness: find(ix.metallic_roughness),
                    normal: find(ix.normal),
                    occlusion: find(ix.occlusion),
                    emissive: find(ix.emissive),
                },
                ext_textures,
            }
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
