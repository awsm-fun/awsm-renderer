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

use awsm_renderer_editor_protocol::mujoco::segment_transform;
use awsm_renderer_editor_protocol::mujoco::{GeomKind, MujocoMaterial, Sidecar};
use awsm_renderer_editor_protocol::{
    AssetEntry, AssetId, AssetSource as SceneAssetSource, CapturedSource, MaterialDef,
    MaterialVariant, MeshDef, MeshRef, MujocoBody, MujocoComponent, MujocoFlex, MujocoGeom,
    MujocoInstance, MujocoSite, MujocoTendonSegment, NodeKind, SkinJoint, SkinnedMeshRef, Trs,
    VariantId,
};

use crate::engine::scene::node::Node;
use crate::prelude::*;

/// Fetch + validate a sidecar and its GLB, and build the instance subtree.
///
/// Returns the root node; the caller inserts it and owns the undo entry.
pub async fn import(sidecar_url: &str) -> Result<Arc<Node>, String> {
    let (doc, meshes, rig) = fetch(sidecar_url).await?;
    Ok(build_subtree(&doc, &meshes, &rig))
}

/// Re-import into an existing instance, in place.
///
/// The point of a re-import is that the *model* changed — recompiled MJCF, new
/// geometry, moved geoms — while the user's work around it did not. So this
/// refreshes everything that comes from the model (poses, geometry, the
/// fingerprint, the geom table) and preserves everything the user owns:
///
/// - the instance root's **id, name and transform** (its authored placement);
/// - each surviving geom's **material palette**, so re-importing never undoes an
///   art pass. A geom that is new in this version gets the sidecar's material.
///
/// Geoms are matched by `geom_id`, which is stable for a given compiled model.
/// Ids that vanished have their nodes removed; ids that appeared get new ones.
pub async fn reimport(sidecar_url: &str, target: &Arc<Node>) -> Result<(), String> {
    let Some(MujocoComponent::Instance(_)) = target.mujoco.get_cloned() else {
        return Err("that node is not a MuJoCo sim instance".to_string());
    };
    let (doc, meshes, rig) = fetch(sidecar_url).await?;

    // Build the fresh subtree, then transplant it onto the existing root rather
    // than diffing two live trees — the build path is the one that is already
    // exercised, and a second "update" path would be free to drift from it.
    let fresh = build_subtree(&doc, &meshes, &rig);

    // What the user owns on each surviving geom, keyed by geom id.
    let mut kept: HashMap<u32, awsm_renderer_editor_protocol::NodeKind> = HashMap::new();
    for child in target.children.lock_ref().iter() {
        if let Some(MujocoComponent::Geom(g)) = child.mujoco.get_cloned() {
            kept.insert(g.geom_id, child.kind.get_cloned());
        }
    }
    for child in fresh.children.lock_ref().iter() {
        let Some(MujocoComponent::Geom(g)) = child.mujoco.get_cloned() else {
            continue;
        };
        let Some(old) = kept.get(&g.geom_id) else {
            continue;
        };
        // Carry the OLD palette onto the NEW geometry: the mesh, shadow and LOD
        // settings are the model's, the materials are the user's.
        let (variants, selected) = match old {
            awsm_renderer_editor_protocol::NodeKind::Mesh {
                material_variants,
                selected_variant,
                ..
            } => (material_variants.clone(), *selected_variant),
            _ => continue,
        };
        if variants.is_empty() {
            continue;
        }
        if let awsm_renderer_editor_protocol::NodeKind::Mesh {
            mesh, shadow, lod, ..
        } = child.kind.get_cloned()
        {
            child
                .kind
                .set(awsm_renderer_editor_protocol::NodeKind::Mesh {
                    mesh,
                    material_variants: variants,
                    selected_variant: selected,
                    shadow,
                    lod,
                });
        }
    }

    // The root keeps its identity and placement; only its component is refreshed.
    target.mujoco.set(fresh.mujoco.get_cloned());
    let new_children: Vec<_> = fresh.children.lock_ref().to_vec();
    target.children.lock_mut().replace_cloned(new_children);
    Ok(())
}

