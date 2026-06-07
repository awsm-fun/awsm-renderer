//! Scene→GPU sync: observe the reactive scene tree and materialize/teardown
//! each node's renderer resources. M4-C materializes primitives + lights; other
//! kinds are passive (no GPU mesh yet).

use std::sync::Arc;

use awsm_meshgen::{
    box_mesh, cone_mesh, cylinder_mesh, plane_mesh, sphere_mesh, sweep_along_curve, torus_mesh,
    CrossSection, MeshData, SweepOpts, UvMode,
};
use awsm_renderer::cameras::{CameraParams, CameraProjectionParams};
use awsm_renderer::raw_mesh::RawMeshData;
use awsm_renderer::transforms::{Transform, TransformKey};
use awsm_scene_schema::PrimitiveShape;
use futures_signals::signal::SignalExt;
use futures_signals::signal_vec::{SignalVecExt, VecDiff};
use glam::{Quat, Vec3, Vec4};

use super::{bridge, material, RendererNode};
use crate::engine::context::{renderer_handle, with_renderer_mut};
use crate::engine::scene::{LightConfig, Node, NodeId, NodeKind, Trs};
use crate::prelude::*;

/// Begin mirroring the controller's scene root onto the renderer.
pub fn start() {
    let scene = controller().scene.clone();
    spawn_local(async move {
        scene
            .nodes
            .signal_vec_cloned()
            .for_each(|diff| async move {
                handle_diff(None, None, diff).await;
            })
            .await;
    });
}

/// Handle one diff on a children list. `parent_id`/`parent_tk` are `None` for the
/// scene root, `Some` for a node's children.
async fn handle_diff(
    parent_id: Option<NodeId>,
    parent_tk: Option<TransformKey>,
    diff: VecDiff<Arc<Node>>,
) {
    match diff {
        VecDiff::Replace { values } => {
            // Tear down whatever was there, then add all.
            for id in order_snapshot(parent_id) {
                remove_node(id).await;
            }
            order_reset(parent_id);
            for (i, node) in values.into_iter().enumerate() {
                add_node(parent_id, parent_tk, i, node).await;
            }
        }
        VecDiff::InsertAt { index, value } => add_node(parent_id, parent_tk, index, value).await,
        VecDiff::Push { value } => {
            let index = order_len(parent_id);
            add_node(parent_id, parent_tk, index, value).await;
        }
        VecDiff::UpdateAt { index, value } => {
            if let Some(id) = order_get(parent_id, index) {
                remove_node(id).await;
            }
            // Replace the slot.
            remove_order_at(parent_id, index);
            add_node(parent_id, parent_tk, index, value).await;
        }
        VecDiff::RemoveAt { index } => {
            if let Some(id) = order_get(parent_id, index) {
                remove_node(id).await;
            }
            remove_order_at(parent_id, index);
        }
        VecDiff::Pop {} => {
            let len = order_len(parent_id);
            if len > 0 {
                if let Some(id) = order_get(parent_id, len - 1) {
                    remove_node(id).await;
                }
                remove_order_at(parent_id, len - 1);
            }
        }
        VecDiff::Move {
            old_index,
            new_index,
        } => {
            // Reorder tracking only (the renderer doesn't care about sibling
            // order); GPU resources are unaffected.
            let b = bridge();
            let mut co = b.child_order.lock().unwrap();
            if let Some(v) = co.get_mut(&parent_id) {
                if old_index < v.len() {
                    let id = v.remove(old_index);
                    let ni = new_index.min(v.len());
                    v.insert(ni, id);
                }
            }
        }
        VecDiff::Clear {} => {
            for id in order_snapshot(parent_id) {
                remove_node(id).await;
            }
            order_reset(parent_id);
        }
    }
}

fn order_len(parent_id: Option<NodeId>) -> usize {
    let b = bridge();
    let co = b.child_order.lock().unwrap();
    co.get(&parent_id).map(|v| v.len()).unwrap_or(0)
}
fn order_get(parent_id: Option<NodeId>, index: usize) -> Option<NodeId> {
    let b = bridge();
    let co = b.child_order.lock().unwrap();
    co.get(&parent_id).and_then(|v| v.get(index).copied())
}
fn order_insert(parent_id: Option<NodeId>, index: usize, id: NodeId) {
    let b = bridge();
    let mut co = b.child_order.lock().unwrap();
    let v = co.entry(parent_id).or_default();
    let i = index.min(v.len());
    v.insert(i, id);
}
fn remove_order_at(parent_id: Option<NodeId>, index: usize) {
    let b = bridge();
    let mut co = b.child_order.lock().unwrap();
    if let Some(v) = co.get_mut(&parent_id) {
        if index < v.len() {
            v.remove(index);
        }
    }
}
fn order_snapshot(parent_id: Option<NodeId>) -> Vec<NodeId> {
    let b = bridge();
    let co = b.child_order.lock().unwrap();
    co.get(&parent_id).cloned().unwrap_or_default()
}
fn order_reset(parent_id: Option<NodeId>) {
    let b = bridge();
    b.child_order.lock().unwrap().insert(parent_id, Vec::new());
}

async fn add_node(
    parent_id: Option<NodeId>,
    parent_tk: Option<TransformKey>,
    index: usize,
    node: Arc<Node>,
) {
    let node_id = node.id;
    let trs = node.transform.get();
    let tk =
        with_renderer_mut(move |r| r.transforms.insert(trs_to_transform(&trs), parent_tk)).await;

    let entry = RendererNode::new(node.clone(), tk);
    bridge()
        .nodes
        .lock()
        .unwrap()
        .insert(node_id, entry.clone());
    order_insert(parent_id, index, node_id);

    // Kind observer — fires on the current value first, so this materializes
    // the node on insert and re-materializes on any kind change.
    {
        let loader = AsyncLoader::new();
        loader.load(clone!(entry => async move {
            entry.node.kind.signal_cloned().for_each(clone!(entry => move |kind| {
                clone!(entry => async move { apply_kind(entry, kind).await; })
            })).await;
        }));
        entry.loaders.lock().unwrap().push(loader);
    }
    // Transform observer — push local transform changes to the renderer.
    {
        let loader = AsyncLoader::new();
        loader.load(clone!(entry => async move {
            entry.node.transform.signal().for_each(move |trs| {
                clone!(entry => async move {
                    with_renderer_mut(move |r| {
                        let _ = r.transforms.set_local(entry.transform_key, trs_to_transform(&trs));
                    }).await;
                })
            }).await;
        }));
        entry.loaders.lock().unwrap().push(loader);
    }
    // Visibility observer — hide/show this node's meshes.
    {
        let loader = AsyncLoader::new();
        loader.load(clone!(entry => async move {
            entry.node.visible.signal().for_each(move |visible| {
                clone!(entry => async move {
                    let meshes: Vec<_> = entry.model_meshes.lock().unwrap().clone();
                    with_renderer_mut(move |r| {
                        for mk in meshes {
                            let _ = r.set_mesh_hidden(mk, !visible);
                        }
                    }).await;
                })
            }).await;
        }));
        entry.loaders.lock().unwrap().push(loader);
    }
    // Children observer — recurse for nested nodes.
    {
        let loader = AsyncLoader::new();
        loader.load(clone!(node => async move {
            node.children.signal_vec_cloned().for_each(move |diff| {
                clone!(node_id => async move { handle_diff(Some(node_id), Some(tk), diff).await; })
            }).await;
        }));
        entry.loaders.lock().unwrap().push(loader);
    }
}

