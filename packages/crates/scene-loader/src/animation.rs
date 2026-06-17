//! Load `scene.animations` (our full-fidelity clips) + `scene.mixer` (the NLA
//! mixer) into the renderer — the player half of the animation round-trip.
//!
//! The pure lowering (`StoredAnimation` → runtime `AnimationClipGroup`,
//! `MixerDoc` → `AnimationMixer`) lives in `awsm_renderer::animation::scene_loader`
//! behind a resolver-closure seam: the *opinionated* part — mapping an abstract
//! [`TrackTarget`] (a node/asset id + property) to a concrete renderer
//! [`AnimationTarget`] (a live `TransformKey` / `MaterialKey` / `LightKey` / …) —
//! is the caller's job. This module is that caller for OUR bundle: it resolves
//! against the key maps [`populate_awsm_scene`](crate::populate_awsm_scene) built
//! while materializing the scene.
//!
//! Mirrors the editor's `animation_sync::resolve_target` (same target→key
//! policy), but resolves against the loader's plain maps instead of the editor's
//! live bridge — the single-source equivalent on the player side.
//!
//! **Driving** the clock is the consumer's job, not the loader's: insert the
//! clips here, then a player advances them each frame with
//! `renderer.update_animations(dt_ms)`, or — as in the in-editor round-trip —
//! the editor render loop pins the pose at the transport playhead. Skinned-mesh
//! Transform tracks resolve to the bone *scene* node's own transform key (which
//! does not yet drive the rig glb's baked joints); driving the skin from our
//! clips is the skin-correspondence follow-on (the rig still poses at bind pose).

use std::collections::HashMap;

use awsm_materials::MaterialShaderId;
use awsm_renderer::animation::scene_loader::{lower_stored_clip, lower_stored_mixer};
use awsm_renderer::animation::{
    AnimationClipGroup, AnimationClipKey, AnimationTarget, BuiltinMaterialParam, CameraParam,
    LightParam, TargetMask,
};
use awsm_renderer::cameras::CameraKey;
use awsm_renderer::decals::DecalKey;
use awsm_renderer::lights::LightKey;
use awsm_renderer::materials::MaterialKey;
use awsm_renderer::meshes::MeshKey;
use awsm_renderer::render_passes::lines::LineKey;
use awsm_renderer::transforms::TransformKey;
use awsm_renderer::AwsmRenderer;
use awsm_scene::animation::{BuiltinParamKind, CameraParamKind, LightParamKind, TrackTarget};
use awsm_scene::{AssetId, CameraConfig, EditorNode, NodeId, Scene};

/// The renderer keys the loader minted per node/asset while materializing the
/// scene, consulted to resolve each animation track's abstract target. Built up
/// across `populate_awsm_scene`'s phases (materials in Phase 1, the rest while
/// uploading meshes in Phase 3).
#[derive(Default)]
pub struct AnimResolveMaps {
    /// Every node's local transform key (one per node, animation T/R/S target).
    pub transforms: HashMap<NodeId, TransformKey>,
    /// Light nodes → their light key.
    pub lights: HashMap<NodeId, LightKey>,
    /// Camera nodes → their camera key (registered in the renderer cameras store).
    pub cameras: HashMap<NodeId, CameraKey>,
    /// `Line` nodes → their inserted [`LineKey`]. Captured here (mirroring
    /// `cameras` / `node_meshes`) so the `NodeHandles` assembly can wire the line
    /// handle back per node. No animation target resolves against lines today;
    /// the map exists purely for the player-grade `NodeHandles.line`.
    pub lines: HashMap<NodeId, LineKey>,
    /// `Decal` nodes → their inserted [`DecalKey`] (present only when the
    /// renderer's `decals` feature is on; otherwise the decal is cleanly skipped
    /// and no entry is recorded). Powers the player-grade `NodeHandles.decal`.
    pub decals: HashMap<NodeId, DecalKey>,
    /// Mesh/skinned nodes → their first renderer mesh key (morph-weight target).
    pub meshes: HashMap<NodeId, MeshKey>,
    /// Mesh/skinned nodes → ALL their renderer mesh keys (a glb node destructures
    /// into one key per primitive). Powers the player loader's `NodeHandles.meshes`
    /// (hide/teardown a whole node); `meshes` above keeps just the first for the
    /// single-target animation path. Empty for non-mesh nodes.
    pub node_meshes: HashMap<NodeId, Vec<MeshKey>>,
    /// Camera nodes → their authored `CameraConfig` (cloned from the scene), so the
    /// player loader can hand the consumer's camera rig the original projection /
    /// behavior alongside the live `CameraKey`.
    pub camera_configs: HashMap<NodeId, CameraConfig>,
    /// Skeleton bone `NodeId` → the rig glb's baked joint `TransformKey` the skin
    /// reads. Built from `SkinnedMeshRef::joints` + the loaded rig glb's
    /// node-index→transform map. A bone's Transform track resolves HERE (driving
    /// the baked joint directly, so the skin deforms) in preference to the bone's
    /// own scene transform key — no per-frame mirror copy needed (the player
    /// equivalent of the editor's skin bridge).
    pub skin_joints: HashMap<NodeId, TransformKey>,
    /// Mesh/skinned nodes → the material key built for them (BuiltinParam target).
    pub node_materials: HashMap<NodeId, MaterialKey>,
    /// Custom-WGSL material asset → the shader id it registered as (Phase 0).
    pub custom_shaders: HashMap<AssetId, MaterialShaderId>,
    /// Custom-WGSL material asset → the first renderer material key built from it
    /// (a Uniform track drives that one — mirrors the editor's
    /// `material_key_for_shader`, which also picks the first match).
    pub custom_materials: HashMap<AssetId, MaterialKey>,
}

