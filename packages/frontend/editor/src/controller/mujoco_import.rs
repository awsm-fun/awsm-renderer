//! Import a compiled MuJoCo model: the `mujoco.json` sidecar plus the geometry
//! GLB it names.
//!
//! We never read MJCF/URDF — MuJoCo's own compiler produced both files, offline,
//! via `awsm-renderer-mujoco-export`. This is purely the assembly step: turn a
//! flat geom table into a scene subtree a simulation can bind to.
//!
//! ## Shape of the result
//!
//! ```text
//! <model>                    Group, MujocoComponent::Instance
//!   (rotate -90° about X)     Z-up right-handed  →  our Y-up
//!   ├── geom 0                Mesh,  MujocoComponent::Geom { geom_id: 0, … }
//!   ├── geom 3                Mesh,  MujocoComponent::Geom { geom_id: 3, … }
//!   └── …                     (ids are the MODEL's, so gaps are normal)
//! ```
//!
//! The instance root is the one node a user places: its transform is the model's
//! initial placement in the composed world, and the convention rotation rides on
//! it, so the pose stream can write raw MuJoCo world poses onto the geoms
//! beneath with no per-frame conversion anywhere.
//!
//! Geom nodes are flat under the root, NOT nested by MuJoCo body. MuJoCo reports
//! every geom's world pose every frame, so a body hierarchy would only be a
//! second place for those poses to be composed — and composing them twice is
//! exactly the drift the flat layout avoids. (Bodies still exist in the sidecar;
//! `MujocoGeom::body` records which one a geom belongs to for the skin path.)

use std::collections::HashMap;

use awsm_renderer_editor_protocol::mujoco::{GeomKind, Sidecar};
use awsm_renderer_editor_protocol::{
    AssetEntry, AssetId, AssetSource as SceneAssetSource, CapturedSource, MeshDef, MeshRef,
    MujocoComponent, MujocoGeom, MujocoInstance, NodeKind, Trs,
};

use crate::engine::scene::node::Node;
use crate::prelude::*;

/// Fetch + validate a sidecar and its GLB, and build the instance subtree.
///
/// Returns the root node; the caller inserts it and owns the undo entry.
pub async fn import(sidecar_url: &str) -> Result<Arc<Node>, String> {
    let doc = fetch_sidecar(sidecar_url).await?;

    // Geometry, if the model has any. An all-primitive model (the DeepMind
    // humanoid) legitimately has no GLB at all.
    let meshes = match &doc.glb {
        Some(rel) => {
            let glb_url = resolve(sidecar_url, rel)?;
            fetch_meshes(&glb_url, &doc).await?
        }
        None => HashMap::new(),
    };

    Ok(build_subtree(&doc, &meshes))
}

async fn fetch_sidecar(url: &str) -> Result<Sidecar, String> {
    let resp = gloo_net::http::Request::get(url)
        .send()
        .await
        .map_err(|e| format!("fetch {url}: {e}"))?;
    if !resp.ok() {
        return Err(format!("fetch {url}: HTTP {}", resp.status()));
    }
    let text = resp.text().await.map_err(|e| format!("read {url}: {e}"))?;
    let doc: Sidecar =
        serde_json::from_str(&text).map_err(|e| format!("parse {url} as a MuJoCo sidecar: {e}"))?;
    // Validate BEFORE building anything. A sidecar with dangling indices would
    // otherwise produce a subtree that renders and can never bind.
    doc.validate().map_err(|e| format!("{url}: {e}"))?;
    Ok(doc)
}

/// Resolve a sidecar-relative path (the `glb` field) against the sidecar's URL.
fn resolve(base: &str, rel: &str) -> Result<String, String> {
    web_sys::Url::new_with_base(rel, base)
        .map(|u| u.href())
        .map_err(|_| format!("cannot resolve {rel:?} against {base:?}"))
}

/// Load the GLB and pull out one `MeshData` per MuJoCo mesh, keyed by sidecar
/// mesh index.
///
/// The GLB is a flat library — one root node per mesh, in mesh order — and the
/// sidecar names each node explicitly. Match by name first and fall back to
/// position, so a GLB that has been through another tool (which may rename or
/// re-order) still binds as long as the names survive.
async fn fetch_meshes(
    glb_url: &str,
    doc: &Sidecar,
) -> Result<HashMap<usize, awsm_renderer_glb_export::MeshData>, String> {
    let import = crate::engine::bridge::gltf::import(glb_url).await?;

    let mut by_name: HashMap<&str, u32> = HashMap::new();
    let mut order: Vec<u32> = Vec::new();
    collect_named(&import.template.roots, &mut by_name, &mut order);

    let mut out = HashMap::new();
    for (i, m) in doc.meshes.iter().enumerate() {
        let node_index = m
            .node
            .as_deref()
            .and_then(|n| by_name.get(n).copied())
            .or_else(|| order.get(i).copied());
        let Some(node_index) = node_index else {
            tracing::warn!(
                "mujoco import: sidecar mesh {i} ({:?}) has no matching node in the GLB",
                m.name
            );
            continue;
        };
        match import.node_meshes.get(&(node_index, None)) {
            Some((mesh, _tangents)) => {
                out.insert(i, mesh.clone());
            }
            None => tracing::warn!("mujoco import: GLB node {node_index} carries no geometry"),
        }
    }
    Ok(out)
}

fn collect_named<'a>(
    nodes: &'a [crate::engine::bridge::asset_template::AssetTemplateNode],
    by_name: &mut HashMap<&'a str, u32>,
    order: &mut Vec<u32>,
) {
    for n in nodes {
        if let Some(label) = n.label.as_deref() {
            by_name.entry(label).or_insert(n.gltf_node_index);
        }
        order.push(n.gltf_node_index);
        collect_named(&n.children, by_name, order);
    }
}