async fn remove_node(node_id: NodeId) {
    // Remove any descendants first.
    for child in order_snapshot(Some(node_id)) {
        Box::pin(remove_node(child)).await;
    }
    {
        let b = bridge();
        b.child_order.lock().unwrap().remove(&Some(node_id));
    }

    let entry = {
        let b = bridge();
        let e = b.nodes.lock().unwrap().remove(&node_id);
        e
    };
    if let Some(entry) = entry {
        teardown(&entry).await;
        // Dropping the entry (and its loaders) cancels the observers.
    }
}

/// Tear down a node's GPU resources (meshes / sub-transforms / owned materials /
/// light). Leaves the node's own `transform_key` alone unless the node itself is
/// being removed (handled by the caller dropping the entry — we also free it).
async fn teardown(entry: &Arc<RendererNode>) {
    let meshes: Vec<_> = entry.model_meshes.lock().unwrap().drain(..).collect();
    let transforms: Vec<_> = entry.model_transforms.lock().unwrap().drain(..).collect();
    let materials: Vec<_> = entry.material_keys.lock().unwrap().drain(..).collect();
    let lines: Vec<_> = entry.line_keys.lock().unwrap().drain(..).collect();
    let decals: Vec<_> = entry.decal_keys.lock().unwrap().drain(..).collect();
    let light = entry.light_key.lock().unwrap().take();
    let camera = entry.camera_key.lock().unwrap().take();
    let node_id = entry.node_id;
    for mk in &meshes {
        bridge().unregister_mesh(*mk);
    }
    with_renderer_mut(move |r| {
        for mk in meshes {
            r.remove_mesh(mk);
        }
        for tk in transforms {
            r.transforms.remove(tk);
        }
        for mat in materials {
            r.remove_material(mat);
        }
        for lk in lines {
            r.remove_line(lk);
        }
        for dk in decals {
            r.remove_decal(dk);
        }
        // Free any particle-emitter runtime this node owns (no-op otherwise).
        super::particles::teardown(r, node_id);
        if let Some(lk) = light {
            r.remove_light(lk);
        }
        if let Some(ck) = camera {
            r.cameras.remove(ck);
        }
    })
    .await;
    bridge().light_node_ids.lock().unwrap().remove(&node_id);
}

/// Materialize (or re-materialize) a node for its current kind.
async fn apply_kind(entry: Arc<RendererNode>, kind: NodeKind) {
    // Camera → Camera: update the params IN PLACE so the `CameraKey` stays
    // stable. Editing a camera param re-emits `node.kind`, but a numeric
    // `SetKind` doesn't bump `anim_revision`, so a lowered
    // `AnimationTarget::Camera { camera }` channel never re-lowers — a
    // teardown + re-insert here would churn the key and strand that target on a
    // freed slot. The camera store is purpose-built for this (it holds the
    // params the animation channel drives). The key is only freed when the node
    // is deleted or changes away from `Camera` (handled by `teardown` below /
    // `remove_node`).
    if let NodeKind::Camera(cfg) = &kind {
        let existing = *entry.camera_key.lock().unwrap();
        if let Some(ck) = existing {
            // The in-place path assumes a camera node owns nothing else that
            // `teardown` would normally free (only the camera key). If a future
            // kind gives camera nodes extra GPU resources, this early return
            // would leak them — trip it in tests.
            debug_assert!(
                entry.model_meshes.lock().unwrap().is_empty()
                    && entry.material_keys.lock().unwrap().is_empty()
                    && entry.light_key.lock().unwrap().is_none(),
                "camera node unexpectedly owns non-camera GPU resources"
            );
            let params = camera_params_from_config(cfg);
            with_renderer_mut(move |r| {
                r.cameras.update(ck, |p| *p = params);
            })
            .await;
            // Keep `last_kind` in step with the applied kind, exactly as the
            // normal path does after its match arm.
            *entry.last_kind.lock().unwrap() = Some(entry.node.kind.get_cloned());
            return;
        }
    }

    // Tear down the previous materialization (no-op on first apply).
    teardown(&entry).await;

    match kind {
        NodeKind::Primitive {
            shape,
            inline_material,
            custom_material,
            shadow,
            ..
        } => {
            materialize_primitive(
                entry.clone(),
                shape,
                inline_material,
                custom_material,
                shadow,
            )
            .await
        }
        NodeKind::Light(cfg) => apply_light(entry.clone(), cfg).await,
        NodeKind::Line(def) => materialize_line(entry.clone(), def).await,
        NodeKind::Curve(def) => materialize_curve_viz(entry.clone(), def).await,
        NodeKind::Sprite(def) => materialize_sprite(entry.clone(), def).await,
        NodeKind::Collider(shape) => materialize_collider(entry.clone(), shape).await,
        NodeKind::Decal(cfg) => materialize_decal(entry.clone(), cfg).await,
        NodeKind::SweepAlongCurve {
            def,
            inline_material,
            ..
        } => materialize_sweep(entry.clone(), def, inline_material).await,
        NodeKind::InstancesAlongCurve(def) => materialize_instances(entry.clone(), def).await,
        NodeKind::ParticleEmitter(def) => materialize_particle(entry.clone(), def).await,
        NodeKind::Mesh {
            mesh,
            inline_material,
            ..
        } => match super::mesh_cache::get_raw(mesh.0) {
            Some(raw) => {
                upload_simple_mesh(entry.clone(), raw, inline_material).await;
            }
            None => {
                tracing::warn!("NodeKind::Mesh {mesh:?}: not in the capture cache; renders empty")
            }
        },
        NodeKind::Model(model_ref) => materialize_model(entry.clone(), model_ref).await,
        NodeKind::Camera(cfg) => materialize_camera(entry.clone(), cfg).await,
        // Group: no procedural geometry, no renderer resource.
        _ => {}
    }

    *entry.last_kind.lock().unwrap() = Some(entry.node.kind.get_cloned());
}

