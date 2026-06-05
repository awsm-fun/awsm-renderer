use glam::{Quat, Vec3};

use awsm_renderer::{
    animation::{
        AnimationClip, AnimationData, AnimationKey, AnimationMorphKey, AnimationPlayer,
        AnimationSampler, TransformAnimation, VertexAnimation,
    },
    buffer::helpers::u8_to_f32_vec,
    meshes::morphs::{GeometryMorphKey, MaterialMorphKey},
    transforms::TransformKey,
    AwsmRenderer,
};

use crate::{
    buffers::accessor::accessor_to_bytes,
    error::{AwsmGltfError, Result},
};

use super::GltfPopulateContext;

/// Per-crate extension trait carrying animation-population methods on
/// `AwsmRenderer`. Internal to this crate.
pub(crate) trait GltfAnimationExt {
    fn populate_gltf_node_animation<'a, 'b: 'a, 'c: 'a>(
        &'a mut self,
        ctx: &'c GltfPopulateContext,
        gltf_node: &'b gltf::Node<'b>,
    ) -> Result<()>;

    fn populate_gltf_animation_morph<'a, 'b: 'a, 'c: 'a>(
        &'a mut self,
        ctx: &'c GltfPopulateContext,
        gltf_sampler: gltf::animation::Sampler<'b>,
        geometry_morph_key: Option<GeometryMorphKey>,
        material_morph_key: Option<MaterialMorphKey>,
    ) -> Result<Vec<AnimationKey>>;

    fn populate_gltf_animation_transform_translation<'a, 'b: 'a, 'c: 'a>(
        &'a mut self,
        ctx: &'c GltfPopulateContext,
        gltf_sampler: gltf::animation::Sampler<'b>,
        transform_key: TransformKey,
    ) -> Result<AnimationKey>;

    fn populate_gltf_animation_transform_rotation<'a, 'b: 'a, 'c: 'a>(
        &'a mut self,
        ctx: &'c GltfPopulateContext,
        gltf_sampler: gltf::animation::Sampler<'b>,
        transform_key: TransformKey,
    ) -> Result<AnimationKey>;

    fn populate_gltf_animation_transform_scale<'a, 'b: 'a, 'c: 'a>(
        &'a mut self,
        ctx: &'c GltfPopulateContext,
        gltf_sampler: gltf::animation::Sampler<'b>,
        transform_key: TransformKey,
    ) -> Result<AnimationKey>;
}

