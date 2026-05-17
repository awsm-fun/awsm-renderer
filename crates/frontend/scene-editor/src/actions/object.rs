//! Duplicate / Split / Deselect / Delete / capture-as-mesh on the current
//! selection.

use crate::scene::{mutate, ModelRef, Node, NodeId, NodeKind};
use crate::state::app_state;

pub fn duplicate() {
    let state = app_state();
    let ids: Vec<NodeId> = {
        let set = state.selected.lock_ref();
        set.iter().copied().collect()
    };
    if ids.is_empty() {
        return;
    }
    let roots = mutate::ancestor_dedup(&state.scene, ids);
    if roots.is_empty() {
        return;
    }

    let previous = state.snapshot_scene();
    let mut new_ids = Vec::with_capacity(roots.len());
    for id in roots {
        if let Some(new_id) = mutate::duplicate_by_id(&state.scene, id) {
            new_ids.push(new_id);
        }
    }
    if new_ids.is_empty() {
        return;
    }
    state.scene.bump_revision();
    state.set_selection(new_ids.iter().copied(), new_ids.first().copied());
    state.commit_history(previous);
    tracing::info!("action: object::duplicate — {} node(s)", new_ids.len());
}

/// Split each splittable selected `Model` node into one editor node per
/// gltf mesh primitive. The original node is repointed at primitive 0
/// (so its `NodeId` and place in the tree are preserved), and primitives
/// 1..N are added as siblings immediately after it sharing the same
/// transform.
///
/// Bulk action: a no-op selection or a selection containing nothing
/// splittable does nothing (and the button is disabled in that case via
/// `AppState::can_split_signal`). Recorded as a single history entry.
pub fn split() {
    let state = app_state();
    let ids: Vec<NodeId> = state.selected.lock_ref().iter().copied().collect();
    if ids.is_empty() {
        return;
    }

    // Snapshot up front so a single undo step rolls the whole batch
    // back, and so we can bail without a commit if nothing actually splits.
    let previous = state.snapshot_scene();
    let mut newly_inserted: Vec<NodeId> = Vec::new();

    for id in ids {
        let Some(node) = mutate::find_by_id(&state.scene, id) else {
            continue;
        };

        // Pull the live mesh count off the bridge entry. If the asset
        // hasn't loaded yet (or this isn't a Model), there's nothing to
        // split right now — skip.
        let Some(entry) = state
            .renderer_bridge
            .nodes
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
        else {
            continue;
        };
        let mesh_count = entry.model_meshes.lock().unwrap().len();
        if mesh_count <= 1 {
            continue;
        }

        let model_ref = match &*node.kind.lock_ref() {
            NodeKind::Model(r) if r.primitive_index.is_none() => r.clone(),
            // Group / Light / Collision / already-split Model → nothing to do.
            _ => continue,
        };

        let base_name = node.name.get_cloned();
        let trs = node.transform.get();

        // Repoint the original at primitive 0. This triggers `apply_kind`
        // in the bridge, which wipes the existing meshes and re-instances
        // just primitive 0.
        node.kind.set(NodeKind::Model(ModelRef {
            asset_id: model_ref.asset_id,
            node_index: model_ref.node_index,
            primitive_index: Some(0),
        }));

        // Determine where to insert the siblings. If `id` is at the top
        // level we splice into `scene.nodes`; otherwise we splice into the
        // parent's `children` immediately after `id`. We have to find the
        // index *after* mutating the original node above — kind change
        // doesn't reorder anything, so the position is stable.
        let parent = mutate::find_parent(&state.scene, id);
        for i in 1..mesh_count {
            let new_node = Node::new_with_transform_and_kind(
                format!("{base_name} ({i})"),
                trs,
                NodeKind::Model(ModelRef {
                    asset_id: model_ref.asset_id,
                    node_index: model_ref.node_index,
                    primitive_index: Some(i as u32),
                }),
            );
            let new_id = new_node.id;

            match &parent {
                Some(parent_node) => {
                    let mut children = parent_node.children.lock_mut();
                    let position = children
                        .iter()
                        .position(|c| c.id == id)
                        .map(|p| p + i)
                        .unwrap_or_else(|| children.len());
                    let clamped = position.min(children.len());
                    children.insert_cloned(clamped, new_node);
                }
                None => {
                    let mut nodes = state.scene.nodes.lock_mut();
                    let position = nodes
                        .iter()
                        .position(|c| c.id == id)
                        .map(|p| p + i)
                        .unwrap_or_else(|| nodes.len());
                    let clamped = position.min(nodes.len());
                    nodes.insert_cloned(clamped, new_node);
                }
            }
            newly_inserted.push(new_id);
        }
    }

    if newly_inserted.is_empty() {
        return;
    }

    state.scene.bump_revision();
    state.commit_history(previous);
    tracing::info!(
        "action: object::split — peeled {} new node(s) off",
        newly_inserted.len()
    );
}

