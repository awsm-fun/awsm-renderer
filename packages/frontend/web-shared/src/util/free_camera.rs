//! Orbit + pan + zoom free camera.
//!
//! Lifted from the game-editor's viewport camera and shared across
//! every frontend app that wants a "look around the scene" mode.
//!
//! # What this is (and is not)
//!
//! `FreeCamera` is a VIEW controller plus authored projection *parameters* —
//! it owns the orbit pose (yaw/pitch/radius/look-at), the projection mode
//! toggle, and the auto near/far framing policy. It builds **no matrices
//! beyond the view**: feed [`FreeCamera::view`] + [`FreeCamera::params`] to
//! `AwsmRenderer::set_camera`, which owns the depth convention and the live
//! surface aspect. That is what lets perspective ↔ orthographic switch without
//! touching the view, and why there is no aspect plumbing here at all.
//!
//! # Coordinate convention
//!
//! Right-handed, **Y-up**, **-Z-forward** (matches `glam::Mat4::look_at_rh`
//! and the gltf spec). The yaw/pitch convention mirrors that:
//!
//! * `yaw == 0` looks down `-Z` (camera at `+Z`, target at origin)
//! * `yaw == π/2` looks down `-X`
//! * `pitch > 0` raises the camera above the horizon (looks down)
//!
//! # Aperture / focus distance
//!
//! Carried on [`FreeCamera::params`] for the renderer's DOF pass; they're
//! caller-tuned per app. Defaults are `aperture = 5.6` and
//! `focus_distance = 10.0` (the shared `CameraParams` defaults). Override via
//! [`FreeCamera::set_aperture`] / [`FreeCamera::set_focus_distance`].

use awsm_renderer::bounds::Aabb;
use awsm_renderer::camera::{CameraParams, CameraProjectionParams};
use awsm_renderer::depth_convention::DepthConvention;
use glam::{Mat4, Vec3};
use serde::{Deserialize, Serialize};

/// Which projection the viewport is currently using.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionMode {
    Perspective,
    Orthographic,
}

impl ProjectionMode {
    pub const ALL: [Self; 2] = [Self::Perspective, Self::Orthographic];

    pub fn label(self) -> &'static str {
        match self {
            Self::Perspective => "Perspective",
            Self::Orthographic => "Orthographic",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Perspective => "perspective",
            Self::Orthographic => "orthographic",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|m| m.id() == id)
    }
}

/// Combined orbit + dual-projection free camera. The projection params for
/// both modes are plain fields (`fov_y`, `half_height`), so toggling `mode`
/// is a cheap enum flip that never disturbs the orbit pose.
#[derive(Debug, Clone)]
pub struct FreeCamera {
    view: CameraView,
    mode: ProjectionMode,
    /// Perspective vertical field-of-view (radians).
    fov_y: f32,
    /// Orthographic half view-volume height (world units) — the zoom state of
    /// the ortho mode (wheel scales it). Half-WIDTH is the renderer's business
    /// (it follows the live aspect at matrix build).
    half_height: f32,
    aabb: Aabb,
    margin: f32,
    aperture: f32,
    focus_distance: f32,
    /// Session-only user override of the clip planes as `(near, far)`. `None`
    /// (default) keeps the AUTO behaviour — near/far are re-derived from the
    /// orbit distance against the framing AABB on every [`Self::params`] call.
    /// `Some` pins both planes.
    clip_override: Option<(f32, f32)>,
    /// Depth convention — used ONLY for the auto near/far POLICY
    /// (`auto_clip_planes`: reverse-Z affords a tighter near plane than
    /// forward-Z's precision-bounded ratio). Never builds a matrix here; the
    /// renderer supplies the convention to the actual projection.
    convention: DepthConvention,
}

impl FreeCamera {
    /// Construct a camera framed around an axis-aligned bounding box.
    /// `convention` must be the renderer's `features.depth()` — it drives the
    /// auto clip-plane policy (see the field docs), nothing else.
    pub fn new_aabb(aabb: Aabb, margin: f32, convention: DepthConvention) -> Self {
        let view = CameraView::new_aabb(&aabb, margin);
        Self {
            view,
            mode: ProjectionMode::Perspective,
            fov_y: 45.0_f32.to_radians(),
            half_height: ortho_half_height(&aabb, margin),
            aabb,
            margin,
            aperture: CameraParams::DEFAULT_APERTURE,
            focus_distance: CameraParams::DEFAULT_FOCUS_DISTANCE,
            clip_override: None,
            convention,
        }
    }

