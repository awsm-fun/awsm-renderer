//! Insert Model / Empty / Light / Collision.

use crate::prelude::*;
use crate::renderer_bridge::asset_cache::{AssetTemplate, AssetTemplateNode};
use crate::scene::{mutate::insert_under, AssetId, ModelRef, Node, NodeId, NodeKind, Trs};
use crate::state::app_state;
use js_sys::Uint8Array;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::File;

pub use crate::scene::LightKind;

pub fn model(file: File) {
    crate::loading_modal::open("Inserting Model", format!("Reading {}…", file.name()));
    spawn_local(async move {
        let result = prepare_model(file).await;
        match result {
            Ok(ModelPrep {
                asset_id,
                filename,
                display_name,
                parent_id,
                template,
            }) => {
                crate::loading_modal::set("Building scene nodes…");
                let inserted =
                    insert_model_tree(asset_id, &filename, &display_name, parent_id, &template);
                // The bridge reacts to `bump_revision` on a microtask
                // and then runs `instantiate_model_template` to allocate
                // the actual GPU instances. Hold the modal up until every
                // freshly inserted Model node has reported Ready/Failed
                // so the user doesn't see a blank window between the
                // modal closing and the geometry appearing.
                crate::loading_modal::set("Materializing on GPU…");
                crate::loading_modal::wait_for_models_ready(&inserted).await;
                crate::loading_modal::close();
            }
            Err(err) => {
                crate::loading_modal::close();
                tracing::error!("Insert Model failed: {err}");
                Modal::error(format!("Insert Model failed: {err}"));
            }
        }
    });
}

struct ModelPrep {
    asset_id: AssetId,
    filename: String,
    display_name: String,
    parent_id: Option<NodeId>,
    template: AssetTemplate,
}

async fn prepare_model(file: File) -> anyhow::Result<ModelPrep> {
    let state = app_state();

    let filename = file.name();
    if filename.is_empty() {
        anyhow::bail!("The chosen file has no name");
    }

    // Read bytes first so we can hash + dedup. With content-hash
    // addressing the disk file lives at `assets/<hash>.<ext>`, so a
    // re-import of identical bytes (regardless of original filename)
    // resolves to the same `AssetId` and reuses the existing disk
    // file — no clobbering.
    let buffer = JsFuture::from(file.array_buffer())
        .await
        .map_err(|err| anyhow::anyhow!("reading file: {:?}", err))?;
    let buffer: js_sys::ArrayBuffer = buffer
        .dyn_into()
        .map_err(|_| anyhow::anyhow!("file.arrayBuffer() did not return an ArrayBuffer"))?;
    let array = Uint8Array::new(&buffer);
    let mut bytes = vec![0u8; array.length() as usize];
    array.copy_to(&mut bytes);
    let content_hash = crate::content_hash::sha256_hex(&bytes);

    let (asset_id, was_existing) = {
        let mut table = state.scene.assets.lock().unwrap();
        match table.find_by_content_hash(&content_hash) {
            Some(id) => (id, true),
            None => (
                table.insert_file_with_hash(filename.clone(), content_hash.clone()),
                false,
            ),
        }
    };

    // Stash bytes for the upload pass (texture cache reads them out
    // of `pending_assets`). Skip if already on disk for this entry —
    // that means the project loaded them from disk earlier and the
    // disk copy is canonical.
    let dir = state.project.lock().unwrap().directory.clone();
    let entry_snapshot = state.scene.assets.lock().unwrap().get(asset_id).cloned();
    let already_on_disk = match (&dir, &entry_snapshot) {
        (Some(dir), Some(entry)) => match awsm_scene_schema::asset_disk_path(asset_id, entry) {
            Some(path) => dir.file_exists(&path).await,
            None => false,
        },
        _ => false,
    };
    let already_pending = state.pending_assets.lock().unwrap().contains_key(&asset_id);
    if !already_on_disk && !already_pending {
        state.pending_assets.lock().unwrap().insert(asset_id, bytes);
    }
    if was_existing {
        tracing::debug!(
            "prepare_model: dedup hit on hash {content_hash} — reusing asset {asset_id} ({})",
            entry_snapshot
                .as_ref()
                .and_then(|e| e.source.display_name())
                .unwrap_or("")
        );
    }

    // Once per import, walk the gltf's materials + textures and surface
    // them in the assets library as editable `MaterialDef` /
    // `TextureDef::Raster` entries — and stash the per-gltf "material
    // index → editor AssetId" map on the gltf's AssetEntry so the
    // `instance_template` materializer can swap the renderer-baked
    // material with our editable extraction.
    //
    // Skipped (silently) if:
    //   - The gltf's AssetEntry already carries a populated
    //     `gltf_material_asset_ids` Vec (re-imported in the same
    //     session, or hydrated from a project.json that captured the
    //     prior extraction).
    //   - The gltf bytes aren't in `pending_assets` (the
    //     `already_on_disk` short-circuit above writes bytes only when
    //     they aren't already on disk). In that case the extraction
    //     would have to read from disk async — for now we accept that
    //     pre-existing on-disk gltfs keep their renderer-baked
    //     materials until re-imported.
    let already_extracted = !state
        .scene
        .assets
        .lock()
        .unwrap()
        .get(asset_id)
        .map(|e| e.gltf_material_asset_ids.is_empty())
        .unwrap_or(true);
    if !already_extracted {
        let bytes_for_gltf = state.pending_assets.lock().unwrap().get(&asset_id).cloned();
        if let Some(bytes) = bytes_for_gltf {
            crate::loading_modal::set("Extracting materials + textures…");
            let extract_label = filename
                .rsplit_once('.')
                .map(|(stem, _)| stem.to_string())
                .unwrap_or_else(|| filename.clone());
            let mut table = state.scene.assets.lock().unwrap();
            extract_gltf_materials_into(
                &mut table,
                &state.pending_assets,
                asset_id,
                &extract_label,
                &bytes,
            );
        }
    }

    // Kick off the asset load + wait for the populated template so we
    // know how many top-level nodes it has. A concurrent insert of the
    // same path will share this load.
    crate::loading_modal::set("Uploading to GPU…");
    let cache = state.renderer_bridge.assets.clone();
    let entry = cache.get_or_load(asset_id);
    let template = entry
        .wait()
        .await
        .map_err(|e| anyhow::anyhow!("load {filename}: {e}"))?;

    let display_name = filename
        .rsplit_once('.')
        .map(|(stem, _)| stem.to_string())
        .unwrap_or_else(|| filename.clone());

    Ok(ModelPrep {
        asset_id,
        filename,
        display_name,
        parent_id: parent_for_insert(&state),
        template,
    })
}

