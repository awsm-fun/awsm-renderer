//! GLB export — lower the live editor scene (or one subtree) to a baked
//! [`awsm_renderer_glb_export::GlbScene`] and serialize it to a `.glb`.
//!
//! This is the standalone "get geometry out" path behind `Request::ExportGlb`
//! (the `EditorController::export_glb_bytes` side-channel) and the
//! `export_scene_glb` / `export_node_glb` MCP tools. The whole-runtime player
//! publish (Phase 6) reuses the same `GlbScene` IR + `write_glb`.
//!
//! ## Material policy (locked)
//! - assigned/inline **PBR** → glTF PBR; **Unlit** → `KHR_materials_unlit`;
//! - custom-WGSL or **Toon** → `AWSM_materials_none` (no embedded material; the
//!   scene/player re-binds the real material on import via the carried id).
//!
//! ## Textures (referenced-only)
//! Export embeds exactly the images the *assigned* materials reference: procedural
//! textures are regenerated + PNG-encoded on the spot; raster textures are read
//! back from the GPU (`texture_png_bytes`). Unreferenced textures are never
//! carried — so reassigning a lighter material drops the heavy ones with no flag.
//! Raster textures not yet uploaded to the GPU are skipped (the material keeps its
//! factors). This is why export is **async**.
//!
//! ## Imported-glTF geometry
//! Imported models are baked into captured `NodeKind::Mesh` nodes at import (their
//! geometry lives in the [`mesh_cache`] store, like every other procedural mesh),
//! so export reads them through the normal Mesh path with no special handling.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use awsm_renderer_editor_protocol::animation::{TrackTarget, TrackValue, TransformProp};
use awsm_renderer_editor_protocol::dynamic_material::MaterialInstance;
use awsm_renderer_editor_protocol::{
    AssetId, AssetSource, CameraConfig, CameraProjection, CrossSectionDef, LightConfig,
    MaterialAlphaMode, MaterialDef, MaterialShading, NodeId, NodeKind, SweepAlongCurveDef,
    SweepUvMode, TextureColorKind, TextureDef, TextureExport, TextureRef,
};
use awsm_renderer_glb_export::{
    write_glb, AlphaMode, AnimInterp, AnimPath, ExportAnimChannel, ExportAnimation, ExportCamera,
    ExportImage, ExportLight, ExportMaterial, ExportNode, GlbScene, ImageMime, MeshData,
    PbrMaterial, TexRef, Trs, UnlitMaterial,
};

use crate::engine::bridge::{material as bridge_material, mesh_cache};
use crate::engine::scene::{mutate, node::Node, Scene};

/// Maps a referenced texture asset → its index in `GlbScene::images`.
type TexIndex = HashMap<AssetId, usize>;

/// Bake the whole scene **including animations** (clips lowered to glTF TRS
/// channels) — the path behind `ExportGlb { node: None }` and the player bundle.
pub async fn export_scene_glb(ctrl: &super::EditorController) -> Result<Vec<u8>, String> {
    let scene = &ctrl.scene;
    let roots: Vec<Arc<Node>> = scene.nodes.lock_ref().iter().cloned().collect();
    let (images, tex_index) = resolve_images(scene, &roots).await;
    // Rig embedding (shared with export_glb): appended AFTER the scene part,
    // so the clip channels lowered against build_index_map's node indices
    // stay valid (appending never shifts existing DFS indices).
    let (rig_scenes, rig_embedded) = collect_rig_scenes(&roots);
    let nodes: Vec<ExportNode> = roots
        .iter()
        .map(|n| node_to_export(scene, n, &tex_index, &rig_embedded))
        .collect();
    let index_map = build_index_map(scene);
    let clips: Vec<_> = ctrl.custom_animations.lock_ref().iter().cloned().collect();
    let animations = lower_clips(&clips, &index_map);
    let mut glb = GlbScene {
        nodes,
        animations,
        images,
        ..Default::default()
    };
    append_rigs(&mut glb, rig_scenes);
    Ok(write_glb(&glb))
}