    /// Convenience: a default cube AABB sized for general
    /// "what's at origin" scenes. 80×80 cube → orbit radius ~76 m,
    /// which sits comfortably outside the largest current game arena
    /// (jetpack-duel's 18 m-radius dome) without being so far that
    /// smaller scenes (pole-balance) look like specks. Tunable per-
    /// game via `FreeCamera::frame_aabb`.
    pub fn new_default_cube(convention: DepthConvention) -> Self {
        Self::new_aabb(Aabb::new_cube(40.0, 40.0), 1.1, convention)
    }

    /// The world→view matrix for the current orbit pose — the first half of
    /// `AwsmRenderer::set_camera`'s arguments.
    pub fn view(&self) -> Mat4 {
        self.view.get_view_matrix()
    }

    /// The camera parameters (projection + clip planes + DoF) — the second
    /// half of `AwsmRenderer::set_camera`'s arguments. Clip planes are the
    /// auto framing policy unless pinned via [`Self::set_clip_override`].
    pub fn params(&self) -> CameraParams {
        let (near, far) = self.clip_override.unwrap_or_else(|| {
            auto_clip_planes(
                &self.view,
                &self.aabb,
                self.margin,
                self.convention.reverse_z,
            )
        });
        let projection = match self.mode {
            ProjectionMode::Perspective => CameraProjectionParams::Perspective {
                fov_y_rad: self.fov_y,
            },
            ProjectionMode::Orthographic => CameraProjectionParams::Orthographic {
                half_height: self.half_height,
            },
        };
        CameraParams {
            projection,
            near,
            far,
            aperture: self.aperture,
            focus_distance: self.focus_distance,
        }
    }

    pub fn set_aperture(&mut self, aperture: f32) {
        self.aperture = aperture;
    }

    /// Pin (`Some((near, far))`) or release (`None` → auto) the clip planes.
    /// Values are sanitised: near is clamped to a positive minimum and far to
    /// beyond near, so a half-typed UI value can't produce a degenerate
    /// projection.
    pub fn set_clip_override(&mut self, clip: Option<(f32, f32)>) {
        self.clip_override = clip.map(|(near, far)| {
            let near = near.max(1e-4);
            let far = far.max(near * 1.001);
            (near, far)
        });
    }

    pub fn set_focus_distance(&mut self, focus_distance: f32) {
        self.focus_distance = focus_distance;
    }

    pub fn projection_mode(&self) -> ProjectionMode {
        self.mode
    }

    /// Switch projection without disturbing the orbit pose — a pure enum flip
    /// (the ortho zoom state persists across switches).
    pub fn set_projection_mode(&mut self, mode: ProjectionMode) {
        self.mode = mode;
    }

    /// Snap the orbit to an explicit yaw/pitch (radians), preserving the current
    /// look-at point + radius. Used by the nav-cube axis-snap (and any external/
    /// MCP camera drive). Convention: `yaw == 0` looks down `-Z`, `yaw == π/2`
    /// looks down `-X`, `pitch > 0` raises the camera (looks down).
    pub fn snap_to(&mut self, yaw: f32, pitch: f32) {
        self.view = CameraView::new(yaw, pitch, self.view.look_at, self.view.radius);
    }

    /// Set the full orbit pose (yaw/pitch radians, look-at point, radius). The
    /// MCP `SetCameraOrbit` entry point — lets a driver compose an arbitrary view
    /// (3/4 front, orbit-around-subject, …).
    pub fn set_orbit(&mut self, yaw: f32, pitch: f32, radius: f32, look_at: Vec3) {
        self.view = CameraView::new(yaw, pitch, look_at, radius);
    }

    /// Set the perspective vertical field-of-view (radians).
    pub fn set_fov_y(&mut self, fov_y: f32) {
        self.fov_y = fov_y;
    }