/// Walk the populated gltf template and build a tree of editor `Node`s
/// mirroring its structure. Mesh-bearing gltf nodes become `Model` nodes
/// (carrying the gltf node index they represent); pure transform-parents
/// become `Group` nodes. Each top-level gltf root becomes a sibling at the
/// chosen insertion point.
/// Builds editor `Node`s from the populated gltf template and inserts
/// them under `parent_id`. Returns the inserted root `Arc<Node>`s so
/// the caller can wait on their asset_status — empty Vec on failure.
fn insert_model_tree(
    asset_id: AssetId,
    filename: &str,
    display_name: &str,
    parent_id: Option<NodeId>,
    template: &AssetTemplate,
) -> Vec<Arc<Node>> {
    if template.roots.is_empty() {
        tracing::warn!("insert::model: template '{filename}' has no top-level nodes");
        Modal::error("This file contains no nodes to insert.");
        return Vec::new();
    }

    let state = app_state();
    let previous = state.snapshot_scene();

    // Multi-root gltf → use the file's display name as a synthetic root
    // name for any unlabelled top-level node, so the tree shows something
    // recognizable.
    let nodes: Vec<Arc<Node>> = template
        .roots
        .iter()
        .map(|root| build_editor_subtree(root, asset_id, Some(display_name)))
        .collect();

    let mut inserted: Vec<Arc<Node>> = Vec::with_capacity(nodes.len());
    let mut first_id: Option<NodeId> = None;
    for node in nodes {
        let id = node.id;
        let node_clone = node.clone();
        if !insert_under(&state.scene, parent_id, node) {
            tracing::error!("insert::model: failed to insert");
            Modal::error("Failed to insert model (parent may have been removed).");
            return Vec::new();
        }
        inserted.push(node_clone);
        if first_id.is_none() {
            first_id = Some(id);
        }
    }

    state.scene.bump_revision();
    if let Some(id) = first_id {
        // Implicit so the *next* insert doesn't nest under this one — the
        // user has to actually click a node to opt into "insert as child."
        state.select_only_implicit(id);
    }
    state.commit_history(previous);
    tracing::info!(
        "action: insert::model — added {filename} as {} top-level node(s)",
        template.roots.len()
    );
    inserted
}