/// Bake the live scene to a **player bundle directory** (`scene.toml` + an
/// `assets/` directory) — the runtime form per the glb-mesh design, replacing the
/// old single-`scene.glb` bundle.
///
/// Emits: `scene.toml` (the runtime `Scene` from `project_to_scene` — nodes /
/// transforms / material-instances / lights / cameras / our-clips / env, meshes
/// by id); `assets/<id>.glb` (one geometry-only glb per mesh that lowered to
/// `RuntimeMesh::Glb` — bare primitives stay procedural in `scene.toml`; no
/// materials/animations in the glb, those are ours); `assets/materials/<name>/…`
/// (custom-material wgsl + sidecars); `assets/<id>.png` (referenced textures).
///
/// Skinned/morph meshes re-export a clean rig glb from their source (skeleton +
/// mesh + skin + morph, built at import via `reexport_clean_scene`); the
/// `scene.toml` SkinnedMesh nodes reference it by `skin.source` → `assets/<source>.glb`.
pub async fn bake_player_bundle(
    ctrl: &super::EditorController,
    options_override: Option<awsm_renderer_editor_protocol::BundleOptions>,
) -> Result<Vec<awsm_renderer_editor_protocol::BundleFile>, String> {
    use awsm_renderer_editor_protocol::{
        assemble_bundle, mesh_glb_filename, BundleFile, RuntimeMesh,
    };
    use awsm_renderer_editor_protocol::{lower_mesh, project_to_scene};

    let project = crate::controller::persistence::to_editor_project(ctrl);
    // Project-persisted export options, with an optional per-call override
    // (MCP `export_player_bundle`) that does NOT touch the persisted value.
    let bundle_options = options_override.unwrap_or(project.bundle_options);
    let compress = compress_options(&bundle_options);
    let mut scene = project_to_scene(&project);
    // Flatten each node's built-in assignment to the MERGED def the editor
    // actually renders with (`builtin_merged`: shared variant ∪ per-mesh
    // inline). Built-in LIBRARY defs don't travel in the bundle — the player
    // reads each node's `inline` as a complete, self-contained MaterialDef —
    // but the inline seed is taken at ASSIGN time, so library edits made
    // after assignment (typically the texture bindings of an art pass) never
    // reach it: the bundle carried the label/factors/extensions yet DROPPED
    // the texture refs, and textured library materials played back untextured.
    flatten_builtin_materials(&mut scene.nodes);
    let mut files: Vec<BundleFile> = Vec::new();

    // 0. Custom-material BUFFER overrides → `assets/<asset>.bin`. Each override
    //    references a content-addressed buffer asset whose words live in the
    //    session `buffer_cache`; emit them keyed by asset id (mirroring how
    //    textures emit `assets/<id>.png`). No ref rewrite needed — the player
    //    fetches `assets/<bref.asset>.bin` directly.
    emit_buffer_overrides(&scene.nodes, &mut files);

    // Mesh assets whose referencing nodes opt **in** to LOD (default on). The
    // toggle is per-node but geometry/levels are per-asset, so an asset gets LOD
    // baked if *any* node using it is LOD-enabled; the per-instance toggle then
    // governs runtime level selection (an opted-out instance pins level 0).
    let mut lod_assets: HashSet<AssetId> = HashSet::new();
    collect_lod_static_assets(&project.nodes, &mut lod_assets);

    // 1. One geometry-only glb per Glb-lowered mesh asset (+ discrete LOD levels
    //    for LOD-enabled, above-floor static meshes) — DEDUPED BY CONTENT.
    //
    //    Duplicated nodes each own a distinct mesh asset with byte-identical
    //    baked geometry (e.g. a floor of 40 duplicated tiles ships the same
    //    ~13 KB glb 40 times without this). Group Glb-lowered assets by their
    //    baked glb bytes; ONE canonical file set (base glb + LOD chain +
    //    clusters) ships per group, `scene.toml` Mesh refs rewrite to the
    //    canonical id, and the duplicate entries drop from the baked asset
    //    table. Materials / transforms are per-node, so collapsing geometry
    //    ids is invisible at runtime. The canonical is the group's lowest
    //    asset id so repeated exports stay byte-stable; LOD is baked for the
    //    canonical when ANY member's nodes opted in. Byte-equality compare
    //    (no hashing): the asset count is small and equality short-circuits.
    struct MeshBake {
        id: AssetId,
        glb: Vec<u8>,
        mesh: MeshData,
        lod_wanted: bool,
    }
    let mut glb_mesh_ids: Vec<AssetId> = project
        .assets
        .entries
        .iter()
        .filter(|(_, entry)| {
            matches!(&entry.source, AssetSource::Mesh(def)
                if matches!(lower_mesh(def), RuntimeMesh::Glb))
        })
        .map(|(id, _)| *id)
        .collect();
    glb_mesh_ids.sort_by_key(|id| id.0);
    let mut canonicals: Vec<MeshBake> = Vec::new();
    let mut mesh_remap: HashMap<AssetId, AssetId> = HashMap::new();
    for id in glb_mesh_ids {
        let Some(raw) = mesh_cache::get_raw(id) else {
            continue;
        };
        let mesh = MeshData {
            positions: raw.positions,
            normals: raw.normals,
            uvs: raw.uv_sets,
            colors: raw.colors,
            indices: raw.indices,
        };
        let glb = write_glb(&GlbScene {
            nodes: vec![ExportNode::new("mesh").with_mesh(mesh.clone())],
            ..Default::default()
        });
        let lod_wanted = lod_assets.contains(&id);
        match canonicals.iter_mut().find(|c| c.glb == glb) {
            Some(canon) => {
                canon.lod_wanted |= lod_wanted;
                mesh_remap.insert(id, canon.id);
            }
            None => canonicals.push(MeshBake {
                id,
                glb,
                mesh,
                lod_wanted,
            }),
        }
    }
    for canon in canonicals {
        let lod_files = if canon.lod_wanted {
            crate::controller::lod_bake::bake_static_lod(
                &canon.id.0.to_string(),
                &canon.mesh,
                &compress,
            )
        } else {
            Vec::new()
        };
        // Cluster-LOD DAG (Phase B) for dense static meshes; consumed
        // at load only when the `virtual_geometry` feature is on.
        let cluster_files =
            crate::controller::lod_bake::bake_static_clusters(&canon.id.0.to_string(), &canon.mesh);
        // Bundle meshes ship under the project's BundleOptions (default
        // meshopt + Smart quantization — docs/plans/compression.md); the
        // canonical DEDUP above stays on the uncompressed bytes. A failed
        // compression falls back to the plain glb — never fail a bake.
        let mesh_glb = match awsm_renderer_glb_export::compress_glb_with(&canon.glb, &compress) {
            Ok(compressed) => {
                tracing::info!(
                    "bundle mesh {}: {} -> {} bytes",
                    canon.id,
                    canon.glb.len(),
                    compressed.len()
                );
                compressed
            }
            Err(e) => {
                tracing::warn!(
                    "bundle mesh {}: compression failed ({e}); shipping uncompressed",
                    canon.id
                );
                canon.glb
            }
        };
        files.push(BundleFile::asset(mesh_glb_filename(canon.id), mesh_glb));
        files.extend(lod_files);
        files.extend(cluster_files);
    }
    if !mesh_remap.is_empty() {
        remap_mesh_refs(&mut scene.nodes, &mesh_remap);
        for dup in mesh_remap.keys() {
            scene.assets.entries.remove(dup);
        }
        tracing::info!(
            "bundle bake: deduped {} identical mesh asset(s) onto shared geometry files",
            mesh_remap.len()
        );
    }

    // 2. Custom-material folders. `material_files` already returns paths rooted
    //    under `assets/` (e.g. `assets/materials/<slug>-<id>/material.wgsl`), so
    //    they go in verbatim — prepending `ASSETS_DIR` here would double it to
    //    `assets/assets/materials/…`.
    for (path, contents) in crate::controller::persistence::material_files(ctrl) {
        files.push(BundleFile::new(path, contents.into_bytes()));
    }

    // 3. Textures the materials reference, resolved PER USE
    //    (docs/plans/compression.md F2: use override > per-texture pref >
    //    slot-based Auto > global). Distinct resolved KTX2 codecs of one asset
    //    become distinct artifacts: the most-used encoding keeps the asset's
    //    id (so unrewritten stragglers still load something sane), the rest
    //    mint DETERMINISTIC variant ids (`AssetId::derive_variant`) whose
    //    entries join the baked asset table and whose refs are rewritten —
    //    the player just loads `assets/<variant>.ktx2` like any texture.
    let roots: Vec<Arc<Node>> = ctrl.scene.nodes.lock_ref().iter().cloned().collect();
    {
        use awsm_renderer_editor_protocol::{resolve_texture_use, ResolvedTextureUse};

        // Custom-material slot semantics: (material id, slot name) → color
        // kind; plus which library materials are built-in (their baked refs
        // live in the flattened `instance.inline`, custom refs in
        // `instance.texture_overrides` — mirrors the old collectors).
        let mut builtin_materials: HashSet<AssetId> = HashSet::new();
        let mut custom_slot_kinds: HashMap<(AssetId, String), TextureColorKind> = HashMap::new();
        for m in ctrl.custom_materials.lock_ref().iter() {
            if m.builtin.get_cloned().is_some() {
                builtin_materials.insert(m.id);
            } else {
                for slot in m.textures.lock_ref().iter() {
                    custom_slot_kinds.insert((m.id, slot.name.clone()), slot.color_kind);
                }
            }
        }

        // One walk primitive over the BAKED scene's texture uses (the
        // authoritative post-flatten refs — rewrites here land in scene.toml).
        fn for_each_baked_texture_use(
            nodes: &mut [awsm_renderer_editor_protocol::EditorNode],
            builtin_materials: &HashSet<AssetId>,
            custom_slot_kinds: &HashMap<(AssetId, String), TextureColorKind>,
            // (slot kind, is-a-BUILT-IN-material use, the ref). Built-in
            // normal slots are the only two-channel packing candidates —
            // custom-WGSL materials sample with user-authored code that
            // can't Z-reconstruct.
            f: &mut impl FnMut(TextureColorKind, bool, &mut awsm_renderer_editor_protocol::TextureRef),
        ) {
            for node in nodes {
                if let Some(variants) = node.kind.material_variants_mut() {
                    for v in variants {
                        if builtin_materials.contains(&v.instance.asset) {
                            v.instance
                                .inline
                                .for_each_texture_use_mut(|k, t| f(k, true, t));
                        } else {
                            for (name, t) in v.instance.texture_overrides.iter_mut() {
                                let kind = custom_slot_kinds
                                    .get(&(v.instance.asset, name.clone()))
                                    .copied()
                                    .unwrap_or_default();
                                f(kind, false, t);
                            }
                        }
                    }
                }
                // Sprite / decal / particle-emitter textures live in their own
                // `texture` field (NOT a material) — they were silently dropped
                // from bundles before this walk visited them, so the player
                // rendered them untextured. They are always sRGB albedo color.
                match &mut node.kind {
                    NodeKind::Sprite(def) => {
                        if let Some(t) = def.texture.as_mut() {
                            f(TextureColorKind::Albedo, false, t);
                        }
                    }
                    NodeKind::Decal(def) => {
                        if let Some(t) = def.texture.as_mut() {
                            f(TextureColorKind::Albedo, false, t);
                        }
                    }
                    NodeKind::ParticleEmitter(def) => {
                        if let Some(t) = def.texture.as_mut() {
                            f(TextureColorKind::Albedo, false, t);
                        }
                    }
                    _ => {}
                }
                for_each_baked_texture_use(
                    &mut node.children,
                    builtin_materials,
                    custom_slot_kinds,
                    f,
                );
            }
        }

        // Pass A — resolve every use, group per asset. `Ktx2Key` = the codec
        // params a KTX2 artifact is encoded with; `asset_level` marks assets
        // with ≥1 non-KTX2 use (those keep the original id for the
        // asset-level artifact, so every KTX2 group mints a variant).
        // The packed flag marks TWO-CHANNEL normal data (X→RGB, Y→A; F3) —
        // only built-in normal-slot uses opt in.
        type Ktx2Key = (bool, bool, bool); // (uastc, srgb, packed)
        let mut ktx2_uses: HashMap<AssetId, HashMap<Ktx2Key, usize>> = HashMap::new();
        let mut asset_level: HashMap<AssetId, TextureExport> = HashMap::new();
        let per_texture_pref = |asset: AssetId| {
            project
                .assets
                .entries
                .get(&asset)
                .and_then(|e| e.texture_export)
        };
        let global = bundle_options.texture_compression;
        for_each_baked_texture_use(
            &mut scene.nodes,
            &builtin_materials,
            &custom_slot_kinds,
            &mut |kind, builtin, tref| match resolve_texture_use(
                tref.export_profile,
                per_texture_pref(tref.asset),
                kind,
                global,
            ) {
                ResolvedTextureUse::Ktx2 { uastc, srgb } => {
                    let packed = builtin && kind == TextureColorKind::Normal;
                    *ktx2_uses
                        .entry(tref.asset)
                        .or_default()
                        .entry((uastc, srgb, packed))
                        .or_default() += 1;
                }
                ResolvedTextureUse::AssetLevel(pref) => {
                    asset_level.insert(tref.asset, pref);
                }
            },
        );

        // Artifact assignment: (asset, key) → artifact id. The primary KTX2
        // group (most uses; ties by key) keeps the original id unless an
        // asset-level artifact already claims it.
        let mut artifacts: HashMap<(AssetId, Ktx2Key), AssetId> = HashMap::new();
        for (&asset, keys) in &ktx2_uses {
            let mut ordered: Vec<(Ktx2Key, usize)> = keys.iter().map(|(k, n)| (*k, *n)).collect();
            ordered.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            let has_asset_level = asset_level.contains_key(&asset);
            for (index, (key, _count)) in ordered.into_iter().enumerate() {
                let artifact = if index == 0 && !has_asset_level {
                    asset
                } else {
                    asset.derive_variant(&format!(
                        "ktx2-u{}-s{}{}",
                        key.0 as u8,
                        key.1 as u8,
                        if key.2 { "-n2" } else { "" }
                    ))
                };
                artifacts.insert((asset, key), artifact);
            }
        }

        // Pass B — rewrite refs to their artifact ids (and strip the
        // authoring-only per-use override from the baked doc). Resolution is
        // pure, so pass A and this pass agree per use.
        for_each_baked_texture_use(
            &mut scene.nodes,
            &builtin_materials,
            &custom_slot_kinds,
            &mut |kind, builtin, tref| {
                if let ResolvedTextureUse::Ktx2 { uastc, srgb } = resolve_texture_use(
                    tref.export_profile,
                    per_texture_pref(tref.asset),
                    kind,
                    global,
                ) {
                    let packed = builtin && kind == TextureColorKind::Normal;
                    tref.asset = artifacts[&(tref.asset, (uastc, srgb, packed))];
                }
                tref.export_profile = None;
            },
        );

        // Pass C — encode one artifact per (asset, encoding), deterministic
        // order. Source bytes are always fetched by the ORIGINAL asset id
        // (variant ids exist only in the baked output).
        enum ArtifactEncode {
            Ktx2 {
                uastc: bool,
                srgb: bool,
                packed: bool,
            },
            Asset(TextureExport),
        }
        let mut bake_list: Vec<(AssetId, AssetId, ArtifactEncode)> = Vec::new();
        for ((asset, key), artifact) in &artifacts {
            bake_list.push((
                *artifact,
                *asset,
                ArtifactEncode::Ktx2 {
                    uastc: key.0,
                    srgb: key.1,
                    packed: key.2,
                },
            ));
        }
        for (asset, pref) in &asset_level {
            bake_list.push((*asset, *asset, ArtifactEncode::Asset(*pref)));
        }
        bake_list.sort_by_key(|(artifact, ..)| artifact.0);

        for (artifact_id, source_id, encode) in bake_list {
            // Referenced by a material ⇒ MUST ship, losslessly, or the export
            // fails. No quiet skip, no lossy fallback (see `texture_source_bytes`).
            let (_name, bytes, mime) =
                texture_source_bytes(&ctrl.scene, source_id).ok_or_else(|| {
                    format!(
                        "bundle texture {source_id}: no original source bytes in the session \
                         cache — re-import the texture (or reload the saved project) and \
                         re-export"
                    )
                })?;
            // Encode under the artifact's resolved parameters. Failures warn
            // and fall back (KTX2 → lossless WebP → source bytes), recording
            // whatever actually shipped — a bake never silently drops a texture.
            // `two_channel` records that the shipped bytes really ARE the
            // packed normal encode (fallbacks/passthrough ship unpacked
            // source-derived bytes, so they must not set the shader flag).
            let (encoding, bytes, two_channel) = match encode {
                ArtifactEncode::Asset(TextureExport::Source) => {
                    (texture_encoding_from_mime(mime), bytes, false)
                }
                ArtifactEncode::Asset(TextureExport::WebpLossless) => {
                    match encode_webp_lossless(&bytes, mime) {
                        Some(webp) => (awsm_renderer_scene::TextureEncoding::Webp, webp, false),
                        None => {
                            tracing::warn!(
                                "bundle texture {artifact_id}: lossless WebP encode failed — \
                                 shipping source {}",
                                mime.ext()
                            );
                            (texture_encoding_from_mime(mime), bytes, false)
                        }
                    }
                }
                ArtifactEncode::Asset(TextureExport::WebpLossy { quality }) => {
                    match encode_webp(&bytes, mime.as_str(), quality as f64).await {
                        Some(webp) => (awsm_renderer_scene::TextureEncoding::Webp, webp, false),
                        None => {
                            tracing::warn!(
                                "bundle texture {artifact_id}: lossy WebP encode failed — \
                                 shipping source {}",
                                mime.ext()
                            );
                            (texture_encoding_from_mime(mime), bytes, false)
                        }
                    }
                }
                // Asset-level KTX2 can't occur (a KTX2 pref resolves per use),
                // but route it through the per-use encoder as slot-neutral
                // color if it ever does.
                ArtifactEncode::Asset(TextureExport::Ktx2 { .. }) | ArtifactEncode::Ktx2 { .. } => {
                    let (uastc, srgb, packed) = match encode {
                        ArtifactEncode::Ktx2 {
                            uastc,
                            srgb,
                            packed,
                        } => (uastc, srgb, packed),
                        _ => (false, true, false),
                    };
                    use awsm_renderer_glb_export::ImageMime;
                    // Imported KTX2 → passthrough verbatim, regardless of
                    // profile — and UNPACKED (we can't re-swizzle a finished
                    // container), so the shader flag stays off.
                    if matches!(mime, ImageMime::Ktx2) {
                        (awsm_renderer_scene::TextureEncoding::Ktx2, bytes, false)
                    } else {
                        match decode_rgba(&bytes, mime) {
                            Some((mut rgba, w, h)) if w % 4 == 0 && h % 4 == 0 => {
                                // Two-channel normal packing (F3): X replicated
                                // into RGB, Y into A — the layout the Basis
                                // transcoder's BC5/EAC-RG11 targets pull their
                                // two planes from (the vendored encoder has no
                                // swizzle API, so pack CPU-side).
                                if packed {
                                    for px in rgba.chunks_exact_mut(4) {
                                        let (x, y) = (px[0], px[1]);
                                        px[0] = x;
                                        px[1] = x;
                                        px[2] = x;
                                        px[3] = y;
                                    }
                                }
                                let params = awsm_renderer_codec_basis::EncodeParams {
                                    uastc,
                                    // Per-USE colorspace: sRGB for color slots,
                                    // linear for data maps — the slot kind
                                    // decided this in resolution.
                                    srgb,
                                    mipmaps: true,
                                    quality: 190,
                                    zstd: true,
                                };
                                let client = BASIS_ENCODER.with(|c| c.clone());
                                match client.encode(&rgba, w, h, &params).await {
                                    Ok(ktx2) => {
                                        tracing::info!(
                                            "bundle texture {artifact_id}: {w}x{h} → KTX2 {}{} \
                                             ({} bytes){}",
                                            if uastc { "UASTC" } else { "ETC1S" },
                                            if packed { " two-channel-normal" } else { "" },
                                            ktx2.len(),
                                            if artifact_id != source_id {
                                                format!(" [variant of {source_id}]")
                                            } else {
                                                String::new()
                                            }
                                        );
                                        (awsm_renderer_scene::TextureEncoding::Ktx2, ktx2, packed)
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            "bundle texture {artifact_id}: KTX2 encode failed \
                                             ({e}) — falling back to lossless WebP"
                                        );
                                        match encode_webp_lossless(&bytes, mime) {
                                            Some(webp) => (
                                                awsm_renderer_scene::TextureEncoding::Webp,
                                                webp,
                                                false,
                                            ),
                                            None => {
                                                (texture_encoding_from_mime(mime), bytes, false)
                                            }
                                        }
                                    }
                                }
                            }
                            Some((_, w, h)) => {
                                // WebGPU requires block-compressed base dimensions
                                // to be multiples of 4 — fall back per plan.
                                tracing::info!(
                                    "bundle texture {artifact_id}: {w}x{h} not a multiple of 4 \
                                     — lossless WebP instead of KTX2"
                                );
                                match encode_webp_lossless(&bytes, mime) {
                                    Some(webp) => {
                                        (awsm_renderer_scene::TextureEncoding::Webp, webp, false)
                                    }
                                    None => (texture_encoding_from_mime(mime), bytes, false),
                                }
                            }
                            None => {
                                tracing::warn!(
                                    "bundle texture {artifact_id}: decode failed — shipping \
                                     source {}",
                                    mime.ext()
                                );
                                (texture_encoding_from_mime(mime), bytes, false)
                            }
                        }
                    }
                }
            };
            files.push(BundleFile::asset(
                format!("{artifact_id}.{}", encoding.ext()),
                bytes,
            ));
            if artifact_id == source_id {
                if let Some(entry) = scene.assets.entries.get_mut(&artifact_id) {
                    entry.texture_encoding = Some(encoding);
                    entry.texture_two_channel_normal = two_channel;
                }
            } else {
                // Mint the variant's baked asset-table entry: same source
                // (provenance/labels), no content hash (the bytes differ from
                // the original's), the encoding that actually shipped.
                let mut entry = scene
                    .assets
                    .entries
                    .get(&source_id)
                    .cloned()
                    .unwrap_or_else(|| {
                        awsm_renderer_scene::AssetEntry::new(
                            awsm_renderer_scene::AssetSource::Filename(format!("{source_id}")),
                        )
                    });
                entry.content_hash = String::new();
                entry.gltf_material_asset_ids = Vec::new();
                entry.gltf_image_asset_ids = Vec::new();
                entry.texture_encoding = Some(encoding);
                entry.texture_two_channel_normal = two_channel;
                scene.assets.entries.insert(artifact_id, entry);
            }
        }
    }

    // 4. Skinned meshes: one clean rig glb (skeleton + mesh + skin + morph, built
    // at import via reexport_clean_scene) per imported source. The scene.toml
    // SkinnedMesh nodes reference `skin.source` → `assets/<source>.glb`.
    // `out` = every skinned source (always emitted); `lod` = the subset whose
    // referencing nodes are LOD-enabled (also gets a simplified level chain).
    fn collect_skinned(node: &Node, out: &mut HashSet<AssetId>, lod: &mut HashSet<AssetId>) {
        if let NodeKind::SkinnedMesh { skin, lod: cfg, .. } = &node.kind.get_cloned() {
            out.insert(skin.source);
            if cfg.enabled {
                lod.insert(skin.source);
            }
        }
        for c in node.children.lock_ref().iter() {
            collect_skinned(c, out, lod);
        }
    }
    let mut skinned_sources: HashSet<AssetId> = HashSet::new();
    let mut lod_skinned: HashSet<AssetId> = HashSet::new();
    for n in &roots {
        collect_skinned(n, &mut skinned_sources, &mut lod_skinned);
    }
    for &src in &skinned_sources {
        if let Some(glb) = crate::engine::bridge::skinned_bake_cache::get_rig_glb(src) {
            // Bake LOD levels (from the rig glb bytes) before `glb` is moved.
            let lod_files = if lod_skinned.contains(&src) {
                crate::controller::lod_bake::bake_skinned_lod(&src.0.to_string(), &glb, &compress)
            } else {
                Vec::new()
            };
            // The BUNDLE copy of the rig sheds its embedded materials/images
            // (the player applies scene.toml materials to every rig primitive
            // — the bundle already ships the textures as assets/*.ktx2, so
            // the embedded copies were pure duplication that even got
            // transcoded-and-dropped at load) and then compresses under the
            // project's BundleOptions like every other bundle mesh. The strip
            // stays UNCONDITIONAL (dead bytes, nothing to configure). The
            // SAVE-format rig in the project stays untouched. Fallbacks never
            // fail a bake.
            let bundle_rig =
                awsm_renderer_glb_export::strip_materials_and_images(&glb).and_then(|stripped| {
                    awsm_renderer_glb_export::compress_glb_with(&stripped, &compress)
                });
            let bundle_rig = match bundle_rig {
                Ok(out) => {
                    tracing::info!(
                        "bundle rig {src}: {} -> {} bytes (stripped + compressed)",
                        glb.len(),
                        out.len()
                    );
                    out
                }
                Err(e) => {
                    tracing::warn!(
                        "bundle rig {src}: strip/compress failed ({e}); shipping original"
                    );
                    glb
                }
            };
            files.push(BundleFile::asset(
                awsm_renderer_editor_protocol::mesh_glb_filename(src),
                bundle_rig,
            ));
            files.extend(lod_files);
        }
    }

    // 5. View-only cluster ("nanite") meshes: the pre-baked DAG per `ClusterMesh`
    //    node, read from the session-local `cluster_cache`. `cluster_files` returns
    //    paths already rooted at `assets/<source>.clusters.bin` — the SAME name the
    //    runtime `NodeKind::ClusterMesh` arm fetches — so they go in verbatim.
    for (path, bytes) in crate::controller::persistence::cluster_files(ctrl) {
        files.push(BundleFile::new(path, bytes));
    }

    // 6. Baked-asset-table hygiene. The editor keeps `AssetSource::Filename`
    //    as import-provenance (a UI label; the editor's on-disk path derives
    //    from `content_hash`), but in a BAKED bundle that filename is the only
    //    path downstream tooling (CAS publishers re-hashing file-backed
    //    entries) can resolve the shipped bytes by. Two fixes:
    //    * a shipped skinned rig's entry is rewritten to name the file that
    //      ACTUALLY ships (`assets/<uuid>.glb`, step 4) instead of the
    //      original import filename (which is not in the bundle), and
    //    * Filename entries nothing references at runtime (e.g. the original
    //      combined glb of a fully static import — its geometry ships as
    //      per-mesh baked glbs) are dropped from the baked table entirely.
    //    Cluster sources keep their entries (their side files ship keyed by
    //    the asset id, `assets/<source>.clusters.bin`).
    {
        fn collect_cluster_sources(node: &Node, out: &mut HashSet<AssetId>) {
            if let NodeKind::ClusterMesh { cluster, .. } = &node.kind.get_cloned() {
                out.insert(cluster.source);
            }
            for c in node.children.lock_ref().iter() {
                collect_cluster_sources(c, out);
            }
        }
        let mut cluster_sources: HashSet<AssetId> = HashSet::new();
        for n in &roots {
            collect_cluster_sources(n, &mut cluster_sources);
        }
        let mut dropped = 0usize;
        scene.assets.entries.retain(|id, entry| {
            if !matches!(entry.source, awsm_renderer_scene::AssetSource::Filename(_)) {
                return true;
            }
            if skinned_sources.contains(id) {
                return true;
            }
            if cluster_sources.contains(id) {
                return true;
            }
            dropped += 1;
            false
        });
        for src in &skinned_sources {
            if let Some(entry) = scene.assets.entries.get_mut(src) {
                if matches!(entry.source, awsm_renderer_scene::AssetSource::Filename(_)) {
                    entry.source = awsm_renderer_scene::AssetSource::Filename(
                        awsm_renderer_editor_protocol::mesh_glb_filename(*src),
                    );
                }
            }
        }
        if dropped > 0 {
            tracing::info!(
                "bundle bake: dropped {dropped} import-provenance asset entr{}                  (no runtime reference)",
                if dropped == 1 { "y" } else { "ies" }
            );
        }
    }

    // 7. Environment skybox / IBL KTX2 cubemaps → the shared `env_ktx_path`
    //    convention (`assets/<id>.ktx2`) the player's `scene_loader::environment`
    //    fetches. STRICT (unlike Save's `ktx_files`): a KTX env id whose bytes
    //    can't be resolved FAILS the export instead of silently baking a bundle
    //    that plays with the built-in default environment. A gradient / built-in
    //    environment references no KTX and emits nothing here.
    files.extend(env_ktx_bundle_files(ctrl).await?);

    // 8. Completeness guard — "editor has it ⇒ the bundle ships it". Every
    //    texture the baked scene references (materials AND sprite/decal/particle)
    //    must have shipped as an asset file; a reference with no bytes renders
    //    untextured in the player, silently. Fail the export loudly instead.
    verify_texture_refs_shipped(&scene, &files)?;

    assemble_bundle(&scene, files).map_err(|e| e.to_string())
}

