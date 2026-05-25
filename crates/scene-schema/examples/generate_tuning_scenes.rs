//! Generates the tuning reference scenes under
//! `assets/world/<name>/project.json`. Run from the workspace root with
//! `cargo run --example generate_tuning_scenes -p awsm-scene-schema`
//! (or via `cargo run --manifest-path crates/scene-schema/Cargo.toml
//! --example generate_tuning_scenes`).
//!
//! Each scene is a programmatically authored `EditorProject` written
//! out as pretty JSON. The editor loads them via `Load…`; the renderer
//! bridge materializes them through the same path that handles
//! hand-authored projects.
//!
//! Scenes:
//! 1. `tuning-1k-meshes`        — 1024 boxes (32×32 grid) + 20 shadow lights.
//! 2. `tuning-64-lights`        — 64 mixed punctual lights + 10 medium meshes.
//! 3. `tuning-mixed-intensity`  — 20 lights with varied intensities (0.1× → 50×).
//! 4. `tuning-open-world`       — terrain plane + ocean plane + skybox + 50 props.
//! 5. `tuning-coverage`         — 100 small props at varying camera distances.
//! 6. `tuning-10k-meshes`       — 10K boxes (100×100×1 grid).
//! 7. `tuning-importance-tiers` — 16 lights spanning a 4×4 (distance, intensity) grid; drives importance-tier cutoff tuning.

use std::{fs, path::PathBuf};

use awsm_scene_schema::{
    AssetTable, CubeFaceUpdateRate, EditorNode, EditorProject, EnvironmentConfig, EvsmCutoff,
    FarCascadeUpdateRate, LightConfig, LightShadowConfig, LightShadowHardness, MaterialDef,
    MaterialShading, MeshShadowConfig, NodeId, NodeKind, PrimitiveShape, ShadowsConfig, Trs,
};

fn main() -> std::io::Result<()> {
    let workspace_root = workspace_root();
    let out_root = workspace_root.join("assets").join("world");
    fs::create_dir_all(&out_root)?;

    for (name, project) in [
        ("tuning-1k-meshes", scene_1k_meshes()),
        ("tuning-64-lights", scene_64_lights()),
        ("tuning-mixed-intensity", scene_mixed_intensity()),
        ("tuning-open-world", scene_open_world()),
        ("tuning-coverage", scene_coverage()),
        ("tuning-10k-meshes", scene_10k_meshes()),
        ("tuning-importance-tiers", scene_importance_tiers()),
    ] {
        let dir = out_root.join(name);
        fs::create_dir_all(&dir)?;
        let path = dir.join("project.json");
        let json =
            serde_json::to_string_pretty(&project).expect("EditorProject serializes cleanly");
        fs::write(&path, json)?;
        println!("wrote {}", path.display());
    }

    Ok(())
}

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR points at crates/scene-schema; the workspace
    // root is two levels up.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or(manifest_dir)
}

// ─────────────────────────────────────────────────────────────────────
// Shared helpers
// ─────────────────────────────────────────────────────────────────────

fn empty_project(name: &str) -> EditorProject {
    EditorProject {
        name: name.to_string(),
        environment: EnvironmentConfig::default(),
        shadows: ShadowsConfig::default(),
        assets: AssetTable::default(),
        nodes: Vec::new(),
    }
}

fn root_group(name: &str, children: Vec<EditorNode>) -> EditorNode {
    EditorNode {
        id: NodeId::new(),
        name: name.to_string(),
        transform: Trs::IDENTITY,
        kind: NodeKind::Group,
        locked: false,
        visible: true,
        prefab: false,
        children,
    }
}

fn box_node(name: &str, position: [f32; 3], dims: [f32; 3], color: [f32; 4]) -> EditorNode {
    EditorNode {
        id: NodeId::new(),
        name: name.to_string(),
        transform: Trs {
            translation: position,
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0, 1.0, 1.0],
        },
        kind: NodeKind::Primitive {
            shape: PrimitiveShape::Box { dims },
            material: None,
            inline_material: MaterialDef {
                base_color: color,
                metallic: 0.0,
                roughness: 0.7,
                shading: MaterialShading::Pbr,
                ..MaterialDef::default()
            },
            shadow: MeshShadowConfig::default(),
        },
        locked: false,
        visible: true,
        prefab: false,
        children: Vec::new(),
    }
}

