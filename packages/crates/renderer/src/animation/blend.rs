//! Pure blend math over [`AnimationData`] — the per-target compositing
//! primitives the NLA mixer uses to fold layer contributions onto an
//! accumulator.
//!
//! Two operations:
//!
//! - [`blend_replace`] moves `acc` toward `layer` by weight `w` (a lerp /
//!   slerp). This is the [`Replace`](super::mixer::LayerMode::Replace) layer
//!   primitive.
//! - [`blend_additive`] adds `w * (layer - reference)` onto `acc`. This is the
//!   [`Additive`](super::mixer::LayerMode::Additive) layer primitive.
//!
//! Both are **pure** (no renderer state) and total: a variant mismatch returns
//! `acc.clone()` rather than panicking (the editor lowering guarantees matching
//! kinds; this is purely defensive). For `Transform` they operate **per-field**
//! (translation / rotation / scale independently), honoring the layer field's
//! `Option`-ness (a `None` layer field leaves that field of `acc` untouched).
//! `acc` is always a fully-populated `Transform` because it is
//! seeded from rest.

use glam::{Quat, Vec2, Vec3, Vec4};

use super::data::{AnimationData, TransformAnimation, VertexAnimation};

/// Shortest-arc lerp toward `layer` by `w`, as a quaternion `slerp` after
/// flipping `layer` to the hemisphere of `acc` (so we always take the short
/// path). The result is normalized by glam's `slerp`.
fn quat_blend_replace(acc: Quat, layer: Quat, w: f32) -> Quat {
    let layer = if acc.dot(layer) < 0.0 { -layer } else { layer };
    acc.slerp(layer, w)
}

/// Per-component lerp.
fn vec3_lerp(acc: Vec3, layer: Vec3, w: f32) -> Vec3 {
    acc + (layer - acc) * w
}

/// Per-index lerp over the overlapping prefix of two weight vectors. Indices
/// beyond `layer`'s length keep `acc`'s value (lengths are assumed equal in
/// practice; this is the defensive path).
fn weights_blend_replace(acc: &[f32], layer: &[f32], w: f32) -> Vec<f32> {
    let mut out = acc.to_vec();
    for (i, out_i) in out.iter_mut().enumerate() {
        if let Some(&l) = layer.get(i) {
            *out_i += (l - *out_i) * w;
        }
    }
    out
}

/// The scaled shortest-path delta quaternion `Quat::IDENTITY.slerp(delta, w)`
/// where `delta = layer * reference.inverse()` (normalized to the short arc).
fn quat_scaled_delta(layer: Quat, reference: Quat, w: f32) -> Quat {
    let mut delta = (layer * reference.inverse()).normalize();
    // Keep the delta on the short arc (w == 0 ⇒ identity contribution).
    if delta.w < 0.0 {
        delta = -delta;
    }
    Quat::IDENTITY.slerp(delta, w)
}

/// Blend the per-field `Transform` accumulator toward `layer` (a
/// [`Replace`](super::mixer::LayerMode::Replace) contribution). `acc` carries
/// all three fields (seeded from rest); a `None` field on `layer` leaves that
/// field of `acc` unchanged.
fn transform_blend_replace(
    acc: &TransformAnimation,
    layer: &TransformAnimation,
    w: f32,
) -> TransformAnimation {
    let translation = match (acc.translation, layer.translation) {
        (Some(a), Some(l)) => Some(vec3_lerp(a, l, w)),
        (a, _) => a,
    };
    let rotation = match (acc.rotation, layer.rotation) {
        (Some(a), Some(l)) => Some(quat_blend_replace(a, l, w)),
        (a, _) => a,
    };
    let scale = match (acc.scale, layer.scale) {
        (Some(a), Some(l)) => Some(vec3_lerp(a, l, w)),
        (a, _) => a,
    };
    TransformAnimation {
        translation,
        rotation,
        scale,
    }
}