/// Bundle-completeness guard: every texture asset referenced by the baked
/// scene must appear as a shipped `assets/<id>.<ext>` file. A referenced
/// texture with no shipped bytes is silent content loss — the player's
/// on-demand `load_texture` 404s and the sprite/decal/material renders
/// untextured. This turns that into a loud, actionable export failure. Covers
/// material slots AND the sprite/decal/particle `texture` fields (whose refs
/// were the concrete gap this guard was written to backstop).
fn verify_texture_refs_shipped(
    scene: &awsm_renderer_editor_protocol::Scene,
    files: &[awsm_renderer_editor_protocol::BundleFile],
) -> Result<(), String> {
    // Asset ids that shipped: parse the `assets/<uuid>.<ext>` file stems.
    let shipped: HashSet<AssetId> = files
        .iter()
        .filter_map(|f| {
            let stem = f.path.strip_prefix("assets/")?.split('.').next()?;
            uuid::Uuid::parse_str(stem).ok().map(AssetId)
        })
        .collect();

    fn check(
        t: &TextureRef,
        shipped: &HashSet<AssetId>,
        seen: &mut HashSet<AssetId>,
        missing: &mut Vec<AssetId>,
    ) {
        if seen.insert(t.asset) && !shipped.contains(&t.asset) {
            missing.push(t.asset);
        }
    }

    fn walk(
        nodes: &[awsm_renderer_editor_protocol::EditorNode],
        shipped: &HashSet<AssetId>,
        seen: &mut HashSet<AssetId>,
        missing: &mut Vec<AssetId>,
    ) {
        for node in nodes {
            if let Some(variants) = node.kind.material_variants() {
                for v in variants {
                    for t in v.instance.inline.texture_refs() {
                        check(t, shipped, seen, missing);
                    }
                    for t in v.instance.texture_overrides.values() {
                        check(t, shipped, seen, missing);
                    }
                }
            }
            match &node.kind {
                NodeKind::Sprite(d) => {
                    if let Some(t) = &d.texture {
                        check(t, shipped, seen, missing);
                    }
                }
                NodeKind::Decal(d) => {
                    if let Some(t) = &d.texture {
                        check(t, shipped, seen, missing);
                    }
                }
                NodeKind::ParticleEmitter(d) => {
                    if let Some(t) = &d.texture {
                        check(t, shipped, seen, missing);
                    }
                }
                _ => {}
            }
            walk(&node.children, shipped, seen, missing);
        }
    }

    let mut seen = HashSet::new();
    let mut missing = Vec::new();
    walk(&scene.nodes, &shipped, &mut seen, &mut missing);

    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "bundle export incomplete: {} referenced texture(s) have no shipped bytes \
             ({}). They would render untextured in the player. This usually means their \
             source bytes aren't cached — re-import the texture(s) and export again.",
            missing.len(),
            missing
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }
}