fn plane_node(
    name: &str,
    position: [f32; 3],
    width: f32,
    depth: f32,
    color: [f32; 4],
) -> EditorNode {
    EditorNode {
        id: NodeId::new(),
        name: name.to_string(),
        transform: Trs {
            translation: position,
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0, 1.0, 1.0],
        },
        kind: NodeKind::Primitive {
            shape: PrimitiveShape::Plane {
                width,
                depth,
                segments_x: 1,
                segments_z: 1,
            },
            material: None,
            inline_material: MaterialDef {
                base_color: color,
                metallic: 0.0,
                roughness: 0.9,
                shading: MaterialShading::Pbr,
                ..MaterialDef::default()
            },
            shadow: MeshShadowConfig::default(),
        },
        locked: false,
        visible: true,
        prefab: false,
        children: Vec::new(),
    }
}

fn sphere_node(name: &str, position: [f32; 3], radius: f32, color: [f32; 4]) -> EditorNode {
    EditorNode {
        id: NodeId::new(),
        name: name.to_string(),
        transform: Trs {
            translation: position,
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0, 1.0, 1.0],
        },
        kind: NodeKind::Primitive {
            shape: PrimitiveShape::Sphere {
                radius,
                segments_long: 24,
                segments_lat: 16,
            },
            material: None,
            inline_material: MaterialDef {
                base_color: color,
                metallic: 0.0,
                roughness: 0.5,
                shading: MaterialShading::Pbr,
                ..MaterialDef::default()
            },
            shadow: MeshShadowConfig::default(),
        },
        locked: false,
        visible: true,
        prefab: false,
        children: Vec::new(),
    }
}

fn directional_light(
    name: &str,
    rotation: [f32; 4],
    color: [f32; 3],
    intensity: f32,
    shadow: LightShadowConfig,
) -> EditorNode {
    EditorNode {
        id: NodeId::new(),
        name: name.to_string(),
        transform: Trs {
            translation: [0.0, 10.0, 0.0],
            rotation,
            scale: [1.0, 1.0, 1.0],
        },
        kind: NodeKind::Light(LightConfig::Directional {
            color,
            intensity,
            shadow,
        }),
        locked: false,
        visible: true,
        prefab: false,
        children: Vec::new(),
    }
}

fn point_light(
    name: &str,
    position: [f32; 3],
    color: [f32; 3],
    intensity: f32,
    range: f32,
    shadow: LightShadowConfig,
) -> EditorNode {
    EditorNode {
        id: NodeId::new(),
        name: name.to_string(),
        transform: Trs {
            translation: position,
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0, 1.0, 1.0],
        },
        kind: NodeKind::Light(LightConfig::Point {
            color,
            intensity,
            range,
            shadow,
        }),
        locked: false,
        visible: true,
        prefab: false,
        children: Vec::new(),
    }
}

#[allow(clippy::too_many_arguments)]
fn spot_light(
    name: &str,
    position: [f32; 3],
    rotation: [f32; 4],
    color: [f32; 3],
    intensity: f32,
    range: f32,
    inner_angle: f32,
    outer_angle: f32,
    shadow: LightShadowConfig,
) -> EditorNode {
    EditorNode {
        id: NodeId::new(),
        name: name.to_string(),
        transform: Trs {
            translation: position,
            rotation,
            scale: [1.0, 1.0, 1.0],
        },
        kind: NodeKind::Light(LightConfig::Spot {
            color,
            intensity,
            range,
            inner_angle,
            outer_angle,
            shadow,
        }),
        locked: false,
        visible: true,
        prefab: false,
        children: Vec::new(),
    }
}

