//! Insert Model / Empty / Light / Collision.

use crate::prelude::*;
use crate::renderer_bridge::asset_cache::{AssetTemplate, AssetTemplateNode};
use crate::scene::{mutate::insert_under, AssetId, ModelRef, Node, NodeId, NodeKind, Trs};
use crate::state::{app_state, project::asset_disk_path};
use js_sys::Uint8Array;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::File;

pub use crate::scene::LightKind;

pub fn model(file: File) {
    open_loading_modal(&file.name());
    spawn_local(async move {
        let result = prepare_model(file).await;
        Modal::close();
        match result {
            Ok(ModelPrep {
                asset_id,
                filename,
                display_name,
                parent_id,
                template,
            }) => {
                insert_model_tree(asset_id, &filename, &display_name, parent_id, &template);
            }
            Err(err) => {
                tracing::error!("Insert Model failed: {err}");
                Modal::error(format!("Insert Model failed: {err}"));
            }
        }
    });
}

fn open_loading_modal(filename: &str) {
    let label = format!("Loading {filename}…");
    Modal::open(move || {
        html!("div", {
            .style("display", "flex")
            .style("flex-direction", "column")
            .style("align-items", "center")
            .style("gap", "1rem")
            .style("padding", "0.5rem 1.5rem")
            .style("color", ColorText::SidebarHeader.value())
            .style("min-width", "320px")
            .child(html!("h2", {
                .style("margin", "0")
                .style("font-size", "1.1rem")
                .text("Inserting Model")
            }))
            .child(html!("p", {
                .style("margin", "0")
                .style("font-size", "0.95rem")
                .style("line-height", "1.4")
                .text(&label)
            }))
        })
    });
    Modal::lock();
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

    // Resolve (or allocate) an `AssetId` for this filename. Re-importing
    // `robot.glb` into the same project resolves to the same id, so the
    // bridge's per-asset cache continues to dedup at the renderer level.
    let asset_id = state
        .scene
        .assets
        .lock()
        .unwrap()
        .insert_filename(filename.clone());

    // Decide whether we need to keep bytes for this asset.
    let dir = state.project.lock().unwrap().directory.clone();
    let disk_path = asset_disk_path(&filename);
    let already_on_disk = match &dir {
        Some(dir) => dir.file_exists(&disk_path).await,
        None => false,
    };
    let already_pending = state.pending_assets.lock().unwrap().contains_key(&asset_id);

    if !already_on_disk && !already_pending {
        let buffer = JsFuture::from(file.array_buffer())
            .await
            .map_err(|err| anyhow::anyhow!("reading file: {:?}", err))?;
        let buffer: js_sys::ArrayBuffer = buffer
            .dyn_into()
            .map_err(|_| anyhow::anyhow!("file.arrayBuffer() did not return an ArrayBuffer"))?;
        let array = Uint8Array::new(&buffer);
        let mut bytes = vec![0u8; array.length() as usize];
        array.copy_to(&mut bytes);
        state.pending_assets.lock().unwrap().insert(asset_id, bytes);
    }

    // Kick off the asset load + wait for the populated template so we
    // know how many top-level nodes it has. A concurrent insert of the
    // same path will share this load.
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
fn insert_model_tree(
    asset_id: AssetId,
    filename: &str,
    display_name: &str,
    parent_id: Option<NodeId>,
    template: &AssetTemplate,
) {
    if template.roots.is_empty() {
        tracing::warn!("insert::model: template '{filename}' has no top-level nodes");
        Modal::error("This file contains no nodes to insert.");
        return;
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

    let mut first_id: Option<NodeId> = None;
    for node in nodes {
        let id = node.id;
        if !insert_under(&state.scene, parent_id, node) {
            tracing::error!("insert::model: failed to insert");
            Modal::error("Failed to insert model (parent may have been removed).");
            return;
        }
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
            AssetEntry {
                source: AssetSource::Texture(TextureDef::Procedural(
                    ProceduralTextureDef::Checker {
                        width: 256,
                        height: 256,
                        cells_x: 8,
                        cells_y: 8,
                        color_a: [0.1, 0.1, 0.1, 1.0],
                        color_b: [0.9, 0.9, 0.9, 1.0],
                    },
                )),
            },
        );
    }
    state.scene.bump_revision();
    state.commit_history(previous);
    tracing::info!("action: insert::texture_asset({id}) — done");
    awsm_web_shared::prelude::Toast::info("New Texture asset created (Checker)");
    Some(id)
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
            AssetEntry {
                source: AssetSource::Material(MaterialDef::default()),
            },
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