/// Lower + insert the scene's clips and mixer into the renderer, returning the
/// inserted clip keys so the host can tear them down on the next load (they
/// outlive any per-node tracking, exactly like the loaded meshes/lights).
///
/// No-op (empty result) when the scene carries no animation data.
pub fn load_animations(
    renderer: &mut AwsmRenderer,
    scene: &Scene,
    maps: &AnimResolveMaps,
) -> Vec<AnimationClipKey> {
    if scene.animations.is_empty() && scene.mixer.layers.is_empty() {
        return Vec::new();
    }

    // Lower every clip first (immutable renderer borrow inside the resolver — a
    // Morph/Uniform target reads the live mesh morph key / dynamic-material
    // layout), collecting owned groups; only then take the mutable borrow to
    // insert them. Same ordering the editor's relower uses.
    let groups: Vec<(AssetId, AnimationClipGroup)> = scene
        .animations
        .iter()
        .map(|clip| {
            (
                clip.id,
                lower_stored_clip(clip, |t| resolve_target(renderer, t, maps)),
            )
        })
        .collect();

    let mut clip_keys: Vec<(AssetId, AnimationClipKey)> = Vec::with_capacity(groups.len());
    let mut loaded: Vec<AnimationClipKey> = Vec::with_capacity(groups.len());
    for (id, group) in groups {
        let key = renderer.animations.insert_clip(group);
        clip_keys.push((id, key));
        loaded.push(key);
    }

    // Map the mixer doc's clip ids → freshly inserted keys, and resolve each
    // masked layer's node set → the transform keys it gates.
    renderer.animations.mixer = lower_stored_mixer(
        &scene.mixer,
        |id| clip_keys.iter().find(|(a, _)| *a == id).map(|(_, k)| *k),
        |nodes, include_descendants| {
            let mut mask = TargetMask::default();
            let expanded;
            let set: &[NodeId] = if include_descendants {
                expanded = expand_descendants(scene, nodes);
                &expanded
            } else {
                nodes
            };
            for nid in set {
                if let Some(tk) = maps.transforms.get(nid) {
                    mask.transforms.insert(*tk);
                }
            }
            mask
        },
    );

    loaded
}

/// Resolve one abstract [`TrackTarget`] to a concrete renderer [`AnimationTarget`]
/// against the loader's key maps. `None` = the target doesn't resolve (the node /
/// material / slot isn't present), so the track is dropped (mirrors the editor's
/// "invalid target" path, minus the pending/retry logic — the loader builds every
/// key up front, so a miss here is genuinely absent).
fn resolve_target(
    renderer: &AwsmRenderer,
    target: &TrackTarget,
    maps: &AnimResolveMaps,
) -> Option<AnimationTarget> {
    match target {
        // T/R/S all drive the node's single transform key; the per-field
        // `TransformAnimation` (built in lowering) isolates which component writes.
        // A skeleton bone resolves to the rig glb's baked joint key (so the skin
        // deforms); any other node to its own scene transform key.
        TrackTarget::Transform { node, .. } => maps
            .skin_joints
            .get(node)
            .or_else(|| maps.transforms.get(node))
            .copied()
            .map(AnimationTarget::Transform),
        TrackTarget::BuiltinParam { node, param } => {
            maps.node_materials
                .get(node)
                .copied()
                .map(|material| AnimationTarget::BuiltinParam {
                    material,
                    param: builtin_param(*param),
                })
        }
        TrackTarget::Light { node, param } => {
            maps.lights
                .get(node)
                .copied()
                .map(|light| AnimationTarget::Light {
                    light,
                    param: light_param(*param),
                })
        }
        TrackTarget::Camera { node, param } => {
            maps.cameras
                .get(node)
                .copied()
                .map(|camera| AnimationTarget::Camera {
                    camera,
                    param: camera_param(*param),
                })
        }
        TrackTarget::Morph { node, .. } => {
            let mesh = maps.meshes.get(node).copied()?;
            renderer
                .meshes
                .geometry_morph_key_for_mesh(mesh)
                .map(|k| AnimationTarget::Morph(k.into()))
        }
        TrackTarget::Uniform { material, name } => {
            let shader_id = maps.custom_shaders.get(material).copied()?;
            let slot = renderer
                .dynamic_material_registration(shader_id)?
                .layout
                .uniforms
                .iter()
                .position(|u| u.name == *name)?;
            let material = maps.custom_materials.get(material).copied()?;
            Some(AnimationTarget::Uniform { material, slot })
        }
    }
}