fn build_editor_subtree(
    template_node: &AssetTemplateNode,
    asset_id: AssetId,
    fallback_name: Option<&str>,
) -> Arc<Node> {
    let name = template_node.label.clone().unwrap_or_else(|| {
        fallback_name
            .map(|n| n.to_string())
            .unwrap_or_else(|| format!("Node {}", template_node.gltf_node_index))
    });

    let kind = if template_node.mesh_keys.is_empty() {
        NodeKind::Group
    } else {
        NodeKind::Model(ModelRef {
            asset_id,
            node_index: template_node.gltf_node_index,
            primitive_index: None,
        })
    };

    let trs = transform_to_trs(&template_node.local);
    let node = Node::new_with_transform_and_kind(name, trs, kind);

    // Children inherit the file's asset id but use their own gltf labels;
    // the synthetic fallback name only applies at the very top.
    for child in &template_node.children {
        let child_node = build_editor_subtree(child, asset_id, None);
        node.children.lock_mut().push_cloned(child_node);
    }
    node
}

fn transform_to_trs(t: &awsm_renderer::transforms::Transform) -> Trs {
    Trs {
        translation: t.translation.to_array(),
        rotation: t.rotation.to_array(),
        scale: t.scale.to_array(),
    }
}

/// If exactly one node is *explicitly* selected (user clicked / picked /
/// nav'd to it), insert under it. Otherwise — and crucially when the
/// selection is leftover from the previous Insert auto-select — insert
/// at the top level.
fn parent_for_insert(state: &crate::state::AppState) -> Option<NodeId> {
    if !state.selection_is_explicit.get() {
        return None;
    }
    let set = state.selected.lock_ref();
    if set.len() == 1 {
        set.iter().next().copied()
    } else {
        None
    }
}

pub fn empty() {
    insert_simple(|| Node::new_group("Empty"), "insert::empty");
}

pub fn light(kind: LightKind) {
    let kind_label = format!("{kind:?}");
    insert_simple(
        move || Node::new_light(kind),
        &format!("insert::light({kind_label})"),
    );
}

pub fn collision_box() {
    insert_simple(
        || Node::new_collision_box("Collider Box"),
        "insert::collision_box",
    );
}

pub fn collision_sphere() {
    insert_simple(
        || Node::new_collision_sphere("Collider Sphere"),
        "insert::collision_sphere",
    );
}

pub fn collision_capsule() {
    insert_simple(
        || Node::new_collision_capsule("Collider Capsule"),
        "insert::collision_capsule",
    );
}

pub fn collision_cylinder() {
    insert_simple(
        || Node::new_collision_cylinder("Collider Cylinder"),
        "insert::collision_cylinder",
    );
}

pub fn collision_cone() {
    insert_simple(
        || Node::new_collision_cone("Collider Cone"),
        "insert::collision_cone",
    );
}

pub fn collision_ellipsoid() {
    insert_simple(
        || Node::new_collision_ellipsoid("Collider Ellipsoid"),
        "insert::collision_ellipsoid",
    );
}

pub fn camera() {
    insert_simple(|| Node::new_camera("Camera"), "insert::camera");
}

// ─────────────────────────────────────────────────────────────────────
// Procedural-node insertions. Each helper inserts a default-constructed
// kind so the node renders out of the box (no asset round-trip needed);
// the inspector lets the user refine knobs after.
// ─────────────────────────────────────────────────────────────────────

pub fn primitive_plane() {
    insert_simple(
        || Node::new_primitive("Plane", awsm_scene_schema::PrimitiveShape::default_plane()),
        "insert::primitive_plane",
    );
}

pub fn primitive_box() {
    insert_simple(
        || Node::new_primitive("Box", awsm_scene_schema::PrimitiveShape::default_box()),
        "insert::primitive_box",
    );
}

