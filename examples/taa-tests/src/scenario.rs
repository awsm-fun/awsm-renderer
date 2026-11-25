//! Scripted, fixed-timestep scenarios (05 §1), keyed by scene name.
//!
//! Pure functions of the frame index `k` (time `t = k / 60 s`): the same `k`
//! always produces the same node transforms and camera, so frame `k` is the
//! same frame on every run. Scripts drive nodes **by name** (the names the
//! scene's `author.js` gives them) relative to their authored transforms.
//! Authored animation clips (skin walk, morphs) play from the bundle and are
//! advanced by the harness with the same fixed timestep.
//!
//! Speeds are chosen in world units for the authored camera so the on-screen
//! motion lands in the ranges the tests need (a few px / frame for trails,
//! ~1 px / frame for the slow-pan blur test) at the default 960×720 canvas.

use std::collections::HashMap;

use awsm_renderer::camera::{CameraParams, CameraProjectionParams};
use awsm_renderer::transforms::Transform;
use glam::{Mat4, Quat, Vec3};

/// Fixed timestep.
pub const FPS: f64 = 60.0;
pub const DT_MS: f64 = 1000.0 / FPS;

/// Seconds at frame `k`.
pub fn time_at(k: u32) -> f64 {
    k as f64 / FPS
}

/// What the scene looked like at load: authored local transforms of every
/// named node, authored cameras (world matrix + params), and scene bounds.
#[derive(Clone, Debug, Default)]
pub struct SceneSetup {
    pub nodes: HashMap<String, Transform>,
    /// `(name, world, params)` in authored order.
    pub cameras: Vec<(String, Mat4, CameraParams)>,
    pub bounds_center: Vec3,
    pub bounds_radius: f32,
    /// Bounds of the meshes drawn at load (hidden prefab templates excluded).
    pub visible_center: Vec3,
    pub visible_radius: f32,
}

impl SceneSetup {
    pub(crate) fn base(&self, name: &str) -> Transform {
        self.nodes.get(name).cloned().unwrap_or_default()
    }

    fn camera(&self, name: &str) -> Option<(Mat4, CameraParams)> {
        self.cameras
            .iter()
            .find(|(n, _, _)| n == name)
            .map(|(_, w, p)| (*w, *p))
    }

    /// The first authored camera, or a bounds-framed fallback.
    fn main_camera(&self) -> (Mat4, CameraParams) {
        self.camera("camera")
            .or_else(|| self.cameras.first().map(|(_, w, p)| (*w, *p)))
            .unwrap_or_else(|| self.bounds_camera(0.8))
    }

    /// A 3/4 view of the DRAWN meshes at `factor` × their radius.
    pub fn visible_camera(&self, yaw: f32, factor: f32) -> (Mat4, CameraParams) {
        let r = self.visible_radius.max(0.05);
        let c = self.visible_center;
        let eye = c + Vec3::new(
            yaw.sin() * r * factor,
            r * factor * 0.5,
            yaw.cos() * r * factor,
        );
        let world = Mat4::look_at_rh(eye, c, Vec3::Y).inverse();
        (
            world,
            CameraParams::perspective(
                45.0_f32.to_radians(),
                (r * 0.01).max(0.005),
                (r * 200.0).max(50.0),
            ),
        )
    }

    /// A 3/4 view at 2.2 × the bounds radius (player-tests' framing).
    pub fn bounds_camera(&self, yaw: f32) -> (Mat4, CameraParams) {
        let r = self.bounds_radius.max(1.0);
        let c = self.bounds_center;
        let eye = c + Vec3::new(yaw.sin() * r * 2.2, r * 1.1, yaw.cos() * r * 2.2);
        let world = Mat4::look_at_rh(eye, c, Vec3::Y).inverse();
        let near = (r * 0.001).max(0.01);
        let far = (r * 200.0).max(100.0);
        (
            world,
            CameraParams::perspective(45.0_f32.to_radians(), near, far),
        )
    }
}

