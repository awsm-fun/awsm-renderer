//! Animation data types and interpolation helpers.

use glam::{Quat, Vec2, Vec3, Vec4};

use crate::transforms::Transform;

use super::interpolate::{
    interpolate_cubic_spline_f32, interpolate_cubic_spline_f64, interpolate_cubic_spline_quat,
    interpolate_cubic_spline_vec2, interpolate_cubic_spline_vec3, interpolate_cubic_spline_vec4,
    interpolate_linear_f32, interpolate_linear_f64, interpolate_linear_quat,
    interpolate_linear_vec2, interpolate_linear_vec3, interpolate_linear_vec4,
};

/// Animation data variants supported by the player.
#[derive(Debug, Clone)]
pub enum AnimationData {
    Transform(TransformAnimation),
    Vertex(VertexAnimation),
    Vec2(Vec2),
    Vec3(Vec3),
    Vec4(Vec4),
    Quat(Quat),
    F32(f32),
    F64(f64),
}

impl Animatable for AnimationData {
    fn interpolate_linear(first: &Self, second: &Self, t: f64) -> Self {
        match (first, second) {
            (AnimationData::Transform(first), AnimationData::Transform(second)) => {
                AnimationData::Transform(TransformAnimation::interpolate_linear(first, second, t))
            }
            (AnimationData::Vertex(first), AnimationData::Vertex(second)) => {
                AnimationData::Vertex(VertexAnimation::interpolate_linear(first, second, t))
            }
            (AnimationData::Vec2(first), AnimationData::Vec2(second)) => {
                AnimationData::Vec2(interpolate_linear_vec2(*first, *second, t))
            }
            (AnimationData::Vec3(first), AnimationData::Vec3(second)) => {
                AnimationData::Vec3(interpolate_linear_vec3(*first, *second, t))
            }
            (AnimationData::Vec4(first), AnimationData::Vec4(second)) => {
                AnimationData::Vec4(interpolate_linear_vec4(*first, *second, t))
            }
            (AnimationData::Quat(first), AnimationData::Quat(second)) => {
                AnimationData::Quat(interpolate_linear_quat(*first, *second, t))
            }
            (AnimationData::F32(first), AnimationData::F32(second)) => {
                AnimationData::F32(interpolate_linear_f32(*first, *second, t))
            }
            (AnimationData::F64(first), AnimationData::F64(second)) => {
                AnimationData::F64(interpolate_linear_f64(*first, *second, t))
            }
            _ => panic!("Cannot interpolate between different animation types"),
        }
    }

    fn interpolate_cubic_spline(
        first_value: &Self,
        first_tangent: &Self,
        second_value: &Self,
        second_tangent: &Self,
        delta_time: f64,
        interpolation_time: f64,
    ) -> Self {
        match (first_value, first_tangent, second_value, second_tangent) {
            (
                AnimationData::Transform(first_value),
                AnimationData::Transform(first_tangent),
                AnimationData::Transform(second_value),
                AnimationData::Transform(second_tangent),
            ) => AnimationData::Transform(TransformAnimation::interpolate_cubic_spline(
                first_value,
                first_tangent,
                second_value,
                second_tangent,
                delta_time,
                interpolation_time,
            )),
            (
                AnimationData::Vertex(first_value),
                AnimationData::Vertex(first_tangent),
                AnimationData::Vertex(second_value),
                AnimationData::Vertex(second_tangent),
            ) => AnimationData::Vertex(VertexAnimation::interpolate_cubic_spline(
                first_value,
                first_tangent,
                second_value,
                second_tangent,
                delta_time,
                interpolation_time,
            )),
            (
                AnimationData::Vec2(first_value),
                AnimationData::Vec2(first_tangent),
                AnimationData::Vec2(second_value),
                AnimationData::Vec2(second_tangent),
            ) => AnimationData::Vec2(interpolate_cubic_spline_vec2(
                *first_value,
                *first_tangent,
                *second_value,
                *second_tangent,
                delta_time,
                interpolation_time,
            )),
            (
                AnimationData::Vec3(first_value),
                AnimationData::Vec3(first_tangent),
                AnimationData::Vec3(second_value),
                AnimationData::Vec3(second_tangent),
            ) => AnimationData::Vec3(interpolate_cubic_spline_vec3(
                *first_value,
                *first_tangent,
                *second_value,
                *second_tangent,
                delta_time,
                interpolation_time,
            )),
            (
                AnimationData::Vec4(first_value),
                AnimationData::Vec4(first_tangent),
                AnimationData::Vec4(second_value),
                AnimationData::Vec4(second_tangent),
            ) => AnimationData::Vec4(interpolate_cubic_spline_vec4(
                *first_value,
                *first_tangent,
                *second_value,
                *second_tangent,
                delta_time,
                interpolation_time,
            )),
            (
                AnimationData::Quat(first_value),
                AnimationData::Quat(first_tangent),
                AnimationData::Quat(second_value),
                AnimationData::Quat(second_tangent),
            ) => AnimationData::Quat(interpolate_cubic_spline_quat(
                *first_value,
                *first_tangent,
                *second_value,
                *second_tangent,
                delta_time,
                interpolation_time,
            )),
            (
                AnimationData::F32(first_value),
                AnimationData::F32(first_tangent),
                AnimationData::F32(second_value),
                AnimationData::F32(second_tangent),
            ) => AnimationData::F32(interpolate_cubic_spline_f32(
                *first_value,
                *first_tangent,
                *second_value,
                *second_tangent,
                delta_time,
                interpolation_time,
            )),
            (
                AnimationData::F64(first_value),
                AnimationData::F64(first_tangent),
                AnimationData::F64(second_value),
                AnimationData::F64(second_tangent),
            ) => AnimationData::F64(interpolate_cubic_spline_f64(
                *first_value,
                *first_tangent,
                *second_value,
                *second_tangent,
                delta_time,
                interpolation_time,
            )),
            _ => panic!("Cannot interpolate between different animation types"),
        }
    }
}