/// Map the project-level [`BundleOptions`] mesh knobs onto the glb-export
/// codec options.
fn compress_options(
    options: &awsm_renderer_editor_protocol::BundleOptions,
) -> awsm_renderer_glb_export::CompressOptions {
    use awsm_renderer_editor_protocol::{MeshCompression, MeshQuantization};
    use awsm_renderer_glb_export::Quantization;
    awsm_renderer_glb_export::CompressOptions {
        meshopt: options.mesh_compression == MeshCompression::Meshopt,
        quantization: match options.mesh_quantization {
            MeshQuantization::Off => Quantization::Off,
            MeshQuantization::Always => Quantization::Always,
            MeshQuantization::Smart => Quantization::Smart {
                threshold_mm: options.smart_threshold_mm,
            },
        },
    }
}
/// Rewrite `NodeKind::Mesh` geometry refs in the baked node tree per the
/// content-dedup remap (duplicate asset id → the canonical id whose glb
/// actually ships). Only static `Mesh` nodes carry a [`MeshRef`]; skinned /
/// cluster sources are separate per-source files and are not remapped here.
fn remap_mesh_refs(
    nodes: &mut [awsm_renderer_editor_protocol::EditorNode],
    remap: &HashMap<AssetId, AssetId>,
) {
    for node in nodes {
        if let awsm_renderer_editor_protocol::NodeKind::Mesh { mesh, .. } = &mut node.kind {
            if let Some(canon) = remap.get(&mesh.0) {
                mesh.0 = *canon;
            }
        }
        remap_mesh_refs(&mut node.children, remap);
    }
}

/// Replace every assigned built-in `MaterialInstance.inline` in the baked
/// node tree with the MERGED def (`builtin_merged`) so the bundle is
/// self-contained: the player renders each node exactly as the editor did,
/// including library texture refs bound after assignment. Dynamic-WGSL
/// assignments (no builtin def) keep their instance untouched — their def
/// travels as a material folder instead.
fn flatten_builtin_materials(nodes: &mut [awsm_renderer_editor_protocol::EditorNode]) {
    let flatten = |inst: &mut awsm_renderer_editor_protocol::dynamic_material::MaterialInstance| {
        if let Some(merged) = crate::engine::bridge::node_sync::builtin_merged(inst) {
            inst.inline = merged;
        }
    };
    for node in nodes {
        // EVERY palette entry ships self-contained — the player lowers each
        // variant's `inline` standalone (the selected one is just the mesh's
        // starting pick).
        if let Some(variants) = node.kind.material_variants_mut() {
            for v in variants.iter_mut() {
                flatten(&mut v.instance);
            }
        }
        flatten_builtin_materials(&mut node.children);
    }
}

/// The environment's KTX2 cubemap files for the player bundle — one
/// `assets/<id>.ktx2` (the shared `env_ktx_path` convention) per referenced
/// skybox / IBL-prefiltered / IBL-irradiance id, REGARDLESS of how the env was
/// applied (Save-embedded reload, ribbon HDR picker, MCP `set_environment` by
/// URL — all of which stash bytes in `env_sync`). A `Url`-sourced asset (e.g. a
/// hand-authored `project.toml`) has no stash entry; its bytes are fetched here
/// and stashed so the next save/export is consistent. Anything else unresolvable
/// is a hard error: the export must not silently drop the authored environment.
async fn env_ktx_bundle_files(
    ctrl: &super::EditorController,
) -> Result<Vec<awsm_renderer_editor_protocol::BundleFile>, String> {
    use awsm_renderer_editor_protocol::{env_ktx_path, BundleFile};
    let env = ctrl.scene.environment.get_cloned();
    let mut out = Vec::new();
    for id in env.ktx_asset_ids() {
        let bytes = match crate::engine::bridge::env_sync::ktx_bytes(id) {
            Some(bytes) => bytes,
            None => {
                let source = ctrl
                    .scene
                    .assets
                    .lock()
                    .unwrap()
                    .entries
                    .get(&id)
                    .map(|e| e.source.clone());
                let Some(AssetSource::Url(url)) = source else {
                    return Err(format!(
                        "environment cubemap {id} has no bytes to export (not in the \
                         session stash, no URL source) — re-apply the environment \
                         (set_environment / HDR picker) and export again"
                    ));
                };
                let bytes = gloo_net::http::Request::get(&url)
                    .send()
                    .await
                    .map_err(|e| format!("environment cubemap {id}: fetch {url}: {e}"))?
                    .binary()
                    .await
                    .map_err(|e| format!("environment cubemap {id}: fetch {url} body: {e}"))?;
                crate::engine::bridge::env_sync::stash_ktx(id, bytes.clone());
                bytes
            }
        };
        out.push(BundleFile::new(env_ktx_path(id), bytes));
    }
    Ok(out)
}