/// If `id` names a **built-in** library material, merge its shared variant
/// settings (shading / alpha / double-sided / vertex-colors / texture-enables)
/// with this mesh's per-mesh uniform values (`inline`: base color / metallic /
/// roughness / emissive) into a final `MaterialDef`. Returns `None` for a dynamic
/// material or an unknown id (callers then try the dynamic path / inline).
/// The geometry COLOR set index a renderer mesh carries (glTF `COLOR_n`), or
/// `None` if it has no vertex-colour attribute. Vertex-colour *usage* is
/// geometry-derived — a material that multiplies by COLOR only makes sense when
/// the mesh actually has it — so the bridge sets `vertex_colors_enabled` + the
/// set index from this rather than an authored bit, mirroring how `populate_gltf`
/// decides it per primitive.
fn mesh_vertex_color_set(
    r: &awsm_renderer::AwsmRenderer,
    mk: awsm_renderer::meshes::MeshKey,
) -> Option<u32> {
    use awsm_renderer::meshes::buffer_info::{
        MeshBufferCustomVertexAttributeInfo, MeshBufferVertexAttributeInfo,
    };
    r.meshes.buffer_info(mk).ok().and_then(|info| {
        info.triangles.vertex_attributes.iter().find_map(|attr| {
            if let MeshBufferVertexAttributeInfo::Custom(
                MeshBufferCustomVertexAttributeInfo::Colors { index, .. },
            ) = attr
            {
                Some(*index)
            } else {
                None
            }
        })
    })
}

fn builtin_merged(
    inst: &awsm_scene_schema::dynamic_material::CustomMaterialInstance,
    inline: &awsm_scene_schema::MaterialDef,
) -> Option<awsm_scene_schema::MaterialDef> {
    use awsm_scene_schema::material::{MaterialAlphaMode, MaterialShading, PbrExtensions};
    use awsm_scene_schema::TextureRef;
    let ctrl = crate::controller::controller();
    let mat =
        crate::controller::custom_material::find_material(&ctrl.custom_materials, inst.material)?;
    let variant = mat.builtin.get_cloned()?;

    // ── The override rule ────────────────────────────────────────────────────
    // VARIANT fields (anything in the pipeline cache key — shading model, alpha
    // *mode*, double-sided cull, vertex-colors, texture-slot *presence*, KHR
    // extension *enables*) come ONLY from the shared `variant`, so every mesh
    // using this material shares one pipeline. Everything else — the entire
    // uniform-buffer surface — is per-mesh: factors + extension *parameters* +
    // Toon knobs + mask cutoff from `inline`, and the bound *image* per declared
    // texture slot from `texture_overrides`. None of these recompile.

    // Texture binding: presence gated by the variant; image swapped per mesh.
    let tex = |slot: &str, default: Option<TextureRef>| -> Option<TextureRef> {
        match default {
            Some(_) => inst.texture_overrides.get(slot).cloned().or(default),
            None => None,
        }
    };

    // Extension PARAMS per mesh, ENABLE from the variant: an extension the
    // material doesn't enable stays off (enabling it would recompile); an enabled
    // one takes this mesh's parameters (falling back to defaults if unseeded).
    macro_rules! merge_ext {
        ($f:ident) => {
            variant
                .extensions
                .$f
                .map(|_| inline.extensions.$f.unwrap_or_default())
        };
    }
    let extensions = PbrExtensions {
        emissive_strength: merge_ext!(emissive_strength),
        ior: merge_ext!(ior),
        specular: merge_ext!(specular),
        transmission: merge_ext!(transmission),
        diffuse_transmission: merge_ext!(diffuse_transmission),
        volume: merge_ext!(volume),
        clearcoat: merge_ext!(clearcoat),
        sheen: merge_ext!(sheen),
        dispersion: merge_ext!(dispersion),
        anisotropy: merge_ext!(anisotropy),
        iridescence: merge_ext!(iridescence),
    };

    // Alpha MODE (Opaque/Mask/Blend) is variant routing; the Mask *cutoff* value
    // is a per-mesh uniform compare, so carry it from inline when both are Mask.
    let alpha_mode = match (&variant.alpha_mode, &inline.alpha_mode) {
        (MaterialAlphaMode::Mask { .. }, MaterialAlphaMode::Mask { cutoff }) => {
            MaterialAlphaMode::Mask { cutoff: *cutoff }
        }
        _ => variant.alpha_mode.clone(),
    };

    // Shading MODEL is variant (selects the renderer Material flavour); the Toon
    // knobs are uniform (one canonical Toon shader_id), so carry them from inline.
    let shading = match (variant.shading, inline.shading) {
        (MaterialShading::Toon { .. }, t @ MaterialShading::Toon { .. }) => t,
        (v, _) => v,
    };

    Some(awsm_scene_schema::MaterialDef {
        base_color: inline.base_color,
        metallic: inline.metallic,
        roughness: inline.roughness,
        emissive: inline.emissive,
        normal_scale: inline.normal_scale,
        occlusion_strength: inline.occlusion_strength,
        base_color_texture: tex("base_color_texture", variant.base_color_texture),
        metallic_roughness_texture: tex(
            "metallic_roughness_texture",
            variant.metallic_roughness_texture,
        ),
        normal_texture: tex("normal_texture", variant.normal_texture),
        occlusion_texture: tex("occlusion_texture", variant.occlusion_texture),
        emissive_texture: tex("emissive_texture", variant.emissive_texture),
        alpha_mode,
        shading,
        extensions,
        // variant-only: double_sided, vertex_colors_enabled, label.
        ..variant
    })
}