/// Expand a set of root nodes to include all their descendants (an
/// include-descendants bone mask). Mirrors the editor's `expand_descendants`,
/// walking the loaded scene tree instead of the live editor scene.
fn expand_descendants(scene: &Scene, roots: &[NodeId]) -> Vec<NodeId> {
    fn find(nodes: &[EditorNode], id: NodeId) -> Option<&EditorNode> {
        for n in nodes {
            if n.id == id {
                return Some(n);
            }
            if let Some(found) = find(&n.children, id) {
                return Some(found);
            }
        }
        None
    }
    fn collect(node: &EditorNode, out: &mut Vec<NodeId>) {
        for child in &node.children {
            out.push(child.id);
            collect(child, out);
        }
    }
    let mut out = Vec::new();
    for root in roots {
        out.push(*root);
        if let Some(n) = find(&scene.nodes, *root) {
            collect(n, &mut out);
        }
    }
    out
}

fn builtin_param(p: BuiltinParamKind) -> BuiltinMaterialParam {
    match p {
        BuiltinParamKind::BaseColor => BuiltinMaterialParam::BaseColor,
        BuiltinParamKind::Metallic => BuiltinMaterialParam::Metallic,
        BuiltinParamKind::Roughness => BuiltinMaterialParam::Roughness,
        BuiltinParamKind::Emissive => BuiltinMaterialParam::Emissive,
    }
}

fn light_param(p: LightParamKind) -> LightParam {
    match p {
        LightParamKind::Intensity => LightParam::Intensity,
        LightParamKind::Color => LightParam::Color,
        LightParamKind::Range => LightParam::Range,
        LightParamKind::InnerAngle => LightParam::InnerAngle,
        LightParamKind::OuterAngle => LightParam::OuterAngle,
    }
}

fn camera_param(p: CameraParamKind) -> CameraParam {
    match p {
        CameraParamKind::FovY => CameraParam::FovY,
        CameraParamKind::Near => CameraParam::Near,
        CameraParamKind::Far => CameraParam::Far,
        CameraParamKind::Aperture => CameraParam::Aperture,
        CameraParamKind::FocusDistance => CameraParam::FocusDistance,
    }
}

#[cfg(test)]
mod tests {
    use super::expand_descendants;
    use awsm_scene::{EditorNode, NodeId, NodeKind, Scene};

    fn node(id: NodeId, children: Vec<EditorNode>) -> EditorNode {
        EditorNode {
            id,
            name: String::new(),
            transform: Default::default(),
            kind: NodeKind::Group,
            locked: false,
            visible: true,
            prefab: false,
            children,
        }
    }

    fn scene_with(nodes: Vec<EditorNode>) -> Scene {
        Scene {
            nodes,
            ..Default::default()
        }
    }

    // expand_descendants is the include-descendants bone mask: a root expands to
    // ITSELF plus every descendant (depth-first), so a clip targeting a skeleton
    // root drives the whole limb, not just the root joint.

    #[test]
    fn expands_root_and_all_descendants_depth_first() {
        // root ── child1 ── grandchild
        //      └─ child2
        let (root, child1, grandchild, child2) =
            (NodeId::new(), NodeId::new(), NodeId::new(), NodeId::new());
        let scene = scene_with(vec![node(
            root,
            vec![
                node(child1, vec![node(grandchild, vec![])]),
                node(child2, vec![]),
            ],
        )]);
        assert_eq!(
            expand_descendants(&scene, &[root]),
            vec![root, child1, grandchild, child2],
            "root first, then depth-first descendants"
        );
    }

    #[test]
    fn unknown_root_yields_only_itself() {
        // A root id absent from the scene is still emitted (the caller's mask
        // entry), just with no descendants to add.
        let ghost = NodeId::new();
        let scene = scene_with(vec![node(NodeId::new(), vec![])]);
        assert_eq!(expand_descendants(&scene, &[ghost]), vec![ghost]);
    }

    #[test]
    fn mid_tree_root_expands_only_its_subtree() {
        // Selecting a non-top-level node expands that node's subtree (find is
        // recursive), not the whole scene.
        let (root, child1, grandchild, child2) =
            (NodeId::new(), NodeId::new(), NodeId::new(), NodeId::new());
        let scene = scene_with(vec![node(
            root,
            vec![
                node(child1, vec![node(grandchild, vec![])]),
                node(child2, vec![]),
            ],
        )]);
        assert_eq!(
            expand_descendants(&scene, &[child1]),
            vec![child1, grandchild],
            "child1's subtree only"
        );
    }

    #[test]
    fn multiple_roots_expand_in_order() {
        let (a, a_kid, b) = (NodeId::new(), NodeId::new(), NodeId::new());
        let scene = scene_with(vec![node(a, vec![node(a_kid, vec![])]), node(b, vec![])]);
        assert_eq!(expand_descendants(&scene, &[a, b]), vec![a, a_kid, b]);
    }
}
