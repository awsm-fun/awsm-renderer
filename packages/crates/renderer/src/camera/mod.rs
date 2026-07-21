//! THE camera module — the one way to drive the renderer's camera, plus the
//! view/projection helpers everything composes from.
//!
//! # Design (docs/plans history: camera consolidation)
//!
//! A camera is two independent halves:
//!
//! - a **view**: a `Mat4` — yours to build however you like (an orbit
//!   controller, a scene node's transform via [`view_from_world`], your own
//!   game/physics/VR code via `Mat4::look_at_rh`, …). A caller owning its view
//!   matrix is FIRST CLASS, not a fallback.
//! - **[`CameraParams`]**: projection kind + clip planes + depth of field —
//!   pure parameters, no matrices.
//!
//! [`AwsmRenderer::set_camera`] joins the two. The renderer supplies the two
//! things a caller most easily gets wrong:
//!
//! - the DEPTH CONVENTION, from its own `features.depth()`, so the projection
//!   and the `reverse_z` flag are structurally incapable of disagreeing (a
//!   mismatch inverts every depth test, and the symptom — geometry occluded
//!   backwards — points nowhere near the camera);
//! - the ASPECT RATIO, from the live surface at render resolution, so it is
//!   right after a resize instead of pinned to startup dimensions.
//!
//! It also derives the camera world position from the view matrix, so the two
//! cannot disagree either. [`CameraMatrices`] is the resulting snapshot — read
//! it back via [`AwsmRenderer::camera_matrices`] (pick rays, gizmos,
//! screen-space math). Building one by hand is only for renderer-less math
//! (native tests): [`CameraMatrices::new`] takes the convention and aspect
//! explicitly.
//!
//! Switching perspective ↔ orthographic touches ONLY [`CameraParams`] — the
//! view is untouched by construction.

mod buffer;
mod store;

pub use buffer::CameraBuffer;
pub use store::{CameraKey, CameraParams, CameraProjectionParams, Cameras};

use awsm_renderer_core::error::AwsmCoreError;
use glam::{Mat4, Vec3};
use thiserror::Error;

use crate::depth_convention::DepthConvention;
use crate::AwsmRenderer;

impl AwsmRenderer {
    /// THE way to set the active camera — call once per frame (or whenever the
    /// camera changes).
    ///
    /// `view` is the world→view matrix (build it however you like — see the
    /// module docs); `params` is everything else. The renderer supplies the
    /// depth convention (from `features.depth()`) and the live surface aspect,
    /// and derives the camera position from `view`, so none of the three can
    /// be wrong.
    pub fn set_camera(&mut self, view: Mat4, params: CameraParams) -> Result<()> {
        let aspect = self.surface_aspect()?;
        let matrices = CameraMatrices::new(self.features.depth(), view, params, aspect);
        self.update_camera(matrices)
    }

    /// The last-set camera snapshot (matrices + derived data), or `None`
    /// before the first [`Self::set_camera`]. This is the read side for pick
    /// rays, gizmo math, and anything else that consumes the current camera.
    pub fn camera_matrices(&self) -> Option<&CameraMatrices> {
        self.camera.last_matrices.as_ref()
    }

    /// Live surface aspect (width / height) at RENDER resolution. Guards a
    /// zero-height surface so a hidden/collapsed canvas cannot produce NaN
    /// matrices.
    fn surface_aspect(&self) -> Result<f32> {
        let (w, h) = self.gpu.current_context_texture_size()?;
        let w = crate::size::scale_extent(w, self.render_scale).max(1);
        let h = crate::size::scale_extent(h, self.render_scale).max(1);
        Ok(w as f32 / h as f32)
    }

    /// Upload `camera_matrices` to the GPU camera buffer. In-crate only —
    /// external callers go through [`Self::set_camera`], which is what makes a
    /// projection/convention mismatch unrepresentable from outside.
    pub(crate) fn update_camera(&mut self, camera_matrices: CameraMatrices) -> Result<()> {
        // Render resolution (scaled), not swap-chain: the camera uniform's
        // viewport feeds shader screen-space math, which runs at render res.
        let (surface_w, surface_h) = self.gpu.current_context_texture_size()?;
        let current_width = crate::size::scale_extent(surface_w, self.render_scale);
        let current_height = crate::size::scale_extent(surface_h, self.render_scale);

        self.camera.update(
            camera_matrices,
            &self.render_textures,
            current_width as f32,
            current_height as f32,
            self.features.depth(),
        )?;

        Ok(())
    }
}

