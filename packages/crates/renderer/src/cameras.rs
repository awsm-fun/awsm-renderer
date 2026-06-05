//! Per-camera parameter store.
//!
//! A lights-shaped slotmap of authorable camera parameters (projection, clip
//! planes, depth-of-field). This is the animation/editor-facing store the
//! `AnimationTarget::Camera` channel drives — it holds the *parameters*, not
//! the per-frame view/projection matrices (those live in
//! [`crate::camera`]). Mirrors the shape of [`crate::lights::Lights`].

use slotmap::{new_key_type, SlotMap};

new_key_type! {
    /// Opaque key for a camera in the [`Cameras`] store.
    pub struct CameraKey;
}

/// Projection parameters for a camera. Mirrors the two projection kinds the
/// renderer supports; `AnimationTarget::Camera { param: FovY }` only touches
/// the perspective arm (it's a no-op on an orthographic camera).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CameraProjectionParams {
    /// Perspective projection driven by vertical field-of-view (radians).
    Perspective { fov_y_rad: f32 },
    /// Orthographic projection driven by half the view-volume height.
    Orthographic { half_height: f32 },
}

/// Authorable per-camera parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraParams {
    /// Projection kind + its driving parameter.
    pub projection: CameraProjectionParams,
    /// Near clip plane.
    pub near: f32,
    /// Far clip plane.
    pub far: f32,
    /// Depth-of-field aperture (f-stop). Mirrors `CameraMatrices.aperture`;
    /// default `5.6`.
    pub aperture: f32,
    /// Depth-of-field focus distance. Default `10.0`.
    pub focus_distance: f32,
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
        CameraParams {
            projection: CameraProjectionParams::Perspective { fov_y_rad: 1.0 },
            near: 0.1,
            far: 100.0,
            aperture: 5.6,
            focus_distance: 10.0,
        }
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