/// Move `acc` toward `layer` by weight `w` (a per-kind lerp / slerp).
///
/// - `F32` / `F64` / `Vec3`: component lerp.
/// - `Quat`: shortest-arc `slerp`.
/// - `Vertex`: per-index lerp.
/// - `Transform`: per-field, honoring layer-field `Option`-ness.
/// - Mismatched variants: returns `acc.clone()`.
pub fn blend_replace(acc: &AnimationData, layer: &AnimationData, w: f32) -> AnimationData {
    match (acc, layer) {
        (AnimationData::F32(a), AnimationData::F32(l)) => AnimationData::F32(a + (l - a) * w),
        (AnimationData::F64(a), AnimationData::F64(l)) => {
            AnimationData::F64(a + (l - a) * w as f64)
        }
        (AnimationData::Vec2(a), AnimationData::Vec2(l)) => {
            AnimationData::Vec2(*a + (*l - *a) * w)
        }
        (AnimationData::Vec3(a), AnimationData::Vec3(l)) => {
            AnimationData::Vec3(vec3_lerp(*a, *l, w))
        }
        (AnimationData::Vec4(a), AnimationData::Vec4(l)) => {
            AnimationData::Vec4(*a + (*l - *a) * w)
        }
        (AnimationData::Quat(a), AnimationData::Quat(l)) => {
            AnimationData::Quat(quat_blend_replace(*a, *l, w))
        }
        (AnimationData::Vertex(a), AnimationData::Vertex(l)) => AnimationData::Vertex(
            VertexAnimation::new(weights_blend_replace(&a.weights, &l.weights, w)),
        ),
        (AnimationData::Transform(a), AnimationData::Transform(l)) => {
            AnimationData::Transform(transform_blend_replace(a, l, w))
        }
        // Defensive — editor lowering guarantees matching kinds.
        _ => acc.clone(),
    }
}

/// Add `w * (layer - reference)` onto a per-field `Transform` accumulator (an
/// [`Additive`](super::mixer::LayerMode::Additive) contribution). A `None`
/// field on `layer` contributes no delta to that field of `acc`; the
/// `reference` field falls back to identity when absent so a present `layer`
/// field still produces a well-defined delta.
fn transform_blend_additive(
    acc: &TransformAnimation,
    layer: &TransformAnimation,
    reference: &TransformAnimation,
    w: f32,
) -> TransformAnimation {
    let translation = match (acc.translation, layer.translation) {
        (Some(a), Some(l)) => {
            let r = reference.translation.unwrap_or(Vec3::ZERO);
            Some(a + (l - r) * w)
        }
        (a, _) => a,
    };
    let rotation = match (acc.rotation, layer.rotation) {
        (Some(a), Some(l)) => {
            let r = reference.rotation.unwrap_or(Quat::IDENTITY);
            Some((quat_scaled_delta(l, r, w) * a).normalize())
        }
        (a, _) => a,
    };
    let scale = match (acc.scale, layer.scale) {
        (Some(a), Some(l)) => {
            let r = reference.scale.unwrap_or(Vec3::ONE);
            Some(a + (l - r) * w)
        }
        (a, _) => a,
    };
    TransformAnimation {
        translation,
        rotation,
        scale,
    }
}