/// What a flex needs to become a real skinned node: the GLB registered as an
/// asset (so the materialiser can decode the rig), plus each GLB node's identity
/// in both index spaces the skin machinery uses.
#[derive(Default)]
pub struct FlexRig {
    /// The GLB's asset id — `SkinnedMeshRef::source`.
    pub asset_id: Option<AssetId>,
    /// GLB node name → (`gltf_node_index`, flat index in the clean rig glb,
    /// local transform). The flat index is what `SkinnedMeshRef::rig_node_index`
    /// and `SkinJoint::index` both address.
    pub nodes: HashMap<String, (u32, u32, awsm_renderer::transforms::Transform)>,
}

async fn fetch(
    sidecar_url: &str,
) -> Result<
    (
        Sidecar,
        HashMap<usize, awsm_renderer_glb_export::MeshData>,
        FlexRig,
    ),
    String,
> {
    let doc = fetch_sidecar(sidecar_url).await?;

    // Geometry, if the model has any. An all-primitive model (the DeepMind
    // humanoid) legitimately has no GLB at all.
    let (meshes, rig) = match &doc.glb {
        Some(rel) => {
            let glb_url = resolve(sidecar_url, rel)?;
            fetch_meshes(&glb_url, &doc).await?
        }
        None => (HashMap::new(), FlexRig::default()),
    };
    Ok((doc, meshes, rig))
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
) -> Result<(HashMap<usize, awsm_renderer_glb_export::MeshData>, FlexRig), String> {
    let import = crate::engine::bridge::gltf::import(glb_url).await?;

    // A flex ships as a skinned mesh, and a skinned node is defined by asset
    // identity plus indices into the re-exported CLEAN RIG GLB — not by a
    // standalone skin object. So when the model has any deformable, register the
    // GLB the same way a model import does, and record both index spaces.
    let mut rig = FlexRig::default();
    if !doc.flexes.is_empty() {
        let asset_id = AssetId::new();
        crate::controller::controller()
            .scene
            .assets
            .lock()
            .unwrap()
            .entries
            .insert(
                asset_id,
                AssetEntry::new(SceneAssetSource::Filename(import.display_name.clone())),
            );
        crate::engine::bridge::bridge()
            .insert_template(asset_id, std::sync::Arc::new(import.template.clone()));
        if let Some(glb) = import.skinned_glb.clone() {
            crate::engine::bridge::skinned_bake_cache::store_rig_glb(asset_id, glb);
        }
        rig.asset_id = Some(asset_id);
        collect_rig_nodes(&import.template.roots, &import.node_flat_indices, &mut rig);
    }

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
    Ok((out, rig))
}