/// View matrix for a camera NODE from its world transform, using the glTF
/// camera convention: the camera looks down its local **-Z** with **+Y** up.
///
/// Robust to the ways a scene-graph transform can be degenerate — scaled axes
/// are normalized, a zero forward/up falls back to `-Z`/`+Y`, and a forward
/// collinear with up picks a perpendicular up — so a badly-authored node
/// yields a usable view instead of NaNs.
pub fn view_from_world(world: Mat4) -> Mat4 {
    let pos = world.w_axis.truncate();
    let mut forward = (-world.z_axis.truncate()).normalize_or_zero();
    let mut up = world.y_axis.truncate().normalize_or_zero();
    if forward == Vec3::ZERO {
        forward = Vec3::NEG_Z;
    }
    if up == Vec3::ZERO {
        up = Vec3::Y;
    }
    // look_at needs forward ∦ up; a camera rolled to look straight along its
    // own up axis would otherwise produce a NaN basis.
    if forward.cross(up).length_squared() < 1e-12 {
        up = if forward.x.abs() < 0.9 {
            Vec3::X
        } else {
            Vec3::Y
        };
        if forward.cross(up).length_squared() < 1e-12 {
            up = Vec3::Z;
        }
    }
    Mat4::look_at_rh(pos, pos + forward, up)
}

/// Camera matrices and parameters — the SNAPSHOT of what the renderer renders
/// with, produced by [`AwsmRenderer::set_camera`] and read back via
/// [`AwsmRenderer::camera_matrices`].
///
/// `#[non_exhaustive]`: construct via [`Self::new`] (or `set_camera`), never a
/// struct literal — that is what keeps the projection, `reverse_z`, `near`/
/// `far` and `position_world` mutually consistent by construction.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct CameraMatrices {
    pub view: Mat4,
    pub projection: Mat4,
    /// Camera world position — derived from `view` at construction.
    pub position_world: Vec3,
    /// Focus distance for depth of field (world units).
    pub focus_distance: f32,
    /// Aperture f-stop for depth of field. Lower = more blur.
    pub aperture: f32,
    /// Depth convention the `projection` was built under (003). Consumers that
    /// derive convention-dependent data from these matrices (frustum-plane
    /// extraction, near/far recovery) read this instead of guessing from the
    /// matrix. MUST match the renderer's `features.reverse_z` — guaranteed
    /// when built through [`AwsmRenderer::set_camera`].
    pub reverse_z: bool,
    /// Near clip plane in world units — carried EXPLICITLY (003 stage 5) so
    /// froxel z-slicing / cascade fitting never recover it from the matrix
    /// (that algebra breaks under reverse-Z and outright fails under
    /// infinite-far, where `proj[2][2] == 0`).
    pub near: f32,
    /// Far clip plane in world units. May be `f32::INFINITY` under the
    /// stage-8 infinite-far projection — consumers that need a finite bound
    /// (froxel slicing, cascade fitting) clamp it themselves.
    pub far: f32,
}

impl CameraMatrices {
    /// Build camera matrices from a view matrix + [`CameraParams`], under an
    /// EXPLICIT depth convention and aspect ratio.
    ///
    /// This exists for renderer-less math (native tests, offline tooling).
    /// With a renderer in hand, use [`AwsmRenderer::set_camera`] — it supplies
    /// `convention` and `aspect` from the renderer itself so they cannot
    /// drift.
    ///
    /// This is the ONE place `CameraProjectionParams` becomes a matrix: the
    /// orthographic arm derives its half-width from `aspect` (which every
    /// caller previously re-derived by hand), and reverse-Z ortho is the
    /// near/far swap inside [`DepthConvention::orthographic`]. The camera
    /// world position is derived from `view`, so it cannot disagree with it.
    pub fn new(convention: DepthConvention, view: Mat4, params: CameraParams, aspect: f32) -> Self {
        let projection = match params.projection {
            CameraProjectionParams::Perspective { fov_y_rad } => {
                convention.perspective(fov_y_rad, aspect, params.near, params.far)
            }
            CameraProjectionParams::Orthographic { half_height } => {
                let half_width = half_height * aspect;
                convention.orthographic(
                    -half_width,
                    half_width,
                    -half_height,
                    half_height,
                    params.near,
                    params.far,
                )
            }
        };
        Self {
            view,
            projection,
            // The view is world→view; its inverse's translation is the camera's
            // world position. Deriving it here (instead of taking it as an
            // argument) removes the possibility of the two disagreeing.
            position_world: view.inverse().w_axis.truncate(),
            focus_distance: params.focus_distance,
            aperture: params.aperture,
            reverse_z: convention.reverse_z,
            near: params.near,
            far: params.far,
        }
    }

    /// Returns the combined view-projection matrix.
    pub fn view_projection(&self) -> Mat4 {
        self.projection * self.view
    }

    /// Returns the inverse view-projection matrix.
    pub fn inv_view_projection(&self) -> Mat4 {
        self.view_projection().inverse()
    }

    /// Returns true if the projection is orthographic.
    pub fn is_orthographic(&self) -> bool {
        // Orthographic projections have m[3][3] = 1.0 (no perspective divide)
        // Perspective projections have m[3][3] = 0.0 (w' = -z for perspective divide)
        // This is the definitive check for standard projection matrices.
        self.projection.w_axis.w.abs() > 0.5
    }
}

/// Result type for camera operations.
type Result<T> = std::result::Result<T, AwsmCameraError>;