/// Collect the mesh-asset ids whose referencing `NodeKind::Mesh` nodes have LOD
/// enabled (static path only — skinned/morph LOD bakes from the rig glb on its
/// own path). Recurses the whole node tree.
fn collect_lod_static_assets(
    nodes: &[awsm_renderer_editor_protocol::EditorNode],
    out: &mut HashSet<AssetId>,
) {
    for n in nodes {
        if let NodeKind::Mesh { mesh, lod, .. } = &n.kind {
            if lod.enabled {
                out.insert(mesh.0);
            }
        }
        collect_lod_static_assets(&n.children, out);
    }
}

/// Emit each custom-material BUFFER override's words as `assets/<asset>.bin`
/// (content-addressed buffer asset, words read from the session `buffer_cache`) —
/// the same id-keyed scheme textures use (`assets/<id>.png`). The player fetches
/// `assets/<bref.asset>.bin`, so the `BufferRef` needs no rewrite. Deduped by
/// asset id (a buffer shared across meshes emits once). Recurses the whole tree
/// (operates on the baked `Scene`'s plain nodes).
fn emit_buffer_overrides(
    nodes: &[awsm_renderer_editor_protocol::EditorNode],
    files: &mut Vec<awsm_renderer_editor_protocol::BundleFile>,
) {
    fn walk(
        nodes: &[awsm_renderer_editor_protocol::EditorNode],
        files: &mut Vec<awsm_renderer_editor_protocol::BundleFile>,
        seen: &mut HashSet<AssetId>,
    ) {
        use awsm_renderer_editor_protocol::BundleFile;
        for node in nodes {
            let instances = node
                .kind
                .material_variants()
                .map(|vs| vs.iter().map(|v| &v.instance).collect::<Vec<_>>())
                .unwrap_or_default();
            for inst in instances {
                for bref in inst.buffer_overrides.values() {
                    if !seen.insert(bref.asset) {
                        continue;
                    }
                    if let Some(words) = crate::engine::bridge::buffer_cache::get(bref.asset) {
                        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
                        files.push(BundleFile::asset(format!("{}.bin", bref.asset), bytes));
                    }
                }
            }
            walk(&node.children, files, seen);
        }
    }
    let mut seen = HashSet::new();
    walk(nodes, files, &mut seen);
}
/// `node.id → depth-first index`, matching `write_glb`'s node flattening (so
/// animation channels reference the right glTF node).
fn build_index_map(scene: &Scene) -> HashMap<NodeId, usize> {
    fn walk(nodes: &[std::sync::Arc<Node>], map: &mut HashMap<NodeId, usize>, next: &mut usize) {
        for n in nodes {
            map.insert(n.id, *next);
            *next += 1;
            walk(&n.children.lock_ref(), map, next);
        }
    }
    let mut map = HashMap::new();
    let mut next = 0;
    walk(&scene.nodes.lock_ref(), &mut map, &mut next);
    map
}

/// Lower editor clips → glTF TRS animations. First cut: **Transform** tracks
/// only (translation/rotation/scale); morph-weight, material-uniform, light, and
/// camera tracks need `KHR_animation_pointer`/morph wiring (follow-on). Cubic
/// tracks are emitted as Linear (glTF CubicSpline needs in/out tangents).
fn lower_clips(
    clips: &[std::sync::Arc<crate::controller::animation::CustomAnimation>],
    index_map: &HashMap<NodeId, usize>,
) -> Vec<ExportAnimation> {
    use awsm_renderer_editor_protocol::animation::SamplerKind;
    let mut out = Vec::new();
    for clip in clips {
        let mut channels = Vec::new();
        for track in clip.tracks.lock_ref().iter() {
            let TrackTarget::Transform { node, prop } = &track.target else {
                continue; // non-TRS targets: follow-on
            };
            let Some(&node_index) = index_map.get(node) else {
                continue;
            };
            let times: Vec<f32> = track.times.get_cloned().iter().map(|t| *t as f32).collect();
            let keys = track.keys.get_cloned();
            if times.is_empty() || keys.len() != times.len() {
                continue;
            }
            let path = match prop {
                TransformProp::Translation => AnimPath::Translation,
                TransformProp::Rotation => AnimPath::Rotation,
                TransformProp::Scale => AnimPath::Scale,
            };
            let mut values = Vec::new();
            let mut ok = true;
            for k in &keys {
                match (prop, &k.value) {
                    (TransformProp::Translation | TransformProp::Scale, TrackValue::Vec3(v)) => {
                        values.extend_from_slice(v)
                    }
                    (TransformProp::Rotation, TrackValue::Quat(q)) => values.extend_from_slice(q),
                    _ => {
                        ok = false;
                        break;
                    }
                }
            }
            if !ok {
                continue;
            }
            let interpolation = match track.sampler.get() {
                SamplerKind::Step => AnimInterp::Step,
                SamplerKind::Linear | SamplerKind::Cubic => AnimInterp::Linear,
            };
            channels.push(ExportAnimChannel {
                node_index,
                path,
                interpolation,
                times,
                values,
            });
        }
        if !channels.is_empty() {
            out.push(ExportAnimation {
                name: clip.name.get_cloned(),
                channels,
            });
        }
    }
    out
}

/// Bake `node` (or the whole scene when `None`) to a binary glTF byte vector.
/// Single-node export carries no animations (channels are scene-flat-indexed);
/// use [`export_scene_glb`] for the whole scene with animations.
pub async fn export_glb(scene: &Scene, node: Option<NodeId>) -> Result<Vec<u8>, String> {
    let roots: Vec<Arc<Node>> = match node {
        Some(id) => vec![mutate::find_by_id(scene, id).ok_or_else(|| format!("no node {id}"))?],
        None => scene.nodes.lock_ref().iter().cloned().collect(),
    };
    let (images, tex_index) = resolve_images(scene, &roots).await;

    let (rig_scenes, rig_embedded) = collect_rig_scenes(&roots);
    let nodes: Vec<ExportNode> = roots
        .iter()
        .map(|n| node_to_export(scene, n, &tex_index, &rig_embedded))
        .collect();
    let mut glb = GlbScene {
        nodes,
        images,
        ..Default::default()
    };
    append_rigs(&mut glb, rig_scenes);
    Ok(write_glb(&glb))
}

/// Rig embedding: SkinnedMesh sources whose clean rig glb is cached (built at
/// import by `reexport_clean_scene`) get the WHOLE rig — skeleton nodes, skin
/// (joints/IBMs/JOINTS_0/WEIGHTS_0) and morph targets — appended to the
/// export, so a scene glb round-trips rigs instead of flattening them to
/// bind-pose statics. The editor-side SkinnedMesh nodes skip their static
/// bake (see `node_to_export`). v1 limitations (logged): the rig embeds at
/// its source placement (edits to the mirror hierarchy don't retarget into
/// the rig), and rig materials are the source defaults (the bundle path
/// re-applies ours from scene.toml).
fn collect_rig_scenes(
    roots: &[Arc<Node>],
) -> (Vec<awsm_renderer_glb_export::GlbScene>, HashSet<AssetId>) {
    fn collect(node: &Node, out: &mut Vec<AssetId>, seen: &mut HashSet<AssetId>) {
        if let NodeKind::SkinnedMesh { skin, .. } = &node.kind.get_cloned() {
            if seen.insert(skin.source) {
                out.push(skin.source);
            }
        }
        for c in node.children.lock_ref().iter() {
            collect(c, out, seen);
        }
    }
    let mut sources = Vec::new();
    let mut seen = HashSet::new();
    for n in roots {
        collect(n, &mut sources, &mut seen);
    }
    let mut rig_scenes = Vec::new();
    let mut rig_embedded = HashSet::new();
    for src in sources {
        let Some(bytes) = crate::engine::bridge::skinned_bake_cache::get_rig_glb(src) else {
            tracing::warn!(
                "glb export: no cached rig glb for source {src} — its skinned \
                 nodes export as bind-pose statics"
            );
            continue;
        };
        match awsm_renderer_glb_export::reexport_clean(&bytes) {
            Some(rig) => {
                rig_embedded.insert(src);
                rig_scenes.push(rig);
            }
            None => tracing::warn!(
                "glb export: cached rig glb for {src} failed to re-parse — \
                 exporting bind-pose statics"
            ),
        }
    }
    (rig_scenes, rig_embedded)
}

