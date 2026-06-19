//! `sweep_along_curve` generator.

use std::f32::consts::TAU;

use awsm_curves::{Curve3, FrameSequence};
use glam::{Vec2, Vec3};

use crate::mesh_data::MeshData;

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum CrossSection {
    /// Flat ribbon of given width, optionally offset in `Y`.
    Strip { width: f32, y_offset: f32 },
    /// Tube of given radius with `radial_segments` around the curve.
    Tube { radius: f32, radial_segments: u32 },
    /// Wall (open-bottom rectangle along the curve).
    Wall { width: f32, height: f32 },
    /// Arbitrary 2D profile (XY).
    Profile { points: Vec<[f32; 2]>, closed: bool },
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum UvMode {
    StretchOnce,
    RepeatByLength {
        u_repeat: f32,
        v_repeat_per_unit: f32,
    },
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SweepOpts {
    pub samples: u32,
    pub uv_mode: UvMode,
    pub up_hint: [f32; 3],
}

impl Default for SweepOpts {
    fn default() -> Self {
        Self {
            samples: 64,
            uv_mode: UvMode::StretchOnce,
            up_hint: [0.0, 1.0, 0.0],
        }
    }
}

/// Sweep a cross-section along a curve, producing a `MeshData`.
pub fn sweep_along_curve<C: Curve3 + ?Sized>(
    curve: &C,
    cross_section: &CrossSection,
    opts: &SweepOpts,
) -> MeshData {
    let samples = opts.samples.max(2) as usize;
    let frames = FrameSequence::parallel_transport(curve, samples, Vec3::from_array(opts.up_hint));

    let profile_points = cross_section_points(cross_section);
    let profile_closed = matches!(cross_section, CrossSection::Tube { .. })
        || matches!(cross_section, CrossSection::Wall { .. })
        || matches!(cross_section, CrossSection::Profile { closed: true, .. });
    let n_profile = profile_points.len();

    let mut positions = Vec::with_capacity(samples * n_profile);
    let mut normals = Vec::with_capacity(samples * n_profile);
    let mut uvs = Vec::with_capacity(samples * n_profile);
    let mut indices = Vec::new();

    // Pre-compute v values along length
    let total_len = curve.total_length(samples);
    let mut accumulated_v = vec![0.0_f32; samples];
    {
        let mut prev = frames.frames[0].position;
        let mut acc = 0.0_f32;
        for (i, f) in frames.frames.iter().enumerate() {
            if i > 0 {
                acc += (f.position - prev).length();
            }
            accumulated_v[i] = acc;
            prev = f.position;
        }
    }

    for (i, frame) in frames.frames.iter().enumerate() {
        let v = match &opts.uv_mode {
            UvMode::StretchOnce => i as f32 / (samples - 1) as f32,
            UvMode::RepeatByLength {
                v_repeat_per_unit, ..
            } => accumulated_v[i] * v_repeat_per_unit,
        };
        for (j, p) in profile_points.iter().enumerate() {
            let local = frame.binormal * p.x + frame.normal * p.y;
            positions.push((frame.position + local).to_array());
            normals.push(local.normalize_or_zero().to_array());
            let u = match &opts.uv_mode {
                UvMode::StretchOnce => j as f32 / (n_profile - 1).max(1) as f32,
                UvMode::RepeatByLength { u_repeat, .. } => {
                    (j as f32 / (n_profile - 1).max(1) as f32) * u_repeat
                }
            };
            uvs.push([u, v]);
        }
    }

    let edge_count = if profile_closed {
        n_profile
    } else {
        n_profile - 1
    };
    for i in 0..(samples - 1) {
        for j in 0..edge_count {
            let a = (i * n_profile + j) as u32;
            let b = (i * n_profile + ((j + 1) % n_profile)) as u32;
            let c = ((i + 1) * n_profile + ((j + 1) % n_profile)) as u32;
            let d = ((i + 1) * n_profile + j) as u32;
            indices.extend_from_slice(&[a, b, c, a, c, d]);
        }
    }

    let mut mesh = MeshData {
        positions,
        normals: Some(normals),
        uvs: vec![uvs],
        colors: None,
        indices,
    };
    mesh.compute_vertex_normals();
    let _ = total_len; // present for potential future per-segment UV math
    mesh
}

fn cross_section_points(cs: &CrossSection) -> Vec<Vec2> {
    match cs {
        CrossSection::Strip { width, y_offset } => {
            let h = width * 0.5;
            vec![Vec2::new(-h, *y_offset), Vec2::new(h, *y_offset)]
        }
        CrossSection::Tube {
            radius,
            radial_segments,
        } => {
            let n = (*radial_segments).max(3) as usize;
            (0..n)
                .map(|i| {
                    let t = i as f32 / n as f32 * TAU;
                    Vec2::new(t.cos() * radius, t.sin() * radius)
                })
                .collect()
        }
        CrossSection::Wall { width, height } => {
            let hw = width * 0.5;
            vec![
                Vec2::new(-hw, 0.0),
                Vec2::new(-hw, *height),
                Vec2::new(hw, *height),
                Vec2::new(hw, 0.0),
            ]
        }
        CrossSection::Profile { points, .. } => {
            points.iter().map(|p| Vec2::new(p[0], p[1])).collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use awsm_curves::CatmullRomCurve;

    #[test]
    fn sweep_tube_has_expected_vertex_count() {
        let curve = CatmullRomCurve::new(vec![Vec3::ZERO, Vec3::X * 10.0], false);
        let cs = CrossSection::Tube {
            radius: 0.5,
            radial_segments: 8,
        };
        let opts = SweepOpts {
            samples: 16,
            ..Default::default()
        };
        let m = sweep_along_curve(&curve, &cs, &opts);
        assert_eq!(m.positions.len(), 16 * 8);
        // every quad becomes 2 triangles = 6 indices
        assert_eq!(m.indices.len(), (16 - 1) * 8 * 6);
    }
}