impl GltfAnimationExt for AwsmRenderer {
    fn populate_gltf_node_animation<'a, 'b: 'a, 'c: 'a>(
        &'a mut self,
        ctx: &'c GltfPopulateContext,
        gltf_node: &'b gltf::Node<'b>,
    ) -> Result<()> {
        let transform_key = ctx
            .key_lookups
            .lock()
            .unwrap()
            .node_index_to_transform
            .get(&gltf_node.index())
            .cloned()
            .unwrap();

        if let Some(node_samplers) = ctx.node_animation_samplers.get(&gltf_node.index()) {
            if let Some(sampler_ref) = node_samplers.translation {
                self.populate_gltf_animation_transform_translation(
                    ctx,
                    ctx.resolve_animation_sampler(sampler_ref)?,
                    transform_key,
                )?;
            }

            if let Some(sampler_ref) = node_samplers.rotation {
                self.populate_gltf_animation_transform_rotation(
                    ctx,
                    ctx.resolve_animation_sampler(sampler_ref)?,
                    transform_key,
                )?;
            }

            if let Some(sampler_ref) = node_samplers.scale {
                self.populate_gltf_animation_transform_scale(
                    ctx,
                    ctx.resolve_animation_sampler(sampler_ref)?,
                    transform_key,
                )?;
            }
        }

        for child in gltf_node.children() {
            self.populate_gltf_node_animation(ctx, &child)?;
        }

        Ok(())
    }

    fn populate_gltf_animation_morph<'a, 'b: 'a, 'c: 'a>(
        &'a mut self,
        ctx: &'c GltfPopulateContext,
        gltf_sampler: gltf::animation::Sampler<'b>,
        geometry_morph_key: Option<GeometryMorphKey>,
        material_morph_key: Option<MaterialMorphKey>,
    ) -> Result<Vec<AnimationKey>> {
        let mut morph_keys = Vec::new();
        let mut animation_keys = Vec::new();

        if let Some(morph_key) = geometry_morph_key {
            morph_keys.push(morph_key.into());
        }

        if let Some(morph_key) = material_morph_key {
            morph_keys.push(morph_key.into());
        }

        for morph_key in morph_keys {
            let targets_len = match morph_key {
                AnimationMorphKey::Geometry(morph_key) => {
                    self.meshes
                        .morphs
                        .geometry
                        .get_info(morph_key)
                        .map_err(|_| AwsmGltfError::MissingMorphForAnimation)?
                        .targets_len
                }
                AnimationMorphKey::Material(morph_key) => {
                    self.meshes
                        .morphs
                        .material
                        .get_info(morph_key)
                        .map_err(|_| AwsmGltfError::MissingMorphForAnimation)?
                        .targets_len
                }
            };

            let clip = gltf_animation_clip_morph_from_buffers(
                &gltf_sampler,
                targets_len,
                &ctx.data.buffers.raw,
            )?;

            let player = AnimationPlayer::new(clip);

            animation_keys.push(self.animations.insert_morph(player, morph_key));
        }

        Ok(animation_keys)
    }

    fn populate_gltf_animation_transform_translation<'a, 'b: 'a, 'c: 'a>(
        &'a mut self,
        ctx: &'c GltfPopulateContext,
        gltf_sampler: gltf::animation::Sampler<'b>,
        transform_key: TransformKey,
    ) -> Result<AnimationKey> {
        let clip = gltf_animation_clip_transform(ctx, &gltf_sampler, TransformTarget::Translation)?;
        let player = AnimationPlayer::new(clip);

        Ok(self.animations.insert_transform(player, transform_key))
    }

    fn populate_gltf_animation_transform_rotation<'a, 'b: 'a, 'c: 'a>(
        &'a mut self,
        ctx: &'c GltfPopulateContext,
        gltf_sampler: gltf::animation::Sampler<'b>,
        transform_key: TransformKey,
    ) -> Result<AnimationKey> {
        let clip = gltf_animation_clip_transform(ctx, &gltf_sampler, TransformTarget::Rotation)?;
        let player = AnimationPlayer::new(clip);

        Ok(self.animations.insert_transform(player, transform_key))
    }

    fn populate_gltf_animation_transform_scale<'a, 'b: 'a, 'c: 'a>(
        &'a mut self,
        ctx: &'c GltfPopulateContext,
        gltf_sampler: gltf::animation::Sampler<'b>,
        transform_key: TransformKey,
    ) -> Result<AnimationKey> {
        let clip = gltf_animation_clip_transform(ctx, &gltf_sampler, TransformTarget::Scale)?;
        let player = AnimationPlayer::new(clip);

        Ok(self.animations.insert_transform(player, transform_key))
    }
}

pub(crate) enum TransformTarget {
    Translation,
    Rotation,
    Scale,
}

impl TransformTarget {
    fn as_str(&self) -> &'static str {
        match self {
            TransformTarget::Translation => "translation",
            TransformTarget::Rotation => "rotation",
            TransformTarget::Scale => "scale",
        }
    }

    fn chunk_size(&self) -> usize {
        match self {
            TransformTarget::Translation => 3,
            TransformTarget::Rotation => 4,
            TransformTarget::Scale => 3,
        }
    }
}

fn gltf_animation_clip_transform(
    ctx: &GltfPopulateContext,
    gltf_sampler: &gltf::animation::Sampler,
    target: TransformTarget,
) -> Result<AnimationClip> {
    gltf_animation_clip_transform_from_buffers(gltf_sampler, target, &ctx.data.buffers.raw)
}

/// Reads a sampler's input accessor (keyframe timestamps) directly from raw
/// glTF buffers. Shared by both the populate path (via
/// `gltf_animation_clip_transform`) and the renderer-free `extract` path.
pub(crate) fn sampler_timestamps_from_buffers(
    gltf_sampler: &gltf::animation::Sampler,
    buffers: &[Vec<u8>],
) -> Result<Vec<f64>> {
    let bytes = accessor_to_bytes(&gltf_sampler.input(), buffers)?;
    Ok(u8_to_f32_vec(&bytes)
        .into_iter()
        .map(|v| v as f64)
        .collect())
}