/// Append each rig with index fixups: skin joints are DFS-flattened node
/// indices (the writer flattens roots in pre-order), so appending rig roots
/// after everything flattened so far shifts them by a uniform offset;
/// node→skin bindings shift by the skins appended so far. Appending never
/// shifts EXISTING node indices, so animation channels lowered against the
/// scene part stay valid.
fn append_rigs(glb: &mut GlbScene, rigs: Vec<awsm_renderer_glb_export::GlbScene>) {
    use awsm_renderer_glb_export::ExportMaterial;
    fn count_nodes(nodes: &[ExportNode]) -> usize {
        nodes.iter().map(|n| 1 + count_nodes(&n.children)).sum()
    }
    fn bump_skin_refs(nodes: &mut [ExportNode], skin_base: usize) {
        for n in nodes {
            if let Some(s) = n.skin.as_mut() {
                *s += skin_base;
            }
            bump_skin_refs(&mut n.children, skin_base);
        }
    }
    // Rig materials carry TexRefs into the RIG scene's own image pool; after
    // the pools concatenate, every ref must shift by the outer pool's size.
    fn bump_mat_images(m: &mut ExportMaterial, image_base: usize) {
        match m {
            ExportMaterial::Pbr(p) => {
                for t in [
                    p.base_color_texture.as_mut(),
                    p.metallic_roughness_texture.as_mut(),
                    p.normal_texture.as_mut(),
                    p.occlusion_texture.as_mut(),
                    p.emissive_texture.as_mut(),
                ]
                .into_iter()
                .flatten()
                {
                    t.image += image_base;
                }
            }
            ExportMaterial::Unlit(u) => {
                if let Some(t) = u.base_color_texture.as_mut() {
                    t.image += image_base;
                }
            }
            ExportMaterial::None { .. } => {}
        }
    }
    fn bump_image_refs(nodes: &mut [ExportNode], image_base: usize) {
        for n in nodes {
            if let Some(m) = n.material.as_mut() {
                bump_mat_images(m, image_base);
            }
            for ep in &mut n.extra_primitives {
                if let Some(m) = ep.material.as_mut() {
                    bump_mat_images(m, image_base);
                }
            }
            bump_image_refs(&mut n.children, image_base);
        }
    }
    for mut rig in rigs {
        let node_offset = count_nodes(&glb.nodes);
        let skin_base = glb.skins.len();
        let image_base = glb.images.len();
        for skin in &mut rig.skins {
            for j in &mut skin.joints {
                *j += node_offset;
            }
        }
        bump_skin_refs(&mut rig.nodes, skin_base);
        bump_image_refs(&mut rig.nodes, image_base);
        glb.skins.extend(rig.skins);
        glb.nodes.extend(rig.nodes);
        glb.images.extend(rig.images);
    }
}

/// Resolve every texture referenced by the exported subtree(s) to embedded PNG
/// images (referenced-only): procedural textures are regenerated + encoded;
/// raster textures are read back from the GPU. Returns the image pool + an
/// `AssetId → image index` map. Textures that can't be resolved (e.g. a raster
/// not yet uploaded) are skipped.
async fn resolve_images(scene: &Scene, roots: &[Arc<Node>]) -> (Vec<ExportImage>, TexIndex) {
    let mut ids: Vec<AssetId> = Vec::new();
    let mut seen: HashSet<AssetId> = HashSet::new();
    for n in roots {
        collect_texture_assets(n, &mut ids, &mut seen);
    }
    let mut images = Vec::new();
    let mut index = TexIndex::new();
    for id in ids {
        if let Some((name, bytes, mime)) = texture_source_bytes(scene, id) {
            index.insert(id, images.len());
            images.push(ExportImage { name, bytes, mime });
        } else {
            tracing::warn!("glb export: texture {id} has no cached source bytes — skipped");
        }
    }
    (images, index)
}

/// Walk a subtree collecting the (unique, ordered) texture asset ids that the
/// nodes' effective materials reference.
fn collect_texture_assets(node: &Node, ids: &mut Vec<AssetId>, seen: &mut HashSet<AssetId>) {
    let kind = node.kind.get_cloned();
    // EVERY palette entry ships in the bundle (the player builds each into a
    // ready key), so every entry's textures must ship too — the selected
    // variant is just one of them.
    let instances: Vec<&awsm_renderer_editor_protocol::dynamic_material::MaterialInstance> = kind
        .material_variants()
        .map(|vs| vs.iter().map(|v| &v.instance).collect())
        .unwrap_or_default();
    for inst in instances {
        // Only a built-in assignment exports glTF textures (its per-mesh `inline`
        // carries the slots); custom-WGSL materials export as AWSM_materials_none.
        let is_builtin = crate::controller::custom_material::find_material(
            &crate::controller::controller().custom_materials,
            inst.asset,
        )
        .map(|m| m.builtin.get_cloned().is_some())
        .unwrap_or(false);
        if is_builtin {
            // Collect from the MERGED def (variant ∪ inline), matching what the
            // bundle's flattened node actually references — a texture bound on
            // the LIBRARY material after assignment lives only on the variant,
            // and reading `inst.inline` alone skipped its bytes.
            let merged = crate::engine::bridge::node_sync::builtin_merged(inst)
                .unwrap_or_else(|| inst.inline.clone());
            for t in material_texture_refs(&merged) {
                if seen.insert(t.asset) {
                    ids.push(t.asset);
                }
            }
        }
    }
    // Sprite / decal / particle-emitter textures are NOT materials — collect
    // their `texture` refs too, else they never ship (rendered untextured).
    let own_texture = match &kind {
        NodeKind::Sprite(d) => d.texture.as_ref(),
        NodeKind::Decal(d) => d.texture.as_ref(),
        NodeKind::ParticleEmitter(d) => d.texture.as_ref(),
        _ => None,
    };
    if let Some(t) = own_texture {
        if seen.insert(t.asset) {
            ids.push(t.asset);
        }
    }
    for c in node.children.lock_ref().iter() {
        collect_texture_assets(c, ids, seen);
    }
}

/// The texture refs a PBR/Unlit `MaterialDef` carries: the five standard glTF
/// slots plus every KHR-extension texture slot (so the player can bind them —
/// mirrors the loader's `bind_extension_textures`).
fn material_texture_refs(def: &MaterialDef) -> Vec<TextureRef> {
    let mut refs: Vec<TextureRef> = [
        &def.base_color_texture,
        &def.metallic_roughness_texture,
        &def.normal_texture,
        &def.occlusion_texture,
        &def.emissive_texture,
    ]
    .into_iter()
    .flatten()
    .cloned()
    .collect();
    let ext = &def.extensions;
    for t in [
        ext.specular.as_ref().and_then(|e| e.tex.as_ref()),
        ext.specular.as_ref().and_then(|e| e.color_tex.as_ref()),
        ext.transmission.as_ref().and_then(|e| e.tex.as_ref()),
        ext.diffuse_transmission
            .as_ref()
            .and_then(|e| e.tex.as_ref()),
        ext.diffuse_transmission
            .as_ref()
            .and_then(|e| e.color_tex.as_ref()),
        ext.volume.as_ref().and_then(|e| e.thickness_tex.as_ref()),
        ext.clearcoat.as_ref().and_then(|e| e.tex.as_ref()),
        ext.clearcoat
            .as_ref()
            .and_then(|e| e.roughness_tex.as_ref()),
        ext.clearcoat.as_ref().and_then(|e| e.normal_tex.as_ref()),
        ext.sheen.as_ref().and_then(|e| e.color_tex.as_ref()),
        ext.sheen.as_ref().and_then(|e| e.roughness_tex.as_ref()),
        ext.anisotropy.as_ref().and_then(|e| e.tex.as_ref()),
        ext.iridescence.as_ref().and_then(|e| e.tex.as_ref()),
        ext.iridescence
            .as_ref()
            .and_then(|e| e.thickness_tex.as_ref()),
    ]
    .into_iter()
    .flatten()
    {
        refs.push(*t);
    }
    refs
}

/// Resolve one texture asset to `(name, png_bytes)`. Procedural → regenerate +
/// encode (sync); raster → GPU readback (async). `None` if missing/unavailable.
/// The ORIGINAL encoded bytes for a texture asset — the only lossless source.
/// Procedural textures regenerate deterministically; raster textures come from
/// the session `texture_cache` (the same bytes the project Save writes).
///
/// Deliberately NO GPU-readback fallback: `texture_png_bytes` linear→sRGB
/// encodes non-sRGB DATA textures on the way out, so a normal map came back
/// mean-shifted ((128,128,255) → ~(184,186,250)) and the player shaded with
/// normals leaning ~30° — a strong view-dependent sheen/fresnel wash the
/// editor viewport never showed. A missing cache entry is a bug to surface
/// (the bundle export FAILS on it; see `save_census` for the load-side oracle),
/// not a case to paper over with corrupted pixels.
///
/// Raster bytes may be JPEG even though the bundle names files `<id>.png` —
/// the browser decoder sniffs content; the extension is only a hint.
fn texture_source_bytes(scene: &Scene, id: AssetId) -> Option<(String, Vec<u8>, ImageMime)> {
    let def = {
        let assets = scene.assets.lock().unwrap();
        match assets.get(id).map(|e| &e.source) {
            Some(AssetSource::Texture(d)) => d.clone(),
            _ => return None,
        }
    };
    match def {
        TextureDef::Procedural(p) => {
            let (rgba, w, h) = bridge_material::procedural_rgba(&p);
            rgba_to_png(&rgba, w, h).map(|png| (format!("texture-{id}"), png, ImageMime::Png))
        }
        TextureDef::Raster { display_name, .. } => crate::engine::bridge::texture_cache::get(id)
            .map(|(bytes, mime)| (display_name, bytes, mime)),
    }
}

/// Map an exported texture's source MIME to the bundle [`TextureEncoding`] the
/// player records and decodes by. The bake ships the source bytes verbatim under
/// the matching extension (`assets/<id>.<ext>`).
fn texture_encoding_from_mime(
    mime: awsm_renderer_glb_export::ImageMime,
) -> awsm_renderer_scene::TextureEncoding {
    use awsm_renderer_glb_export::ImageMime;
    use awsm_renderer_scene::TextureEncoding;
    match mime {
        ImageMime::Png => TextureEncoding::Png,
        ImageMime::Jpeg => TextureEncoding::Jpeg,
        ImageMime::Ktx2 => TextureEncoding::Ktx2,
    }
}
thread_local! {
    /// Basis ENCODER worker client — editor-only (the `encoder` cargo feature
    /// + the encoder module URL exist only here). Lazy: spawned on the first
    /// KTX2 bake.
    static BASIS_ENCODER: awsm_renderer_codec_basis::BasisWorkerClient =
        awsm_renderer_codec_basis::BasisWorkerClient::new(
            awsm_renderer_codec_basis::BasisWorkerConfig::with_encoder(),
        );
}

/// Decode PNG/JPEG source bytes to RGBA8 (pure-Rust `image` crate — same
/// decode the lossless-WebP path uses). `None` for KTX2 (handled upstream as
/// passthrough) or a failed decode.
fn decode_rgba(
    source: &[u8],
    mime: awsm_renderer_glb_export::ImageMime,
) -> Option<(Vec<u8>, u32, u32)> {
    use awsm_renderer_glb_export::ImageMime;
    let format = match mime {
        ImageMime::Png => image::ImageFormat::Png,
        ImageMime::Jpeg => image::ImageFormat::Jpeg,
        ImageMime::Ktx2 => return None,
    };
    let rgba = image::load_from_memory_with_format(source, format)
        .ok()?
        .into_rgba8();
    let (w, h) = rgba.dimensions();
    Some((rgba.into_raw(), w, h))
}