fn shadow_high() -> LightShadowConfig {
    LightShadowConfig {
        cast: true,
        resolution: 1024,
        hardness: LightShadowHardness::Soft,
        max_distance: 60.0,
        cascade_count: 4,
        evsm_cutoff: EvsmCutoff::LastCascade,
        far_cascade_update_rate: FarCascadeUpdateRate::Every4Frames,
        cube_face_update_rate: CubeFaceUpdateRate::EveryFrame,
        ..LightShadowConfig::default()
    }
}

fn shadow_medium() -> LightShadowConfig {
    LightShadowConfig {
        cast: true,
        resolution: 512,
        hardness: LightShadowHardness::Soft,
        max_distance: 40.0,
        ..LightShadowConfig::default()
    }
}

fn shadow_off() -> LightShadowConfig {
    LightShadowConfig {
        cast: false,
        ..LightShadowConfig::default()
    }
}

// ─────────────────────────────────────────────────────────────────────
// Scene 1 — 1k meshes, 20 shadow-casting lights. Drives T1.
// ─────────────────────────────────────────────────────────────────────

fn scene_1k_meshes() -> EditorProject {
    let mut project = empty_project("tuning-1k-meshes");

    // 32×32 grid of small boxes, spaced 2 units apart. Centered on origin.
    let mut grid_children = Vec::with_capacity(1024);
    let spacing = 2.0_f32;
    let count = 32;
    let offset = -(count as f32 - 1.0) * spacing * 0.5;
    for ix in 0..count {
        for iz in 0..count {
            let x = offset + ix as f32 * spacing;
            let z = offset + iz as f32 * spacing;
            // Slight color variation so visual debugging is easier.
            let hue = (ix as f32 * 0.13 + iz as f32 * 0.07) % 1.0;
            grid_children.push(box_node(
                &format!("box_{ix}_{iz}"),
                [x, 0.5, z],
                [1.0, 1.0, 1.0],
                hsv_to_rgba(hue, 0.4, 0.9),
            ));
        }
    }

    // Floor plane sized for the grid plus margin.
    let floor_extent = count as f32 * spacing + 10.0;
    project.nodes.push(plane_node(
        "floor",
        [0.0, -0.01, 0.0],
        floor_extent,
        floor_extent,
        [0.3, 0.3, 0.3, 1.0],
    ));

    // 20 point lights distributed across the grid for shadow-pass tier.
    let mut light_children = Vec::with_capacity(20);
    for i in 0..20 {
        let theta = (i as f32 / 20.0) * std::f32::consts::TAU;
        let r = 20.0;
        let x = theta.cos() * r;
        let z = theta.sin() * r;
        light_children.push(point_light(
            &format!("light_{i}"),
            [x, 5.0, z],
            [1.0, 0.95, 0.85],
            40.0,
            12.0,
            shadow_medium(),
        ));
    }

    project.nodes.push(root_group("grid", grid_children));
    project.nodes.push(root_group("lights", light_children));
    project
}

// ─────────────────────────────────────────────────────────────────────
// Scene 2 — 64 mixed lights + ~10 medium meshes (~500K verts total).
// Drives T2.
// ─────────────────────────────────────────────────────────────────────