/// Trait for types that support animation interpolation.
pub trait Animatable: Clone {
    fn interpolate_linear(first: &Self, second: &Self, t: f64) -> Self;
    fn interpolate_cubic_spline(
        first_value: &Self,
        first_tangent: &Self,
        second_value: &Self,
        second_tangent: &Self,
        delta_time: f64,
        interpolation_time: f64,
    ) -> Self;
}

/// Translation/rotation/scale animation data.
#[derive(Debug, Clone)]
pub struct TransformAnimation {
    pub translation: Option<Vec3>,
    pub rotation: Option<Quat>,
    pub scale: Option<Vec3>,
}

impl TransformAnimation {
    /// Creates a translation-only animation.
    pub fn new_translation(translation: Vec3) -> Self {
        Self {
            translation: Some(translation),
            rotation: None,
            scale: None,
        }
    }
    /// Creates a rotation-only animation.
    pub fn new_rotation(rotation: Quat) -> Self {
        Self {
            translation: None,
            rotation: Some(rotation),
            scale: None,
        }
    }
    /// Creates a scale-only animation.
    pub fn new_scale(scale: Vec3) -> Self {
        Self {
            translation: None,
            rotation: None,
            scale: Some(scale),
        }
    }
    /// Applies this animation onto a transform and returns the result.
    pub fn apply(&self, mut input: Transform) -> Transform {
        if let Some(translation) = &self.translation {
            input.translation = *translation;
        }
        if let Some(rotation) = &self.rotation {
            input.rotation = *rotation;
        }
        if let Some(scale) = &self.scale {
            input.scale = *scale;
        }
        input
    }

    /// Applies this animation in place on a transform.
    pub fn apply_mut(&self, input: &mut Transform) {
        if let Some(translation) = &self.translation {
            input.translation = *translation;
        }
        if let Some(rotation) = &self.rotation {
            input.rotation = *rotation;
        }
        if let Some(scale) = &self.scale {
            input.scale = *scale;
        }
    }
}

impl Animatable for TransformAnimation {
    fn interpolate_linear(first: &Self, second: &Self, t: f64) -> Self {
        let translation = match (first.translation, second.translation) {
            (Some(first), Some(second)) => Some(interpolate_linear_vec3(first, second, t)),
            (Some(first), _) => Some(first),
            _ => None,
        };

        let rotation = match (first.rotation, second.rotation) {
            (Some(first), Some(second)) => Some(interpolate_linear_quat(first, second, t)),
            (Some(first), _) => Some(first),
            _ => None,
        };
        let scale = match (first.scale, second.scale) {
            (Some(first), Some(second)) => Some(interpolate_linear_vec3(first, second, t)),
            (Some(first), _) => Some(first),
            _ => None,
        };

        Self {
            translation,
            rotation,
            scale,
        }
    }

    fn interpolate_cubic_spline(
        first_value: &Self,
        first_tangent: &Self,
        second_value: &Self,
        second_tangent: &Self,
        delta_time: f64,
        interpolation_time: f64,
    ) -> Self {
        let translation = match (
            first_value.translation,
            first_tangent.translation,
            second_value.translation,
            second_tangent.translation,
        ) {
            (Some(first_value), Some(first_tangent), Some(second_value), Some(second_tangent)) => {
                Some(interpolate_cubic_spline_vec3(
                    first_value,
                    first_tangent,
                    second_value,
                    second_tangent,
                    delta_time,
                    interpolation_time,
                ))
            }
            _ => None,
        };

        let rotation = match (
            first_value.rotation,
            first_tangent.rotation,
            second_value.rotation,
            second_tangent.rotation,
        ) {
            (Some(first_value), Some(first_tangent), Some(second_value), Some(second_tangent)) => {
                Some(interpolate_cubic_spline_quat(
                    first_value,
                    first_tangent,
                    second_value,
                    second_tangent,
                    delta_time,
                    interpolation_time,
                ))
            }
            _ => None,
        };

        let scale = match (
            first_value.scale,
            first_tangent.scale,
            second_value.scale,
            second_tangent.scale,
        ) {
            (Some(first_value), Some(first_tangent), Some(second_value), Some(second_tangent)) => {
                Some(interpolate_cubic_spline_vec3(
                    first_value,
                    first_tangent,
                    second_value,
                    second_tangent,
                    delta_time,
                    interpolation_time,
                ))
            }
            _ => None,
        };

        Self {
            translation,
            rotation,
            scale,
        }
    }
}