pub fn primitive_sphere() {
    insert_simple(
        || {
            Node::new_primitive(
                "Sphere",
                awsm_scene_schema::PrimitiveShape::default_sphere(),
            )
        },
        "insert::primitive_sphere",
    );
}

pub fn primitive_cylinder() {
    insert_simple(
        || {
            Node::new_primitive(
                "Cylinder",
                awsm_scene_schema::PrimitiveShape::default_cylinder(),
            )
        },
        "insert::primitive_cylinder",
    );
}

pub fn primitive_cone() {
    insert_simple(
        || Node::new_primitive("Cone", awsm_scene_schema::PrimitiveShape::default_cone()),
        "insert::primitive_cone",
    );
}

pub fn primitive_torus() {
    insert_simple(
        || Node::new_primitive("Torus", awsm_scene_schema::PrimitiveShape::default_torus()),
        "insert::primitive_torus",
    );
}

pub fn curve() {
    insert_simple(|| Node::new_curve("Curve"), "insert::curve");
}

pub fn line() {
    insert_simple(|| Node::new_line("Line"), "insert::line");
}

pub fn sprite() {
    insert_simple(|| Node::new_sprite("Sprite"), "insert::sprite");
}

pub fn particle() {
    insert_simple(
        || Node::new_particle("Particle Emitter"),
        "insert::particle",
    );
}

pub fn sweep() {
    insert_simple(|| Node::new_sweep("Sweep"), "insert::sweep");
}

pub fn instances() {
    insert_simple(|| Node::new_instances("Instances"), "insert::instances");
}

pub fn mesh() {
    insert_simple(|| Node::new_mesh("Mesh"), "insert::mesh");
}

/// Create a new `AssetSource::Material(MaterialDef::default())` entry in
/// the project's asset table. Returns the fresh `AssetId` so a caller
/// can immediately point a node at it via `MaterialRef`.
///
/// The asset has no associated node — it's purely a sharable resource.
/// To edit its `MaterialDef`, use the future asset-inspector pane or
/// hand-edit `project.json` (the existing material inspector accepts a
/// `&mut MaterialDef` from any source).
/// Create a new `AssetSource::Texture(Procedural(...))` entry seeded
/// with a default Checker pattern. Returns the fresh `AssetId` so
/// callers can immediately bind it from `MaterialDef.base_color_texture`
/// or `SpriteDef.texture` etc.
pub fn texture_asset() -> Option<awsm_scene_schema::AssetId> {
    use awsm_scene_schema::{AssetEntry, AssetId, AssetSource, ProceduralTextureDef, TextureDef};
    let state = app_state();
    let previous = state.snapshot_scene();
    let id = AssetId::new();
    {
        let mut table = state.scene.assets.lock().unwrap();
        table.entries.insert(
            id,
            AssetEntry::new(AssetSource::Texture(TextureDef::Procedural(
                ProceduralTextureDef::Checker {
                    width: 256,
                    height: 256,
                    cells_x: 8,
                    cells_y: 8,
                    color_a: [0.1, 0.1, 0.1, 1.0],
                    color_b: [0.9, 0.9, 0.9, 1.0],
                },
            ))),
        );
    }
    state.scene.bump_revision();
    state.commit_history(previous);
    tracing::info!("action: insert::texture_asset({id}) — done");
    awsm_web_shared::prelude::Toast::info("New Texture asset created (Checker)");
    Some(id)
}

/// File-backed texture asset. Reads the picked image's bytes into
/// `pending_assets` and creates an `AssetSource::Texture(Raster {
/// filename })` entry. Save flushes the bytes to
/// `assets/<filename>` (same convention as the glTF-extractor's
/// raster entries). The texture cache decodes via the `image` crate
/// on first bind, so the resulting AssetId works anywhere a
/// `TextureRef` is accepted — material slots, sprites, particles.
///
/// Content-addressed: the on-disk path is `assets/<hash>.<ext>`,
/// where the hash dedups identical bytes within the project. Picking
/// the same file twice resolves to the same `AssetId` and toasts
/// "already exists" instead of clobbering.
pub fn texture_asset_from_file(file: File) {
    crate::loading_modal::open("Adding Texture", format!("Reading {}…", file.name()));
    spawn_local(async move {
        let result = prepare_texture_from_file(file).await;
        crate::loading_modal::close();
        match result {
            Ok(AddTextureResult::Added { display_name }) => {
                awsm_web_shared::prelude::Toast::info(format!("Added texture: {display_name}"));
            }
            Ok(AddTextureResult::Existing { display_name }) => {
                awsm_web_shared::prelude::Toast::info(format!(
                    "Texture already exists: {display_name}"
                ));
            }
            Err(err) => {
                tracing::error!("Add Texture failed: {err}");
                Modal::error(format!("Add Texture failed: {err}"));
            }
        }
    });
}

