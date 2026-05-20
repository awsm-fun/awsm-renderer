//! KHR_lights_punctual: walk the scene and create renderer lights for any
//! glTF nodes that reference a punctual light.

use glam::{Mat4, Vec3, Vec4Swizzles};

use awsm_renderer::{
    lights::{Light, LightKey},
    AwsmRenderer,
};

use super::GltfPopulateContext;
use crate::{data::GltfData, error::Result};

/// Walks every node in the scene and inserts a renderer `Light` for nodes
/// that reference `KHR_lights_punctual`. The world transform is computed
/// directly from the node ancestry — transforms are normally driven by the
/// renderer's transform tree later, but lights are uploaded as a flat array
/// rather than parented to a transform, so we bake the world pose here.
pub(crate) fn populate_gltf_lights(
    renderer: &mut AwsmRenderer,
    ctx: &GltfPopulateContext,
) -> Result<Vec<LightKey>> {
    populate_lights_from_doc(renderer, &ctx.data)
}

/// Like `populate_gltf_lights`, but driven by a `GltfData` directly so it
/// can be re-run after the initial populate (e.g. when the model-tests
/// app re-enables "model only" lighting and needs to re-insert lights
/// that were previously removed).
pub fn populate_lights_from_doc(
    renderer: &mut AwsmRenderer,
    data: &GltfData,
) -> Result<Vec<LightKey>> {
    let mut keys = Vec::new();

    let doc = &data.doc;
    let scene = match doc.default_scene() {
        Some(s) => Some(s),
        None => doc.scenes().next(),
    };
    let Some(scene) = scene else {
        return Ok(keys);
    };

    for node in scene.nodes() {
        walk_node(renderer, &node, Mat4::IDENTITY, &mut keys)?;
    }

    Ok(keys)
}

fn walk_node(
    renderer: &mut AwsmRenderer,
    node: &gltf::Node,
    parent_world: Mat4,
    keys: &mut Vec<LightKey>,
) -> Result<()> {
    let local = match node.transform() {
        gltf::scene::Transform::Matrix { matrix } => Mat4::from_cols_array_2d(&matrix),
        gltf::scene::Transform::Decomposed {
            translation,
            rotation,
            scale,
        } => Mat4::from_scale_rotation_translation(
            Vec3::from_array(scale),
            glam::Quat::from_array(rotation),
            Vec3::from_array(translation),
        ),
    };
    let world = parent_world * local;

    if let Some(gltf_light) = node.light() {
        if let Some(light) = to_renderer_light(&gltf_light, &world) {
            // glTF doesn't carry shadow-cast/receive intent — leave
            // params unregistered; callers can opt in later via
            // `AwsmRenderer::set_light_shadow_params`.
            let key = renderer.insert_light(light, None)?;
            keys.push(key);
        }
    }

    for child in node.children() {
        walk_node(renderer, &child, world, keys)?;
    }

    Ok(())
}

fn to_renderer_light(gltf_light: &gltf::khr_lights_punctual::Light, world: &Mat4) -> Option<Light> {
    // World position is the translation column.
    let position: [f32; 3] = world.w_axis.xyz().to_array();

    // glTF convention: lights point along their local -Z axis. Strip
    // translation, normalize because the world matrix may include scale.
    let local_forward = Vec3::new(0.0, 0.0, -1.0);
    let world_forward = world.transform_vector3(local_forward);
    let direction = if world_forward.length_squared() > 1e-12 {
        world_forward.normalize().to_array()
    } else {
        [0.0, 0.0, -1.0]
    };

    let color = gltf_light.color();
    let intensity = gltf_light.intensity();
    // glTF stores range as `Option<f32>`; 0 in our renderer means "unlimited".
    let range = gltf_light.range().unwrap_or(0.0);

    Some(match gltf_light.kind() {
        gltf::khr_lights_punctual::Kind::Directional => Light::Directional {
            color,
            intensity,
            direction,
        },
        gltf::khr_lights_punctual::Kind::Point => Light::Point {
            color,
            intensity,
            position,
            range,
        },
        gltf::khr_lights_punctual::Kind::Spot {
            inner_cone_angle,
            outer_cone_angle,
        } => Light::Spot {
            color,
            intensity,
            position,
            direction,
            range,
            inner_angle: inner_cone_angle,
            outer_angle: outer_cone_angle,
        },
    })
}