pub fn deselect() {
    let state = app_state();
    if state.selected.lock_ref().is_empty() {
        return;
    }
    state.clear_selection();
    tracing::info!("action: object::deselect — done");
}

/// Snapshot a procedural-mesh node's current geometry into a fresh
/// `AssetSource::Mesh(MeshDef)` asset and re-point the node at it via
/// `NodeKind::Mesh`. Supports `Primitive` + `SweepAlongCurve` — the
/// node's authored material binding rides along onto the resulting
/// `Mesh` kind.
///
/// The captured bytes are bitcode-`CapturedMesh` and live in two
/// places after this returns:
/// - `pending_assets` keyed by the new `AssetId`, ready to be flushed
///   to `assets/<asset-id>.mesh.bin` on Save.
/// - `mesh_cache` (decoded) so the next materialize skips the disk
///   round-trip entirely.
///
/// Returns the new `AssetId` on success. Returns `None` if the node's
/// kind isn't capturable, the curve lookup failed, etc.
pub fn capture_as_mesh_asset(node_id: NodeId) -> Option<awsm_scene_schema::AssetId> {
    use awsm_scene_schema::{
        AssetEntry, AssetId, AssetSource, CapturedSource, MeshDef, MeshRef, PrimitiveShape,
    };

    let state = app_state();
    let scene = state.scene.clone();
    let node = mutate::find_by_id(&scene, node_id)?;
    let current_kind = node.kind.get_cloned();

    // Snapshot the geometry via the shared kind_to_mesh_data helper.
    // Anything not capturable (Group / Light / Curve / etc.) falls
    // through; bail with a warn so the caller can show feedback.
    let Some(mesh_data) = crate::renderer_bridge::procedural_sync::kind_to_mesh_data(&current_kind)
    else {
        tracing::warn!(
            "capture_as_mesh_asset: node {node_id} kind isn't capturable (only Primitive / Sweep today)"
        );
        return None;
    };
    let (label, material_ref, inline_material, captured_source) = match current_kind {
        NodeKind::Primitive {
            shape,
            material,
            inline_material,
        } => {
            let label = match &shape {
                PrimitiveShape::Plane { .. } => "Captured plane",
                PrimitiveShape::Box { .. } => "Captured box",
                PrimitiveShape::Sphere { .. } => "Captured sphere",
                PrimitiveShape::Cylinder { .. } => "Captured cylinder",
                PrimitiveShape::Cone { .. } => "Captured cone",
                PrimitiveShape::Torus { .. } => "Captured torus",
            };
            (
                label.to_string(),
                material,
                inline_material,
                CapturedSource::Primitive(shape),
            )
        }
        NodeKind::SweepAlongCurve {
            def,
            material,
            inline_material,
        } => (
            "Captured sweep".to_string(),
            material,
            inline_material,
            CapturedSource::Sweep(def),
        ),
        _ => unreachable!("kind_to_mesh_data returned Some for a non-capturable kind"),
    };

    let captured = mesh_data_to_captured(mesh_data);
    let bytes = bitcode::serialize(&captured).ok()?;
    let asset_id = AssetId::new();

    let previous = state.snapshot_scene();
    {
        let mut table = scene.assets.lock().unwrap();
        table.entries.insert(
            asset_id,
            AssetEntry::new(AssetSource::Mesh(MeshDef {
                label: label.clone(),
                source: Some(captured_source),
            })),
        );
    }
    state.pending_assets.lock().unwrap().insert(asset_id, bytes);
    crate::renderer_bridge::mesh_cache::insert(asset_id, captured);

    // Re-point the node at the new Mesh asset, preserving the material
    // binding so the visual is identical post-capture.
    node.kind.set(NodeKind::Mesh {
        mesh: MeshRef(asset_id),
        material: material_ref,
        inline_material,
    });

    scene.bump_revision();
    state.commit_history(previous);
    tracing::info!("action: object::capture_as_mesh_asset({node_id}) -> {asset_id} ({label})");
    awsm_web_shared::prelude::Toast::info(format!("Captured: {label}"));
    Some(asset_id)
}