enum AddTextureResult {
    Added { display_name: String },
    Existing { display_name: String },
}

async fn prepare_texture_from_file(file: File) -> anyhow::Result<AddTextureResult> {
    use awsm_scene_schema::{AssetEntry, AssetSource, TextureDef};
    let state = app_state();
    let display_name = file.name();
    if display_name.is_empty() {
        anyhow::bail!("The chosen file has no name");
    }
    let buffer = JsFuture::from(file.array_buffer())
        .await
        .map_err(|err| anyhow::anyhow!("reading file: {:?}", err))?;
    let buffer: js_sys::ArrayBuffer = buffer
        .dyn_into()
        .map_err(|_| anyhow::anyhow!("file.arrayBuffer() did not return an ArrayBuffer"))?;
    let array = Uint8Array::new(&buffer);
    let mut bytes = vec![0u8; array.length() as usize];
    array.copy_to(&mut bytes);
    let content_hash = crate::content_hash::sha256_hex(&bytes);

    // Dedup: if any entry already carries this hash, return its
    // display name without touching the table or pending bytes.
    if let Some(existing_id) = state
        .scene
        .assets
        .lock()
        .unwrap()
        .find_by_content_hash(&content_hash)
    {
        let existing_name = state
            .scene
            .assets
            .lock()
            .unwrap()
            .display_name(existing_id)
            .map(|s| s.to_string())
            .unwrap_or_else(|| display_name.clone());
        tracing::info!(
            "action: insert::texture_asset_from_file — dedup hit ({existing_id}, {existing_name})"
        );
        return Ok(AddTextureResult::Existing {
            display_name: existing_name,
        });
    }

    let previous = state.snapshot_scene();
    let id = AssetId::new();
    state.pending_assets.lock().unwrap().insert(id, bytes);
    {
        let mut table = state.scene.assets.lock().unwrap();
        table.entries.insert(
            id,
            AssetEntry::new_with_hash(
                AssetSource::Texture(TextureDef::Raster {
                    display_name: display_name.clone(),
                }),
                content_hash,
            ),
        );
    }
    state.scene.bump_revision();
    state.commit_history(previous);
    tracing::info!("action: insert::texture_asset_from_file({id}, {display_name}) — done");
    Ok(AddTextureResult::Added { display_name })
}

pub fn material_asset() -> Option<awsm_scene_schema::AssetId> {
    use awsm_scene_schema::{AssetEntry, AssetId, AssetSource, MaterialDef};
    let state = app_state();
    let previous = state.snapshot_scene();
    let id = AssetId::new();
    {
        let mut table = state.scene.assets.lock().unwrap();
        table.entries.insert(
            id,
            AssetEntry::new(AssetSource::Material(MaterialDef::default())),
        );
    }
    state.scene.bump_revision();
    state.commit_history(previous);
    tracing::info!("action: insert::material_asset({id}) — done");
    awsm_web_shared::prelude::Toast::info("New Material asset created");
    Some(id)
}

fn insert_simple(make_node: impl FnOnce() -> Arc<Node>, op_label: &str) {
    let state = app_state();
    let previous = state.snapshot_scene();
    let parent_id = parent_for_insert(&state);
    let node = make_node();
    let node_id = node.id;
    if !insert_under(&state.scene, parent_id, node) {
        tracing::error!("{op_label}: failed to insert");
        Modal::error("Failed to insert node (parent may have been removed).");
        return;
    }
    state.scene.bump_revision();
    // Implicit so successive `Insert > Empty` clicks stay flat instead
    // of nesting each new node inside the previous one.
    state.select_only_implicit(node_id);
    state.commit_history(previous);
    tracing::info!("action: {op_label} — done");
}