/// Add `w * (layer - reference)` onto `acc` (the additive-layer primitive).
///
/// - `F32` / `F64` / `Vec3`: `acc + w * (layer - reference)`.
/// - `Quat`: premultiply `acc` by the `w`-scaled shortest-path delta
///   `layer * reference.inverse()` (delta scaled via
///   `Quat::IDENTITY.slerp(delta, w)`), then normalize.
/// - `Vertex`: per-index `acc + w * (layer - reference)`.
/// - `Transform`: per-field additive, honoring layer-field `Option`-ness.
/// - Mismatched variants: returns `acc.clone()`.
pub fn blend_additive(
    acc: &AnimationData,
    layer: &AnimationData,
    reference: &AnimationData,
    w: f32,
) -> AnimationData {
    match (acc, layer, reference) {
        (AnimationData::F32(a), AnimationData::F32(l), AnimationData::F32(r)) => {
            AnimationData::F32(a + w * (l - r))
        }
        (AnimationData::F64(a), AnimationData::F64(l), AnimationData::F64(r)) => {
            AnimationData::F64(a + w as f64 * (l - r))
        }
        (AnimationData::Vec2(a), AnimationData::Vec2(l), AnimationData::Vec2(r)) => {
            AnimationData::Vec2(*a + (*l - *r) * w)
        }
        (AnimationData::Vec3(a), AnimationData::Vec3(l), AnimationData::Vec3(r)) => {
            AnimationData::Vec3(*a + (*l - *r) * w)
        }
        (AnimationData::Vec4(a), AnimationData::Vec4(l), AnimationData::Vec4(r)) => {
            AnimationData::Vec4(*a + (*l - *r) * w)
        }
        (AnimationData::Quat(a), AnimationData::Quat(l), AnimationData::Quat(r)) => {
            AnimationData::Quat((quat_scaled_delta(*l, *r, w) * *a).normalize())
        }
        (AnimationData::Vertex(a), AnimationData::Vertex(l), AnimationData::Vertex(r)) => {
            let mut out = a.weights.clone();
            for (i, out_i) in out.iter_mut().enumerate() {
                let lv = l.weights.get(i).copied().unwrap_or(0.0);
                let rv = r.weights.get(i).copied().unwrap_or(0.0);
                *out_i += w * (lv - rv);
            }
            AnimationData::Vertex(VertexAnimation::new(out))
        }
        (AnimationData::Transform(a), AnimationData::Transform(l), AnimationData::Transform(r)) => {
            AnimationData::Transform(transform_blend_additive(a, l, r, w))
        }
        // Defensive — editor lowering guarantees matching kinds.
        _ => acc.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn as_f32(d: &AnimationData) -> f32 {
        match d {
            AnimationData::F32(v) => *v,
            other => panic!("expected F32, got {other:?}"),
        }
    }
    fn as_vec3(d: &AnimationData) -> Vec3 {
        match d {
            AnimationData::Vec3(v) => *v,
            other => panic!("expected Vec3, got {other:?}"),
        }
    }
    fn as_quat(d: &AnimationData) -> Quat {
        match d {
            AnimationData::Quat(q) => *q,
            other => panic!("expected Quat, got {other:?}"),
        }
    }
    fn as_transform(d: &AnimationData) -> TransformAnimation {
        match d {
            AnimationData::Transform(t) => t.clone(),
            other => panic!("expected Transform, got {other:?}"),
        }
    }

    // ---- blend_replace --------------------------------------------------

    #[test]
    fn replace_f32_endpoints_and_midpoint() {
        let acc = AnimationData::F32(2.0);
        let layer = AnimationData::F32(10.0);
        assert!((as_f32(&blend_replace(&acc, &layer, 0.0)) - 2.0).abs() < 1e-6);
        assert!((as_f32(&blend_replace(&acc, &layer, 1.0)) - 10.0).abs() < 1e-6);
        assert!((as_f32(&blend_replace(&acc, &layer, 0.5)) - 6.0).abs() < 1e-6);
    }

    #[test]
    fn replace_vec3_endpoints_and_midpoint() {
        let acc = AnimationData::Vec3(Vec3::new(0.0, 0.0, 0.0));
        let layer = AnimationData::Vec3(Vec3::new(2.0, 4.0, 6.0));
        assert!(as_vec3(&blend_replace(&acc, &layer, 0.0)).abs_diff_eq(Vec3::ZERO, 1e-6));
        assert!(
            as_vec3(&blend_replace(&acc, &layer, 1.0)).abs_diff_eq(Vec3::new(2.0, 4.0, 6.0), 1e-6)
        );
        assert!(
            as_vec3(&blend_replace(&acc, &layer, 0.5)).abs_diff_eq(Vec3::new(1.0, 2.0, 3.0), 1e-6)
        );
    }

    #[test]
    fn replace_quat_endpoints_and_midpoint() {
        let acc = AnimationData::Quat(Quat::IDENTITY);
        let layer = AnimationData::Quat(Quat::from_rotation_z(std::f32::consts::FRAC_PI_2));
        // w=0 ⇒ acc
        assert!(as_quat(&blend_replace(&acc, &layer, 0.0)).abs_diff_eq(Quat::IDENTITY, 1e-5));
        // w=1 ⇒ layer
        assert!(as_quat(&blend_replace(&acc, &layer, 1.0))
            .abs_diff_eq(Quat::from_rotation_z(std::f32::consts::FRAC_PI_2), 1e-5));
        // w=0.5 ⇒ slerp midpoint == 45° rotation about Z
        let mid = as_quat(&blend_replace(&acc, &layer, 0.5));
        let expected = Quat::from_rotation_z(std::f32::consts::FRAC_PI_4);
        assert!(
            mid.abs_diff_eq(expected, 1e-5) || mid.abs_diff_eq(-expected, 1e-5),
            "mid={mid:?} expected={expected:?}"
        );
    }

    #[test]
    fn replace_quat_takes_short_arc() {
        // layer expressed on the far hemisphere (negated) must still slerp the
        // short way (≡ identity-ward), not the long way.
        let acc = AnimationData::Quat(Quat::IDENTITY);
        let far = -Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
        let mid = as_quat(&blend_replace(&acc, &AnimationData::Quat(far), 0.5));
        let expected = Quat::from_rotation_z(std::f32::consts::FRAC_PI_4);
        assert!(
            mid.abs_diff_eq(expected, 1e-5) || mid.abs_diff_eq(-expected, 1e-5),
            "short-arc failed: mid={mid:?}"
        );
    }

    #[test]
    fn replace_transform_per_field_optionality() {
        // acc is fully populated (seeded from rest).
        let acc = AnimationData::Transform(TransformAnimation {
            translation: Some(Vec3::new(1.0, 1.0, 1.0)),
            rotation: Some(Quat::IDENTITY),
            scale: Some(Vec3::splat(2.0)),
        });
        // layer drives translation only.
        let layer = AnimationData::Transform(TransformAnimation {
            translation: Some(Vec3::new(3.0, 3.0, 3.0)),
            rotation: None,
            scale: None,
        });
        let out = as_transform(&blend_replace(&acc, &layer, 1.0));
        // translation replaced
        assert!(out
            .translation
            .unwrap()
            .abs_diff_eq(Vec3::new(3.0, 3.0, 3.0), 1e-6));
        // rotation / scale untouched (still acc's)
        assert!(out.rotation.unwrap().abs_diff_eq(Quat::IDENTITY, 1e-6));
        assert!(out.scale.unwrap().abs_diff_eq(Vec3::splat(2.0), 1e-6));
    }

    #[test]
    fn replace_mismatched_returns_acc() {
        let acc = AnimationData::F32(5.0);
        let layer = AnimationData::Vec3(Vec3::ONE);
        assert!((as_f32(&blend_replace(&acc, &layer, 1.0)) - 5.0).abs() < 1e-6);
    }

    // ---- blend_additive -------------------------------------------------

    #[test]
    fn additive_f32_adds_scaled_delta() {
        let acc = AnimationData::F32(5.0);
        let layer = AnimationData::F32(9.0);
        let reference = AnimationData::F32(7.0);
        // w=0 ⇒ acc unchanged
        assert!((as_f32(&blend_additive(&acc, &layer, &reference, 0.0)) - 5.0).abs() < 1e-6);
        // w=1 ⇒ acc + (9-7) = 7
        assert!((as_f32(&blend_additive(&acc, &layer, &reference, 1.0)) - 7.0).abs() < 1e-6);
        // w=0.5 ⇒ acc + 0.5*(2) = 6
        assert!((as_f32(&blend_additive(&acc, &layer, &reference, 0.5)) - 6.0).abs() < 1e-6);
    }

    #[test]
    fn additive_f32_ref_equals_layer_no_change() {
        let acc = AnimationData::F32(5.0);
        let layer = AnimationData::F32(9.0);
        let reference = AnimationData::F32(9.0);
        assert!((as_f32(&blend_additive(&acc, &layer, &reference, 1.0)) - 5.0).abs() < 1e-6);
    }

    #[test]
    fn additive_quat_ref_equals_layer_is_identity() {
        let acc = Quat::from_rotation_x(0.3);
        let layer = Quat::from_rotation_y(0.7);
        let out = as_quat(&blend_additive(
            &AnimationData::Quat(acc),
            &AnimationData::Quat(layer),
            &AnimationData::Quat(layer), // ref == layer ⇒ delta is identity
            1.0,
        ));
        assert!(out.abs_diff_eq(acc, 1e-5), "out={out:?} acc={acc:?}");
    }

    #[test]
    fn additive_quat_delta_composes() {
        // reference = identity, layer = 90° about Z ⇒ full-weight delta is 90°
        // about Z premultiplied onto acc.
        let acc = Quat::from_rotation_x(0.2);
        let layer = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
        let reference = Quat::IDENTITY;
        let out = as_quat(&blend_additive(
            &AnimationData::Quat(acc),
            &AnimationData::Quat(layer),
            &AnimationData::Quat(reference),
            1.0,
        ));
        let expected = (layer * acc).normalize();
        assert!(
            out.abs_diff_eq(expected, 1e-5) || out.abs_diff_eq(-expected, 1e-5),
            "out={out:?} expected={expected:?}"
        );
    }

    #[test]
    fn additive_transform_per_field_optionality() {
        let acc = AnimationData::Transform(TransformAnimation {
            translation: Some(Vec3::new(1.0, 0.0, 0.0)),
            rotation: Some(Quat::IDENTITY),
            scale: Some(Vec3::ONE),
        });
        // layer drives translation only.
        let layer = AnimationData::Transform(TransformAnimation {
            translation: Some(Vec3::new(5.0, 0.0, 0.0)),
            rotation: None,
            scale: None,
        });
        let reference = AnimationData::Transform(TransformAnimation {
            translation: Some(Vec3::new(2.0, 0.0, 0.0)),
            rotation: None,
            scale: None,
        });
        let out = as_transform(&blend_additive(&acc, &layer, &reference, 1.0));
        // translation: 1 + (5-2) = 4
        assert!(out
            .translation
            .unwrap()
            .abs_diff_eq(Vec3::new(4.0, 0.0, 0.0), 1e-6));
        // rotation / scale untouched (no layer delta)
        assert!(out.rotation.unwrap().abs_diff_eq(Quat::IDENTITY, 1e-6));
        assert!(out.scale.unwrap().abs_diff_eq(Vec3::ONE, 1e-6));
    }

    #[test]
    fn additive_mismatched_returns_acc() {
        let acc = AnimationData::F32(5.0);
        let layer = AnimationData::Vec3(Vec3::ONE);
        let reference = AnimationData::Vec3(Vec3::ZERO);
        assert!((as_f32(&blend_additive(&acc, &layer, &reference, 1.0)) - 5.0).abs() < 1e-6);
    }

    /// No-drift: the real loop RE-SEEDS the accumulator from a constant rest
    /// every frame, so an additive layer with constant inputs must produce the
    /// SAME result every iteration (no accumulation / drift).
    #[test]
    fn additive_seed_from_rest_no_drift_f32() {
        let rest = AnimationData::F32(5.0);
        let layer = AnimationData::F32(9.0);
        let reference = AnimationData::F32(7.0);

        let mut first: Option<f32> = None;
        for _ in 0..100 {
            // seed-from-rest each iteration (mirrors update_animations).
            let acc = rest.clone();
            let out = as_f32(&blend_additive(&acc, &layer, &reference, 0.5));
            match first {
                None => first = Some(out),
                Some(f) => assert!((out - f).abs() < 1e-7, "drift: {out} != {f}"),
            }
        }
        // sanity: value is acc + 0.5*(9-7) = 6
        assert!((first.unwrap() - 6.0).abs() < 1e-6);
    }

    /// No-drift for quaternions: re-seeding from rest yields an identical
    /// quaternion every iteration (premultiply does not accumulate).
    #[test]
    fn additive_seed_from_rest_no_drift_quat() {
        let rest = Quat::from_rotation_x(0.2);
        let layer = Quat::from_rotation_z(0.8);
        let reference = Quat::from_rotation_z(0.1);

        let mut first: Option<Quat> = None;
        for _ in 0..100 {
            let acc = AnimationData::Quat(rest);
            let out = as_quat(&blend_additive(
                &acc,
                &AnimationData::Quat(layer),
                &AnimationData::Quat(reference),
                0.5,
            ));
            match first {
                None => first = Some(out),
                Some(f) => assert!(out.abs_diff_eq(f, 1e-6), "drift: {out:?} != {f:?}"),
            }
        }
    }

    /// No-drift for a per-field Transform additive layer.
    #[test]
    fn additive_seed_from_rest_no_drift_transform() {
        let rest = TransformAnimation {
            translation: Some(Vec3::new(1.0, 2.0, 3.0)),
            rotation: Some(Quat::from_rotation_y(0.3)),
            scale: Some(Vec3::splat(1.5)),
        };
        let layer = TransformAnimation {
            translation: Some(Vec3::new(4.0, 4.0, 4.0)),
            rotation: Some(Quat::from_rotation_y(0.9)),
            scale: Some(Vec3::splat(2.0)),
        };
        let reference = TransformAnimation {
            translation: Some(Vec3::new(2.0, 2.0, 2.0)),
            rotation: Some(Quat::from_rotation_y(0.1)),
            scale: Some(Vec3::splat(1.0)),
        };

        let mut first: Option<TransformAnimation> = None;
        for _ in 0..100 {
            let acc = AnimationData::Transform(rest.clone());
            let out = as_transform(&blend_additive(
                &acc,
                &AnimationData::Transform(layer.clone()),
                &AnimationData::Transform(reference.clone()),
                0.5,
            ));
            match &first {
                None => first = Some(out),
                Some(f) => {
                    assert!(out
                        .translation
                        .unwrap()
                        .abs_diff_eq(f.translation.unwrap(), 1e-6));
                    assert!(out.rotation.unwrap().abs_diff_eq(f.rotation.unwrap(), 1e-6));
                    assert!(out.scale.unwrap().abs_diff_eq(f.scale.unwrap(), 1e-6));
                }
            }
        }
    }
}