fn mesh_data_to_captured(m: awsm_meshgen::MeshData) -> awsm_scene_schema::CapturedMesh {
    awsm_scene_schema::CapturedMesh {
        positions: m.positions,
        normals: m.normals,
        uvs: m.uvs,
        colors: m.colors,
        indices: m.indices,
    }
}

/// Re-snapshot `source_node_id`'s current geometry into the existing
/// `target_asset_id` (an `AssetSource::Mesh` entry), overwriting the
/// bytes that asset points at. Every node referencing the asset picks
/// up the new geometry on the next materialize.
///
/// The user-flow: open the Mesh asset in the right sidebar, pick a
/// Primitive / Sweep source node in the tree, click "Re-capture from
/// source". The asset's AssetId is stable; only the bytes change.
///
/// Returns `true` on success. Returns `false` and logs when the source
/// kind isn't capturable, the target isn't a Mesh asset, etc.
pub fn recapture_into_existing(
    source_node_id: NodeId,
    target_asset_id: awsm_scene_schema::AssetId,
) -> bool {
    use awsm_scene_schema::{AssetSource, CapturedSource};
    let state = app_state();
    let scene = state.scene.clone();

    let source_node = match mutate::find_by_id(&scene, source_node_id) {
        Some(n) => n,
        None => {
            tracing::warn!("recapture_into_existing: source node {source_node_id} not found");
            return false;
        }
    };
    let source_kind = source_node.kind.get_cloned();
    let Some(mesh_data) = crate::renderer_bridge::procedural_sync::kind_to_mesh_data(&source_kind)
    else {
        tracing::warn!(
            "recapture_into_existing: source node {source_node_id} kind isn't capturable"
        );
        return false;
    };
    // Verify the target is actually a Mesh asset before clobbering it.
    {
        let table = scene.assets.lock().unwrap();
        if !matches!(
            table.get(target_asset_id).map(|e| &e.source),
            Some(AssetSource::Mesh(_))
        ) {
            tracing::warn!(
                "recapture_into_existing: target {target_asset_id} isn't an AssetSource::Mesh"
            );
            return false;
        }
    }

    let new_source = match &source_kind {
        NodeKind::Primitive { shape, .. } => Some(CapturedSource::Primitive(shape.clone())),
        NodeKind::SweepAlongCurve { def, .. } => Some(CapturedSource::Sweep(def.clone())),
        _ => None,
    };
    let captured = mesh_data_to_captured(mesh_data);
    let Ok(bytes) = bitcode::serialize(&captured) else {
        return false;
    };

    let previous = state.snapshot_scene();
    // Refresh the MeshDef.source metadata so the inspector keeps
    // showing the params the bytes were captured from.
    if let Some(new_source) = new_source {
        let mut table = scene.assets.lock().unwrap();
        if let Some(entry) = table.entries.get_mut(&target_asset_id) {
            if let AssetSource::Mesh(def) = &mut entry.source {
                def.source = Some(new_source);
            }
        }
    }
    // Overwrite pending bytes + cache. The AssetId stays the same so
    // every referencing NodeKind::Mesh keeps its binding; bumping the
    // scene revision triggers re-materialize through the bridge's
    // standard observer chain.
    state
        .pending_assets
        .lock()
        .unwrap()
        .insert(target_asset_id, bytes);
    crate::renderer_bridge::mesh_cache::insert(target_asset_id, captured);

    // Force every NodeKind::Mesh that references `target_asset_id` to
    // re-materialize. The kind value didn't change, so we have to
    // explicitly invalidate the bridge's identity cache (F-A fast
    // path); without it the apply_kind short-circuit would skip the
    // rematerialize. Then re-setting the kind triggers the observer
    // → fresh read from mesh_cache (which we just overwrote).
    let referencing = collect_mesh_nodes_referencing(&scene, target_asset_id);
    for node in referencing {
        crate::renderer_bridge::RendererNode::invalidate_apply_kind_cache(node.id);
        let kind = node.kind.get_cloned();
        node.kind.set(kind);
    }

    scene.bump_revision();
    state.commit_history(previous);
    tracing::info!(
        "action: object::recapture_into_existing(source={source_node_id}, target={target_asset_id}) — done"
    );
    awsm_web_shared::prelude::Toast::info("Re-captured mesh bytes; live in scene next frame");
    true
}