    /// Re-frame the orbit around an explicit AABB with `margin` (1.0 = tight).
    /// The MCP `FrameNode` entry point — fits a chosen subject in view.
    pub fn frame_aabb(&mut self, aabb: Aabb, margin: f32) {
        self.aabb = aabb;
        self.margin = margin;
        self.view = CameraView::new_aabb(&self.aabb, self.margin);
        // `CameraView::new_aabb` sets the orbit distance to `bounding_radius *
        // margin`, which IGNORES the perspective FOV — fine for the nominal
        // default-cube framing, but for an explicit "frame THIS node" it placed the
        // camera far inside the ≥ r/sin(fov/2) distance a real fit needs, so the
        // subject overflowed the frame as an extreme close-up (the P2 bug). Here we
        // know the live FOV, so override the orbit distance to actually enclose the
        // bounding sphere (+ `margin` breathing room). The `.max(..)` is a defensive
        // floor so the camera never lands inside the bounds for an odd margin/FOV.
        let bounding_radius = self.aabb.size().length() * 0.5;
        let half_fov = (self.fov_y * 0.5).max(0.01);
        let fit_distance = bounding_radius / half_fov.sin();
        self.view
            .set_radius((fit_distance * margin).max(bounding_radius * 1.05));
        // Re-seat the ortho zoom on the new subject so a mode switch after
        // framing shows the same subject, not the previous zoom level.
        self.half_height = ortho_half_height(&self.aabb, self.margin);
    }

    /// Reset the orbit to the default framing (the `new_default_cube` pose) —
    /// look-at back at the origin, default yaw/pitch + radius — preserving the
    /// current projection mode. Backs the "Reset View" action.
    pub fn reset_default(&mut self) {
        self.aabb = Aabb::new_cube(40.0, 40.0);
        self.margin = 1.1;
        self.view = CameraView::new_aabb(&self.aabb, self.margin);
        self.half_height = ortho_half_height(&self.aabb, self.margin);
    }

    pub fn on_pointer_down(&mut self) {
        self.view.on_pointer_down();
    }

    pub fn on_pointer_move(&mut self, x: i32, y: i32, is_panning: bool) {
        self.view.on_pointer_move(x as f32, y as f32, is_panning);
    }

    pub fn on_pointer_up(&mut self) {
        self.view.on_pointer_up();
    }

    pub fn on_wheel(&mut self, delta: f64) {
        self.view.on_wheel(delta as f32);
        // Ortho zoom tracks the wheel in BOTH modes so a mid-session mode
        // switch lands at a zoom level consistent with how far the user has
        // wheeled, not at the stale level from the last time ortho was active.
        self.half_height = (self.half_height * (1.0 + delta as f32 * 0.001)).max(0.001);
    }
}

/// Initial/reframed ortho half-height: the framing AABB's bounding-sphere
/// radius plus margin — mirrors the orbit-radius seed, so perspective and
/// ortho show a comparably-sized subject on first switch.
fn ortho_half_height(aabb: &Aabb, margin: f32) -> f32 {
    (aabb.size().length() * 0.5 * margin).max(0.001)
}

#[derive(Debug, Clone)]
pub struct CameraView {
    /// Point the camera orbits around.
    pub look_at: Vec3,
    /// Distance from look_at.
    pub radius: f32,
    pub sensitivity: f32,

    yaw: f32,
    pitch: f32,
    dragging: bool,
}

impl CameraView {
    pub fn new_aabb(aabb: &Aabb, margin: f32) -> Self {
        let center = aabb.center();
        let size = aabb.size();

        let bounding_radius = size.length() * 0.5;
        let radius = bounding_radius * margin;

        // Start head-on: looking from +Z axis, slightly above.
        // yaw: 0 = looking from +Z, π/2 = from +X, π = from -Z, 3π/2 = from -X
        let yaw = 0.0;
        // pitch: positive = camera above looking down
        let pitch = 0.3; // ~17° above horizon, looking down slightly

        Self::new(yaw, pitch, center, radius)
    }

    pub fn new_default(radius: f32) -> Self {
        // head-on view from -Z, X/Y at zero — useful for sanity-checking.
        let yaw: f32 = std::f32::consts::PI;
        let pitch: f32 = 0.0;
        let look_at = Vec3::ZERO;
        Self::new(yaw, pitch, look_at, radius)
    }

    pub fn new(yaw: f32, pitch: f32, look_at: Vec3, radius: f32) -> Self {
        Self {
            look_at,
            radius,
            yaw,
            pitch,
            dragging: false,
            sensitivity: 0.005,
        }
    }

