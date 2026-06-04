//! Scene→GPU sync: observe the reactive scene tree and materialize/teardown
//! each node's renderer resources. M4-C materializes primitives + lights; other
//! kinds are passive (no GPU mesh yet).

use std::sync::Arc;

use awsm_meshgen::{
    box_mesh, cone_mesh, cylinder_mesh, plane_mesh, sphere_mesh, torus_mesh, MeshData,
};
use awsm_renderer::raw_mesh::RawMeshData;
use awsm_renderer::transforms::{Transform, TransformKey};
use awsm_scene_schema::PrimitiveShape;
use futures_signals::signal::SignalExt;
use futures_signals::signal_vec::{SignalVecExt, VecDiff};
use glam::{Quat, Vec3};

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
    let light = entry.light_key.lock().unwrap().take();
    let node_id = entry.node_id;
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
        if let Some(lk) = light {
            r.remove_light(lk);
        }
    })
    .await;
    bridge().light_node_ids.lock().unwrap().remove(&node_id);
}

/// Materialize (or re-materialize) a node for its current kind.
async fn apply_kind(entry: Arc<RendererNode>, kind: NodeKind) {
    // Tear down the previous materialization (no-op on first apply).
    teardown(&entry).await;

    match kind {
        NodeKind::Primitive {
            shape,
            inline_material,
            ..
        } => materialize_primitive(entry.clone(), shape, inline_material).await,
        NodeKind::Light(cfg) => apply_light(entry.clone(), cfg).await,
        // Group / Camera / Collider / Mesh / Model / Curve / Sweep / Instances /
        // Line / Sprite / Particle / Decal: no GPU mesh in M4-C.
        _ => {}
    }

    *entry.last_kind.lock().unwrap() = Some(entry.node.kind.get_cloned());
}

async fn materialize_primitive(
    entry: Arc<RendererNode>,
    shape: PrimitiveShape,
    inline: awsm_scene_schema::MaterialDef,
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
    let mat_key = material::insert_material(&mut r, &inline);
    let sub_tk = r.transforms.insert(Transform::IDENTITY, Some(parent_tk));
    match r.add_raw_mesh(raw, sub_tk, mat_key) {
        Ok(mk) => {
            if let Err(e) = r.finalize_gpu_textures().await {
                tracing::warn!("finalize_gpu_textures: {e}");
            }
            drop(r);
            entry.model_meshes.lock().unwrap().push(mk);
            entry.model_transforms.lock().unwrap().push(sub_tk);
            entry.material_keys.lock().unwrap().push(mat_key);
        }
        Err(e) => {
            r.transforms.remove(sub_tk);
            r.remove_material(mat_key);
            tracing::error!("materialize primitive failed: {e}");
        }
    }
}

async fn apply_light(entry: Arc<RendererNode>, cfg: LightConfig) {
    let trs = entry.node.transform.get();
    let pos = Vec3::from_array(trs.translation);
    let dir = (Quat::from_array(trs.rotation) * Vec3::NEG_Z).normalize_or_zero();
    let light = light_from_config(&cfg, pos, dir);
    let node_id = entry.node_id;

    // M4-C: no shadows (None) — shadow wiring lands in M6.
    let key = with_renderer_mut(move |r| r.insert_light(light, None)).await;
    match key {
        Ok(k) => {
            *entry.light_key.lock().unwrap() = Some(k);
            bridge().light_node_ids.lock().unwrap().insert(node_id);
        }
        Err(e) => tracing::error!("insert_light failed: {e:?}"),
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

fn primitive_to_mesh(shape: &PrimitiveShape) -> MeshData {
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

fn trs_to_transform(trs: &Trs) -> Transform {
    Transform {
        translation: Vec3::from_array(trs.translation),
        rotation: Quat::from_array(trs.rotation),
        scale: Vec3::from_array(trs.scale),
    }
}