async fn materialize_primitive(
    entry: Arc<RendererNode>,
    shape: PrimitiveShape,
    inline: awsm_scene_schema::MaterialDef,
    custom_material: Option<awsm_scene_schema::dynamic_material::CustomMaterialInstance>,
    shadow: awsm_scene_schema::MeshShadowConfig,
) {
    let mesh = primitive_to_mesh(&shape);
    let raw = RawMeshData {
        positions: mesh.positions,
        normals: mesh.normals,
        uvs: mesh.uvs,
        colors: mesh.colors,
        indices: mesh.indices,
    };
    let parent_tk = entry.transform_key;

    // Hold the renderer lock across the upload so we can finalize the GPU
    // texture pool / bind groups afterwards (committing the material so the mesh
    // actually draws — the archived editor batched this in instance_batcher).
    let handle = renderer_handle();
    let mut r = handle.lock().await;
    // Resolve the assigned library material (by stable id):
    //   • a built-in → merge its shared *variant* settings with this mesh's
    //     per-mesh *uniform* values (`inline`) → one Material::Pbr/Unlit/Toon;
    //   • a registered dynamic WGSL material → its registered bucket;
    //   • otherwise (unassigned / not-yet-registered) → the mesh's inline material.
    // Material resolution. A mesh with NO assigned material (or an assignment that
    // can't be resolved yet) renders flat **magenta** — the missing-material
    // sentinel — NOT a default PBR material. `inline` is purely the per-mesh
    // *uniform* store for a built-in assignment (base colour / metallic / … — see
    // the material model note in inspector.rs::material_editor); it never stands in
    // as a material on its own.
    let mat_key = match custom_material.as_ref() {
        Some(inst) => {
            if let Some(merged) = builtin_merged(inst, &inline) {
                material::insert_material(&mut r, &merged)
            } else if let Some(k) = super::dynamic::insert_custom(&mut r, inst) {
                k
            } else {
                material::insert_magenta(&mut r)
            }
        }
        None => material::insert_magenta(&mut r),
    };
    let sub_tk = r.transforms.insert(Transform::IDENTITY, Some(parent_tk));
    match r.add_raw_mesh(raw, sub_tk, mat_key) {
        Ok(mk) => {
            if let Err(e) = r.finalize_gpu_textures().await {
                tracing::warn!("finalize_gpu_textures: {e}");
            }
            let _ = r.set_mesh_shadow_flags(mk, mesh_shadow_flags_from_config(&shadow));
            drop(r);
            entry.model_meshes.lock().unwrap().push(mk);
            entry.model_transforms.lock().unwrap().push(sub_tk);
            entry.material_keys.lock().unwrap().push(mat_key);
            bridge().register_mesh(mk, entry.node_id);
        }
        Err(e) => {
            r.transforms.remove(sub_tk);
            r.remove_material(mat_key);
            tracing::error!("materialize primitive failed: {e}");
        }
    }
}

/// Materialize a `Model` node by **duplicating** its glTF template meshes under
/// the node's own transform. The template (built at import time from
/// `populate_gltf`) holds the renderer mesh keys for each glTF node; we look up
/// this node's by `node_index` and call `duplicate_mesh_with_transform` so the
/// copy is parented to *this* editor node — moving the node moves the mesh.
/// Skinning is preserved: the duplicate keeps its joint references (the joints
/// still live in the renderer transform tree as the hidden template).
async fn materialize_model(entry: Arc<RendererNode>, model_ref: awsm_scene_schema::ModelRef) {
    let Some(template) = bridge().get_template(model_ref.asset_id) else {
        tracing::warn!(
            "Model {:?}: no node template cached; renders empty",
            model_ref.asset_id
        );
        return;
    };
    let Some(tnode) = template.find_by_node_index(model_ref.node_index) else {
        tracing::warn!(
            "Model node_index {} not found in template; renders empty",
            model_ref.node_index
        );
        return;
    };
    // Each mesh: (mesh_key, is_skinned, gltf material index). `primitive_index =
    // Some(i)` would peel a single primitive (Split); we materialize them all.
    type Triple = (awsm_renderer::meshes::MeshKey, bool, Option<usize>);
    let triples: Vec<Triple> = match model_ref.primitive_index {
        None => tnode
            .mesh_keys
            .iter()
            .copied()
            .zip(tnode.mesh_is_skinned.iter().copied())
            .zip(tnode.mesh_gltf_material_indices.iter().copied())
            .map(|((mk, s), mi)| (mk, s, mi))
            .collect(),
        Some(i) => {
            let i = i as usize;
            match (
                tnode.mesh_keys.get(i),
                tnode.mesh_is_skinned.get(i),
                tnode.mesh_gltf_material_indices.get(i),
            ) {
                (Some(mk), Some(s), Some(mi)) => vec![(*mk, *s, *mi)],
                _ => Vec::new(),
            }
        }
    };
    if triples.is_empty() {
        return;
    }

    let visible = entry.node.visible.get();
    let shadow_flags = mesh_shadow_flags_from_config(&model_ref.shadow);
    let parent_tk = entry.transform_key;

    let mut created = Vec::new();
    let mut material_keys = Vec::new();
    let mut to_register = Vec::new();
    {
        let handle = renderer_handle();
        let mut r = handle.lock().await;
        // The node's assigned library material drives every primitive it renders.
        // Resolved exactly like a Primitive/Mesh — a built-in merges this node's
        // per-mesh inline uniforms + texture overrides over the shared variant; a
        // dynamic material applies its declared overrides; an *unassigned* node
        // renders the flat-magenta missing-material sentinel. The material is
        // resolved PER PRIMITIVE because vertex-color usage is geometry-derived:
        // glTF uses COLOR_0 iff the primitive carries it (matching the renderer's
        // native per-primitive behaviour), not from any authored material bit — so
        // a primitive with vertex colours gets `vertex_colors_enabled` flipped on
        // (its own pipeline variant) while a sibling without them does not.
        for (mk, skinned, _mat_idx) in &triples {
            let vertex_color_set = mesh_vertex_color_set(&r, *mk);
            // Skinned meshes keep rendering in place (their original copy) —
            // duplicating collapses the skin; non-skinned are duplicated under
            // this node's transform so the editor node owns + moves them.
            let target = if *skinned {
                *mk
            } else {
                match r.duplicate_mesh_with_transform(*mk, parent_tk) {
                    Ok(new_mesh) => {
                        let _ = r.set_mesh_hidden(new_mesh, !visible);
                        let _ = r.set_mesh_shadow_flags(new_mesh, shadow_flags);
                        created.push(new_mesh);
                        new_mesh
                    }
                    Err(e) => {
                        tracing::warn!("Model: duplicate_mesh_with_transform failed: {e}");
                        continue;
                    }
                }
            };
            let mat_key = match model_ref.material.as_ref() {
                Some(inst) => {
                    if let Some(mut merged) = builtin_merged(inst, &model_ref.inline_material) {
                        merged.vertex_colors_enabled = vertex_color_set.is_some();
                        material::insert_material_vc(&mut r, &merged, vertex_color_set)
                    } else if let Some(k) = super::dynamic::insert_custom(&mut r, inst) {
                        k
                    } else {
                        material::insert_magenta(&mut r)
                    }
                }
                None => material::insert_magenta(&mut r),
            };
            let _ = r.set_mesh_material(target, mat_key);
            material_keys.push(mat_key);
            to_register.push(target);
        }
        if let Err(e) = r.finalize_gpu_textures().await {
            tracing::warn!("finalize_gpu_textures (model): {e}");
        }
    }

    for mk in &to_register {
        bridge().register_mesh(*mk, entry.node_id);
    }
    // Duplicates + the materials we inserted are owned (torn down) by this node;
    // skinned originals belong to the populate pass and stay put.
    entry.model_meshes.lock().unwrap().extend(created);
    entry.material_keys.lock().unwrap().extend(material_keys);
}

