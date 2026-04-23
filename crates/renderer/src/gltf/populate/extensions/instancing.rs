use crate::gltf::buffers::accessor::{accessor_to_vec, AccessorVec};
use crate::gltf::error::{AwsmGltfError, Result};
use crate::transforms::Transform;
use crate::{gltf::populate::GltfPopulateContext, AwsmRenderer};
use anyhow::anyhow;
use glam::{Quat, Vec3};

impl AwsmRenderer {
    pub(crate) fn populate_gltf_node_extension_instancing<'a, 'b: 'a, 'c: 'a>(
        &'a mut self,
        ctx: &'c GltfPopulateContext,
        gltf_node: &'b gltf::Node<'b>,
    ) -> Result<()> {
        if let Some(ext) = gltf_node
            .extension_value("EXT_mesh_gpu_instancing")
            .and_then(|ext| ext.get("attributes"))
        {
            let translations =
                if let Some(index) = ext.get("translation").or(ext.get("TRANSLATION")) {
                    let index = index.as_u64().ok_or(AwsmGltfError::ExtInstancing(anyhow!(
                        "translation isn't a number"
                    )))? as usize;
                    let accessor =
                        ctx.data
                            .doc
                            .accessors()
                            .nth(index)
                            .ok_or(AwsmGltfError::ExtInstancing(anyhow!(
                                "no accesor for translation at {index}"
                            )))?;
                    match accessor_to_vec(&accessor, &ctx.data.buffers.raw)? {
                        AccessorVec::Vec3F32(values) => Some(values),
                        _ => {
                            return Err(AwsmGltfError::ExtInstancing(anyhow!(
                                "translation isn't a Vec3F32"
                            )));
                        }
                    }
                } else {
                    None
                };

            let rotations = if let Some(index) = ext.get("rotation").or(ext.get("ROTATION")) {
                let index = index.as_u64().ok_or(AwsmGltfError::ExtInstancing(anyhow!(
                    "rotationisn't a number"
                )))? as usize;
                let accessor =
                    ctx.data
                        .doc
                        .accessors()
                        .nth(index)
                        .ok_or(AwsmGltfError::ExtInstancing(anyhow!(
                            "no accesor for rotationat {index}"
                        )))?;
                let rotations = match accessor_to_vec(&accessor, &ctx.data.buffers.raw)? {
                    AccessorVec::Vec4F32(values) => values,
                    AccessorVec::Vec4U8(values) => values
                        .iter()
                        .map(|v| [v[0] as f32, v[1] as f32, v[2] as f32, v[3] as f32])
                        .collect::<Vec<_>>(),
                    AccessorVec::Vec4U16(values) => values
                        .iter()
                        .map(|v| [v[0] as f32, v[1] as f32, v[2] as f32, v[3] as f32])
                        .collect::<Vec<_>>(),
                    AccessorVec::Vec4U32(values) => values
                        .iter()
                        .map(|v| [v[0] as f32, v[1] as f32, v[2] as f32, v[3] as f32])
                        .collect::<Vec<_>>(),
                    AccessorVec::Vec4I8(values) => values
                        .iter()
                        .map(|v| [v[0] as f32, v[1] as f32, v[2] as f32, v[3] as f32])
                        .collect::<Vec<_>>(),
                    AccessorVec::Vec4I16(values) => values
                        .iter()
                        .map(|v| [v[0] as f32, v[1] as f32, v[2] as f32, v[3] as f32])
                        .collect::<Vec<_>>(),
                    _ => {
                        return Err(AwsmGltfError::ExtInstancing(anyhow!(
                            "translation isn't a Vec3F32"
                        )));
                    }
                };

                Some(rotations)
            } else {
                None
            };

            let scales = if let Some(index) = ext.get("scale").or(ext.get("SCALE")) {
                let index = index.as_u64().ok_or(AwsmGltfError::ExtInstancing(anyhow!(
                    "scale isn't a number"
                )))? as usize;
                let accessor =
                    ctx.data
                        .doc
                        .accessors()
                        .nth(index)
                        .ok_or(AwsmGltfError::ExtInstancing(anyhow!(
                            "no accesor for scale at {index}"
                        )))?;
                match accessor_to_vec(&accessor, &ctx.data.buffers.raw)? {
                    AccessorVec::Vec3F32(values) => Some(values),
                    _ => {
                        return Err(AwsmGltfError::ExtInstancing(anyhow!(
                            "scale isn't a Vec3F32"
                        )));
                    }
                }
            } else {
                None
            };

            let count = match (&translations, &rotations, &scales) {
                (Some(t), _, _) => t.len(),
                (_, Some(r), _) => r.len(),
                (_, _, Some(s)) => s.len(),
                _ => 0,
            };

            let mut transforms = Vec::with_capacity(count);

            for i in 0..count {
                let mut transform = Transform::IDENTITY;

                if let Some(translation) = translations.as_ref().map(|t| t[i]) {
                    transform = transform.with_translation(Vec3::from_array(translation));
                }

                if let Some(rotation) = rotations.as_ref().map(|r| r[i]) {
                    transform = transform.with_rotation(Quat::from_array(rotation));
                }

                if let Some(scale) = scales.as_ref().map(|s| s[i]) {
                    transform = transform.with_scale(Vec3::from_array(scale));
                }

                transforms.push(transform);
            }

            if count > 0 {
                let key = *ctx
                    .key_lookups
                    .lock()
                    .unwrap()
                    .node_index_to_transform
                    .get(&gltf_node.index())
                    .ok_or(AwsmGltfError::ExtInstancing(anyhow!(
                        "no transform key for node {}",
                        gltf_node.index()
                    )))?;

                self.instances
                    .transform_insert(key, &transforms)
                    .map_err(|e| {
                        AwsmGltfError::ExtInstancing(anyhow::anyhow!(
                            "instance transform buffer overflow: {e}"
                        ))
                    })?;
                ctx.transform_is_instanced.lock().unwrap().insert(key);
            }
        }

        for child in gltf_node.children() {
            self.populate_gltf_node_extension_instancing(ctx, &child)?;
        }

        Ok(())
    }
}