/// Re-capture a Mesh asset's bytes from a freshly-edited
/// `CapturedSource` (the inspector's editable copy of the original
/// shape / sweep def). Mirrors `recapture_into_existing` but without
/// needing a source node — the def lives on the MeshDef itself.
/// Used by the Mesh asset inspector when the user mutates the
/// captured-source params directly.
pub fn recapture_from_source_def(
    target_asset_id: awsm_scene_schema::AssetId,
    source: &awsm_scene_schema::CapturedSource,
) -> bool {
    use awsm_scene_schema::{AssetSource, CapturedSource};
    let state = app_state();
    let scene = state.scene.clone();

    // Synthesize the temporary kind the shared kind_to_mesh_data
    // helper expects, then evaluate the geometry through the same
    // meshgen path the live materializer uses. For Primitive that's
    // a one-line wrap; for Sweep the helper handles the curve lookup
    // (which still references a live curve_node).
    let temp_kind = match source.clone() {
        CapturedSource::Primitive(shape) => NodeKind::Primitive {
            shape,
            material: None,
            inline_material: Default::default(),
        },
        CapturedSource::Sweep(def) => NodeKind::SweepAlongCurve {
            def,
            material: None,
            inline_material: Default::default(),
        },
    };
    let Some(mesh_data) = crate::renderer_bridge::procedural_sync::kind_to_mesh_data(&temp_kind)
    else {
        tracing::warn!("recapture_from_source_def: synthesized kind didn't produce mesh data");
        return false;
    };

    // Verify the target is actually a Mesh asset before clobbering.
    {
        let table = scene.assets.lock().unwrap();
        if !matches!(
            table.get(target_asset_id).map(|e| &e.source),
            Some(AssetSource::Mesh(_))
        ) {
            return false;
        }
    }

    let captured = mesh_data_to_captured(mesh_data);
    let Ok(bytes) = bitcode::serialize(&captured) else {
        return false;
    };

    let previous = state.snapshot_scene();
    {
        let mut table = scene.assets.lock().unwrap();
        if let Some(entry) = table.entries.get_mut(&target_asset_id) {
            if let AssetSource::Mesh(def) = &mut entry.source {
                def.source = Some(source.clone());
            }
        }
    }
    state
        .pending_assets
        .lock()
        .unwrap()
        .insert(target_asset_id, bytes);
    crate::renderer_bridge::mesh_cache::insert(target_asset_id, captured);

    // Same re-materialize pattern recapture_into_existing uses.
    let referencing = collect_mesh_nodes_referencing(&scene, target_asset_id);
    for node in referencing {
        crate::renderer_bridge::RendererNode::invalidate_apply_kind_cache(node.id);
        let kind = node.kind.get_cloned();
        node.kind.set(kind);
    }

    scene.bump_revision();
    state.commit_history(previous);
    true
}

fn collect_mesh_nodes_referencing(
    scene: &crate::scene::Scene,
    asset_id: awsm_scene_schema::AssetId,
) -> Vec<std::sync::Arc<Node>> {
    fn walk(
        nodes: &[std::sync::Arc<Node>],
        asset_id: awsm_scene_schema::AssetId,
        out: &mut Vec<std::sync::Arc<Node>>,
    ) {
        for n in nodes.iter() {
            if let NodeKind::Mesh { mesh, .. } = &*n.kind.lock_ref() {
                if mesh.0 == asset_id {
                    out.push(n.clone());
                }
            }
            let children = n.children.lock_ref();
            walk(&children, asset_id, out);
        }
    }
    let mut out = Vec::new();
    let nodes = scene.nodes.lock_ref();
    walk(&nodes, asset_id, &mut out);
    out
}

pub fn delete() {
    let state = app_state();
    let ids: Vec<NodeId> = {
        let set = state.selected.lock_ref();
        set.iter().copied().collect()
    };
    if ids.is_empty() {
        return;
    }
    let roots = mutate::ancestor_dedup(&state.scene, ids);
    if roots.is_empty() {
        return;
    }

    let previous = state.snapshot_scene();
    let mut removed = 0;
    for id in roots {
        if mutate::remove_by_id(&state.scene, id).is_some() {
            removed += 1;
        }
    }
    if removed == 0 {
        return;
    }
    state.scene.bump_revision();
    state.clear_selection();
    state.commit_history(previous);
    tracing::info!("action: object::delete — {} node(s)", removed);
}
