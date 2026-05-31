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
//! 8. `tuning-1024-lights`      — ~1000 point lights spread over a 100m oversized floor + a transparent pane + a 40-light corner cluster; the GPU light-culling acceptance fixture (see docs/PERFORMANCE.md §5h).

use std::{fs, path::PathBuf};

use awsm_scene_schema::{
    AssetEntry, AssetId, AssetSource, AssetTable, CubeFaceUpdateRate, EditorNode, EditorProject,
    EnvironmentConfig, EvsmCutoff, FarCascadeUpdateRate, LightConfig, LightShadowConfig,
    LightShadowHardness, MaterialAlphaMode, MaterialDef, MaterialShading, MeshShadowConfig, NodeId,
    NodeKind, PrimitiveShape, ShadowsConfig, TextureDef, TextureRef, Trs,
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
        ("tuning-1024-lights", scene_1024_lights()),
        ("tuning-cull-debug", scene_cull_debug()),
        ("tuning-50-materials", scene_50_materials()),
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

// Cull-correctness probe scene. A large floor lit by an 8×8 grid of
// point lights (10m spacing, range 4 → disjoint spheres). All opaque
// meshes shade via the per-pixel GPU **froxel** path (clustered forward),
// so the "Light Heatmap" debug toggle shows each froxel's applied-light
// count: clean spatial structure (low counts near lights, black where no
// light reaches, distance-dependent coarseness) confirms the froxel cull
// bins only the lights that actually reach each froxel. Use it to sanity-
// check cull behaviour across camera types (perspective + orthographic).
fn scene_cull_debug() -> EditorProject {
    let mut project = empty_project("tuning-cull-debug");
    project.nodes.push(plane_node(
        "floor_oversized",
        [0.0, -0.01, 0.0],
        100.0,
        100.0,
        [0.25, 0.25, 0.28, 1.0],
    ));
    // 8×8 grid, 10m spacing, range 4 → 64 lights, disjoint spheres
    // (2m gap), and enough *in-frustum* lights overlapping the floor that
    // its per-mesh bucket exceeds the oversized threshold (16) → froxel
    // path. range 4 at height 2.5 still reaches the floor (≈3.1m disc).
    let coords = [-35.0_f32, -25.0, -15.0, -5.0, 5.0, 15.0, 25.0, 35.0];
    let mut lights = Vec::with_capacity(64);
    let mut i = 0;
    for &x in coords.iter() {
        for &z in coords.iter() {
            let hue = i as f32 / 64.0;
            lights.push(point_light(
                &format!("probe_{i}"),
                [x, 2.5, z],
                hsv_to_rgb_arr(hue, 0.7, 1.0),
                30.0,
                4.0,
                shadow_off(),
            ));
            i += 1;
        }
    }
    project.nodes.push(root_group("lights", lights));
    project
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
        custom_materials: Vec::new(),
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
            custom_material: None,
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
            custom_material: None,
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
            custom_material: None,
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
            custom_material: None,
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

// ─────────────────────────────────────────────────────────────────────
// Scene — 50 materials. Fixture for the materials-system specialization.
// 50 meshes, each with its own material, exercising the "many small
// opaque materials" path:
//   - 30 PBR meshes spanning distinct feature-sets (varying texture-slot
//     presence + vertex colors). Each distinct feature-set resolves to
//     its own specialized opaque bucket.
//   - 8 Toon meshes (Toon is specialized too).
//   - 6 Unlit meshes.
//
// NOTE: custom/dynamic materials are intentionally NOT in this scene.
// The scene-editor's live materialization path (renderer_bridge/
// node_sync.rs) does not yet consume a Primitive's `custom_material`
// field — per-mesh custom materials are unimplemented in the editor's
// render path (the `build_custom_instance` bridge exists but is not
// wired into materialization). A custom-material mesh would render with
// its inline fallback, not its shader, so including them here would
// measure nothing. Dynamic-material registration/compile batching is
// validated separately (material-editor + native tests). The PBR
// feature-sets are where the opaque-specialization delta actually lives.
//
// A single tiny placeholder texture is referenced in *varying slot
// combinations* — the feature-hash keys on slot presence, not content,
// so one PNG fans out to many feature-sets.
//
// Loaded non-interactively for measurement via
//   window.wasmBindings.load_scene_by_path("tuning-50-materials")
// (debug builds only). See docs/DEBUGGING-PREVIEW.md.
// ─────────────────────────────────────────────────────────────────────

/// SHA-256 of `assets/world/tuning-50-materials/assets/<hash>.png` — the
/// placeholder texture's content-hash-addressed filename. Must match the
/// file on disk (the loader fetches by this path).
const PLACEHOLDER_TEX_HASH: &str =
    "7310c0157395b5732c24711abbbc644ae273587de38077045470c9c7ea129bdb";

fn fifty_materials_pos(idx: i32) -> [f32; 3] {
    // 10×5 grid, centered on X, receding in +Z.
    let cols = 10;
    let r = idx / cols;
    let c = idx % cols;
    let spacing = 2.5_f32;
    [
        (c as f32 - (cols as f32 - 1.0) * 0.5) * spacing,
        0.7,
        2.0 + r as f32 * spacing,
    ]
}

fn prim_node(name: &str, pos: [f32; 3], shape: PrimitiveShape, inline: MaterialDef) -> EditorNode {
    EditorNode {
        id: NodeId::new(),
        name: name.to_string(),
        transform: Trs {
            translation: pos,
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0, 1.0, 1.0],
        },
        kind: NodeKind::Primitive {
            shape,
            material: None,
            inline_material: inline,
            custom_material: None,
            shadow: MeshShadowConfig::default(),
        },
        locked: false,
        visible: true,
        prefab: false,
        children: Vec::new(),
    }
}

fn alt_shape(idx: i32) -> PrimitiveShape {
    if idx % 2 == 0 {
        PrimitiveShape::Box {
            dims: [1.0, 1.0, 1.0],
        }
    } else {
        PrimitiveShape::Sphere {
            radius: 0.6,
            segments_long: 24,
            segments_lat: 16,
        }
    }
}

/// Build a PBR material from a texture-slot presence bitmask:
/// bit0=base_color, bit1=metallic_roughness, bit2=normal, bit3=occlusion,
/// bit4=emissive.
fn pbr_from_mask(tex: TextureRef, mask: u32, vcolor: bool, color: [f32; 4]) -> MaterialDef {
    MaterialDef {
        base_color: color,
        base_color_texture: (mask & 1 != 0).then_some(tex),
        metallic_roughness_texture: (mask & 2 != 0).then_some(tex),
        normal_texture: (mask & 4 != 0).then_some(tex),
        occlusion_texture: (mask & 8 != 0).then_some(tex),
        emissive_texture: (mask & 16 != 0).then_some(tex),
        emissive: if mask & 16 != 0 {
            [0.4, 0.25, 0.1]
        } else {
            [0.0, 0.0, 0.0]
        },
        metallic: 0.2,
        roughness: 0.55,
        vertex_colors_enabled: vcolor,
        shading: MaterialShading::Pbr,
        ..MaterialDef::default()
    }
}

fn scene_50_materials() -> EditorProject {
    let mut project = empty_project("tuning-50-materials");

    // Register the single placeholder texture asset (content-hash
    // addressed; the file lives at assets/<hash>.png next to project.json).
    let tex = {
        let entry = AssetEntry::new_with_hash(
            AssetSource::Texture(TextureDef::Raster {
                display_name: "placeholder.png".to_string(),
            }),
            PLACEHOLDER_TEX_HASH.to_string(),
        );
        let id = AssetId::new();
        project.assets.entries.insert(id, entry);
        TextureRef(id)
    };

    let mut meshes: Vec<EditorNode> = Vec::with_capacity(50);
    let mut idx = 0i32;

    // ── 30 PBR meshes across 16 distinct feature-sets ──────────────────
    // First 16 are distinct (one per feature-set); the remaining 14
    // re-use earlier feature-sets so the after-overhaul dedup (same
    // feature-set → same bucket) is exercised too.
    let pbr_masks: [u32; 16] = [
        0b00000, // base-color factor only — the smallest PBR shader
        0b00001, // +base_color texture
        0b00011, // +metallic_roughness texture
        0b00111, // +normal
        0b01111, // +occlusion
        0b11111, // all 5 texture slots
        0b00100, // normal only
        0b00101, // base + normal
        0b00110, // mr + normal
        0b01000, // occlusion only
        0b10000, // emissive texture only
        0b10001, // base + emissive
        0b10101, // base + normal + emissive
        0b01010, // mr + occlusion
        0b11010, // mr + occlusion + emissive
        0b01101, // base + normal + occlusion
    ];
    for i in 0..36 {
        let mask = pbr_masks[(i as usize) % pbr_masks.len()];
        let vcolor = i % 5 == 3; // a few with vertex colors → distinct sets
        let hue = i as f32 / 30.0;
        let mat = pbr_from_mask(tex, mask, vcolor, hsv_to_rgba(hue, 0.5, 0.9));
        meshes.push(prim_node(
            &format!(
                "pbr_{i:02}_mask{mask:05b}{}",
                if vcolor { "_vc" } else { "" }
            ),
            fifty_materials_pos(idx),
            alt_shape(idx),
            mat,
        ));
        idx += 1;
    }

    // ── 8 Toon meshes — varied bands/rim + some textured ───────────────
    for i in 0..8 {
        let bands = 2 + (i % 4) as u32; // 2..5
        let rim = 0.2 + (i % 3) as f32 * 0.3;
        let textured = i % 2 == 0;
        let mat = MaterialDef {
            base_color: hsv_to_rgba(0.55 + i as f32 * 0.03, 0.6, 0.9),
            base_color_texture: textured.then_some(tex),
            shading: MaterialShading::Toon {
                diffuse_bands: bands,
                rim_strength: rim,
            },
            ..MaterialDef::default()
        };
        meshes.push(prim_node(
            &format!("toon_{i}_b{bands}"),
            fifty_materials_pos(idx),
            alt_shape(idx),
            mat,
        ));
        idx += 1;
    }

    // ── 6 Unlit meshes — some textured ─────────────────────────────────
    for i in 0..6 {
        let textured = i % 2 == 0;
        let mat = MaterialDef {
            base_color: hsv_to_rgba(0.05 + i as f32 * 0.04, 0.7, 0.95),
            base_color_texture: textured.then_some(tex),
            shading: MaterialShading::Unlit,
            ..MaterialDef::default()
        };
        meshes.push(prim_node(
            &format!("unlit_{i}"),
            fifty_materials_pos(idx),
            alt_shape(idx),
            mat,
        ));
        idx += 1;
    }

    debug_assert_eq!(meshes.len(), 50, "scene must have exactly 50 meshes");
    project.nodes.push(root_group("meshes", meshes));

    // Floor + lights so the opaque meshes are lit (the custom screen-space
    // materials ignore lighting, but PBR/Toon need it).
    project.nodes.push(plane_node(
        "floor",
        [0.0, -0.01, 0.0],
        60.0,
        60.0,
        [0.28, 0.28, 0.3, 1.0],
    ));
    project.nodes.push(directional_light(
        "sun",
        [0.3826834, 0.0, 0.0, 0.9238795],
        [1.0, 0.96, 0.9],
        4.0,
        shadow_medium(),
    ));
    project.nodes.push(point_light(
        "fill",
        [0.0, 8.0, 8.0],
        [0.8, 0.85, 1.0],
        30.0,
        40.0,
        shadow_off(),
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

// ─────────────────────────────────────────────────────────────────────
// Scene 8 — GPU light-culling acceptance fixture.
//
// ~1000 point lights (one shy of MAX_PUNCTUAL_LIGHTS = 1024) spread
// over a 100m × 100m floor plane that exceeds the
// OVERSIZED_AABB_DIAGONAL_METERS threshold (so it routes through the
// oversized → froxel opaque path), plus:
//   - ~50 small prop boxes / spheres as the no-regression baseline
//     (these stay on the per-mesh CPU slice path);
//   - a single ~3m blend-mode glass pane in front of a back wall to
//     exercise the transparent → froxel path;
//   - an intentional 40-light corner cluster at (-45, _, -45) that
//     concentrates enough lights into a single 16-pixel × 32-slice
//     froxel to trigger the auto-grow overflow path on first frame.
//
// See docs/PERFORMANCE.md §5h (Lighting & light culling).
// ─────────────────────────────────────────────────────────────────────

fn scene_1024_lights() -> EditorProject {
    let mut project = empty_project("tuning-1024-lights");

    // Oversized floor — 100m × 100m, diagonal ≈ 141m > 50m threshold.
    project.nodes.push(plane_node(
        "floor_oversized",
        [0.0, -0.01, 0.0],
        100.0,
        100.0,
        [0.22, 0.22, 0.25, 1.0],
    ));

    // Back wall — also oversized; gives the transparent pane something
    // to refract/blend against.
    project.nodes.push(box_node(
        "back_wall",
        [0.0, 5.0, -45.0],
        [80.0, 10.0, 0.5],
        [0.5, 0.45, 0.4, 1.0],
    ));

    // ── Small prop baseline (~50 meshes well under the 50m diagonal) ──
    // These stay on the per-mesh slice path — they're the
    // regression-check baseline.
    let mut props = Vec::with_capacity(50);
    let prop_count_x = 10;
    let prop_count_z = 5;
    let prop_spacing = 7.0_f32;
    let prop_offset_x = -(prop_count_x as f32 - 1.0) * prop_spacing * 0.5;
    let prop_offset_z = -(prop_count_z as f32 - 1.0) * prop_spacing * 0.5;
    for ix in 0..prop_count_x {
        for iz in 0..prop_count_z {
            let x = prop_offset_x + ix as f32 * prop_spacing;
            let z = prop_offset_z + iz as f32 * prop_spacing;
            let hue = (ix as f32 * 0.13 + iz as f32 * 0.21) % 1.0;
            if (ix + iz) % 2 == 0 {
                props.push(box_node(
                    &format!("prop_box_{ix}_{iz}"),
                    [x, 0.6, z],
                    [1.0, 1.2, 1.0],
                    hsv_to_rgba(hue, 0.4, 0.85),
                ));
            } else {
                props.push(sphere_node(
                    &format!("prop_sphere_{ix}_{iz}"),
                    [x, 0.8, z],
                    0.8,
                    hsv_to_rgba(hue, 0.5, 0.9),
                ));
            }
        }
    }
    project.nodes.push(root_group("props", props));

    // ── Transparent pane ──────────────────────────────────────────────
    // A thin glass plate in front of the back wall. Uses Blend alpha
    // mode so it lands on the transparent shader path that consumes
    // the froxel list directly.
    let glass_pane = EditorNode {
        id: NodeId::new(),
        name: "glass_pane".to_string(),
        transform: Trs {
            translation: [0.0, 4.0, -35.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0, 1.0, 1.0],
        },
        kind: NodeKind::Primitive {
            shape: PrimitiveShape::Box {
                dims: [12.0, 6.0, 0.1],
            },
            material: None,
            inline_material: MaterialDef {
                base_color: [0.8, 0.9, 1.0, 0.35],
                metallic: 0.0,
                roughness: 0.15,
                alpha_mode: MaterialAlphaMode::Blend,
                shading: MaterialShading::Pbr,
                ..MaterialDef::default()
            },
            custom_material: None,
            shadow: MeshShadowConfig::default(),
        },
        locked: false,
        visible: true,
        prefab: false,
        children: Vec::new(),
    };
    project.nodes.push(glass_pane);

    // ── Lights ────────────────────────────────────────────────────────
    // Target: ~1000 lights. We split into two groups:
    //   - 960 lights in a quasi-random 32×30 grid spread across the
    //     full 100m floor. Per-froxel steady state stays in the 2–8
    //     range (each light's range ≈ 6m, much smaller than the 100m
    //     extent).
    //   - 40 lights tightly clustered at the (-45, _, -45) corner.
    //     All within ~3m of each other, all with overlapping ranges,
    //     so a single froxel sees 40+ lights and triggers
    //     `max_per_froxel_capacity` overflow → auto-grow on frame 2.
    //
    // Shadows: none (per-light shadow-pass cost would dominate the
    // profile and isn't what this fixture targets).
    let mut lights = Vec::with_capacity(1024);

    let grid_x = 32_i32;
    let grid_z = 30_i32;
    let extent = 45.0_f32; // half-extent across the floor
    for ix in 0..grid_x {
        for iz in 0..grid_z {
            // Quasi-random jitter so the grid isn't aligned with the
            // froxel grid (would otherwise put every cell's lights in
            // exactly the same froxel).
            let fx = ix as f32 / (grid_x - 1).max(1) as f32;
            let fz = iz as f32 / (grid_z - 1).max(1) as f32;
            let jitter_x = ((ix * 73 + iz * 17) % 31) as f32 / 31.0 - 0.5;
            let jitter_z = ((ix * 37 + iz * 53) % 29) as f32 / 29.0 - 0.5;
            let x = (fx * 2.0 - 1.0) * extent + jitter_x * 1.5;
            let z = (fz * 2.0 - 1.0) * extent + jitter_z * 1.5;
            let h = 1.0 + ((ix + iz) % 5) as f32 * 0.4;
            let hue = ((ix * 7 + iz * 11) % 100) as f32 / 100.0;
            let color = hsv_to_rgb_arr(hue, 0.6, 1.0);
            lights.push(point_light(
                &format!("grid_light_{ix}_{iz}"),
                [x, h, z],
                color,
                12.0,
                6.0,
                shadow_off(),
            ));
        }
    }

    // 40-light overflow cluster — all within ~3m, all overlapping.
    for i in 0..40 {
        let theta = (i as f32 / 40.0) * std::f32::consts::TAU;
        let r = 1.0 + (i % 4) as f32 * 0.4;
        let x = -45.0 + theta.cos() * r;
        let z = -45.0 + theta.sin() * r;
        let h = 1.2 + (i % 3) as f32 * 0.3;
        let hue = (i as f32 * 0.027) % 1.0;
        lights.push(point_light(
            &format!("cluster_{i}"),
            [x, h, z],
            hsv_to_rgb_arr(hue, 0.7, 1.0),
            8.0,
            5.0,
            shadow_off(),
        ));
    }

    project.nodes.push(root_group("lights", lights));

    // A single directional light keeps IBL/ambient roughly consistent
    // with the other tuning scenes — exercises the directional global
    // prefix path (which the cull pass deliberately bypasses).
    project.nodes.push(directional_light(
        "sun",
        // ~30° tilt down + slight Y rotation. Quaternion authored
        // directly to avoid pulling in a math dep — derived from
        // axis-angle (1,0,0) * -30°.
        [-0.2588, 0.0, 0.0, 0.9659],
        [1.0, 0.96, 0.88],
        2.0,
        shadow_off(),
    ));

    project
}
