use std::sync::Arc;

use glam::Mat4;

use awsm_renderer::{buffer::helpers::u8_to_f32_vec, AwsmRenderer};

use crate::{
    buffers::accessor::accessor_to_bytes,
    error::{AwsmGltfError, Result},
};

use super::GltfPopulateContext;

/// Per-crate extension trait carrying skin-population methods on
/// `AwsmRenderer`. Internal to this crate.
pub(crate) trait GltfSkinExt {
    fn populate_gltf_node_skin<'a, 'b: 'a, 'c: 'a>(
        &'a mut self,
        ctx: &'c GltfPopulateContext,
        gltf_node: &'b gltf::Node<'b>,
    ) -> Result<()>;
}

impl GltfSkinExt for AwsmRenderer {
    #[allow(clippy::only_used_in_recursion)]
    fn populate_gltf_node_skin<'a, 'b: 'a, 'c: 'a>(
        &'a mut self,
        ctx: &'c GltfPopulateContext,
        gltf_node: &'b gltf::Node<'b>,
    ) -> Result<()> {
        // first mark all the joints from this node, recursively
        // this is important for the mesh population so it knows to ignore the transforms from joints
        // (according to spec, the transforms of mesh-skinning nodes are ignored)
        mark_gltf_node_as_joint(ctx, gltf_node)?;

        if let Some(skin) = gltf_node.skin() {
            // note: skin.skeleton() is not actually used for anything, it's just decorative to help authoring tools

            let mut joints = Vec::with_capacity(skin.joints().len());
            let node_to_transform = &ctx.key_lookups.lock().unwrap().node_index_to_transform;
            for joint_node in skin.joints() {
                let transform_key = *node_to_transform.get(&joint_node.index()).ok_or(
                    AwsmGltfError::SkinJointTransformNotFound(joint_node.index()),
                )?;
                joints.push(transform_key);
            }

            let inverse_bind_matrices = match skin.inverse_bind_matrices() {
                Some(accessor) => {
                    let bytes = accessor_to_bytes(&accessor, &ctx.data.buffers.raw)?;
                    let values = u8_to_f32_vec(&bytes);
                    let matrices = values
                        .chunks(16)
                        .map(Mat4::from_cols_slice)
                        .collect::<Vec<_>>();

                    // from the spec: "The order of joints is defined by the skin.joints array and it
                    // MUST match the order of inverseBindMatrices accessor elements (when the latter is present)"

                    if matrices.len() != skin.joints().len() {
                        return Err(AwsmGltfError::InvalidSkinInverseBindMatrixCount {
                            matrix_count: matrices.len(),
                            joint_count: skin.joints().len(),
                        });
                    }

                    Some(matrices)
                }
                None => None,
            };

            ctx.node_to_skin_transform.lock().unwrap().insert(
                gltf_node.index(),
                Arc::new((joints, inverse_bind_matrices.unwrap_or_default())),
            );
        }

        for child in gltf_node.children() {
            self.populate_gltf_node_skin(ctx, &child)?;
        }

        Ok(())
    }
}

fn mark_gltf_node_as_joint(ctx: &GltfPopulateContext, gltf_node: &gltf::Node) -> Result<()> {
    if let Some(skin) = gltf_node.skin() {
        let mut transform_is_joint = ctx.transform_is_joint.lock().unwrap();
        let node_to_transform = &ctx.key_lookups.lock().unwrap().node_index_to_transform;
        for joint_node in skin.joints() {
            let transform_key = node_to_transform.get(&joint_node.index()).unwrap();
            transform_is_joint.insert(*transform_key);
        }
    }

    for child in gltf_node.children() {
        mark_gltf_node_as_joint(ctx, &child)?;
    }

    Ok(())
}
