//! Per-camera parameter store.
//!
//! A lights-shaped slotmap of authorable camera parameters (projection, clip
//! planes, depth-of-field). This is the animation/editor-facing store the
//! `AnimationTarget::Camera` channel drives — it holds the *parameters*, not
//! the per-frame view/projection matrices (those live in
//! [`super::CameraMatrices`]). Mirrors the shape of [`crate::lights::Lights`].
//!
//! [`CameraParams`] is also the parameter half of the renderer's ONE
//! camera-setting entry point, [`crate::AwsmRenderer::set_camera`] — the store
//! and the live camera speak the same type by design.

use slotmap::{new_key_type, SlotMap};

new_key_type! {
    /// Opaque key for a camera in the [`Cameras`] store.
    pub struct CameraKey;
}

/// Projection parameters for a camera. Mirrors the two projection kinds the
/// renderer supports; `AnimationTarget::Camera { param: FovY }` only touches
/// the perspective arm (it's a no-op on an orthographic camera).
///
/// This is a *parameter* form — the matrix is only built where the depth
/// convention and live aspect ratio are both known
/// ([`super::CameraMatrices::new`]), so neither can drift.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CameraProjectionParams {
    /// Perspective projection driven by vertical field-of-view (radians).
    Perspective { fov_y_rad: f32 },
    /// Orthographic projection driven by half the view-volume height (world
    /// units) — the half-WIDTH follows from the live aspect ratio at matrix
    /// build, so there are no left/right/bottom/top values to transpose.
    Orthographic { half_height: f32 },
}

/// Authorable per-camera parameters: projection + clip planes + depth of
/// field. The parameter half of [`crate::AwsmRenderer::set_camera`] (the view
/// matrix is the other half), and the value type of the [`Cameras`] store.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraParams {
    /// Projection kind + its driving parameter.
    pub projection: CameraProjectionParams,
    /// Near clip plane (world units).
    pub near: f32,
    /// Far clip plane (world units). Carried even where the projection matrix
    /// ignores it (infinite-far reverse-Z) — froxel slicing / cascade fitting
    /// clamp against it.
    pub far: f32,
    /// Depth-of-field aperture (f-stop). Default
    /// [`Self::DEFAULT_APERTURE`].
    pub aperture: f32,
    /// Depth-of-field focus distance (world units). Default
    /// [`Self::DEFAULT_FOCUS_DISTANCE`].
    pub focus_distance: f32,
}

impl CameraParams {
    /// The one depth-of-field aperture default (f/5.6), shared by every
    /// constructor and consumer — the store, the editor free camera and the
    /// scene loader all used 5.6 already; the renderer's old matrix builder
    /// baking f/16 was the odd one out.
    pub const DEFAULT_APERTURE: f32 = 5.6;
    /// The one depth-of-field focus-distance default (10 m).
    pub const DEFAULT_FOCUS_DISTANCE: f32 = 10.0;

    /// Perspective camera parameters (`fov_y_rad` = vertical field of view in
    /// radians) with default depth of field.
    pub fn perspective(fov_y_rad: f32, near: f32, far: f32) -> Self {
        Self {
            projection: CameraProjectionParams::Perspective { fov_y_rad },
            near,
            far,
            aperture: Self::DEFAULT_APERTURE,
            focus_distance: Self::DEFAULT_FOCUS_DISTANCE,
        }
    }

    /// Orthographic camera parameters (`half_height` = half the view-volume
    /// height in world units; width follows the live aspect) with default
    /// depth of field.
    pub fn orthographic(half_height: f32, near: f32, far: f32) -> Self {
        Self {
            projection: CameraProjectionParams::Orthographic { half_height },
            near,
            far,
            aperture: Self::DEFAULT_APERTURE,
            focus_distance: Self::DEFAULT_FOCUS_DISTANCE,
        }
    }
}

/// A slotmap of [`CameraParams`], keyed by [`CameraKey`].
#[derive(Debug, Clone, Default)]
pub struct Cameras {
    store: SlotMap<CameraKey, CameraParams>,
}

impl Cameras {
    /// Creates an empty camera store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts a camera and returns its key.
    pub fn insert(&mut self, params: CameraParams) -> CameraKey {
        self.store.insert(params)
    }

    /// Removes a camera, returning its parameters if it existed.
    pub fn remove(&mut self, key: CameraKey) -> Option<CameraParams> {
        self.store.remove(key)
    }

    /// Returns a camera's parameters, or `None` if the key is unknown.
    pub fn get(&self, key: CameraKey) -> Option<&CameraParams> {
        self.store.get(key)
    }

    /// Returns true if the store contains the given key.
    pub fn contains(&self, key: CameraKey) -> bool {
        self.store.contains_key(key)
    }

    /// Mutates a camera in place. Returns true if the key existed (and `f`
    /// ran), false otherwise.
    pub fn update(&mut self, key: CameraKey, f: impl FnOnce(&mut CameraParams)) -> bool {
        if let Some(params) = self.store.get_mut(key) {
            f(params);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> CameraParams {
        CameraParams::perspective(1.0, 0.1, 100.0)
    }

    #[test]
    fn constructors_carry_the_one_dof_default() {
        let p = CameraParams::perspective(1.0, 0.1, 100.0);
        assert_eq!(p.aperture, CameraParams::DEFAULT_APERTURE);
        assert_eq!(p.focus_distance, CameraParams::DEFAULT_FOCUS_DISTANCE);
        let o = CameraParams::orthographic(4.0, 0.1, 100.0);
        assert_eq!(o.aperture, CameraParams::DEFAULT_APERTURE);
        assert_eq!(o.focus_distance, CameraParams::DEFAULT_FOCUS_DISTANCE);
        assert_eq!(
            o.projection,
            CameraProjectionParams::Orthographic { half_height: 4.0 }
        );
    }

    #[test]
    fn insert_get_round_trip() {
        let mut cameras = Cameras::new();
        let key = cameras.insert(params());
        assert!(cameras.contains(key));
        assert_eq!(cameras.get(key), Some(&params()));
    }

    #[test]
    fn update_mutates_and_reports_existence() {
        let mut cameras = Cameras::new();
        let key = cameras.insert(params());
        let ran = cameras.update(key, |p| p.near = 0.5);
        assert!(ran);
        assert_eq!(cameras.get(key).unwrap().near, 0.5);
    }

    #[test]
    fn update_returns_false_for_stale_key() {
        let mut cameras = Cameras::new();
        let key = cameras.insert(params());
        let removed = cameras.remove(key);
        assert_eq!(removed, Some(params()));
        // key is now stale
        let ran = cameras.update(key, |p| p.near = 0.5);
        assert!(!ran);
        assert!(!cameras.contains(key));
        assert!(cameras.get(key).is_none());
    }

    #[test]
    fn remove_returns_none_for_stale_key() {
        let mut cameras = Cameras::new();
        let key = cameras.insert(params());
        assert!(cameras.remove(key).is_some());
        assert!(cameras.remove(key).is_none());
    }
}