/// Re-encode a source image to LOSSLESS WebP via the pure-Rust `image` crate
/// (`WebPEncoder::new_lossless`, VP8L — no libwebp/C dependency, so it works in
/// wasm). Decodes the PNG/JPEG source to RGBA8, then encodes: the result is
/// pixel-identical to the source but typically smaller than PNG. `Some(bytes)` on
/// success; `None` (caller falls back to the source bytes) on any decode/encode
/// failure. Unlike [`encode_webp`], this is fully deterministic and needs no
/// browser canvas.
fn encode_webp_lossless(
    source: &[u8],
    mime: awsm_renderer_glb_export::ImageMime,
) -> Option<Vec<u8>> {
    use awsm_renderer_glb_export::ImageMime;
    let format = match mime {
        ImageMime::Png => image::ImageFormat::Png,
        ImageMime::Jpeg => image::ImageFormat::Jpeg,
        // KTX2 sources never re-encode to WebP — they passthrough (or the
        // caller already fell back before reaching here).
        ImageMime::Ktx2 => return None,
    };
    let rgba = image::load_from_memory_with_format(source, format)
        .ok()?
        .into_rgba8();
    let (w, h) = rgba.dimensions();
    let mut out = Vec::new();
    image::codecs::webp::WebPEncoder::new_lossless(&mut out)
        .encode(rgba.as_raw(), w, h, image::ExtendedColorType::Rgba8)
        .ok()?;
    Some(out)
}

/// Re-encode a source image (`source` bytes of `source_mime`) to WebP at
/// `quality` (0.0–1.0) via the browser's `OffscreenCanvas.convertToBlob`. Lossy
/// WebP with a quality knob, no C/libwebp dependency — the editor runs in the
/// browser, so we let it encode. `Some(webp_bytes)` on success; `None` (caller
/// falls back to the source bytes) if decode / canvas / encode fails.
async fn encode_webp(source: &[u8], source_mime: &str, quality: f64) -> Option<Vec<u8>> {
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;

    // Decode the source to an ImageBitmap (browser-native), then draw it onto a
    // plain 2D OffscreenCanvas we can re-encode from.
    let bitmap = awsm_renderer_core::image::bitmap::load_u8(source, source_mime, None)
        .await
        .ok()?;
    let canvas = web_sys::OffscreenCanvas::new(bitmap.width(), bitmap.height()).ok()?;
    let ctx = canvas
        .get_context("2d")
        .ok()??
        .dyn_into::<web_sys::OffscreenCanvasRenderingContext2d>()
        .ok()?;
    ctx.draw_image_with_image_bitmap(&bitmap, 0.0, 0.0).ok()?;

    let opts = web_sys::ImageEncodeOptions::new();
    opts.set_type("image/webp");
    opts.set_quality(quality.clamp(0.0, 1.0));
    let blob: web_sys::Blob = JsFuture::from(canvas.convert_to_blob_with_options(&opts).ok()?)
        .await
        .ok()?
        .dyn_into()
        .ok()?;
    let buf = JsFuture::from(blob.array_buffer()).await.ok()?;
    Some(js_sys::Uint8Array::new(&buf).to_vec())
}

/// Encode tightly-packed RGBA8 to PNG bytes (via the `image` crate).
fn rgba_to_png(rgba: &[u8], w: u32, h: u32) -> Option<Vec<u8>> {
    let img = image::RgbaImage::from_raw(w, h, rgba.to_vec())?;
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .ok()?;
    Some(bytes)
}

fn node_to_export(
    scene: &Scene,
    node: &Node,
    tex_index: &TexIndex,
    rig_embedded: &HashSet<AssetId>,
) -> ExportNode {
    let trs = node.transform.get();
    let mut out = ExportNode {
        name: node.name.get_cloned(),
        transform: Trs {
            translation: trs.translation,
            rotation: trs.rotation,
            scale: trs.scale,
        },
        ..Default::default()
    };

    let kind = node.kind.get_cloned();
    // Every geometry kind — including imported models, now baked into captured
    // Mesh nodes — bakes its triangles inline via `node_mesh` (from the
    // captured-mesh store). The mesh is the node's RAW local-space geometry;
    // `out.transform` (the editor node's transform, mirrored from the glTF node's
    // local at import) already places it, so applying any extra matrix here would
    // double-transform.
    // A SkinnedMesh whose source rig is embedded wholesale (see export_glb's
    // rig-embedding pass) must NOT also bake its static bind-pose copy —
    // geometry would double.
    let rig_covers_this = matches!(
        &kind,
        NodeKind::SkinnedMesh { skin, .. } if rig_embedded.contains(&skin.source)
    );
    if !rig_covers_this {
        if let Some(mesh) = node_mesh(scene, &kind) {
            out.mesh = Some(mesh);
            if has_material_slot(&kind) {
                out.material = Some(map_material(kind.selected_material(), tex_index));
            }
        }
    }
    match &kind {
        NodeKind::Light(cfg) => out.light = Some(map_light(cfg)),
        NodeKind::Camera(cfg) => out.camera = Some(map_camera(cfg)),
        // Explicit instancer: glTF (as this writer emits it) has no GPU-
        // instancing representation, so bake one child node PER authored
        // instance, each carrying the instanced mesh asset's triangles at that
        // instance's transform. This keeps the export CORRECT — the geometry
        // and scene bounds match what renders — at the cost of duplicating the
        // mesh buffers per instance (the writer has no mesh sharing;
        // `EXT_mesh_gpu_instancing` / shared-mesh emission is the follow-up if
        // exported size matters for huge instancers). Per-instance colours are
        // not glTF-representable and are dropped; instances render
        // flat-default live, so each copy exports the default PBR def to
        // match. A nil / un-captured mesh ref exports as the bare transform
        // node (nothing renders live either).
        NodeKind::Instancer(def) => {
            if let Some(raw) = mesh_cache::get_raw(def.mesh.0) {
                let mesh = MeshData {
                    positions: raw.positions,
                    normals: raw.normals,
                    uvs: raw.uv_sets,
                    colors: raw.colors,
                    indices: raw.indices,
                };
                let material = map_material_def(&MaterialDef::default(), None, tex_index);
                out.children = def
                    .transforms
                    .iter()
                    .enumerate()
                    .map(|(i, t)| ExportNode {
                        name: format!("{}_instance_{i}", out.name),
                        transform: Trs {
                            translation: t.translation,
                            rotation: t.rotation,
                            scale: t.scale,
                        },
                        mesh: Some(mesh.clone()),
                        material: Some(material.clone()),
                        ..Default::default()
                    })
                    .collect();
            }
        }
        // Group + non-geometry leaves export as plain transform nodes; their
        // children still recurse below.
        _ => {}
    }

    // Appended after any instancer-baked children above (a node's authored
    // children and its baked instance copies coexist).
    out.children.extend(
        node.children
            .lock_ref()
            .iter()
            .map(|c| node_to_export(scene, c, tex_index, rig_embedded)),
    );
    out
}

/// Resolve any geometry node to baked triangles: Mesh → the captured-mesh store
/// (every geometry node — primitive / sweep / lathe / SDF / imported-glTF — is a
/// Mesh backed by a baked `ModifierStack`). `None` for non-geometry kinds. Shared
/// by GLB export and the `MeshStats`/`MeshCrossSection` introspection queries +
/// vertex-highlight. (`scene` is unused now that all geometry resolves from the
/// store, but kept for signature stability with the introspection callers.)
pub(crate) fn node_mesh(_scene: &Scene, kind: &NodeKind) -> Option<MeshData> {
    match kind {
        NodeKind::Mesh { mesh, .. } => mesh_cache::get_raw(mesh.0).map(|r| MeshData {
            positions: r.positions,
            normals: r.normals,
            uvs: r.uv_sets,
            colors: r.colors,
            indices: r.indices,
        }),
        // A skinned mesh exports its **bind-pose** geometry (the simplest correct
        // path: GLB export is static, and the bind pose is what `drop_skinning`
        // would bake). Resolved from the session-local bind-pose bake cache;
        // `None` after a cold reload (no cached bake) — flagged as a limitation.
        NodeKind::SkinnedMesh { skin, .. } => crate::engine::bridge::skinned_bake_cache::get(
            skin.source,
            skin.node_index,
            skin.primitive_index,
        ),
        _ => None,
    }
}

/// Whether this node kind carries a material palette (geometry kinds).
fn has_material_slot(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Mesh { .. } | NodeKind::SkinnedMesh { .. } | NodeKind::ClusterMesh { .. }
    )
}

/// Resolve a node's material assignment to the export representation.
///
/// - Unassigned (`None`) → [`ExportMaterial::None`] with no id.
/// - A **built-in** assignment (the asset resolves to a built-in library
///   material) → its per-mesh `inline` def mapped to glTF (built-ins ARE
///   glTF-representable).
/// - A **custom-WGSL** assignment (or one that doesn't resolve to a built-in) →
///   [`ExportMaterial::None`] carrying the assigned id for scene-level
///   re-resolution on import.
fn map_material(material: Option<&MaterialInstance>, tex_index: &TexIndex) -> ExportMaterial {
    let Some(inst) = material else {
        return ExportMaterial::None { id: None };
    };
    let is_builtin = crate::controller::custom_material::find_material(
        &crate::controller::controller().custom_materials,
        inst.asset,
    )
    .map(|m| m.builtin.get_cloned().is_some())
    .unwrap_or(false);
    if is_builtin {
        // Export the MERGED def (variant ∪ inline) — same fix as the player
        // bundle: a texture (or extension) bound on the LIBRARY material after
        // assignment lives only on the variant, and exporting `inst.inline`
        // alone dropped it from the glb.
        let merged = crate::engine::bridge::node_sync::builtin_merged(inst)
            .unwrap_or_else(|| inst.inline.clone());
        map_material_def(&merged, Some(inst.asset), tex_index)
    } else {
        ExportMaterial::None {
            id: Some(inst.asset.to_string()),
        }
    }
}

