//! Interpolation helpers for animation sampling.

use glam::{Quat, Vec3};

/// Linearly interpolates two Vec3 values.
pub fn interpolate_linear_vec3(first: Vec3, second: Vec3, t: f64) -> Vec3 {
    first.lerp(second, t as f32)
}

/// Linearly interpolates two quaternions.
pub fn interpolate_linear_quat(first: Quat, second: Quat, t: f64) -> Quat {
    first.slerp(second, t as f32)
}

/// Linearly interpolates two f32 values.
pub fn interpolate_linear_f32(first: f32, second: f32, t: f64) -> f32 {
    first + t as f32 * (second - first)
}

/// Linearly interpolates two f64 values.
pub fn interpolate_linear_f64(first: f64, second: f64, t: f64) -> f64 {
    first + t * (second - first)
}

/// Cubic spline interpolation for Vec3 values.
pub fn interpolate_cubic_spline_vec3(
    first_value: Vec3,
    first_tangent: Vec3,
    second_value: Vec3,
    second_tangent: Vec3,
    delta_time: f64,
    interpolation_time: f64,
) -> Vec3 {
    let delta_time = delta_time as f32;
    let interpolation_time = interpolation_time as f32;

    let t2 = interpolation_time * interpolation_time;
    let t3 = t2 * interpolation_time;

    let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
    let h10 = t3 - 2.0 * t2 + interpolation_time;
    let h01 = -2.0 * t3 + 3.0 * t2;
    let h11 = t3 - t2;

    (h00 * first_value)
        + (h10 * first_tangent * delta_time)
        + (h01 * second_value)
        + (h11 * second_tangent * delta_time)
}

/// Cubic spline interpolation for quaternions.
pub fn interpolate_cubic_spline_quat(
    first_value: Quat,
    first_tangent: Quat,
    mut second_value: Quat,
    mut second_tangent: Quat,
    delta_time: f64,
    interpolation_time: f64,
) -> Quat {
    // Convert time and interpolation factor into f32 (assuming Quat is f32-based)
    let delta_time = delta_time as f32;
    let t = interpolation_time as f32;

    // 1) Ensure the second quaternion is in the same "hemisphere" as the first.
    //    If they're opposite in sign, flip the second so interpolation takes the short path.
    if first_value.dot(second_value) < 0.0 {
        second_value = -second_value;
        second_tangent = -second_tangent;
    }

    // 2) Hermite basis functions
    let t2 = t * t;
    let t3 = t2 * t;
    let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
    let h10 = t3 - 2.0 * t2 + t;
    let h01 = -2.0 * t3 + 3.0 * t2;
    let h11 = t3 - t2;

    // 3) Weighted combination, including time-scaled tangents
    //    NOTE: Depending on your quaternion library, `quat * scalar` usually does
    //          component-wise scaling, not quaternion * quaternion multiplication.
    let blended = first_value * h00
        + (first_tangent * (h10 * delta_time))
        + (second_value * h01)
        + (second_tangent * (h11 * delta_time));

    // 4) Re-normalize the result so it's still a valid rotation quaternion
    blended.normalize()
}

/// Cubic spline interpolation for f32 values.
pub fn interpolate_cubic_spline_f32(
    first_value: f32,
    first_tangent: f32,
    second_value: f32,
    second_tangent: f32,
    delta_time: f64,
    interpolation_time: f64,
) -> f32 {
    let delta_time = delta_time as f32;
    let interpolation_time = interpolation_time as f32;

    let t2 = interpolation_time * interpolation_time;
    let t3 = t2 * interpolation_time;
    let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
    let h10 = t3 - 2.0 * t2 + interpolation_time;
    let h01 = -2.0 * t3 + 3.0 * t2;
    let h11 = t3 - t2;

    (h00 * first_value)
        + (h10 * first_tangent * delta_time)
        + (h01 * second_value)
        + (h11 * second_tangent * delta_time)
}

/// Cubic spline interpolation for f64 values.
pub fn interpolate_cubic_spline_f64(
    first_value: f64,
    first_tangent: f64,
    second_value: f64,
    second_tangent: f64,
    delta_time: f64,
    interpolation_time: f64,
) -> f64 {
    let t2 = interpolation_time * interpolation_time;
    let t3 = t2 * interpolation_time;
    let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
    let h10 = t3 - 2.0 * t2 + interpolation_time;
    let h01 = -2.0 * t3 + 3.0 * t2;
    let h11 = t3 - t2;

    (h00 * first_value)
        + (h10 * first_tangent * delta_time)
        + (h01 * second_value)
        + (h11 * second_tangent * delta_time)
}