/// Everything the harness applies before rendering frame `k`.
#[derive(Clone, Debug)]
pub struct FrameDrive {
    /// Camera world matrix (camera looks down local −Z, +Y up).
    pub camera_world: Mat4,
    pub camera_params: CameraParams,
    /// Local transforms to set on named nodes this frame.
    pub transforms: Vec<(String, Transform)>,
    /// A FLAGGED camera cut happens at this frame (the harness resets the
    /// temporal history first). Unflagged switches exist too (taa-cut 240).
    pub cut: bool,
    /// Nodes that teleported at this frame (their previous transform is
    /// meaningless: `Transforms::reset_history` once that hook exists).
    pub teleports: Vec<String>,
    /// Morph weights to set on named nodes' morphed meshes this frame
    /// (after the clips advance, so the script wins).
    pub morphs: Vec<(String, Vec<f32>)>,
    /// Label of the scripted phase this frame belongs to.
    pub phase: &'static str,
}

/// A scenario: its length, its phases and its per-frame drive.
#[derive(Clone, Debug)]
pub struct Scenario {
    pub scene: String,
    pub frames: u32,
    /// `(label, first_frame, end_frame_exclusive)`.
    pub phases: Vec<(&'static str, u32, u32)>,
    /// The node whose coverage defines the trail band (05 §4.2), if any, and
    /// the half extents of the box it is authored as (its exact hull).
    pub tracked: Option<(&'static str, Vec3)>,
    /// Nodes the harness flags as HUD meshes after load (`set_mesh_hud`).
    pub hud_nodes: Vec<&'static str>,
    /// Nodes hidden from load until the given frame, then shown
    /// (`set_mesh_hidden`): a pop-in (`<scene>@popin`).
    pub appear: Vec<(&'static str, u32)>,
    /// Nodes moved by an offset from their scripted place on one frame only
    /// (`<scene>@popjump`: shown far away a frame before they are posed).
    pub displaced: Vec<(&'static str, u32, Vec3)>,
    /// Nodes given this scale every frame (`bloom-post@thin`).
    pub rescaled: Vec<(&'static str, Vec3)>,
    /// Nodes whose PBR emissive factor is multiplied by this at load.
    pub emissive_gain: Vec<(&'static str, f32)>,
    /// The camera holds frame 0's pose (`…@thinstill`).
    pub still_camera: bool,
}

impl Scenario {
    pub fn phase_at(&self, k: u32) -> &'static str {
        self.phases
            .iter()
            .find(|(_, a, b)| k >= *a && k < *b)
            .map(|(n, _, _)| *n)
            .unwrap_or("end")
    }

    pub fn phase_range(&self, name: &str) -> Option<(u32, u32)> {
        self.phases
            .iter()
            .find(|(n, _, _)| *n == name)
            .map(|(_, a, b)| (*a, *b))
    }
}

/// `taa-motion@popin` (round 3, `TaaTuning::reactive_new_objects`): the
/// cube is hidden until k12 and then appears, still, over the checker floor
/// (taa-motion's still phase: nothing moves, the camera neither). Its
/// silhouette is a new object's: what its pixels held is the floor's
/// history. 30 frames.
pub fn popin_scenario(base: &str) -> Scenario {
    Scenario {
        scene: base.to_string(),
        frames: 30,
        phases: vec![("hidden", 0, 12), ("shown", 12, 30)],
        tracked: None,
        hud_nodes: vec![],
        appear: vec![("cube", 12)],
        displaced: vec![],
        rescaled: vec![],
        emissive_gain: vec![],
        still_camera: false,
    }
}

/// `bloom-post@thin` / `bloom-post@thinstill` (round 3, `TaaBloomSource`):
/// the bloom scene with its emissive spheres made thin and small: `emissive-2`
/// a thin line about 1 px thick at HDR ~50 (red), `emissive-5` a glint
/// smaller than a pixel at HDR ~5, `emissive-10` a glint about 2 px across at
/// HDR ~50. Bloom as authored. The generic slow orbit, or (`thinstill`) the
/// camera held at frame 0.
pub fn bloom_thin_scenario(base: &str, still: bool) -> Scenario {
    Scenario {
        scene: base.to_string(),
        frames: 120,
        phases: vec![("orbit", 0, 120)],
        tracked: None,
        hud_nodes: vec![],
        appear: vec![],
        displaced: vec![],
        rescaled: vec![
            ("emissive-2", Vec3::new(4.0, 0.03, 0.03)),
            ("emissive-5", Vec3::splat(0.025)),
            ("emissive-10", Vec3::splat(0.07)),
        ],
        emissive_gain: vec![("emissive-2", 25.0), ("emissive-10", 5.0)],
        still_camera: still,
    }
}

/// `taa-motion@popjump`: as `@popin`, but the cube is shown at k12 3 m to
/// the side (on screen, over the floor) and posed in place at k13, the way an app's
/// visibility command can land a frame before the matching pose (Dreidel
/// Derby's respawned coins). No `reset_motion_history` for the jump.
pub fn popjump_scenario(base: &str) -> Scenario {
    Scenario {
        displaced: vec![("cube", 12, Vec3::new(3.0, 0.0, 0.0))],
        ..popin_scenario(base)
    }
}

/// The scenario for a scene. Unknown scenes (the existing test scenes, used
/// for the TAA-off baseline) get the generic slow orbit.
pub fn scenario_for(scene: &str) -> Scenario {
    let s_ = |frames, phases: Vec<(&'static str, u32, u32)>| Scenario {
        scene: scene.to_string(),
        frames,
        phases,
        tracked: None,
        hud_nodes: vec![],
        appear: vec![],
        displaced: vec![],
        rescaled: vec![],
        emissive_gain: vec![],
        still_camera: false,
    };
    let s =
        |frames, phases: Vec<(&'static str, u32, u32)>, tracked, hud: Vec<&'static str>| Scenario {
            scene: scene.to_string(),
            frames,
            phases,
            tracked,
            hud_nodes: hud,
            appear: vec![],
            displaced: vec![],
            rescaled: vec![],
            emissive_gain: vec![],
            still_camera: false,
        };
    match scene {
        // still 0.5 s → moving 2 s → stopped 1 s → fast 1 s
        "taa-motion" => s(
            270,
            vec![
                ("still", 0, 30),
                ("moving", 30, 150),
                ("stopped", 150, 210),
                ("fast", 210, 270),
            ],
            Some(("cube", Vec3::splat(0.5))),
            vec![],
        ),
        // still → orbit 2 s → slow strafe 1 s (≈1 px/frame) → stop 1 s
        "taa-orbit" => s(
            300,
            vec![
                ("still", 0, 30),
                ("orbit", 30, 150),
                ("strafe", 150, 240),
                ("stopped", 240, 300),
            ],
            None,
            vec![],
        ),
        // the authored clips play throughout
        "taa-skin" => s(180, vec![("playing", 0, 180)], None, vec![]),
        // still → slide away 1 s → hold → slide back 1 s → hold
        "taa-reveal" => s(
            240,
            vec![
                ("still", 0, 30),
                ("away", 30, 90),
                ("open", 90, 120),
                ("back", 120, 180),
                ("closed", 180, 240),
            ],
            Some(("wall", Vec3::new(1.5, 1.25, 0.1))),
            vec![],
        ),
        // static (convergence + flicker) → spinner turns → slow pan
        "taa-thin" => s(
            300,
            vec![("static", 0, 120), ("spin", 120, 210), ("pan", 210, 300)],
            Some(("spinner", Vec3::splat(0.5))),
            vec![],
        ),
        "taa-transparent" => s(
            240,
            vec![("still", 0, 30), ("moving", 30, 210), ("stopped", 210, 240)],
            Some(("backdrop", Vec3::new(2.0, 1.5, 0.05))),
            vec![],
        ),
        "taa-hud" => s(
            180,
            vec![("still", 0, 30), ("orbit", 30, 150), ("stopped", 150, 180)],
            None,
            vec!["hud-panel", "hud-marker"],
        ),
        // camera switches at 2 s (flagged cut) and 4 s (unflagged); teleports every second
        "taa-cut" => s(
            360,
            vec![("cam-a", 0, 120), ("cam-b", 120, 240), ("cam-c", 240, 360)],
            Some(("teleporter", Vec3::splat(0.4))),
            vec![],
        ),
        "taa-vertex-hook" => s(
            180,
            vec![("waving", 0, 180)],
            Some(("wave", Vec3::new(1.5, 0.25, 0.75))),
            vec![],
        ),
        // LOD scenes: dolly out so the selection switches levels.
        s if s.starts_with("lod-") || s == "taa-cluster" => s_(120, vec![("dolly", 0, 120)]),
        _ => s(120, vec![("orbit", 0, 120)], None, vec![]),
    }
}

/// Smooth 0→1 over `[a, b]` frames, linear (constant speed) inside.
fn ramp(k: u32, a: u32, b: u32) -> f32 {
    if k <= a {
        0.0
    } else if k >= b {
        1.0
    } else {
        (k - a) as f32 / (b - a) as f32
    }
}

/// Orbit a camera world matrix about a vertical axis through `pivot`.
fn orbit_world(world: Mat4, pivot: Vec3, yaw: f32) -> Mat4 {
    Mat4::from_translation(pivot)
        * Mat4::from_rotation_y(yaw)
        * Mat4::from_translation(-pivot)
        * world
}

/// Translate a camera along its own right axis.
fn strafe_world(world: Mat4, dist: f32) -> Mat4 {
    let right = world.x_axis.truncate().normalize_or_zero();
    Mat4::from_translation(right * dist) * world
}

/// Horizontal world size of one pixel at distance `depth` for a perspective
/// camera (used to turn px/frame targets into world speeds).
pub fn world_per_pixel(params: &CameraParams, depth: f32, height_px: u32) -> f32 {
    match params.projection {
        CameraProjectionParams::Perspective { fov_y_rad } => {
            2.0 * depth * (fov_y_rad * 0.5).tan() / height_px as f32
        }
        CameraProjectionParams::Orthographic { half_height } => {
            2.0 * half_height / height_px as f32
        }
    }
}

/// The drive for frame `k`.
pub fn drive(sc: &Scenario, k: u32, setup: &SceneSetup) -> FrameDrive {
    let (mut cam_world, mut cam_params) = setup.main_camera();
    let mut transforms: Vec<(String, Transform)> = Vec::new();
    let mut cut = false;
    let mut teleports = Vec::new();
    let mut morphs = Vec::new();
    let t = time_at(k) as f32;
    // A held camera (`…@thinstill`): every scripted pose as at frame 0.
    let k = if sc.still_camera { 0 } else { k };

    match sc.scene.as_str() {
        "taa-motion" => {
            // Cube: +x at 1.8 m/s while moving (≈ 3.3 px/frame at the
            // authored camera, 960×720), −x at 4.8 m/s while fast (≈ 8.7
            // px/frame); yaw 90°/s while moving, −180°/s while fast.
            let base = setup.base("cube");
            let moving = ramp(k, 30, 150) * 2.0; // seconds of motion so far (0..2)
            let fast = ramp(k, 210, 270); // 0..1 s
            let x = -1.8 + 1.8 * moving - 4.8 * fast;
            let yaw = std::f32::consts::FRAC_PI_2 * moving - std::f32::consts::PI * fast;
            transforms.push((
                "cube".into(),
                Transform {
                    translation: base.translation + Vec3::new(x, 0.0, 0.0),
                    rotation: Quat::from_rotation_y(yaw) * base.rotation,
                    scale: base.scale,
                },
            ));
        }
        "taa-orbit" => {
            // 30°/s orbit for 2 s, then a strafe at ~1 px/frame, then stop.
            let pivot = setup
                .nodes
                .get("pivot")
                .map(|t| t.translation)
                .unwrap_or(Vec3::ZERO);
            let yaw = 30f32.to_radians() * 2.0 * ramp(k, 30, 150);
            cam_world = orbit_world(cam_world, pivot, yaw);
            let depth = (cam_world.w_axis.truncate() - pivot).length();
            let px = world_per_pixel(&cam_params, depth, 720);
            let strafe = px * (k.clamp(150, 240) - 150) as f32; // 1 px/frame at the pivot depth
            cam_world = strafe_world(cam_world, strafe);
        }
        "taa-reveal" => {
            let base = setup.base("wall");
            let x = 3.0 * (ramp(k, 30, 90) - ramp(k, 120, 180));
            transforms.push((
                "wall".into(),
                Transform {
                    translation: base.translation + Vec3::new(x, 0.0, 0.0),
                    ..base
                },
            ));
        }
        "taa-thin" => {
            let base = setup.base("spinner");
            let yaw = std::f32::consts::PI * 1.5 * ramp(k, 120, 210); // 180°/s
            transforms.push((
                "spinner".into(),
                Transform {
                    rotation: Quat::from_rotation_y(yaw) * base.rotation,
                    ..base
                },
            ));
            let depth = cam_world.w_axis.truncate().length().max(1.0);
            let px = world_per_pixel(&cam_params, depth, 720);
            cam_world = strafe_world(cam_world, px * 0.5 * (k.clamp(210, 300) - 210) as f32);
        }
        "taa-transparent" => {
            let base = setup.base("backdrop");
            // Backdrop slides 2 m right then back (triangle wave, 1.33 m/s).
            let u = ramp(k, 30, 210);
            let x = 2.0 * (1.0 - (2.0 * u - 1.0).abs());
            transforms.push((
                "backdrop".into(),
                Transform {
                    translation: base.translation + Vec3::new(x, 0.0, 0.0),
                    ..base
                },
            ));
        }
        "taa-hud" => {
            let cam0 = cam_world;
            let yaw = 30f32.to_radians() * 2.0 * ramp(k, 30, 150);
            cam_world = orbit_world(cam_world, Vec3::ZERO, yaw);
            // HUD nodes are authored in world space in front of the authored
            // camera; keep them rigidly attached to it, as a game's HUD is.
            let follow = cam_world * cam0.inverse();
            for name in &sc.hud_nodes {
                let base = setup.base(name);
                let (scale, rotation, translation) =
                    (follow * base.to_matrix()).to_scale_rotation_translation();
                transforms.push((
                    name.to_string(),
                    Transform {
                        translation,
                        rotation,
                        scale,
                    },
                ));
            }
        }
        "taa-cut" => {
            let names = ["cam-a", "cam-b", "cam-c"];
            let idx = ((k / 120) as usize) % names.len();
            if let Some((w, p)) = setup.camera(names[idx]) {
                cam_world = w;
                cam_params = p;
            }
            // Camera switches at 120 and 240: the first is FLAGGED (the
            // harness calls reset_temporal_history), the second is NOT, so the
            // renderer's camera-jump guard has to catch it.
            cut = k == 120;
            let base = setup.base("teleporter");
            let spots = [
                Vec3::new(-1.5, 0.0, 0.0),
                Vec3::new(1.5, 0.0, 0.5),
                Vec3::new(0.0, 0.0, -1.5),
            ];
            let spot = spots[((k / 60) as usize) % spots.len()];
            transforms.push((
                "teleporter".into(),
                Transform {
                    translation: base.translation + spot,
                    ..base
                },
            ));
            if k > 0 && k % 60 == 0 {
                teleports.push("teleporter".to_string());
            }
        }
        name if name.starts_with("lod-") || name == "taa-cluster" => {
            // Dolly from 1.2× to 12× the bounds radius over 2 s (slow orbit
            // too), so discrete LOD levels / the cluster cut switch on screen.
            let u = ramp(k, 0, 120);
            let r = setup.bounds_radius.max(1.0);
            let dist = r * (1.2 + 10.8 * u * u);
            let yaw = 0.8 + 0.003 * k as f32;
            let c = setup.bounds_center;
            let eye = c + Vec3::new(yaw.sin() * dist, dist * 0.5, yaw.cos() * dist);
            cam_world = Mat4::look_at_rh(eye, c, Vec3::Y).inverse();
            cam_params = CameraParams::perspective(
                45.0_f32.to_radians(),
                (r * 0.001).max(0.01),
                (r * 400.0).max(100.0),
            );
        }
        // Close 3/4 view of the drawn clones (not the hidden template), slow
        // orbit, so the skinned clones fill the frame.
        "prefab-skinned-morph" => {
            let (w, p) = setup.visible_camera(0.8 + 0.003 * k as f32, 2.2);
            cam_world = w;
            cam_params = p;
        }
        // Still camera; the vertex hook animates with time.
        "taa-vertex-hook" => {}
        "taa-skin" => {
            // The Fox walks by its own clip. The morph cube is driven here:
            // the player path does not apply morph clips (pre-existing; the
            // weights stay 0), so the script sets them, 2 s period.
            let w = 0.5 - 0.5 * (std::f32::consts::TAU * t / 2.0).cos();
            morphs.push(("morph-cube".to_string(), vec![w, 1.0 - w]));
        }
        _ => {
            // Generic (existing scenes, TAA-off baseline): a slow orbit about
            // the bounds centre, ~0.17°/frame.
            let yaw = 0.8 + 0.003 * k as f32;
            let (w, p) = setup.bounds_camera(yaw);
            cam_world = w;
            cam_params = p;
        }
    }
    FrameDrive {
        camera_world: cam_world,
        camera_params: cam_params,
        transforms,
        cut,
        teleports,
        morphs,
        phase: sc.phase_at(k),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> SceneSetup {
        let mut s = SceneSetup {
            bounds_radius: 3.0,
            ..Default::default()
        };
        s.nodes.insert("cube".into(), Transform::IDENTITY);
        s.nodes.insert("wall".into(), Transform::IDENTITY);
        s.cameras.push((
            "camera".into(),
            Mat4::look_at_rh(Vec3::new(0.0, 2.0, 8.0), Vec3::ZERO, Vec3::Y).inverse(),
            CameraParams::perspective(0.8, 0.1, 100.0),
        ));
        s
    }

    #[test]
    fn deterministic_and_pure() {
        let sc = scenario_for("taa-motion");
        let st = setup();
        for k in [0, 31, 77, 149, 220] {
            let a = drive(&sc, k, &st);
            let b = drive(&sc, k, &st);
            assert_eq!(a.camera_world, b.camera_world);
            assert_eq!(a.transforms[0].1.translation, b.transforms[0].1.translation);
        }
    }

    #[test]
    fn motion_cube_speed_is_constant_while_moving_and_zero_when_stopped() {
        let sc = scenario_for("taa-motion");
        let st = setup();
        let x = |k| drive(&sc, k, &st).transforms[0].1.translation.x;
        let v1 = x(61) - x(60);
        let v2 = x(121) - x(120);
        assert!((v1 - 1.8 / 60.0).abs() < 1e-5 && (v1 - v2).abs() < 1e-5);
        assert_eq!(x(10), x(20));
        assert_eq!(x(160), x(200));
        assert_eq!(sc.phase_at(0), "still");
        assert_eq!(sc.phase_at(150), "stopped");
    }

    #[test]
    fn reveal_wall_returns_home() {
        let sc = scenario_for("taa-reveal");
        let st = setup();
        let x = |k| drive(&sc, k, &st).transforms[0].1.translation.x;
        assert_eq!(x(0), 0.0);
        assert!((x(100) - 3.0).abs() < 1e-6);
        assert!(x(200).abs() < 1e-6);
    }

    #[test]
    fn cut_frames_and_teleports() {
        let sc = scenario_for("taa-cut");
        let st = setup();
        let cuts: Vec<u32> = (0..360).filter(|k| drive(&sc, *k, &st).cut).collect();
        assert_eq!(cuts, vec![120]);
        let tps: Vec<u32> = (0..360)
            .filter(|k| !drive(&sc, *k, &st).teleports.is_empty())
            .collect();
        assert_eq!(tps, vec![60, 120, 180, 240, 300]);
    }

    #[test]
    fn generic_orbit_is_used_for_unknown_scenes() {
        let sc = scenario_for("kitchen-sink");
        let st = setup();
        assert_ne!(
            drive(&sc, 0, &st).camera_world,
            drive(&sc, 60, &st).camera_world
        );
    }
}