/// Resolve a `TextureRef` to an export `TexRef` (image index + uv set), if the
/// referenced texture was embedded.
fn tex_ref(t: &Option<TextureRef>, tex_index: &TexIndex) -> Option<TexRef> {
    let t = t.as_ref()?;
    let image = *tex_index.get(&t.asset)?;
    Some(TexRef {
        image,
        tex_coord: t.uv_index,
        // The editor doesn't author KHR_texture_transform yet (import-only follow-up).
        transform: None,
    })
}

fn map_material_def(
    def: &MaterialDef,
    assigned: Option<AssetId>,
    tex_index: &TexIndex,
) -> ExportMaterial {
    match def.shading {
        MaterialShading::Pbr => ExportMaterial::Pbr(PbrMaterial {
            name: def.label.clone(),
            base_color: def.base_color,
            metallic: def.metallic,
            roughness: def.roughness,
            emissive: def.emissive,
            alpha_mode: map_alpha(&def.alpha_mode),
            double_sided: def.double_sided,
            base_color_texture: tex_ref(&def.base_color_texture, tex_index),
            metallic_roughness_texture: tex_ref(&def.metallic_roughness_texture, tex_index),
            normal_texture: tex_ref(&def.normal_texture, tex_index),
            occlusion_texture: tex_ref(&def.occlusion_texture, tex_index),
            emissive_texture: tex_ref(&def.emissive_texture, tex_index),
            // The editor doesn't author KHR_* scalar extensions on its materials yet
            // (they ride glTF import-only; round-tripping them through the editor is a
            // separate follow-up). Absent → glTF defaults.
            ior: None,
            emissive_strength: None,
            extensions_json: Default::default(),
        }),
        MaterialShading::Unlit => ExportMaterial::Unlit(UnlitMaterial {
            name: def.label.clone(),
            base_color: def.base_color,
            alpha_mode: map_alpha(&def.alpha_mode),
            double_sided: def.double_sided,
            base_color_texture: tex_ref(&def.base_color_texture, tex_index),
        }),
        // Toon / FlipBook aren't glTF-representable (cel bands; time-driven
        // atlas cells) → none + the assigned id (if any) for scene-level
        // re-resolution on import. Deliberately NOT an unlit fallback: a
        // frozen atlas GRID renders wrong in any viewer — an absent material
        // is the honest export (the bundle path re-applies from scene.toml).
        MaterialShading::Toon { .. } | MaterialShading::FlipBook { .. } => ExportMaterial::None {
            id: assigned.map(|a| a.to_string()),
        },
    }
}

fn map_alpha(m: &MaterialAlphaMode) -> AlphaMode {
    match m {
        MaterialAlphaMode::Opaque => AlphaMode::Opaque,
        MaterialAlphaMode::Mask { cutoff } => AlphaMode::Mask { cutoff: *cutoff },
        MaterialAlphaMode::Blend => AlphaMode::Blend,
    }
}

fn map_light(cfg: &LightConfig) -> ExportLight {
    match *cfg {
        LightConfig::Directional {
            color, intensity, ..
        } => ExportLight::Directional { color, intensity },
        LightConfig::Point {
            color,
            intensity,
            range,
            ..
        } => ExportLight::Point {
            color,
            intensity,
            range: Some(range),
        },
        LightConfig::Spot {
            color,
            intensity,
            range,
            inner_angle,
            outer_angle,
            ..
        } => ExportLight::Spot {
            color,
            intensity,
            range: Some(range),
            inner_cone_angle: inner_angle,
            outer_cone_angle: outer_angle,
        },
    }
}

fn map_camera(cfg: &CameraConfig) -> ExportCamera {
    match cfg.projection {
        CameraProjection::Perspective { fov_y_rad } => ExportCamera::Perspective {
            yfov: fov_y_rad,
            aspect_ratio: None,
            znear: cfg.near,
            zfar: Some(cfg.far),
        },
        CameraProjection::Orthographic { half_height } => ExportCamera::Orthographic {
            xmag: half_height,
            ymag: half_height,
            znear: cfg.near,
            zfar: cfg.far,
        },
    }
}

/// Bake a `SweepAlongCurve` to triangles by resolving its referenced curve node
/// from the scene tree (mirrors the renderer-bridge `materialize_sweep`). Shared
/// with `ConvertToEditableMesh` (which bakes a sweep into a captured mesh).
pub(crate) fn sweep_mesh(scene: &Scene, def: &SweepAlongCurveDef) -> Option<MeshData> {
    use awsm_renderer_curves::CatmullRomCurve;
    use awsm_renderer_meshgen::{sweep_along_curve, CrossSection, SweepOpts, UvMode};
    use glam::Vec3;

    if def.curve_node.is_nil() {
        return None;
    }
    let curve_node = mutate::find_by_id(scene, def.curve_node)?;
    let curve_def = match curve_node.kind.get_cloned() {
        NodeKind::Curve(c) => c,
        _ => return None,
    };
    let curve = CatmullRomCurve::new(
        curve_def
            .control_points
            .iter()
            .map(|p| Vec3::from_array(*p))
            .collect(),
        curve_def.closed,
    );
    let cs = match def.cross_section.clone() {
        CrossSectionDef::Strip { width, y_offset } => CrossSection::Strip { width, y_offset },
        CrossSectionDef::Tube {
            radius,
            radial_segments,
        } => CrossSection::Tube {
            radius,
            radial_segments,
        },
        CrossSectionDef::Wall { width, height } => CrossSection::Wall { width, height },
        CrossSectionDef::Profile { points, closed } => CrossSection::Profile { points, closed },
    };
    let opts = SweepOpts {
        samples: def.samples,
        uv_mode: match def.uv_mode {
            SweepUvMode::StretchOnce => UvMode::StretchOnce,
            SweepUvMode::RepeatByLength {
                u_repeat,
                v_repeat_per_unit,
            } => UvMode::RepeatByLength {
                u_repeat,
                v_repeat_per_unit,
            },
        },
        up_hint: def.up_hint,
    };
    Some(sweep_along_curve(&curve, &cs, &opts))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Axis 3 (006): data maps (normals/roughness) must survive the lossless
    /// WebP bundle default BYTE-EXACT — a lossy re-encode of a normal map
    /// corrupts shading in ways no golden catches early. Round-trips a
    /// synthetic normal-map-like gradient (every channel exercised, including
    /// alpha) through encode_webp_lossless and asserts pixel identity.
    #[test]
    fn lossless_webp_roundtrips_data_maps_byte_exact() {
        let (w, h) = (64u32, 64u32);
        let mut rgba = image::RgbaImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                // A plausible tangent-space normal encoding + varying alpha:
                // exactly the kind of non-photographic data lossy paths mangle.
                let nx = (x * 4) as u8;
                let ny = (y * 4) as u8;
                let nz = 255 - ((x + y) as u8);
                let a = 255 - (y as u8);
                rgba.put_pixel(x, y, image::Rgba([nx, ny, nz, a]));
            }
        }
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(rgba.clone())
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("encode source png");

        let webp = encode_webp_lossless(&png, awsm_renderer_glb_export::ImageMime::Png)
            .expect("lossless webp encode");
        let decoded = image::load_from_memory_with_format(&webp, image::ImageFormat::WebP)
            .expect("decode webp")
            .into_rgba8();
        assert_eq!(
            decoded.as_raw(),
            rgba.as_raw(),
            "lossless WebP must be pixel-identical for data maps"
        );
    }

    /// The completeness guard catches the sprite/decal/particle texture gap:
    /// their `texture` fields live outside materials, so a bake that forgets
    /// them would ship a bundle whose player renders them untextured. The
    /// guard must FAIL loudly naming each unshipped texture, and PASS once the
    /// bytes ship. This is the "editor has it ⇒ the bundle ships it" backstop.
    #[test]
    fn export_guard_catches_unshipped_sprite_decal_particle_textures() {
        use awsm_renderer_editor_protocol::{
            particle::ParticleEmitterDef, BundleFile, DecalConfig, EditorNode, NodeId, Scene,
            SpriteDef,
        };

        fn tref(asset: AssetId) -> TextureRef {
            TextureRef {
                asset,
                uv_index: 0,
                transform: None,
                sampler: None,
                flow: None,
                export_profile: None,
            }
        }
        fn node(name: &str, kind: NodeKind) -> EditorNode {
            EditorNode {
                id: NodeId::new(),
                name: name.into(),
                transform: Default::default(),
                kind,
                locked: false,
                visible: true,
                prefab: false,
                children: vec![],
            }
        }

        let (sprite_tex, decal_tex, particle_tex) =
            (AssetId::new(), AssetId::new(), AssetId::new());
        let sprite = node(
            "s",
            NodeKind::Sprite(SpriteDef {
                texture: Some(tref(sprite_tex)),
                ..Default::default()
            }),
        );
        let decal = node(
            "d",
            NodeKind::Decal(DecalConfig {
                texture: Some(tref(decal_tex)),
                ..Default::default()
            }),
        );
        let particle = node(
            "p",
            NodeKind::ParticleEmitter(ParticleEmitterDef {
                texture: Some(tref(particle_tex)),
                ..Default::default()
            }),
        );
        let scene = Scene {
            nodes: vec![sprite, decal, particle],
            ..Default::default()
        };

        // Nothing shipped → the guard fails, naming all three ids.
        let err = verify_texture_refs_shipped(&scene, &[]).unwrap_err();
        for id in [sprite_tex, decal_tex, particle_tex] {
            assert!(
                err.contains(&id.to_string()),
                "guard must name the unshipped texture {id}; got: {err}"
            );
        }

        // All three shipped as assets/<id>.png → the guard passes.
        let files: Vec<BundleFile> = [sprite_tex, decal_tex, particle_tex]
            .iter()
            .map(|id| BundleFile::asset(format!("{id}.png"), vec![0u8; 4]))
            .collect();
        assert!(
            verify_texture_refs_shipped(&scene, &files).is_ok(),
            "guard must pass once every referenced texture ships"
        );
    }
}