fn scene_64_lights() -> EditorProject {
    let mut project = empty_project("tuning-64-lights");

    project.nodes.push(plane_node(
        "floor",
        [0.0, -0.01, 0.0],
        80.0,
        80.0,
        [0.25, 0.25, 0.25, 1.0],
    ));

    // 10 high-poly spheres (24×16 = ~700 verts each * 10 = 7K verts —
    // the "500K verts" in the brief assumes glb assets; spheres are a
    // lighter stand-in but the per-mesh cost is what matters for
    // light-per-mesh sums).
    let mut mesh_children = Vec::with_capacity(10);
    for i in 0..10 {
        let theta = (i as f32 / 10.0) * std::f32::consts::TAU;
        let r = 15.0;
        let x = theta.cos() * r;
        let z = theta.sin() * r;
        mesh_children.push(sphere_node(
            &format!("mesh_{i}"),
            [x, 2.0, z],
            2.0,
            [0.8, 0.8, 0.9, 1.0],
        ));
    }
    project.nodes.push(root_group("meshes", mesh_children));

    // 64 lights — mix of point + spot. Shadow only on the first 10
    // (the rest are non-casters so the per-light light-list cost is
    // measurable independent of shadow-pass cost).
    let mut light_children = Vec::with_capacity(64);
    for i in 0..64 {
        let theta = (i as f32 / 64.0) * std::f32::consts::TAU;
        let r = 25.0 + (i % 4) as f32 * 3.0;
        let x = theta.cos() * r;
        let z = theta.sin() * r;
        let h = 1.5 + (i % 3) as f32 * 2.5;
        let cast = i < 10;
        let shadow = if cast { shadow_medium() } else { shadow_off() };
        let color = hsv_to_rgb_arr(((i as f32) * 0.37) % 1.0, 0.6, 1.0);
        if i % 2 == 0 {
            light_children.push(point_light(
                &format!("point_{i}"),
                [x, h, z],
                color,
                30.0,
                10.0,
                shadow,
            ));
        } else {
            light_children.push(spot_light(
                &format!("spot_{i}"),
                [x, h + 2.0, z],
                [
                    std::f32::consts::FRAC_1_SQRT_2,
                    0.0,
                    0.0,
                    std::f32::consts::FRAC_1_SQRT_2,
                ], // 90° X rotation — points downward
                color,
                40.0,
                12.0,
                0.4,
                0.8,
                shadow,
            ));
        }
    }
    project.nodes.push(root_group("lights", light_children));
    project
}

// ─────────────────────────────────────────────────────────────────────
// Scene 3 — 20 lights at varied intensities (0.1× → 50×). Drives T3.
// ─────────────────────────────────────────────────────────────────────

fn scene_mixed_intensity() -> EditorProject {
    let mut project = empty_project("tuning-mixed-intensity");

    project.nodes.push(plane_node(
        "floor",
        [0.0, -0.01, 0.0],
        40.0,
        40.0,
        [0.3, 0.3, 0.3, 1.0],
    ));

    // A grid of small boxes for the lights to interact with.
    let mut box_children = Vec::with_capacity(64);
    let spacing = 2.0_f32;
    let count = 8;
    let offset = -(count as f32 - 1.0) * spacing * 0.5;
    for ix in 0..count {
        for iz in 0..count {
            box_children.push(box_node(
                &format!("box_{ix}_{iz}"),
                [
                    offset + ix as f32 * spacing,
                    0.5,
                    offset + iz as f32 * spacing,
                ],
                [1.0, 1.0, 1.0],
                [0.7, 0.7, 0.7, 1.0],
            ));
        }
    }
    project.nodes.push(root_group("boxes", box_children));

    // 20 point lights spread across the range, intensities 0.1 .. 50.
    let mut light_children = Vec::with_capacity(20);
    for i in 0..20 {
        let t = i as f32 / 19.0; // 0..1
                                 // Log-spaced intensity from 0.1 to 50.
        let intensity = 0.1 * (500.0_f32).powf(t);
        let theta = (i as f32 / 20.0) * std::f32::consts::TAU;
        let r = 12.0;
        let x = theta.cos() * r;
        let z = theta.sin() * r;
        light_children.push(point_light(
            &format!("light_{i}_int{:.1}", intensity),
            [x, 4.0, z],
            [1.0, 0.9, 0.8],
            intensity,
            15.0,
            shadow_high(),
        ));
    }
    project.nodes.push(root_group("lights", light_children));
    project
}

// ─────────────────────────────────────────────────────────────────────
// Scene 4 — terrain + ocean + skybox + 50 props. Drives T6.
// ─────────────────────────────────────────────────────────────────────

