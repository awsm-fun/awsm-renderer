//! Editor-authored camera configuration.
//!
//! Cameras are first-class scene nodes (`NodeKind::Camera`) alongside
//! Lights and Colliders. The author places them, orients them, and
//! optionally points a per-game hook (e.g. `player_camera` inside the
//! `robot` prefab) at one — the player frontend uses that node's
//! transform + config to drive its tracking camera.
//!
//! The same config travels through the per-game WIT into the engine's
//! `session-config`. Today the engine itself doesn't consume it
//! (rendering happens client-side), but the game-server roadmap calls
//! for **server-side frustum culling / interest management**: only
//! send a player the world state visible from their camera's frustum,
//! removing wallhack vectors at the wire level. Plumbing the config
//! end-to-end now means the day we flip on culling, we don't have a
//! schema migration on top of the algorithm work.

use super::tree::NodeId;

/// Projection model. Perspective is the common case (3D games);
/// orthographic is here for top-down / 2.5D variants and editor
/// preview cameras.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum CameraProjection {
    /// Standard pin-hole projection. `fov_y_rad` is the **vertical**
    /// field-of-view in radians; the horizontal FOV is derived from
    /// the viewport's aspect ratio at render time so the camera
    /// behaves correctly across window sizes.
    Perspective { fov_y_rad: f32 },
    /// Parallel projection. `half_height` is the world-space half
    /// extent of the visible region along the camera's local Y axis;
    /// width is derived from aspect ratio at render time.
    Orthographic { half_height: f32 },
}

/// Full camera config. Near / far clip distances are common to both
/// projection variants. Authors edit these directly in the properties
/// panel; the editor wireframe draws the resulting frustum so the
/// geometry is visible while authoring.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct CameraConfig {
    pub projection: CameraProjection,
    /// Near clip plane in meters. Anything closer than this is not
    /// rendered (and not visible for culling purposes).
    pub near: f32,
    /// Far clip plane in meters. Same idea, at the opposite end.
    pub far: f32,
    /// Authored camera behavior. `Static` is the default and matches today's
    /// behavior (the camera transform is whatever the author / per-game code
    /// set on the camera node). Other variants drive the camera transform
    /// each frame from a target / curve.
    #[serde(default = "CameraBehavior::default_static")]
    pub behavior: CameraBehavior,
}

/// Per-frame camera-transform driver.
///
/// Evaluated by the editor / player renderer-bridge **after** all node-transform
/// commits for the frame and **before** view-matrix submission, so follow /
/// orbit cams are same-frame, not one-frame-stale.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum CameraBehavior {
    /// Camera transform is whatever the author / per-game module sets.
    /// Use for first-person cameras driven from gameplay code.
    #[default]
    Static,
    /// Track a target node's position with a fixed offset.
    Follow {
        target: NodeId,
        offset: [f32; 3],
        look_at_target: bool,
    },
    /// Orbit a target node at a fixed distance / pitch / yaw, optionally
    /// auto-rotating around it.
    OrbitTarget {
        target: NodeId,
        distance: f32,
        pitch: f32,
        yaw: f32,
        auto_rotate_speed: f32,
    },
    /// Travel along an authored curve, optionally looking at a target node
    /// (or at the point `look_ahead_distance` ahead on the curve).
    RailAlongCurve {
        curve: NodeId,
        look_ahead_distance: f32,
        target: Option<NodeId>,
    },
}

impl CameraBehavior {
    pub fn default_static() -> Self {
        Self::Static
    }
}

impl CameraConfig {
    pub fn default_perspective() -> Self {
        Self {
            projection: CameraProjection::Perspective {
                // 60° vertical — middle-of-the-road game default.
                fov_y_rad: std::f32::consts::FRAC_PI_3,
            },
            near: 0.1,
            far: 200.0,
            behavior: CameraBehavior::Static,
        }
    }

    pub fn default_orthographic() -> Self {
        Self {
            projection: CameraProjection::Orthographic { half_height: 5.0 },
            near: 0.1,
            far: 200.0,
            behavior: CameraBehavior::Static,
        }
    }
}