/// Parses a transform (translation / rotation / scale) animation sampler into
/// an `AnimationClip` directly from raw glTF buffers. This carries the canonical
/// Linear / Step / CubicSpline + chunk-size logic; both the populate path and
/// the renderer-free `extract` path delegate here so the parsing stays
/// byte-identical.
pub(crate) fn gltf_animation_clip_transform_from_buffers(
    gltf_sampler: &gltf::animation::Sampler,
    target: TransformTarget,
    buffers: &[Vec<u8>],
) -> Result<AnimationClip> {
    let times = sampler_timestamps_from_buffers(gltf_sampler, buffers)?;
    let duration = (times.last().copied().unwrap_or(0.0) - times[0]) as f64;
    let values = accessor_to_bytes(&gltf_sampler.output(), buffers)?;
    let values = u8_to_f32_vec(&values);

    let values = values
        .chunks(target.chunk_size())
        .map(|chunk| {
            AnimationData::Transform(match target {
                TransformTarget::Translation => {
                    TransformAnimation::new_translation(Vec3::from_slice(chunk))
                }
                TransformTarget::Rotation => {
                    TransformAnimation::new_rotation(Quat::from_slice(chunk))
                }
                TransformTarget::Scale => TransformAnimation::new_scale(Vec3::from_slice(chunk)),
            })
        })
        .collect();

    let sampler = match gltf_sampler.interpolation() {
        gltf::animation::Interpolation::Linear => AnimationSampler::Linear { times, values },
        gltf::animation::Interpolation::Step => AnimationSampler::Step { times, values },
        gltf::animation::Interpolation::CubicSpline => {
            let mut in_tangents = Vec::with_capacity(values.len() / 3);
            let mut spline_vertices = Vec::with_capacity(values.len() / 3);
            let mut out_tangents = Vec::with_capacity(values.len() / 3);

            for x in values.chunks_exact(3) {
                in_tangents.push(x[0].clone());
                spline_vertices.push(x[1].clone());
                out_tangents.push(x[2].clone());
            }

            AnimationSampler::CubicSpline {
                times,
                in_tangents,
                values: spline_vertices,
                out_tangents,
            }
        }
    };

    Ok(AnimationClip::new(
        Some(format!("transform {}", target.as_str())),
        duration,
        sampler,
    ))
}

/// Parses a morph-target-weights animation sampler into an `AnimationClip`
/// whose values are `AnimationData::Vertex` chunks of `targets_len` weights
/// each, directly from raw glTF buffers. Mirrors the sampler parsing in
/// `populate_gltf_animation_morph`; both paths delegate here so the parsing
/// stays byte-identical.
pub(crate) fn gltf_animation_clip_morph_from_buffers(
    gltf_sampler: &gltf::animation::Sampler,
    targets_len: usize,
    buffers: &[Vec<u8>],
) -> Result<AnimationClip> {
    let times = sampler_timestamps_from_buffers(gltf_sampler, buffers)?;
    let duration = (times.last().copied().unwrap_or(0.0) - times[0]) as f64;
    let values = accessor_to_bytes(&gltf_sampler.output(), buffers)?;
    let values = u8_to_f32_vec(&values);

    let values: Vec<AnimationData> = values
        .chunks(targets_len)
        .map(|chunk| AnimationData::Vertex(VertexAnimation::new(chunk.to_vec())))
        .collect();

    let sampler = match gltf_sampler.interpolation() {
        gltf::animation::Interpolation::Linear => AnimationSampler::Linear { times, values },
        gltf::animation::Interpolation::Step => AnimationSampler::Step { times, values },
        gltf::animation::Interpolation::CubicSpline => {
            let mut in_tangents = Vec::with_capacity(values.len() / 3);
            let mut spline_vertices = Vec::with_capacity(values.len() / 3);
            let mut out_tangents = Vec::with_capacity(values.len() / 3);

            for x in values.chunks_exact(3) {
                in_tangents.push(x[0].clone());
                spline_vertices.push(x[1].clone());
                out_tangents.push(x[2].clone());
            }

            AnimationSampler::CubicSpline {
                times,
                in_tangents,
                values: spline_vertices,
                out_tangents,
            }
        }
    };

    Ok(AnimationClip::new(
        Some("morph".to_string()),
        duration,
        sampler,
    ))
}