/// Walk the glTF in `bytes`, build a `TextureDef::Raster` asset for
/// each embedded image, a `MaterialDef` asset for each material, and
/// stamp the resulting `material_index → AssetId` map onto the gltf
/// asset's entry so `instance_template` can override the renderer-
/// baked materials per primitive.
///
/// glTF features handled:
/// - PBR scalars/colors (baseColorFactor / metallic / roughness / emissive)
/// - Double-sided + alpha mode (Opaque / Mask{cutoff} / Blend)
/// - Textures: baseColor, metallicRoughness, normal, occlusion, emissive
///
/// glTF features that fall back to the renderer-baked material until
/// `MaterialDef` grows support: clearcoat / sheen / transmission /
/// volume / iridescence / specular / anisotropy / KHR_materials_unlit,
/// plus the `KHR_texture_transform` per-texture xform / non-zero
/// `texCoord` set indices. Images supplied via external `uri` (rather
/// than an embedded glb buffer view) are also skipped — gltf models
/// in the wild are almost always glb so this is rarely load-bearing.
///
/// Takes the asset table + pending-bytes map as explicit arguments so
/// callers can drive it either against the live scene state (Insert
/// Model) or against a `SceneSnapshot` (project Load, before
/// `apply_to`).
pub(crate) fn extract_gltf_materials_into(
    assets: &mut awsm_scene_schema::AssetTable,
    pending_assets: &Mutex<std::collections::HashMap<AssetId, Vec<u8>>>,
    gltf_asset_id: AssetId,
    display_name: &str,
    bytes: &[u8],
) {
    use awsm_scene_schema::{
        AssetEntry, AssetSource, MaterialAlphaMode, MaterialDef, TextureDef, TextureRef,
    };

    let gltf = match gltf::Gltf::from_slice(bytes) {
        Ok(g) => g,
        Err(err) => {
            tracing::warn!("extract_gltf_materials({display_name}): parse failed: {err}");
            return;
        }
    };

    let blob = gltf.blob.clone().unwrap_or_default();
    let document = gltf.document;

    // ── Pass 1: each gltf image → Texture::Raster asset ────────────────
    //
    // We extract by *image index* (not texture index), because the same
    // image can back multiple gltf textures with different samplers; one
    // editor Texture asset per image keeps the assets library tidy
    // without losing fidelity.
    let mut image_to_texture_asset: std::collections::HashMap<usize, AssetId> =
        std::collections::HashMap::new();

    for image in document.images() {
        let img_idx = image.index();
        let (raw_bytes, ext) = match image.source() {
            gltf::image::Source::View { view, mime_type } => {
                let buffer_idx = view.buffer().index();
                // The glb's binary chunk is buffer 0; external .gltf
                // buffers aren't loaded here (we'd need an async fetch).
                // Skip with a debug log so the user can investigate.
                if buffer_idx != 0 {
                    tracing::debug!(
                        "extract_gltf_materials({display_name}): image {img_idx} \
                         references buffer {buffer_idx}, skipping (only buffer 0 \
                         is read from the glb blob)"
                    );
                    continue;
                }
                let start = view.offset();
                let len = view.length();
                if start + len > blob.len() {
                    tracing::warn!(
                        "extract_gltf_materials({display_name}): image {img_idx} \
                         buffer view {start}..{} exceeds blob len {}",
                        start + len,
                        blob.len()
                    );
                    continue;
                }
                (blob[start..start + len].to_vec(), mime_to_ext(mime_type))
            }
            gltf::image::Source::Uri { mime_type, .. } => {
                tracing::debug!(
                    "extract_gltf_materials({display_name}): image {img_idx} is \
                     URI-sourced (mime={mime_type:?}); skipping extraction (only \
                     embedded buffer views are read)"
                );
                continue;
            }
        };
        // Dedup: if the table already has an entry for these bytes
        // (because the user previously uploaded the same PNG via the
        // image picker, or another glTF embeds the same texture),
        // reuse its AssetId rather than creating a parallel entry.
        let extracted_label = format!("{display_name}__tex_{img_idx}.{ext}");
        let extracted_hash = crate::content_hash::sha256_hex(&raw_bytes);
        let texture_asset_id = if let Some(existing) = assets.find_by_content_hash(&extracted_hash)
        {
            existing
        } else {
            let id = AssetId::new();
            assets.entries.insert(
                id,
                AssetEntry::new_with_hash(
                    AssetSource::Texture(TextureDef::Raster {
                        display_name: extracted_label,
                    }),
                    extracted_hash,
                ),
            );
            // Park the encoded bytes in `pending_assets` keyed by the
            // texture's AssetId so `texture_cache::get_or_upload` can
            // pick them up at materialize time, and `save_inner` can
            // flush them to `assets/<hash>.<ext>` on save.
            pending_assets.lock().unwrap().insert(id, raw_bytes);
            id
        };
        image_to_texture_asset.insert(img_idx, texture_asset_id);
    }

    // ── Pass 2: each gltf material → MaterialDef asset ─────────────────
    let mut material_asset_ids: Vec<AssetId> = Vec::new();
    for material in document.materials() {
        let Some(mat_idx) = material.index() else {
            // glTF allows a primitive to have no material set — the
            // spec default material handles those at render time. We
            // skip generating an editable asset for that case; the
            // `instance_template` override is gated on `Some(idx)` so
            // those primitives keep the renderer's baked default.
            continue;
        };

        let pbr = material.pbr_metallic_roughness();
        let alpha_mode = match material.alpha_mode() {
            gltf::material::AlphaMode::Opaque => MaterialAlphaMode::Opaque,
            gltf::material::AlphaMode::Mask => MaterialAlphaMode::Mask {
                cutoff: material.alpha_cutoff().unwrap_or(0.5),
            },
            gltf::material::AlphaMode::Blend => MaterialAlphaMode::Blend,
        };

        let resolve_texture = |info_image_idx: usize| -> Option<TextureRef> {
            image_to_texture_asset
                .get(&info_image_idx)
                .copied()
                .map(TextureRef)
        };
        let base_color_texture = pbr
            .base_color_texture()
            .and_then(|info| resolve_texture(info.texture().source().index()));
        let metallic_roughness_texture = pbr
            .metallic_roughness_texture()
            .and_then(|info| resolve_texture(info.texture().source().index()));
        let normal_texture = material
            .normal_texture()
            .and_then(|info| resolve_texture(info.texture().source().index()));
        let occlusion_texture = material
            .occlusion_texture()
            .and_then(|info| resolve_texture(info.texture().source().index()));
        let emissive_texture = material
            .emissive_texture()
            .and_then(|info| resolve_texture(info.texture().source().index()));

        let def = MaterialDef {
            label: material
                .name()
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("{display_name} · material {mat_idx}")),
            base_color: pbr.base_color_factor(),
            base_color_texture,
            metallic: pbr.metallic_factor(),
            roughness: pbr.roughness_factor(),
            metallic_roughness_texture,
            emissive: material.emissive_factor(),
            emissive_texture,
            normal_texture,
            occlusion_texture,
            double_sided: material.double_sided(),
            vertex_colors_enabled: false,
            alpha_mode,
            ..MaterialDef::default()
        };

        let material_asset_id = AssetId::new();
        assets.entries.insert(
            material_asset_id,
            AssetEntry::new(AssetSource::Material(def)),
        );
        // The Vec is indexed by gltf material index, so we have to
        // pad if the document skips any (glTF docs typically pack
        // them densely from 0 but we don't assume).
        while material_asset_ids.len() < mat_idx {
            material_asset_ids.push(AssetId::default());
        }
        if material_asset_ids.len() == mat_idx {
            material_asset_ids.push(material_asset_id);
        } else {
            material_asset_ids[mat_idx] = material_asset_id;
        }
    }

    // ── Stamp the per-primitive lookup onto the gltf's AssetEntry ──────
    {
        if let Some(entry) = assets.entries.get_mut(&gltf_asset_id) {
            entry.gltf_material_asset_ids = material_asset_ids;
        }
    }

    tracing::info!(
        "extract_gltf_materials({display_name}): extracted {} materials, {} textures",
        document.materials().len(),
        image_to_texture_asset.len()
    );
}

/// Map a glTF image MIME type to a file-extension string used for the
/// generated `TextureDef::Raster.filename`. The on-disk file always
/// carries the original encoded bytes (PNG or JPEG; we don't transcode),
/// so the extension preserves the format.
fn mime_to_ext(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        _ => "bin",
    }
}
