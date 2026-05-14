//! Spawn shapes + forces for particles.

use glam::Vec3;

#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SpawnShape {
    Point,
    Sphere { radius: f32 },
    Cone { angle_radians: f32, direction: [f32; 3] },
}

impl SpawnShape {
    /// Sample a position offset + initial velocity direction for a new particle.
    pub fn sample(&self, rng: &mut impl FnMut() -> f32) -> (Vec3, Vec3) {
        match self {
            SpawnShape::Point => (Vec3::ZERO, Vec3::Y),
            SpawnShape::Sphere { radius } => {
                let dir = sample_unit_sphere(rng);
                (dir * *radius * rng(), dir)
            }
            SpawnShape::Cone { angle_radians, direction } => {
                let dir = Vec3::from_array(*direction).normalize_or_zero();
                let dir = if dir.length_squared() < 1.0e-6 { Vec3::Y } else { dir };
                let cone_dir = sample_cone(dir, *angle_radians, rng);
                (Vec3::ZERO, cone_dir)
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Force {
    Gravity { acceleration: [f32; 3] },
    LinearDrag { coefficient: f32 },
}

fn sample_unit_sphere(rng: &mut impl FnMut() -> f32) -> Vec3 {
    let u1 = rng();
    let u2 = rng();
    let z = 1.0 - 2.0 * u1;
    let r = (1.0 - z * z).max(0.0).sqrt();
    let phi = std::f32::consts::TAU * u2;
    Vec3::new(r * phi.cos(), r * phi.sin(), z)
}

fn sample_cone(dir: Vec3, half_angle: f32, rng: &mut impl FnMut() -> f32) -> Vec3 {
    let cos_a = half_angle.cos();
    let u1 = rng();
    let u2 = rng();
    let cos_t = 1.0 - u1 * (1.0 - cos_a);
    let sin_t = (1.0 - cos_t * cos_t).max(0.0).sqrt();
    let phi = std::f32::consts::TAU * u2;
    let local = Vec3::new(sin_t * phi.cos(), sin_t * phi.sin(), cos_t);
    // Rotate local Z to dir
    let z = Vec3::Z;
    if dir.dot(z) > 0.9999 {
        return local;
    }
    if dir.dot(z) < -0.9999 {
        return -local;
    }
    let axis = z.cross(dir).normalize();
    let angle = z.dot(dir).clamp(-1.0, 1.0).acos();
    glam::Quat::from_axis_angle(axis, angle).mul_vec3(local)
}