/// MuJoCo is Z-up right-handed; our world is Y-up right-handed. One rotation, on
/// the instance root, for the whole model — so raw MuJoCo world poses can be
/// written straight onto the geoms as locals.
fn convention_rotation() -> [f32; 4] {
    glam::Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2).to_array()
}

fn build_subtree(
    doc: &Sidecar,
    meshes: &HashMap<usize, awsm_renderer_glb_export::MeshData>,
) -> Arc<Node> {
    let model_name = doc
        .model_name
        .clone()
        .unwrap_or_else(|| doc.source.filename.clone());

    let root = Node::new_with_transform_and_kind(
        model_name.clone(),
        Trs {
            rotation: convention_rotation(),
            ..Trs::default()
        },
        NodeKind::Group,
    );
    root.mujoco
        .set(Some(MujocoComponent::Instance(MujocoInstance {
            model_name: doc.model_name.clone(),
            ..MujocoInstance::new(doc.source.clone(), doc.geoms.len() as u32)
        })));

    // One mesh ASSET per MuJoCo mesh, shared by every geom that uses it — many
    // geoms reference the same mesh (Go2's four legs are one set of parts), and
    // minting a copy each would multiply the geometry by the instance count.
    let mut mesh_assets: HashMap<usize, MeshRef> = HashMap::new();
    for (i, mesh) in meshes {
        let label = doc.meshes[*i]
            .name
            .clone()
            .unwrap_or_else(|| format!("mesh {i}"));
        mesh_assets.insert(*i, mint_mesh(&label, mesh));
    }

    let visible = awsm_renderer_editor_protocol::mujoco::DEFAULT_VISIBLE_GROUPS;
    let mut skipped_kinds: Vec<GeomKind> = Vec::new();

    for (geom_id, geom) in doc.geoms.iter().enumerate() {
        if !visible.contains(&geom.group) {
            continue;
        }
        let name = geom
            .name
            .clone()
            .unwrap_or_else(|| format!("geom {geom_id}"));
        // The geom's WORLD pose in the model's initial configuration, applied as a
        // local under the convention root. NOT `geom.pos`/`geom.quat`, which are
        // body-relative: the geom nodes are flat, so a body-relative offset would
        // stack every limb on the origin instead of placing it. This is also
        // exactly the shape a pose-stream frame carries, so the simulation's first
        // frame continues from the initial render instead of jumping.
        let trs = Trs {
            translation: [
                geom.world_pos[0] as f32,
                geom.world_pos[1] as f32,
                geom.world_pos[2] as f32,
            ],
            // MuJoCo quaternions are [w, x, y, z]; ours are [x, y, z, w].
            rotation: [
                geom.world_quat[1] as f32,
                geom.world_quat[2] as f32,
                geom.world_quat[3] as f32,
                geom.world_quat[0] as f32,
            ],
            scale: [1.0, 1.0, 1.0],
        };

        let kind = match geom.kind {
            GeomKind::Mesh => match geom.mesh.and_then(|m| mesh_assets.get(&m)).cloned() {
                Some(mesh) => NodeKind::Mesh {
                    mesh,
                    material_variants: Vec::new(),
                    selected_variant: None,
                    shadow: Default::default(),
                    lod: Default::default(),
                },
                None => {
                    tracing::warn!("mujoco import: geom {geom_id} has no usable mesh; skipping");
                    continue;
                }
            },
            // Primitive geoms (plane/sphere/capsule/box/…) are the next
            // increment. They still get a node — with its geom id — so the
            // binding is complete and only the *rendering* is missing.
            other => {
                if !skipped_kinds.contains(&other) {
                    skipped_kinds.push(other);
                }
                NodeKind::Group
            }
        };

        let node = Node::new_with_transform_and_kind(name, trs, kind);
        node.mujoco.set(Some(MujocoComponent::Geom(MujocoGeom {
            geom_id: geom_id as u32,
            group: geom.group,
            kind: geom.kind,
            body: geom.body as u32,
        })));
        // Sim-bound: the stream owns this transform, so the gizmo is off.
        node.locked.set(true);
        root.children.lock_mut().push_cloned(node);
    }

    if !skipped_kinds.is_empty() {
        tracing::warn!(
            "mujoco import: {model_name} has geom kinds not yet rendered ({skipped_kinds:?}); \
             they were imported as empty nodes so the geom-id binding stays complete"
        );
    }
    root
}

/// Register one captured-geometry mesh asset. Mirrors the glTF importer's
/// `mint_imported_mesh`, minus the source-file back-reference: a MuJoCo mesh's
/// editable source is the compiled model, which we deliberately do not archive.
fn mint_mesh(label: &str, mesh: &awsm_renderer_glb_export::MeshData) -> MeshRef {
    use awsm_renderer_editor_protocol::{MeshBase, ModifierStack};

    let mesh_id = AssetId::new();
    crate::engine::bridge::mesh_cache::store_with_id(
        mesh_id,
        crate::engine::bridge::mesh_cache::from_mesh_data(mesh.clone()),
    );
    crate::controller::controller()
        .scene
        .assets
        .lock()
        .unwrap()
        .entries
        .insert(
            mesh_id,
            AssetEntry::new(SceneAssetSource::Mesh(MeshDef {
                label: label.to_string(),
                source: Some(CapturedSource::Editable),
                editable: true,
                stack: ModifierStack {
                    base: MeshBase::Captured(MeshRef(mesh_id)),
                    modifiers: vec![],
                },
                overrides: Default::default(),
            })),
        );
    MeshRef(mesh_id)
}