/// Authored polyline (`NodeKind::Line`) → fat-line strip. The fat-line pipeline
/// reads world-space positions, so the node transform is baked in CPU-side.
async fn materialize_line(entry: Arc<RendererNode>, def: awsm_scene_schema::LineDef) {
    if def.points.len() < 2 {
        return;
    }
    let parent_tk = entry.transform_key;
    let positions: Vec<Vec3> = def.points.iter().map(|p| Vec3::from_array(p.pos)).collect();
    let colors: Vec<Vec4> = def
        .points
        .iter()
        .map(|p| Vec4::from_array(p.color))
        .collect();
    let entry2 = entry.clone();
    let line_key = with_renderer_mut(move |r| {
        let world = r
            .transforms
            .get_world(parent_tk)
            .copied()
            .unwrap_or(glam::Mat4::IDENTITY);
        let positions_world: Vec<Vec3> = positions
            .iter()
            .map(|p| world.transform_point3(*p))
            .collect();
        match r.add_line_strip(
            &positions_world,
            &colors,
            def.width_px,
            def.depth_test_always,
        ) {
            Ok(key) => key,
            Err(err) => {
                tracing::warn!("materialize_line: add_line_strip failed: {err}");
                None
            }
        }
    })
    .await;
    if let Some(key) = line_key {
        entry2.line_keys.lock().unwrap().push(key);
    }
}

/// Curve viz (`NodeKind::Curve`) → a sampled Catmull-Rom polyline drawn as a
/// magenta fat-line (the curve itself emits no game geometry; sweeps/instances
/// consume it). World-space, parent transform baked in.
async fn materialize_curve_viz(entry: Arc<RendererNode>, def: awsm_scene_schema::CurveDef) {
    if def.control_points.len() < 2 {
        return;
    }
    let parent_tk = entry.transform_key;
    let entry2 = entry.clone();
    let line_key = with_renderer_mut(move |r| {
        use awsm_curves::{CatmullRomCurve, Curve3};
        let curve = CatmullRomCurve::new(
            def.control_points
                .iter()
                .map(|p| Vec3::from_array(*p))
                .collect(),
            def.closed,
        );
        let samples = def.sample_count.max(2) as usize;
        let mut positions = curve.get_spaced_points(samples);
        if positions.is_empty() {
            return None;
        }
        let world = r
            .transforms
            .get_world(parent_tk)
            .copied()
            .unwrap_or(glam::Mat4::IDENTITY);
        for p in positions.iter_mut() {
            *p = world.transform_point3(*p);
        }
        if def.closed {
            if let Some(first) = positions.first().copied() {
                positions.push(first);
            }
        }
        let colors: Vec<Vec4> = vec![Vec4::new(1.0, 0.45, 0.85, 0.95); positions.len()];
        // Wider than a hairline so the curve reads clearly in the viewport —
        // a thin line is nearly invisible against the ground grid, especially
        // for flat (default) curves.
        match r.add_line_strip(&positions, &colors, 3.0, false) {
            Ok(key) => key,
            Err(err) => {
                tracing::warn!("materialize_curve_viz: add_line_strip failed: {err}");
                None
            }
        }
    })
    .await;
    if let Some(key) = line_key {
        entry2.line_keys.lock().unwrap().push(key);
    }
}

/// Textured/tinted quad (`NodeKind::Sprite`) → a `sprite_quad` mesh with the
/// renderer's billboard mode. Single-cell unlit-ish quad (the flipbook-animated
/// variant is the follow-on); sprites don't cast/receive shadows.
async fn materialize_sprite(entry: Arc<RendererNode>, def: awsm_scene_schema::SpriteDef) {
    use awsm_meshgen::sprite_quad;
    use awsm_renderer::meshes::mesh::BillboardMode;

    let mesh = sprite_quad(def.size[0], def.size[1]);
    let raw = RawMeshData {
        positions: mesh.positions,
        normals: mesh.normals,
        uvs: mesh.uvs,
        colors: mesh.colors,
        indices: mesh.indices,
    };
    let sprite_mat = awsm_scene_schema::MaterialDef {
        base_color: def.tint,
        metallic: 0.0,
        roughness: 1.0,
        emissive: [def.tint[0] * 1.8, def.tint[1] * 1.8, def.tint[2] * 1.8],
        double_sided: true,
        ..awsm_scene_schema::MaterialDef::default()
    };
    let mode = match def.billboard {
        awsm_scene_schema::BillboardMode::None => BillboardMode::None,
        awsm_scene_schema::BillboardMode::YAxis => BillboardMode::YAxis,
        awsm_scene_schema::BillboardMode::Full => BillboardMode::Full,
    };
    let parent_tk = entry.transform_key;

    let handle = renderer_handle();
    let mut r = handle.lock().await;
    let mat_key = material::insert_material(&mut r, &sprite_mat);
    let sub_tk = r.transforms.insert(Transform::IDENTITY, Some(parent_tk));
    match r.add_raw_mesh(raw, sub_tk, mat_key) {
        Ok(mk) => {
            if let Err(e) = r.finalize_gpu_textures().await {
                tracing::warn!("sprite finalize_gpu_textures: {e}");
            }
            let _ = r.set_mesh_billboard_mode(mk, mode);
            drop(r);
            entry.model_meshes.lock().unwrap().push(mk);
            entry.model_transforms.lock().unwrap().push(sub_tk);
            entry.material_keys.lock().unwrap().push(mat_key);
            bridge().register_mesh(mk, entry.node_id);
        }
        Err(e) => {
            r.transforms.remove(sub_tk);
            r.remove_material(mat_key);
            tracing::error!("materialize sprite failed: {e}");
        }
    }
}