fn scene_open_world() -> EditorProject {
    let mut project = empty_project("tuning-open-world");

    // 1 km × 1 km terrain (single segmented plane stand-in).
    project.nodes.push(EditorNode {
        id: NodeId::new(),
        name: "terrain".to_string(),
        transform: Trs::IDENTITY,
        kind: NodeKind::Primitive {
            shape: PrimitiveShape::Plane {
                width: 1000.0,
                depth: 1000.0,
                segments_x: 64,
                segments_z: 64,
            },
            material: None,
            inline_material: MaterialDef {
                base_color: [0.35, 0.4, 0.2, 1.0],
                metallic: 0.0,
                roughness: 0.9,
                shading: MaterialShading::Pbr,
                ..MaterialDef::default()
            },
            shadow: MeshShadowConfig::default(),
        },
        locked: false,
        visible: true,
        prefab: false,
        children: Vec::new(),
    });

    // Ocean plane just under the terrain — blue + glossy.
    project.nodes.push(plane_node(
        "ocean",
        [0.0, -0.5, 0.0],
        1500.0,
        1500.0,
        [0.05, 0.2, 0.4, 1.0],
    ));

    // 50 props (boxes) scattered across the terrain in a pseudo-random
    // grid. Deterministic positions so the scene round-trips.
    let mut props = Vec::with_capacity(50);
    for i in 0..50 {
        let h = i as f32 * 1.618_034;
        let x = ((h * 12.9898).sin() * 43758.547).fract() * 800.0 - 400.0;
        let z = ((h * 78.233).sin() * 43758.547).fract() * 800.0 - 400.0;
        let size = 1.5 + ((h * 0.3).fract() * 3.0);
        props.push(box_node(
            &format!("prop_{i}"),
            [x, size * 0.5, z],
            [size, size, size],
            [0.6, 0.5, 0.4, 1.0],
        ));
    }
    project.nodes.push(root_group("props", props));

    // One strong directional sun.
    project.nodes.push(directional_light(
        "sun",
        // 30° from vertical, slight Y-yaw — quaternion(axis-angle):
        // axis = (1,0,0), angle = 0.785 rad (≈45°)
        [0.3826834, 0.0, 0.0, 0.9238795],
        [1.0, 0.95, 0.85],
        5.0,
        shadow_high(),
    ));
    project
}

// ─────────────────────────────────────────────────────────────────────
// Scene 5 — 100 small props at varying camera distances. Drives T4.
// ─────────────────────────────────────────────────────────────────────

fn scene_coverage() -> EditorProject {
    let mut project = empty_project("tuning-coverage");

    project.nodes.push(plane_node(
        "floor",
        [0.0, -0.01, 0.0],
        200.0,
        200.0,
        [0.3, 0.3, 0.3, 1.0],
    ));

    // Props arranged in a line receding from the camera. Each at a
    // fixed step in Z so the projected screen-pixel coverage decreases
    // monotonically — useful for picking a coverage threshold.
    let mut props = Vec::with_capacity(100);
    for i in 0..100 {
        let z = -2.0 - (i as f32) * 1.0; // -2, -3, ..., -101
                                         // Spread along X to avoid total overlap.
        let x = ((i as f32) * 0.7).sin() * 2.0;
        let y = 0.25;
        props.push(box_node(
            &format!("prop_{i:03}"),
            [x, y, z],
            [0.5, 0.5, 0.5],
            [0.8, 0.5, 0.3, 1.0],
        ));
    }
    project.nodes.push(root_group("props", props));

    // One directional light so the props are shaded.
    project.nodes.push(directional_light(
        "sun",
        [0.3826834, 0.0, 0.0, 0.9238795],
        [1.0, 1.0, 1.0],
        4.0,
        shadow_medium(),
    ));
    project
}

// ─────────────────────────────────────────────────────────────────────
// Scene 6 — 10K boxes (100×100). Drives T5 SceneSpatial rebuild
// thresholds.
// ─────────────────────────────────────────────────────────────────────