/// Camera-related errors.
#[derive(Error, Debug)]
pub enum AwsmCameraError {
    #[error("[camera] {0:?}")]
    Core(#[from] AwsmCoreError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec4;

    const BOTH: [DepthConvention; 2] = [
        DepthConvention { reverse_z: false },
        DepthConvention { reverse_z: true },
    ];

    /// Every invariant test sweeps BOTH depth conventions — two earlier tests
    /// on this branch passed against broken code because they only covered
    /// one, the same blind spot as the bugs they guarded.
    #[test]
    fn position_is_derived_from_the_view_matrix() {
        let eye = Vec3::new(3.0, -2.0, 7.5);
        let view = Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Y);
        for convention in BOTH {
            let m = CameraMatrices::new(
                convention,
                view,
                CameraParams::perspective(1.0, 0.1, 100.0),
                1.6,
            );
            assert!(
                m.position_world.abs_diff_eq(eye, 1e-4),
                "reverse_z={}: derived position {:?} != eye {eye:?}",
                convention.reverse_z,
                m.position_world
            );
            assert_eq!(m.reverse_z, convention.reverse_z);
            assert_eq!((m.near, m.far), (0.1, 100.0));
        }
    }

    /// The ortho half-WIDTH comes from aspect — the derivation that used to be
    /// hand-written in three places. A point at x = half_height·aspect must
    /// land exactly on the NDC x=1 edge under both conventions.
    #[test]
    fn ortho_half_width_follows_aspect() {
        let (half_height, aspect) = (4.0_f32, 2.5_f32);
        for convention in BOTH {
            let m = CameraMatrices::new(
                convention,
                Mat4::IDENTITY,
                CameraParams::orthographic(half_height, 0.1, 100.0),
                aspect,
            );
            let clip = m.projection * Vec4::new(half_height * aspect, half_height, -1.0, 1.0);
            let ndc = clip / clip.w;
            assert!(
                (ndc.x - 1.0).abs() < 1e-5 && (ndc.y - 1.0).abs() < 1e-5,
                "reverse_z={}: ({}, {}) should be the top-right edge",
                convention.reverse_z,
                ndc.x,
                ndc.y
            );
            assert!(m.is_orthographic());
        }
    }

    /// Reverse-Z ortho is the near/far SWAP — through the full params path,
    /// not just `DepthConvention` in isolation (this is the regression that
    /// hand-rolled `Mat4::orthographic_rh` reintroduces).
    #[test]
    fn ortho_depth_follows_the_convention() {
        let ndc_z = |m: &CameraMatrices, view_z: f32| {
            let clip = m.projection * Vec4::new(0.0, 0.0, view_z, 1.0);
            clip.z / clip.w
        };
        for convention in BOTH {
            let m = CameraMatrices::new(
                convention,
                Mat4::IDENTITY,
                CameraParams::orthographic(1.0, 0.5, 100.0),
                1.0,
            );
            let near_ndc = ndc_z(&m, -0.5);
            let far_ndc = ndc_z(&m, -100.0);
            if convention.reverse_z {
                assert!((near_ndc - 1.0).abs() < 1e-5 && far_ndc.abs() < 1e-5);
            } else {
                assert!(near_ndc.abs() < 1e-5 && (far_ndc - 1.0).abs() < 1e-5);
            }
        }
    }

    #[test]
    fn view_from_world_matches_look_at_for_a_plain_camera_node() {
        // A camera at (0, 2, 5) yawed 90° left: world -Z (its look direction)
        // becomes world -X.
        let world = Mat4::from_translation(Vec3::new(0.0, 2.0, 5.0))
            * Mat4::from_rotation_y(std::f32::consts::FRAC_PI_2);
        let view = view_from_world(world);
        let expected =
            Mat4::look_at_rh(Vec3::new(0.0, 2.0, 5.0), Vec3::new(-1.0, 2.0, 5.0), Vec3::Y);
        assert!(view.abs_diff_eq(expected, 1e-5), "{view:?} != {expected:?}");
    }

    #[test]
    fn view_from_world_survives_scaled_and_degenerate_transforms() {
        // Non-uniform scale must not shear the view basis.
        let scaled = Mat4::from_translation(Vec3::new(1.0, 2.0, 3.0))
            * Mat4::from_scale(Vec3::new(3.0, 0.5, 2.0));
        let v = view_from_world(scaled);
        assert!(v.is_finite(), "scaled transform produced {v:?}");

        // Zero basis (a collapsed node) falls back to -Z/+Y instead of NaN.
        let zero = Mat4::from_scale(Vec3::ZERO);
        let v = view_from_world(zero);
        assert!(v.is_finite(), "zero transform produced {v:?}");

        // Forward collinear with up. A RIGID transform can never produce this
        // (rotations move both axes together), so build the degenerate matrix
        // by hand — bad authored data, a broken importer. Without the
        // perpendicular-up fallback, look_at's cross product is zero and the
        // whole basis goes NaN.
        let degenerate = Mat4::from_cols(
            Vec4::new(1.0, 0.0, 0.0, 0.0),
            Vec4::new(0.0, 1.0, 0.0, 0.0),  // up = +Y
            Vec4::new(0.0, -1.0, 0.0, 0.0), // forward = -z_axis = +Y too
            Vec4::new(0.0, 0.0, 0.0, 1.0),
        );
        let v = view_from_world(degenerate);
        assert!(v.is_finite(), "collinear forward/up produced {v:?}");
    }
}