/// Collider (`NodeKind::Collider`) → an editor-overlay wireframe of the shape,
/// drawn as a world-baked fat-line segment list (one-shot; re-materializes on
/// shape/transform change via the kind observer).
async fn materialize_collider(entry: Arc<RendererNode>, shape: awsm_scene_schema::ColliderShape) {
    let parent_tk = entry.transform_key;
    let entry2 = entry.clone();
    let line_key = with_renderer_mut(move |r| {
        let world = r
            .transforms
            .get_world(parent_tk)
            .copied()
            .unwrap_or(glam::Mat4::IDENTITY);
        let (positions, colors) = super::collider_wire::build(&shape, &world);
        if positions.is_empty() {
            return None;
        }
        match r.add_line_segments(&positions, &colors, 1.5, false) {
            Ok(key) => key,
            Err(err) => {
                tracing::warn!("materialize_collider: add_line_segments failed: {err}");
                None
            }
        }
    })
    .await;
    if let Some(key) = line_key {
        entry2.line_keys.lock().unwrap().push(key);
    }
}

/// Projection decal (`NodeKind::Decal`) → inserts the renderer decal (inert
/// until a texture is assigned) plus a unit-cube volume wireframe so the decal
/// is placeable/visible in the editor (the projection volume).
async fn materialize_decal(entry: Arc<RendererNode>, cfg: awsm_scene_schema::DecalConfig) {
    let parent_tk = entry.transform_key;
    let entry2 = entry.clone();
    let alpha = cfg.alpha;
    with_renderer_mut(move |r| {
        let world = r
            .transforms
            .get_world(parent_tk)
            .copied()
            .unwrap_or(glam::Mat4::IDENTITY);
        match r.insert_decal(world, 0, alpha) {
            Ok(key) => entry2.decal_keys.lock().unwrap().push(key),
            Err(err) => tracing::warn!("insert_decal: {err:?}"),
        }
        let (positions, colors) = super::collider_wire::build(
            &awsm_scene_schema::ColliderShape::Box {
                half_extents: [0.5, 0.5, 0.5],
            },
            &world,
        );
        if !positions.is_empty() {
            if let Ok(Some(lk)) = r.add_line_segments(&positions, &colors, 1.5, false) {
                entry2.line_keys.lock().unwrap().push(lk);
            }
        }
    })
    .await;
}

/// The single curve node referenced by a sweep/instances node, if it exists and
/// is a `Curve`.
fn lookup_curve_def(node_id: NodeId) -> Option<awsm_scene_schema::CurveDef> {
    let b = bridge();
    let entry = b.nodes.lock().unwrap().get(&node_id).cloned()?;
    match entry.node.kind.get_cloned() {
        NodeKind::Curve(c) => Some(c),
        _ => None,
    }
}

/// Insert an inline-material mesh + track it on the node (the shared path for
/// procedural geometry that isn't a primitive: sweeps, instances, shared mesh).
async fn upload_simple_mesh(
    entry: Arc<RendererNode>,
    raw: RawMeshData,
    inline: awsm_scene_schema::MaterialDef,
) -> Option<awsm_renderer::meshes::MeshKey> {
    let parent_tk = entry.transform_key;
    let handle = renderer_handle();
    let mut r = handle.lock().await;
    let mat_key = material::insert_material(&mut r, &inline);
    let sub_tk = r.transforms.insert(Transform::IDENTITY, Some(parent_tk));
    match r.add_raw_mesh(raw, sub_tk, mat_key) {
        Ok(mk) => {
            if let Err(e) = r.finalize_gpu_textures().await {
                tracing::warn!("upload_simple_mesh finalize: {e}");
            }
            drop(r);
            entry.model_meshes.lock().unwrap().push(mk);
            entry.model_transforms.lock().unwrap().push(sub_tk);
            entry.material_keys.lock().unwrap().push(mat_key);
            bridge().register_mesh(mk, entry.node_id);
            Some(mk)
        }
        Err(e) => {
            r.transforms.remove(sub_tk);
            r.remove_material(mat_key);
            tracing::error!("upload_simple_mesh failed: {e}");
            None
        }
    }
}