/// Per-vertex weight animation data.
///
/// `mask` is the set of morph-target indices this animation **drives** (bit `i`
/// set ⇒ index `i` is driven). `None` means the animation drives the whole
/// vector — the glTF path, where every keyframe carries all weights. A masked
/// animation (the editor's per-index morph tracks) contributes only its driven
/// indices when applied or blended; undriven indices keep the accumulator /
/// rest value, so two tracks driving different indices of one mesh compose
/// instead of stomping each other (the morph analogue of
/// [`TransformAnimation`]'s per-field `Option`-ness). Masked animations
/// support up to 64 targets; [`Self::new_single`] falls back to unmasked
/// (whole-vector) semantics beyond that.
#[derive(Debug, Clone)]
pub struct VertexAnimation {
    pub weights: Vec<f32>,
    pub mask: Option<u64>,
}

impl VertexAnimation {
    /// Creates a vertex animation from morph weights, driving the WHOLE vector
    /// (no mask) — the glTF whole-vector path.
    pub fn new(weights: Vec<f32>) -> Self {
        Self {
            weights,
            mask: None,
        }
    }

    /// Creates a vertex animation driving a **single** morph index: a weight
    /// vector of length `index + 1` whose slot `index` carries `weight`, masked
    /// so only that index is driven (every other index keeps its current /
    /// rest value under apply + blend). An `index >= 64` (beyond the mask
    /// width — morph-target counts never approach this in practice) falls back
    /// to an unmasked whole vector.
    pub fn new_single(index: usize, weight: f32) -> Self {
        let mut weights = vec![0.0_f32; index + 1];
        weights[index] = weight;
        Self {
            weights,
            mask: (index < 64).then(|| 1u64 << index),
        }
    }

    /// Whether this animation drives morph index `index` (see `mask`).
    #[inline]
    pub fn drives(&self, index: usize) -> bool {
        match self.mask {
            None => true,
            Some(mask) => index < 64 && (mask >> index) & 1 == 1,
        }
    }

    /// Applies weights to a copy of the provided data.
    pub fn apply(&self, input: Vec<f32>) -> Vec<f32> {
        let mut result = input;
        self.apply_mut(&mut result);
        result
    }

    /// Applies weights in place on the provided data — only the driven indices
    /// (an unmasked animation writes every index it carries). The unmasked
    /// path is a straight slice copy (the hot glTF/single-track path).
    pub fn apply_mut(&self, other: &mut [f32]) {
        let n = other.len().min(self.weights.len());
        match self.mask {
            None => other[..n].copy_from_slice(&self.weights[..n]),
            Some(mask) => {
                // A mask only addresses indices 0..64 (cap defends the shift;
                // constructors never produce a masked animation longer than 64).
                for (i, slot) in other.iter_mut().enumerate().take(n.min(64)) {
                    if (mask >> i) & 1 == 1 {
                        *slot = self.weights[i];
                    }
                }
            }
        }
    }
}

/// The union of two driven-index masks (`None` = drives all, absorbing).
fn mask_union(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a | b),
        _ => None,
    }
}

impl Animatable for VertexAnimation {
    fn interpolate_linear(first: &Self, second: &Self, t: f64) -> Self {
        if first.weights.len() != second.weights.len() {
            panic!("Cannot interpolate between animations of different lengths");
        }

        let mut results = Vec::with_capacity(first.weights.len());

        for i in 0..first.weights.len() {
            let weight = interpolate_linear_f32(first.weights[i], second.weights[i], t);
            results.push(weight);
        }

        Self {
            weights: results,
            // Keys of one sampler share one mask; union is the defensive join.
            mask: mask_union(first.mask, second.mask),
        }
    }

    fn interpolate_cubic_spline(
        first_value: &Self,
        first_tangent: &Self,
        second_value: &Self,
        second_tangent: &Self,
        delta_time: f64,
        interpolation_time: f64,
    ) -> Self {
        if first_value.weights.len() != first_tangent.weights.len()
            || first_value.weights.len() != second_value.weights.len()
            || first_value.weights.len() != second_tangent.weights.len()
        {
            panic!("Cannot interpolate between animations of different lengths");
        }

        let mut results = Vec::with_capacity(first_value.weights.len());

        for i in 0..first_value.weights.len() {
            let weight = interpolate_cubic_spline_f32(
                first_value.weights[i],
                first_tangent.weights[i],
                second_value.weights[i],
                second_tangent.weights[i],
                delta_time,
                interpolation_time,
            );
            results.push(weight);
        }

        Self {
            weights: results,
            // Keys of one sampler share one mask; union is the defensive join.
            mask: mask_union(first_value.mask, second_value.mask),
        }
    }
}