fn scene_10k_meshes() -> EditorProject {
    let mut project = empty_project("tuning-10k-meshes");

    let mut grid_children = Vec::with_capacity(10_000);
    let spacing = 1.5_f32;
    let count = 100;
    let offset = -(count as f32 - 1.0) * spacing * 0.5;
    for ix in 0..count {
        for iz in 0..count {
            let hue = (ix as f32 * 0.013 + iz as f32 * 0.007) % 1.0;
            grid_children.push(box_node(
                &format!("box_{ix}_{iz}"),
                [
                    offset + ix as f32 * spacing,
                    0.4,
                    offset + iz as f32 * spacing,
                ],
                [0.8, 0.8, 0.8],
                hsv_to_rgba(hue, 0.3, 0.9),
            ));
        }
    }

    let floor_extent = count as f32 * spacing + 20.0;
    project.nodes.push(plane_node(
        "floor",
        [0.0, -0.01, 0.0],
        floor_extent,
        floor_extent,
        [0.25, 0.25, 0.25, 1.0],
    ));

    project.nodes.push(root_group("grid", grid_children));

    // Two directional lights for some shadowing pressure but without
    // overwhelming the 10K-mesh-tier perf measurements.
    project.nodes.push(directional_light(
        "sun",
        [0.3826834, 0.0, 0.0, 0.9238795],
        [1.0, 0.95, 0.85],
        4.0,
        shadow_high(),
    ));
    project
}

// Small HSV→RGB helper for visual variety without pulling in a color
// crate. `h, s, v` in `[0, 1]`.
fn hsv_to_rgba(h: f32, s: f32, v: f32) -> [f32; 4] {
    let [r, g, b] = hsv_to_rgb_arr(h, s, v);
    [r, g, b, 1.0]
}
fn hsv_to_rgb_arr(h: f32, s: f32, v: f32) -> [f32; 3] {
    let i = (h * 6.0).floor();
    let f = h * 6.0 - i;
    let p = v * (1.0 - s);
    let q = v * (1.0 - f * s);
    let t = v * (1.0 - (1.0 - f) * s);
    match i as i32 % 6 {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

// ─────────────────────────────────────────────────────────────────────
// Scene 7 — importance-tier histogram source. Drives importance-tier cutoff tuning.
//
// 4 × 4 grid: distance from camera ∈ {1, 5, 15, 50} × intensity ∈
// {1, 10, 100, 1000}. With the camera at origin facing +X (the
// editors default), lights are placed along a ring at the given
// distance so the score = intensity / (1 + d²) lands at predictable
// values:
//
//   d \ i      1         10        100       1000
//    1     0.5(M)    5(U)      50(U)    500(U)
//    5     0.04(L)   0.4(M)    3.8(H)   38(U)
//   15     0.004(L)  0.04(L)   0.44(M)  4.4(U)
//   50     0.0004(L) 0.004(L)  0.04(L)  0.4(M)
//
// With the current cutoffs (0.1 / 1 / 4): 6 Low, 4 Medium, 1 High,
// 5 Ultra. The High band is sparse — only a single (d=5, i=100)
// combo lands there. T3 re-tuning either widens High (bump Ultra
// cutoff above 4) or accepts that mid-distance + bright is rare.
// ─────────────────────────────────────────────────────────────────────

fn scene_importance_tiers() -> EditorProject {
    let mut project = empty_project("tuning-importance-tiers");

    // A 40×40 floor + a few props so the lights have something to
    // illuminate. The visual output is incidental — what matters is
    // the per-light score the renderer computes during
    // `refresh_light_importance_budgets`.
    project.nodes.push(plane_node(
        "floor",
        [0.0, -0.01, 0.0],
        80.0,
        80.0,
        [0.3, 0.3, 0.3, 1.0],
    ));

    let mut light_children = Vec::with_capacity(16);
    let distances = [1.0_f32, 5.0, 15.0, 50.0];
    let intensities = [1.0_f32, 10.0, 100.0, 1000.0];
    for (di, &dist) in distances.iter().enumerate() {
        for (ii, &intensity) in intensities.iter().enumerate() {
            // Spread the 16 lights around a circle at each distance
            // tier so they dont occlude each other in the editors
            // default view.
            let theta = (di * 4 + ii) as f32 / 16.0 * std::f32::consts::TAU;
            let x = theta.cos() * dist;
            let z = theta.sin() * dist;
            let hue = (di * 4 + ii) as f32 / 16.0;
            light_children.push(point_light(
                &format!("light_d{:.0}_i{:.0}", dist, intensity),
                [x, 3.0, z],
                hsv_to_rgb_arr(hue, 0.7, 1.0),
                intensity,
                dist.max(3.0),
                shadow_high(),
            ));
        }
    }
    project.nodes.push(root_group("lights", light_children));
    project
}