    /// Override the orbit distance from `look_at` (used by `frame_aabb` to seat the
    /// camera at an FOV-aware fit distance). Floored at a small positive value.
    pub fn set_radius(&mut self, radius: f32) {
        self.radius = radius.max(0.01);
    }

    /// Right-handed look-at view matrix.
    pub fn get_view_matrix(&self) -> Mat4 {
        let cam_pos = self.get_position();
        Mat4::look_at_rh(cam_pos, self.look_at, Vec3::Y)
    }

    /// Current camera world position. Spherical → Cartesian.
    pub fn get_position(&self) -> Vec3 {
        let x = self.radius * self.pitch.cos() * self.yaw.sin();
        let y = self.radius * self.pitch.sin();
        let z = self.radius * self.pitch.cos() * self.yaw.cos();
        self.look_at + Vec3::new(x, y, z)
    }

    pub fn on_pointer_down(&mut self) {
        self.dragging = true;
    }

    pub fn on_pointer_move(&mut self, delta_x: f32, delta_y: f32, is_panning: bool) {
        if !self.dragging {
            return;
        }
        if is_panning {
            self.pan(delta_x, delta_y);
            return;
        }
        self.yaw -= delta_x * self.sensitivity;
        self.pitch -= delta_y * self.sensitivity;
        // Clamp pitch to just under ±90° to prevent flipping.
        let limit = std::f32::consts::FRAC_PI_2 - 0.0001;
        self.pitch = self.pitch.clamp(-limit, limit);
    }

    pub fn on_pointer_up(&mut self) {
        self.dragging = false;
    }

    pub fn on_wheel(&mut self, delta_y: f32) {
        let zoom_factor = 1.0 + delta_y * 0.001;
        self.radius = (self.radius * zoom_factor).max(0.1);
    }

    fn pan(&mut self, delta_x: f32, delta_y: f32) {
        let cam_pos = self.get_position();
        let forward = (self.look_at - cam_pos).normalize();
        let right = forward.cross(Vec3::Y).normalize();
        let up = right.cross(forward).normalize();

        let pan_scale = self.radius * self.sensitivity;
        let translation = right * (delta_x * pan_scale) - up * (delta_y * pan_scale);

        self.look_at += translation;
    }
}

/// Depth-precision-aware auto near/far for the orbit/free camera.
///
/// The old formula floored `near` at `0.01` while `far` tracked
/// `2·boundingRadius`, so any time the eye sat closer than ~2× the scene radius
/// (i.e. almost always) the far:near ratio blew past 100,000:1 and the
/// `Depth32Float` buffer z-fought badly; a too-small/stale AABB additionally
/// let `far` clip large scenes. This instead:
///
/// * makes `far` cover the whole scene from this viewpoint **and** stay a few ×
///   the orbit distance and the scene radius, so a stale or too-small AABB can
///   never clip near/far geometry, and
/// * derives `near` from `far` at a **bounded ~5000:1 ratio** (well within
///   float32 depth's comfort zone → no z-fighting), while capping it at half the
///   orbit distance so it can never clip the geometry being framed.
///
/// Shared by the perspective and orthographic modes (the ratio only matters
/// for perspective's non-linear depth, but a robust, clip-free `far` helps
/// both).
fn auto_clip_planes(view: &CameraView, aabb: &Aabb, margin: f32, reverse_z: bool) -> (f32, f32) {
    let radius = (aabb.size().length() * 0.5 * margin).max(1.0);
    let distance = view.get_position().distance(view.look_at);
    let far = ((distance + radius) * 2.0)
        .max(distance * 4.0)
        .max(radius * 4.0);
    let near = if reverse_z {
        // Reverse-Z (003 stage 9): float depth precision is near-uniform, so
        // the bounded ~5000:1 far:near ratio that forward-Z needed to avoid
        // z-fighting is unnecessary — near no longer scales with far. Keep a
        // small floor (clipping, not precision) and stay proportional to the
        // orbit distance so extreme close-ups don't clip.
        (distance * 0.002).clamp(0.05, (distance * 0.5).max(0.05))
    } else {
        (far / 5000.0).clamp(0.05, (distance * 0.5).max(0.05))
    };
    (near, far)
}