/// Sweep a cross-section along the referenced curve (`NodeKind::SweepAlongCurve`)
/// → solid geometry. Renders only once its `curve_node` points at a real Curve.
async fn materialize_sweep(
    entry: Arc<RendererNode>,
    def: awsm_scene_schema::SweepAlongCurveDef,
    inline: awsm_scene_schema::MaterialDef,
) {
    use awsm_curves::CatmullRomCurve;
    use awsm_scene_schema::{CrossSectionDef, SweepUvMode};

    // Nil curve ref = "not wired up yet"; render empty quietly until the user picks one.
    if def.curve_node.is_nil() {
        return;
    }
    let Some(curve_def) = lookup_curve_def(def.curve_node) else {
        tracing::warn!("SweepAlongCurve references missing/!curve node");
        return;
    };
    let curve = CatmullRomCurve::new(
        curve_def
            .control_points
            .iter()
            .map(|p| Vec3::from_array(*p))
            .collect(),
        curve_def.closed,
    );
    let cs = match def.cross_section {
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
    let mesh = sweep_along_curve(&curve, &cs, &opts);
    let raw = RawMeshData {
        positions: mesh.positions,
        normals: mesh.normals,
        uvs: mesh.uvs,
        colors: mesh.colors,
        indices: mesh.indices,
    };
    upload_simple_mesh(entry, raw, inline).await;
}

/// Place copies of a source primitive along the referenced curve
/// (`NodeKind::InstancesAlongCurve`) via GPU instancing. Renders once both its
/// `curve_node` (a Curve) and `source_node` (a Primitive) point at real nodes.
async fn materialize_instances(
    entry: Arc<RendererNode>,
    def: awsm_scene_schema::InstancesAlongCurveDef,
) {
    use awsm_curves::{CatmullRomCurve, Curve3, FrameSequence};
    use awsm_renderer::instances::InstanceAttr;

    // Both refs are optional; a nil sentinel just means "not wired up yet" — the
    // node renders empty until the user picks a curve + a source primitive.
    if def.curve_node.is_nil() || def.source_node.is_nil() {
        return;
    }
    let Some(curve_def) = lookup_curve_def(def.curve_node) else {
        tracing::warn!("InstancesAlongCurve references missing curve node");
        return;
    };
    let shape = {
        let b = bridge();
        let src = b.nodes.lock().unwrap().get(&def.source_node).cloned();
        match src.map(|e| e.node.kind.get_cloned()) {
            Some(NodeKind::Primitive { shape, .. }) => shape,
            _ => {
                tracing::warn!("InstancesAlongCurve source node is missing/not a Primitive");
                return;
            }
        }
    };

    let curve = CatmullRomCurve::new(
        curve_def
            .control_points
            .iter()
            .map(|p| Vec3::from_array(*p))
            .collect(),
        curve_def.closed,
    );
    let total_len = curve.total_length(curve_def.sample_count.max(8) as usize);
    let spacing = def.spacing.max(0.05);
    let count = ((total_len / spacing).floor() as usize).max(1);
    let frames = FrameSequence::parallel_transport(&curve, count.max(2), Vec3::Y);

    let has_colors = !def.per_instance_colors.is_empty();
    let mut transforms = Vec::with_capacity(count);
    let mut attrs = Vec::with_capacity(count);
    for (i, frame) in frames.frames.iter().enumerate() {
        let mut translation = frame.position;
        if def.side_offset.abs() > 1.0e-4 {
            translation += frame.binormal * def.side_offset;
        }
        let rotation = if def.orient_to_tangent {
            frame.rotation()
        } else {
            Quat::IDENTITY
        };
        transforms.push(Transform {
            translation,
            rotation,
            scale: Vec3::ONE,
        });
        let rgba = if has_colors {
            def.per_instance_colors[i.min(def.per_instance_colors.len() - 1)]
        } else {
            [1.0, 1.0, 1.0, 1.0]
        };
        attrs.push(InstanceAttr::from_rgba_alpha_size(rgba, 1.0, 1.0));
    }

    let mesh = primitive_to_mesh(&shape);
    let raw = RawMeshData {
        positions: mesh.positions,
        normals: mesh.normals,
        uvs: mesh.uvs,
        colors: mesh.colors,
        indices: mesh.indices,
    };
    let mesh_key = upload_simple_mesh(entry, raw, awsm_scene_schema::MaterialDef::default()).await;
    if let Some(mk) = mesh_key {
        with_renderer_mut(move |r| {
            if let Err(err) = r.enable_mesh_instancing_opaque(mk, &transforms) {
                tracing::warn!("enable_mesh_instancing_opaque failed: {err}");
            }
            if has_colors {
                if let Ok(tk) = r.meshes.get(mk).map(|m| m.transform_key) {
                    if let Err(err) = r.set_mesh_instance_attrs(tk, &attrs) {
                        tracing::warn!("set_mesh_instance_attrs failed: {err}");
                    }
                }
            }
        })
        .await;
    }
}

/// Particle emitter (`NodeKind::ParticleEmitter`) → an auto-playing simulator +
/// instanced billboard quad, ticked each frame by the render loop.
async fn materialize_particle(
    entry: Arc<RendererNode>,
    def: awsm_scene_schema::ParticleEmitterDef,
) {
    let parent_tk = entry.transform_key;
    let node_id = entry.node_id;
    with_renderer_mut(move |r| {
        let world_pos = r
            .transforms
            .get_world(parent_tk)
            .map(|m| m.w_axis.truncate())
            .unwrap_or(Vec3::ZERO);
        super::particles::materialize(r, node_id, parent_tk, world_pos, &def);
    })
    .await;
}

async fn apply_light(entry: Arc<RendererNode>, cfg: LightConfig) {
    let trs = entry.node.transform.get();
    let pos = Vec3::from_array(trs.translation);
    let dir = (Quat::from_array(trs.rotation) * Vec3::NEG_Z).normalize_or_zero();
    let light = light_from_config(&cfg, pos, dir);
    let node_id = entry.node_id;

    let shadow_params = light_shadow_params_from_config(cfg.shadow());
    let casts = shadow_params.cast;
    let parent_tk = entry.transform_key;
    let key = with_renderer_mut(move |r| {
        let key = r.insert_light(light, Some(shadow_params));
        // Bind the light to its node transform so the per-frame
        // `update_from_transforms` re-derives position/direction whenever the
        // light node moves/rotates — without this a directional light's
        // direction is frozen at materialize time and casts no useful shadow.
        if let Ok(k) = key {
            r.lights.bind_transform(k, parent_tk);
        }
        key
    })
    .await;
    // Lazily compile the shadow pipelines when a casting light first lands so the
    // next frame can draw shadows (no-op once compiled / when nothing casts).
    if casts {
        let handle = renderer_handle();
        let mut r = handle.lock().await;
        if let Err(e) = r.ensure_shadow_pipelines_compiled().await {
            tracing::warn!("ensure_shadow_pipelines_compiled: {e:?}");
        }
    }
    match key {
        Ok(k) => {
            *entry.light_key.lock().unwrap() = Some(k);
            bridge().light_node_ids.lock().unwrap().insert(node_id);
        }
        Err(e) => tracing::error!("insert_light failed: {e:?}"),
    }
}

/// Materialize a `Camera` node into the renderer's camera-params store. The node
/// has no GPU geometry — this slot mirrors the node's `CameraConfig` and is what
/// an `AnimationTarget::Camera` channel mutates. The render loop reads this slot
/// (not the node config directly) so an animated camera is live; for a static
/// camera the slot equals the config, so the projection is unchanged.
///
/// `apply_kind` tears down (removing any prior slot) before this runs, and the
/// kind observer re-fires on every `SetKind`, so editing the camera config
/// re-inserts a slot that reflects the new config — keeping store and config in
/// sync without a separate observer.
async fn materialize_camera(entry: Arc<RendererNode>, cfg: awsm_scene_schema::CameraConfig) {
    let params = camera_params_from_config(&cfg);
    let key = with_renderer_mut(move |r| r.cameras.insert(params)).await;
    *entry.camera_key.lock().unwrap() = Some(key);
}

/// Schema camera config → renderer camera params. Maps the projection kind +
/// clip planes; depth-of-field (`aperture`/`focus_distance`) isn't authored on
/// the node config yet, so it defaults to the same values `scene_camera_matrices`
/// has always used (`5.6` / `10.0`).
fn camera_params_from_config(cfg: &awsm_scene_schema::CameraConfig) -> CameraParams {
    use awsm_scene_schema::CameraProjection;
    let projection = match cfg.projection {
        CameraProjection::Perspective { fov_y_rad } => {
            CameraProjectionParams::Perspective { fov_y_rad }
        }
        CameraProjection::Orthographic { half_height } => {
            CameraProjectionParams::Orthographic { half_height }
        }
    };
    CameraParams {
        projection,
        near: cfg.near,
        far: cfg.far,
        aperture: 5.6,
        focus_distance: 10.0,
    }
}

/// Schema → runtime light shadow params.
fn light_shadow_params_from_config(
    cfg: &awsm_scene_schema::LightShadowConfig,
) -> awsm_renderer::shadows::LightShadowParams {
    use awsm_renderer::shadows as r;
    use awsm_scene_schema as s;
    r::LightShadowParams {
        cast: cfg.cast,
        depth_bias: cfg.depth_bias,
        normal_bias: cfg.normal_bias,
        resolution: cfg.resolution,
        hardness: match cfg.hardness {
            s::LightShadowHardness::Hard => r::LightShadowHardness::Hard,
            s::LightShadowHardness::Soft => r::LightShadowHardness::Soft,
            s::LightShadowHardness::Pcss => r::LightShadowHardness::Pcss,
        },
        pcss_penumbra_scale: cfg.pcss_penumbra_scale,
        max_distance: cfg.max_distance,
        cascade_count: cfg.cascade_count,
        cascade_split_lambda: cfg.cascade_split_lambda,
        evsm_cutoff: match cfg.evsm_cutoff {
            s::EvsmCutoff::Off => r::EvsmCutoff::Off,
            s::EvsmCutoff::LastCascade => r::EvsmCutoff::LastCascade,
            s::EvsmCutoff::LastTwoCascades => r::EvsmCutoff::LastTwoCascades,
        },
        far_cascade_update_rate: match cfg.far_cascade_update_rate {
            s::FarCascadeUpdateRate::EveryFrame => r::FarCascadeUpdateRate::EveryFrame,
            s::FarCascadeUpdateRate::Every2Frames => r::FarCascadeUpdateRate::Every2Frames,
            s::FarCascadeUpdateRate::Every4Frames => r::FarCascadeUpdateRate::Every4Frames,
            s::FarCascadeUpdateRate::Every8Frames => r::FarCascadeUpdateRate::Every8Frames,
        },
        cube_face_update_rate: match cfg.cube_face_update_rate {
            s::CubeFaceUpdateRate::EveryFrame => r::CubeFaceUpdateRate::EveryFrame,
            s::CubeFaceUpdateRate::Every2Frames => r::CubeFaceUpdateRate::Every2Frames,
            s::CubeFaceUpdateRate::Every4Frames => r::CubeFaceUpdateRate::Every4Frames,
            s::CubeFaceUpdateRate::Every8Frames => r::CubeFaceUpdateRate::Every8Frames,
        },
    }
}

/// Schema → runtime per-mesh shadow cast/receive flags.
fn mesh_shadow_flags_from_config(
    cfg: &awsm_scene_schema::MeshShadowConfig,
) -> awsm_renderer::shadows::MeshShadowFlags {
    awsm_renderer::shadows::MeshShadowFlags {
        cast: cfg.cast,
        receive: cfg.receive,
    }
}

fn light_from_config(
    cfg: &LightConfig,
    position: Vec3,
    direction: Vec3,
) -> awsm_renderer::lights::Light {
    use awsm_renderer::lights::Light;
    match cfg {
        LightConfig::Directional {
            color, intensity, ..
        } => Light::Directional {
            color: *color,
            intensity: *intensity,
            direction: direction.to_array(),
        },
        LightConfig::Point {
            color,
            intensity,
            range,
            ..
        } => Light::Point {
            color: *color,
            intensity: *intensity,
            position: position.to_array(),
            range: *range,
        },
        LightConfig::Spot {
            color,
            intensity,
            range,
            inner_angle,
            outer_angle,
            ..
        } => Light::Spot {
            color: *color,
            intensity: *intensity,
            position: position.to_array(),
            direction: direction.to_array(),
            range: *range,
            inner_angle: *inner_angle,
            outer_angle: *outer_angle,
        },
    }
}

pub fn primitive_to_mesh(shape: &PrimitiveShape) -> MeshData {
    match shape {
        PrimitiveShape::Plane {
            width,
            depth,
            segments_x,
            segments_z,
        } => plane_mesh(*width, *depth, *segments_x, *segments_z),
        PrimitiveShape::Box { dims } => box_mesh(Vec3::from_array(*dims)),
        PrimitiveShape::Sphere {
            radius,
            segments_long,
            segments_lat,
        } => sphere_mesh(*radius, *segments_long, *segments_lat),
        PrimitiveShape::Cylinder {
            radius,
            height,
            radial_segments,
        } => cylinder_mesh(*radius, *height, *radial_segments),
        PrimitiveShape::Cone {
            radius,
            height,
            radial_segments,
        } => cone_mesh(*radius, *height, *radial_segments),
        PrimitiveShape::Torus {
            radius,
            thickness,
            segments_major,
            segments_minor,
        } => torus_mesh(*radius, *thickness, *segments_major, *segments_minor),
    }
}

pub(crate) fn trs_to_transform(trs: &Trs) -> Transform {
    Transform {
        translation: Vec3::from_array(trs.translation),
        rotation: Quat::from_array(trs.rotation),
        scale: Vec3::from_array(trs.scale),
    }
}