/// Index every GLB node by NAME, in both spaces the skin machinery uses.
///
/// Name is the key because it is the only identity the sidecar and the GLB
/// share — the same reason the mesh lookup uses it.
fn collect_rig_nodes(
    nodes: &[crate::engine::bridge::asset_template::AssetTemplateNode],
    flat: &HashMap<u32, u32>,
    rig: &mut FlexRig,
) {
    for n in nodes {
        if let Some(label) = n.label.as_deref() {
            if let Some(&index) = flat.get(&n.gltf_node_index) {
                rig.nodes.insert(
                    label.to_string(),
                    (n.gltf_node_index, index, n.local.clone()),
                );
            }
        }
        collect_rig_nodes(&n.children, flat, rig);
    }
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
    rig: &FlexRig,
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
            site_count: doc.sites.len() as u32,
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

    // One library material per sidecar material, minted up front so every geom
    // that shares a MuJoCo material shares ours (editing "metal" once repaints
    // every metal part, which is the whole point of bringing them into the
    // library rather than inlining per node).
    let mat_assets: Vec<AssetId> = doc
        .materials
        .iter()
        .enumerate()
        .map(|(i, m)| {
            mint_material(
                m.name.clone().unwrap_or_else(|| format!("material {i}")),
                pbr_from_mujoco(m),
            )
        })
        .collect();
    // Geoms with no material fall back to their own `rgba` (Go2's collision
    // geoms, and plenty of hand-written MJCF). Deduped by colour so a model that
    // paints twenty geoms the same red gets one material, not twenty.
    let mut rgba_materials: HashMap<[u32; 4], AssetId> = HashMap::new();
    // Primitive geoms sharing identical shape params share one mesh asset — the
    // humanoid is 19 capsules drawn from a handful of distinct sizes.
    let mut primitive_meshes: HashMap<(GeomKind, [u32; 3]), MeshRef> = HashMap::new();

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
        let mut trs = Trs {
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

        // The geom's material: the sidecar's, or one minted from its own `rgba`.
        // MuJoCo always has an answer here, so a geom should never come out
        // magenta (our deliberately-unassigned sentinel).
        let material = match geom.material.and_then(|m| mat_assets.get(m)).copied() {
            Some(id) => id,
            None => {
                let key = geom.rgba.map(|c| c.to_bits());
                *rgba_materials.entry(key).or_insert_with(|| {
                    mint_material(
                        format!(
                            "rgba {:.2} {:.2} {:.2}",
                            geom.rgba[0], geom.rgba[1], geom.rgba[2]
                        ),
                        MaterialDef {
                            base_color: geom.rgba,
                            alpha_mode: alpha_mode(geom.rgba[3]),
                            ..MaterialDef::default()
                        },
                    )
                })
            }
        };
        let (material_variants, selected_variant) = palette(material);

        let kind = match geom.kind {
            // Heightfields were baked to a mesh at export (the grid is static
            // after compile), so they take the mesh path — nothing in the editor
            // or the scene format knows what a heightfield is.
            GeomKind::Mesh | GeomKind::Hfield => {
                match geom.mesh.and_then(|m| mesh_assets.get(&m)).cloned() {
                    Some(mesh) => NodeKind::Mesh {
                        mesh,
                        material_variants,
                        selected_variant,
                        shadow: Default::default(),
                        lod: Default::default(),
                    },
                    None => {
                        tracing::warn!(
                            "mujoco import: geom {geom_id} has no usable mesh; skipping"
                        );
                        continue;
                    }
                }
            }
            // Primitive geoms: box/sphere/capsule/cylinder/ellipsoid/plane.
            other => match crate::controller::mujoco_primitive::build(other, geom.size) {
                Some(prim) => {
                    let key = (other, geom.size.map(|v| (v as f32).to_bits()));
                    let mesh = *primitive_meshes
                        .entry(key)
                        .or_insert_with(|| mint_mesh(&prim.label, &prim.mesh));
                    trs.scale = prim.scale;
                    NodeKind::Mesh {
                        mesh,
                        material_variants,
                        selected_variant,
                        shadow: Default::default(),
                        lod: Default::default(),
                    }
                }
                // Heightfields and SDF geoms have no primitive form. They still
                // get a node, with its geom id, so the binding stays complete and
                // only the rendering is missing.
                None => {
                    if !skipped_kinds.contains(&other) {
                        skipped_kinds.push(other);
                    }
                    NodeKind::Group
                }
            },
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

    // Sites: massless marker frames, same treatment as primitive geoms but on
    // their own id space. Their group is MuJoCo's `sitegroup`, which shares the
    // 0-2 default.
    for (site_id, site) in doc.sites.iter().enumerate() {
        if !visible.contains(&site.group) {
            continue;
        }
        let Some(prim) = crate::controller::mujoco_primitive::build(site.kind, site.size) else {
            continue;
        };
        let material = match site.material.and_then(|m| mat_assets.get(m)).copied() {
            Some(id) => id,
            None => {
                let key = site.rgba.map(|c| c.to_bits());
                *rgba_materials.entry(key).or_insert_with(|| {
                    mint_material(
                        format!(
                            "rgba {:.2} {:.2} {:.2}",
                            site.rgba[0], site.rgba[1], site.rgba[2]
                        ),
                        MaterialDef {
                            base_color: site.rgba,
                            alpha_mode: alpha_mode(site.rgba[3]),
                            ..MaterialDef::default()
                        },
                    )
                })
            }
        };
        let (material_variants, selected_variant) = palette(material);
        let key = (site.kind, site.size.map(|v| (v as f32).to_bits()));
        let mesh = *primitive_meshes
            .entry(key)
            .or_insert_with(|| mint_mesh(&prim.label, &prim.mesh));
        let node = Node::new_with_transform_and_kind(
            site.name
                .clone()
                .unwrap_or_else(|| format!("site {site_id}")),
            Trs {
                translation: [
                    site.world_pos[0] as f32,
                    site.world_pos[1] as f32,
                    site.world_pos[2] as f32,
                ],
                rotation: [
                    site.world_quat[1] as f32,
                    site.world_quat[2] as f32,
                    site.world_quat[3] as f32,
                    site.world_quat[0] as f32,
                ],
                scale: prim.scale,
            },
            NodeKind::Mesh {
                mesh,
                material_variants,
                selected_variant,
                shadow: Default::default(),
                lod: Default::default(),
            },
        );
        node.mujoco.set(Some(MujocoComponent::Site(MujocoSite {
            site_id: site_id as u32,
            group: site.group,
            kind: site.kind,
            body: site.body as u32,
        })));
        node.locked.set(true);
        root.children.lock_mut().push_cloned(node);
    }

    // Flexes: one surface mesh node each, at the initial-pose shape. Its
    // vertices are already world-space, so the node's transform is identity —
    // a deformable has no rigid frame to place it by.
    let mut flex_vertex_counts = vec![0u32; doc.flexes.len()];
    for (flex_id, flex) in doc.flexes.iter().enumerate() {
        flex_vertex_counts[flex_id] = flex.vertex_count as u32;
        if !visible.contains(&flex.group) {
            continue;
        }
        let Some(&mesh) = mesh_assets.get(&flex.mesh) else {
            // dim == 1 (a rope) bakes no surface, so there is nothing to draw.
            continue;
        };
        let material = match flex.material.and_then(|m| mat_assets.get(m)).copied() {
            Some(id) => id,
            None => {
                let key = flex.rgba.map(|c| c.to_bits());
                *rgba_materials.entry(key).or_insert_with(|| {
                    mint_material(
                        format!(
                            "rgba {:.2} {:.2} {:.2}",
                            flex.rgba[0], flex.rgba[1], flex.rgba[2]
                        ),
                        MaterialDef {
                            base_color: flex.rgba,
                            alpha_mode: alpha_mode(flex.rgba[3]),
                            // A cloth is a zero-thickness surface: MuJoCo draws
                            // it from both sides and so must we, or half the
                            // flag disappears the moment it flips over.
                            double_sided: true,
                            ..MaterialDef::default()
                        },
                    )
                })
            }
        };
        let (material_variants, selected_variant) = palette(material);
        let label = flex
            .name
            .clone()
            .unwrap_or_else(|| format!("flex {flex_id}"));
        let glb_node = doc.meshes[flex.mesh].node.clone().unwrap_or_default();

        // A flex deforms by linear blend skinning EXACTLY (measured to the
        // nanometre against MuJoCo), so it imports as a skinned mesh: the bodies
        // that move it become joint nodes, the body channel drives those, and
        // the GPU does the deformation with correctly skinned normals. No vertex
        // ever crosses the wire.
        let rig_entry = rig
            .asset_id
            .zip(rig.nodes.get(&glb_node))
            .filter(|_| !flex.joint_bodies.is_empty() && flex.vertex_count > 0);

        let mut joints = Vec::new();
        let mut joint_nodes = Vec::new();
        if rig_entry.is_some() {
            for (j, body) in flex.joint_bodies.iter().enumerate() {
                let Some(&(_, joint_flat, ref local)) =
                    rig.nodes.get(&format!("{glb_node}_joint_{j}"))
                else {
                    continue;
                };
                let jnode = Node::new_with_transform_and_kind(
                    format!("{label} joint {j}"),
                    Trs {
                        translation: local.translation.to_array(),
                        rotation: local.rotation.to_array(),
                        scale: local.scale.to_array(),
                    },
                    NodeKind::Group,
                );
                jnode.mujoco.set(Some(MujocoComponent::Body(MujocoBody {
                    body_id: *body as u32,
                })));
                jnode.locked.set(true);
                joints.push(SkinJoint {
                    node: jnode.id,
                    index: joint_flat,
                });
                joint_nodes.push(jnode);
            }
        }
        // All or nothing: a partially-bound skin deforms toward whichever bodies
        // happened to resolve, which looks like a physics bug rather than an
        // import one.
        let skinned = rig_entry.filter(|_| joints.len() == flex.joint_bodies.len());
        // Fires for a SHORT bind and for a zero bind alike. Gating on "some
        // joints resolved" would stay silent in the loudest case of all — a GLB
        // whose joint nodes are named differently than this importer expects,
        // where nothing binds and the flex quietly ships un-deformable.
        if skinned.is_none() && rig_entry.is_some() {
            tracing::warn!(
                "mujoco import: flex {flex_id} bound {}/{} skin joints — importing it \
                 at its bind pose instead",
                joints.len(),
                flex.joint_bodies.len()
            );
            joint_nodes.clear();
        }
        // Joints must exist BEFORE the skinned mesh that references them: the
        // materialiser needs every bone live in the bridge to build the skin.
        for jnode in joint_nodes {
            root.children.lock_mut().push_cloned(jnode);
        }

        let node = match skinned {
            Some((asset_id, &(gltf_node_index, rig_node_index, _))) => {
                Node::new_with_transform_and_kind(
                    label,
                    Trs::default(),
                    NodeKind::SkinnedMesh {
                        skin: SkinnedMeshRef {
                            source: asset_id,
                            node_index: gltf_node_index,
                            rig_node_index,
                            primitive_index: None,
                            joints,
                        },
                        material_variants,
                        selected_variant,
                        shadow: Default::default(),
                        lod: Default::default(),
                    },
                )
            }
            None => Node::new_with_transform_and_kind(
                label,
                Trs::default(),
                NodeKind::Mesh {
                    mesh,
                    material_variants,
                    selected_variant,
                    shadow: Default::default(),
                    lod: Default::default(),
                },
            ),
        };
        node.mujoco.set(Some(MujocoComponent::Flex(MujocoFlex {
            flex_id: flex_id as u32,
            group: flex.group,
            vertex_count: flex.vertex_count as u32,
            body_attached: !flex.joint_bodies.is_empty(),
        })));
        node.locked.set(true);
        root.children.lock_mut().push_cloned(node);
    }

    // Tendons: each is a chain of `max_waypoints - 1` cylinder segments, ALL of
    // them minted now. The waypoint count changes as the tendon wraps around
    // geometry mid-run, and a pose stream cannot create nodes — so the pool is
    // sized to the model's ceiling and the unused tail simply starts hidden.
    let mut tendon_capacity = vec![0u32; doc.tendons.len()];
    if !doc.tendons.is_empty() {
        let segment_mesh = mint_mesh(
            "tendon segment",
            &crate::controller::mujoco_primitive::unit_cylinder_z(),
        );
        for (tendon_id, tendon) in doc.tendons.iter().enumerate() {
            if !visible.contains(&tendon.group) {
                continue;
            }
            let segments = tendon.max_waypoints.saturating_sub(1);
            if segments == 0 {
                continue;
            }
            tendon_capacity[tendon_id] = tendon.max_waypoints;
            let material = match tendon.material.and_then(|m| mat_assets.get(m)).copied() {
                Some(id) => id,
                None => {
                    let key = tendon.rgba.map(|c| c.to_bits());
                    *rgba_materials.entry(key).or_insert_with(|| {
                        mint_material(
                            format!(
                                "rgba {:.2} {:.2} {:.2}",
                                tendon.rgba[0], tendon.rgba[1], tendon.rgba[2]
                            ),
                            MaterialDef {
                                base_color: tendon.rgba,
                                alpha_mode: alpha_mode(tendon.rgba[3]),
                                ..MaterialDef::default()
                            },
                        )
                    })
                }
            };
            let label = tendon
                .name
                .clone()
                .unwrap_or_else(|| format!("tendon {tendon_id}"));
            let width = tendon.width as f32;
            let waypoint = |i: usize| -> Option<[f32; 3]> {
                tendon
                    .world_waypoints
                    .get(i)
                    .map(|p| [p[0] as f32, p[1] as f32, p[2] as f32])
            };
            for segment in 0..segments as usize {
                let span = waypoint(segment).zip(waypoint(segment + 1));
                let (translation, rotation, scale) = match span {
                    Some((a, b)) => segment_transform(a, b, width),
                    // Beyond the initial pose's waypoints: a spare, parked at
                    // the origin with no length until the stream needs it.
                    None => ([0.0; 3], [0.0, 0.0, 0.0, 1.0], [width, width, 0.0]),
                };
                let (material_variants, selected_variant) = palette(material);
                let node = Node::new_with_transform_and_kind(
                    format!("{label} {segment}"),
                    Trs {
                        translation,
                        rotation,
                        scale,
                    },
                    NodeKind::Mesh {
                        mesh: segment_mesh,
                        material_variants,
                        selected_variant,
                        shadow: Default::default(),
                        lod: Default::default(),
                    },
                );
                node.visible.set(span.is_some());
                node.mujoco
                    .set(Some(MujocoComponent::TendonSegment(MujocoTendonSegment {
                        tendon_id: tendon_id as u32,
                        segment: segment as u32,
                        group: tendon.group,
                    })));
                node.locked.set(true);
                root.children.lock_mut().push_cloned(node);
            }
        }
    }
    if let Some(MujocoComponent::Instance(mut instance)) = root.mujoco.get_cloned() {
        instance.tendon_capacity = tendon_capacity;
        instance.flex_vertex_counts = flex_vertex_counts;
        instance.body_count = doc.bodies.len() as u32;
        root.mujoco.set(Some(MujocoComponent::Instance(instance)));
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

/// MuJoCo materials are Phong-ish; ours are metallic-roughness PBR. There is no
/// exact correspondence, so this is a documented approximation, chosen to look
/// right on the menagerie models rather than to be theoretically pure:
///
/// - `rgba` → base colour, and an alpha below 1 turns on blending;
/// - `shininess` is MuJoCo's normalized gloss, so `roughness = 1 - shininess`;
/// - `reflectance` is the only mirror-like term MuJoCo has, so it becomes
///   `metallic`. MuJoCo has no metalness concept at all, and most models leave
///   this at 0 — which is the right answer for painted plastic and anodized
///   metal alike under an IBL;
/// - `emission` scales the base colour into `emissive`.
///
/// `specular` is deliberately dropped: in a metallic-roughness model the
/// specular intensity of a dielectric is fixed, and folding MuJoCo's value into
/// metallic would make every menagerie part (which sets `specular = 0.5` by
/// default) read as half-metal.
fn pbr_from_mujoco(m: &MujocoMaterial) -> MaterialDef {
    MaterialDef {
        base_color: m.rgba,
        metallic: m.reflectance.clamp(0.0, 1.0),
        roughness: (1.0_f32 - m.shininess).clamp(0.0, 1.0),
        emissive: [
            m.rgba[0] * m.emission,
            m.rgba[1] * m.emission,
            m.rgba[2] * m.emission,
        ],
        alpha_mode: alpha_mode(m.rgba[3]),
        ..MaterialDef::default()
    }
}

fn alpha_mode(alpha: f32) -> awsm_renderer_editor_protocol::material::MaterialAlphaMode {
    use awsm_renderer_editor_protocol::material::MaterialAlphaMode;
    if alpha < 0.999 {
        MaterialAlphaMode::Blend
    } else {
        MaterialAlphaMode::Opaque
    }
}

/// Add a built-in PBR material to the assignable library, the same way the glTF
/// importer does — so a MuJoCo material behaves like any other: renameable,
/// editable, reusable on non-MuJoCo geometry.
///
/// An existing built-in material with the same name AND the same definition is
/// reused rather than duplicated. That makes re-import idempotent — the same
/// model recompiled would otherwise mint a fresh set of "metal"/"black"/… on
/// every pass and leave the old ones stranded in the library — while a material
/// that merely shares a name but differs in definition still gets its own entry.
/// Matching on the definition (not just the name) also means a user who has
/// edited "metal" keeps their edit: the incoming def no longer matches, so their
/// material is left alone and the model's version arrives beside it.
fn mint_material(name: String, mut def: MaterialDef) -> AssetId {
    use crate::controller::custom_material::CustomMaterial;
    use awsm_renderer_editor_protocol::MaterialShading;

    {
        let ctrl = crate::controller::controller();
        let existing = ctrl.custom_materials.lock_ref();
        for m in existing.iter() {
            if m.name.get_cloned() != name {
                continue;
            }
            if let Some(d) = m.builtin.get_cloned() {
                let mut candidate = def.clone();
                candidate.label = d.label.clone();
                if d == candidate {
                    return m.id;
                }
            }
        }
    }

    let id = AssetId::new();
    def.label = name.clone();
    let mat = CustomMaterial::new_builtin(id, name, MaterialShading::Pbr);
    mat.builtin.set(Some(def));
    crate::controller::controller()
        .custom_materials
        .lock_mut()
        .push_cloned(mat);
    id
}

/// A one-entry material palette assigning `id` to a node, seeded exactly as the
/// glTF importer seeds one (a clone of the library defaults as the per-mesh
/// `inline`), so `builtin_merged` renders it identically.
fn palette(id: AssetId) -> (Vec<MaterialVariant>, Option<VariantId>) {
    use awsm_renderer_editor_protocol::dynamic_material::MaterialInstance;

    let ctrl = crate::controller::controller();
    let found = crate::controller::custom_material::find_material(&ctrl.custom_materials, id);
    let name = found
        .as_ref()
        .map(|m| m.name.get_cloned())
        .unwrap_or_else(|| "Material".to_string());
    let inline = found
        .and_then(|m| m.builtin.get_cloned())
        .unwrap_or_default();
    let v = MaterialVariant {
        id: VariantId::new(),
        name,
        instance: MaterialInstance {
            asset: id,
            inline,
            ..Default::default()
        },
    };
    let vid = v.id;
    (vec![v], Some(vid))
}

/// Fetch a recorded capture and bake it into a clip driving `instance`'s geoms.
///
/// The fingerprint check is the point of failure worth being loud about: a
/// capture of a *different* model would bake into poses that are individually
/// plausible and collectively nonsense, driving the wrong robot. Catching it here
/// costs one string comparison; not catching it costs a debugging session.
pub async fn import_capture(
    capture_url: &str,
    instance: &Arc<Node>,
    reduction: awsm_renderer_editor_protocol::mujoco::bake::Reduction,
) -> Result<awsm_renderer_editor_protocol::StoredAnimation, String> {
    use awsm_renderer_editor_protocol::mujoco::bake;

    let Some(MujocoComponent::Instance(inst)) = instance.mujoco.get_cloned() else {
        return Err("that node is not a MuJoCo sim instance".to_string());
    };

    let resp = gloo_net::http::Request::get(capture_url)
        .send()
        .await
        .map_err(|e| format!("fetch {capture_url}: {e}"))?;
    if !resp.ok() {
        return Err(format!("fetch {capture_url}: HTTP {}", resp.status()));
    }
    let text = resp
        .text()
        .await
        .map_err(|e| format!("read {capture_url}: {e}"))?;
    let capture: awsm_renderer_editor_protocol::mujoco::Capture = serde_json::from_str(&text)
        .map_err(|e| format!("parse {capture_url} as a MuJoCo capture: {e}"))?;
    capture
        .validate()
        .map_err(|e| format!("{capture_url}: {e}"))?;

    if capture.source.sha256 != inst.source.sha256 {
        return Err(format!(
            "this capture is of a different model: it fingerprints {} ({}), \
             but the instance was imported from {} ({}). Re-record against the \
             same model file, or re-import the instance.",
            capture.source.filename,
            &capture.source.sha256[..12.min(capture.source.sha256.len())],
            inst.source.filename,
            &inst.source.sha256[..12.min(inst.source.sha256.len())],
        ));
    }
    if capture.geom_count != inst.geom_count {
        return Err(format!(
            "capture has {} geoms, the instance has {}",
            capture.geom_count, inst.geom_count
        ));
    }

    // The binding is derived from the live tree, not from a stored map.
    let mut binding = std::collections::HashMap::new();
    collect_geoms(instance, &mut binding);
    if binding.is_empty() {
        return Err("that instance has no geom nodes to drive".to_string());
    }

    let name = capture
        .source
        .filename
        .rsplit('/')
        .next()
        .unwrap_or("capture")
        .to_string();
    bake::bake(&capture, &binding, name, reduction).map_err(|e| e.to_string())
}

fn collect_geoms(
    node: &Arc<Node>,
    out: &mut std::collections::HashMap<u32, awsm_renderer_editor_protocol::NodeId>,
) {
    for child in node.children.lock_ref().iter() {
        if let Some(MujocoComponent::Geom(g)) = child.mujoco.get_cloned() {
            out.insert(g.geom_id, child.id);
        }
        collect_geoms(child, out);
    }
}
