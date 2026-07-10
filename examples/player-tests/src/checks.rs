//! The checks themselves (docs/plans/007). Each prints one `PLAYER-TEST` line
//! through [`Report`]; the scene list + expectations are parametrized in
//! [`SCENES`].

use anyhow::{anyhow, Result};
use awsm_renderer::features::RendererFeatures;
use awsm_renderer::AwsmRenderer;
use awsm_renderer_scene::Trs;
use awsm_renderer_scene_loader::{load_scene_for_player, LoadedScene};

use crate::harness::{
    count_authored_nodes, create_renderer, destroy_renderer, fetch_scene, now_ms, orbit_eye,
    run_frames, scene_bounds, url_flag_value, url_has_flag,
};
use crate::report::{set_hud, Report};

/// Default bundle server — `task test-scenes` (CORS-enabled).
const DEFAULT_BUNDLES_ORIGIN: &str = "http://localhost:9084";

/// Steady-state frame budget for the instancing check (ms, rAF-to-rAF).
const INSTANCING_FRAME_BUDGET_MS: f64 = 20.0;
/// Instancing renders thousands of copies through ONE instanced mesh row —
/// the renderer-side mesh (instance-record) count must stay tiny.
const INSTANCING_MAX_MESHES: usize = 10;
/// Prefab spawn/despawn cycles.
const PREFAB_CHURN_CYCLES: usize = 20;
/// Load/unload cycles for the no-clone-surface fallback of prefab-churn.
const LOAD_UNLOAD_CYCLES: usize = 5;
/// Startup-census ceilings (empty scene, all optional families feature-gated
/// off). Generous on purpose: the census records exact numbers in the detail;
/// the assert only catches a family (bloom/ssr/decal/cluster/picker) compiling
/// speculatively, which shows up as a step-change, not a ±2 drift.
const CENSUS_MAX_RENDER_PIPELINES: usize = 40;
const CENSUS_MAX_COMPUTE_PIPELINES: usize = 40;
const CENSUS_MAX_SHADERS: usize = 60;

/// Which extra per-scene scenario a scene carries beyond load + counts.
#[derive(Clone, Copy, PartialEq)]
enum Extra {
    None,
    Instancing,
    NaniteStreaming,
    LodTriDrop,
}

/// One test scene: bundle name + rough expectations + renderer features.
struct SceneSpec {
    name: &'static str,
    /// Minimum materialized (static, non-prefab) node count.
    min_nodes: usize,
    /// Whether the scene must materialize at least one renderer mesh.
    expect_meshes: bool,
    features: fn() -> RendererFeatures,
    extra: Extra,
}

fn base_features() -> RendererFeatures {
    RendererFeatures {
        gpu_culling: true,
        // Examples stay forward-Z (the features default; docs/plans/003).
        ..Default::default()
    }
}

fn kitchen_sink_features() -> RendererFeatures {
    RendererFeatures {
        // kitchen-sink authors decals; without the gate the loader cleanly
        // skips them, which would under-count what the scene exercises.
        decals: true,
        ..base_features()
    }
}

fn lod_features() -> RendererFeatures {
    RendererFeatures {
        lod: true,
        ..base_features()
    }
}

fn nanite_features() -> RendererFeatures {
    RendererFeatures {
        virtual_geometry: true,
        // Streaming residency: the budget hook a player uses. The editor's
        // `?stream`/`?streambudget=N` URL flags are editor-side sugar over
        // this same field, so the harness reads the flags itself and feeds
        // the hook directly (see `nanite_budget`).
        cluster_streaming: true,
        cluster_streaming_budget: nanite_budget(),
        ..base_features()
    }
}

/// `?streambudget=N` → `Some(N)`; plain `?stream` (or nothing) → loader default.
fn nanite_budget() -> Option<usize> {
    url_flag_value("streambudget").and_then(|v| v.parse().ok())
}

/// The parametrized scene list (plan 007 check 1).
const SCENES: &[SceneSpec] = &[
    SceneSpec {
        name: "kitchen-sink",
        min_nodes: 10,
        expect_meshes: true,
        features: kitchen_sink_features,
        extra: Extra::None,
    },
    SceneSpec {
        name: "anim-skinned",
        min_nodes: 3,
        expect_meshes: true,
        features: base_features,
        extra: Extra::None,
    },
    SceneSpec {
        name: "lights-many",
        min_nodes: 40,
        expect_meshes: true,
        features: base_features,
        extra: Extra::None,
    },
    SceneSpec {
        name: "lod-classic",
        min_nodes: 4,
        expect_meshes: true,
        features: lod_features,
        extra: Extra::LodTriDrop,
    },
    SceneSpec {
        name: "lod-nanite",
        min_nodes: 3,
        expect_meshes: true,
        features: nanite_features,
        extra: Extra::NaniteStreaming,
    },
    SceneSpec {
        name: "instancing-stress",
        min_nodes: 4,
        expect_meshes: true,
        features: base_features,
        extra: Extra::Instancing,
    },
    SceneSpec {
        name: "prefab-skinned-morph",
        min_nodes: 3,
        expect_meshes: true,
        features: base_features,
        extra: Extra::None,
    },
];

/// The scene whose bundle carries `prefab = true` roots — the clone-surface
/// scene for the prefab-churn check.
const PREFAB_CHURN_SCENE: &str = "prefab-static";

pub async fn run_all() {
    let mut report = Report::default();
    let origin = url_flag_value("bundles").unwrap_or_else(|| DEFAULT_BUNDLES_ORIGIN.to_string());
    let origin = origin.trim_end_matches('/').to_string();
    let filter: Option<Vec<String>> =
        url_flag_value("scenes").map(|s| s.split(',').map(|x| x.trim().to_string()).collect());

    tracing::info!("player-tests: bundles origin = {origin}");

    // ── 6. startup-census — BEFORE any load, so nothing is warm. ────────────
    set_hud("player-tests: startup-census");
    report.emit_result("startup-census", startup_census().await);

    // ── 1..4, 7. per-scene: load-transaction, counts, + extras. ─────────────
    for spec in SCENES {
        if let Some(filter) = &filter {
            if !filter.iter().any(|f| f == spec.name) {
                continue;
            }
        }
        set_hud(&format!("player-tests: loading {}", spec.name));
        run_scene(spec, &origin, &mut report).await;
    }

    // ── 5. prefab-churn — spawn/despawn a duplicated subtree, no leak. ──────
    let churn_enabled = filter
        .as_ref()
        .map(|f| f.iter().any(|s| s == PREFAB_CHURN_SCENE))
        .unwrap_or(true);
    if churn_enabled {
        set_hud("player-tests: prefab-churn");
        report.emit_result("prefab-churn", prefab_churn(&origin).await);
    }

    report.complete();
}

/// Renderer object/pipeline counts, snapshotted for leak/census assertions.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Counts {
    meshes: usize,
    mesh_resources: usize,
    geometry_bytes: usize,
    transforms: usize,
    render_pipelines: usize,
    compute_pipelines: usize,
    shaders: usize,
}

fn counts(renderer: &AwsmRenderer) -> Counts {
    Counts {
        meshes: renderer.meshes.len(),
        mesh_resources: renderer.meshes.resource_count(),
        geometry_bytes: renderer.meshes.geometry_pool_used_bytes(),
        transforms: renderer.transforms.len(),
        render_pipelines: renderer.pipelines.render.len(),
        compute_pipelines: renderer.pipelines.compute.len(),
        shaders: renderer.shaders.len(),
    }
}

/// Check 6 — fresh renderer, no load: the pipeline/shader floor. The lazy
/// families (bloom/SSR effects variants, decal, cluster, picker) must be
/// absent: decal/cluster/picker are structurally absent here (their
/// `RendererFeatures` gates are off, matching a lean player build) and
/// bloom/SSR are off in the default post-process config, so the recorded
/// floor IS the no-lazy-family number; the ceilings catch a family joining
/// the floor speculatively.
async fn startup_census() -> Result<String> {
    // Everything opt-in stays OFF — the leanest player shape.
    let (mut renderer, _canvas) = create_renderer(RendererFeatures::default()).await?;
    // Two empty frames so anything the first render lazily builds is counted.
    let (center, radius) = scene_bounds(&renderer);
    run_frames(&mut renderer, center, radius, 2, |_| {
        orbit_eye(center, radius, 2.5, 0.8)
    })
    .await?;
    let c = counts(&renderer);
    destroy_renderer(renderer);
    let detail = format!(
        "empty-scene floor: render_pipelines={} compute_pipelines={} shaders={} (ceilings {}/{}/{}); bloom/ssr off by default, decal/cluster/picking feature-gated off",
        c.render_pipelines,
        c.compute_pipelines,
        c.shaders,
        CENSUS_MAX_RENDER_PIPELINES,
        CENSUS_MAX_COMPUTE_PIPELINES,
        CENSUS_MAX_SHADERS
    );
    if c.render_pipelines > CENSUS_MAX_RENDER_PIPELINES
        || c.compute_pipelines > CENSUS_MAX_COMPUTE_PIPELINES
        || c.shaders > CENSUS_MAX_SHADERS
    {
        return Err(anyhow!("census over ceiling — {detail}"));
    }
    Ok(detail)
}

/// Checks 1+2 (+ extras) for one scene. Always emits the same set of lines so
/// the driver's total is deterministic; a failed load fails the dependent
/// checks with a "skipped" detail.
async fn run_scene(spec: &SceneSpec, origin: &str, report: &mut Report) {
    let load_name = format!("load-transaction:{}", spec.name);
    let counts_name = format!("counts:{}", spec.name);
    let extra_name = match spec.extra {
        Extra::None => None,
        Extra::Instancing => Some("instancing".to_string()),
        Extra::NaniteStreaming => Some("nanite-streaming".to_string()),
        Extra::LodTriDrop => Some("lod-tri-drop".to_string()),
    };

    match load_scene(spec, origin).await {
        Ok((mut renderer, loaded, authored, load_ms)) => {
            report.emit(
                &load_name,
                true,
                &format!(
                    "one begin→declare-all→commit load in {load_ms:.0}ms; {} nodes, {} prefab templates",
                    loaded.nodes.len(),
                    loaded.prefabs.len()
                ),
            );
            report.emit_result(
                &counts_name,
                counts_check(spec, &renderer, &loaded, authored),
            );
            if let Some(extra_name) = extra_name {
                let result = match spec.extra {
                    Extra::Instancing => instancing_check(&mut renderer).await,
                    Extra::NaniteStreaming => nanite_check(&mut renderer).await,
                    Extra::LodTriDrop => lod_tri_drop_check(&mut renderer).await,
                    Extra::None => unreachable!(),
                };
                report.emit_result(&extra_name, result);
            }
            destroy_renderer(renderer);
        }
        Err(err) => {
            report.emit(&load_name, false, &format!("{err:#}"));
            report.emit(&counts_name, false, "skipped — load failed");
            if let Some(extra_name) = extra_name {
                report.emit(&extra_name, false, "skipped — load failed");
            }
        }
    }
}

/// Cold-load one bundle into a fresh renderer via the player path, then drive
/// three sanity frames (a load that can't render is a failed load).
async fn load_scene(
    spec: &SceneSpec,
    origin: &str,
) -> Result<(AwsmRenderer, LoadedScene, usize, f64)> {
    let bundle_base = format!("{origin}/{}/bundle", spec.name);
    let scene = fetch_scene(&bundle_base).await?;
    let authored = count_authored_nodes(&scene.nodes);
    let (mut renderer, _canvas) = create_renderer((spec.features)()).await?;
    let assets = awsm_renderer_scene_loader::assets::HttpAssets::new(bundle_base);
    let t0 = now_ms();
    let loaded = load_scene_for_player(&mut renderer, &scene, &assets, |_| {}).await?;
    let load_ms = now_ms() - t0;
    renderer.update_transforms();
    let (center, radius) = scene_bounds(&renderer);
    run_frames(&mut renderer, center, radius, 3, |_| {
        orbit_eye(center, radius, 2.2, 0.8)
    })
    .await?;
    Ok((renderer, loaded, authored, load_ms))
}

/// Check 2 — mesh + node counts are >0 and match rough expectations (the
/// authored count parsed from the bundle's `scene.toml` bounds them above).
fn counts_check(
    spec: &SceneSpec,
    renderer: &AwsmRenderer,
    loaded: &LoadedScene,
    authored: usize,
) -> Result<String> {
    let nodes = loaded.nodes.len();
    let meshes = renderer.meshes.len();
    let tris = renderer.meshes.visible_triangle_count();
    let detail = format!(
        "nodes={nodes} (authored={authored}, expected ≥{}), renderer meshes={meshes}, visible_tris={tris}",
        spec.min_nodes
    );
    if nodes == 0 || nodes < spec.min_nodes {
        return Err(anyhow!("node count out of range — {detail}"));
    }
    if nodes > authored {
        return Err(anyhow!(
            "materialized more static nodes than authored — {detail}"
        ));
    }
    if spec.expect_meshes && meshes == 0 {
        return Err(anyhow!("no renderer meshes — {detail}"));
    }
    Ok(detail)
}

/// Check 3 — instancing-stress: steady-state frame time under budget across 60
/// frames, while the renderer-side mesh count stays tiny (thousands of
/// instances ride ONE instanced mesh row, not N mesh records).
async fn instancing_check(renderer: &mut AwsmRenderer) -> Result<String> {
    let meshes = renderer.meshes.len();
    let (center, radius) = scene_bounds(renderer);
    // 10 warmup + 60 measured frames, slow orbit to keep culling/LOD honest.
    let stamps = run_frames(renderer, center, radius, 70, |i| {
        orbit_eye(center, radius, 2.0, 0.8 + i as f32 * 0.01)
    })
    .await?;
    let deltas: Vec<f64> = stamps.windows(2).map(|w| w[1] - w[0]).collect();
    let measured = &deltas[10..];
    let avg = measured.iter().sum::<f64>() / measured.len() as f64;
    let max = measured.iter().cloned().fold(0.0, f64::max);
    let detail = format!(
        "steady-state avg {avg:.2}ms (max {max:.2}ms) over {} frames (budget {INSTANCING_FRAME_BUDGET_MS}ms); renderer meshes={meshes} (max {INSTANCING_MAX_MESHES})",
        measured.len()
    );
    if avg >= INSTANCING_FRAME_BUDGET_MS {
        return Err(anyhow!("frame budget blown — {detail}"));
    }
    if meshes >= INSTANCING_MAX_MESHES {
        return Err(anyhow!("instances materialized as mesh records — {detail}"));
    }
    Ok(detail)
}

/// Check 4 — lod-nanite under the streaming budget. The editor's `?stream` /
/// `?streambudget=N` flags are host-side sugar over
/// `RendererFeatures::cluster_streaming{,_budget}` (the loader's budget hook);
/// this harness reads the same flags and feeds the hook directly, so the
/// player path is exercised with or without them.
async fn nanite_check(renderer: &mut AwsmRenderer) -> Result<String> {
    let (center, radius) = scene_bounds(renderer);
    // Orbit in and out so the cluster cut re-selects across distances.
    let stamps = run_frames(renderer, center, radius, 30, |i| {
        let t = i as f32 / 30.0;
        orbit_eye(center, radius, 1.3 + 2.0 * t, 0.6 + t * 1.5)
    })
    .await?;
    let budget = match nanite_budget() {
        Some(n) => format!("{n} tris (?streambudget)"),
        None if url_has_flag("stream") => "loader default (?stream)".to_string(),
        None => "loader default".to_string(),
    };
    if renderer.meshes.is_empty() {
        return Err(anyhow!("no meshes after nanite load"));
    }
    Ok(format!(
        "cluster cut renders under streaming residency; budget={budget}; {} frames rendered; budget hook=RendererFeatures::cluster_streaming_budget",
        stamps.len() - 1
    ))
}

/// Check 7 — lod-classic: rendered/visible triangle count drops between a near
/// and a far camera (the same `visible_triangle_count` the editor status bar
/// reads via memory_stats).
/// "Near" means near the LOD'd OBJECT, not the scene: selection projects the
/// chain's object-space bake error by distance to the base mesh, and the
/// scene's bounds are floor-dominated (14×14 plane), so a whole-scene framing
/// sits past EVERY switch distance — near and far then select the same
/// (coarsest) level and the count never moves (this check's original
/// false-FAIL: 14,222 = sphere 12,288 + plane 2 + helmet LOD3 1,932 at both
/// 1.2× and 400× scene radius). So the camera frames the chain's base-mesh
/// world AABB (`renderer.lod` is the registry `update_lod_selection` walks) at
/// 2× its radius, then steps out until a coarser level engages; the assert is
/// on the selection reroute (level index rises) AND the triangle drop.
///
/// Calibration note (renderer-side finding, recorded by the detail line): the
/// bake's QEM-sqrt errors are so small (helmet: 4e-4..4.6e-3 object units,
/// radius 1.27) that at 600px/45° the level-1/2 switch distances (~0.3u/1.0u)
/// are INSIDE the mesh — LOD0/1 can never display from an exterior camera.
async fn lod_tri_drop_check(renderer: &mut AwsmRenderer) -> Result<String> {
    // The first registered chain = the LOD'd mesh under test.
    let Some((base_key, errors)) = renderer
        .lod
        .iter_mut()
        .map(|(k, c)| (k, c.levels.iter().map(|l| l.error).collect::<Vec<_>>()))
        .next()
    else {
        return Err(anyhow!(
            "no discrete LOD chain registered after load (renderer.lod empty)"
        ));
    };
    let aabb = renderer
        .meshes
        .get(base_key)
        .map_err(|e| anyhow!("LOD base mesh lookup: {e}"))?
        .world_aabb
        .clone()
        .ok_or_else(|| anyhow!("LOD base mesh has no world AABB"))?;
    let center = (aabb.min + aabb.max) * 0.5;
    let radius = ((aabb.max - aabb.min).length() * 0.5).max(0.1);
    let level = |renderer: &AwsmRenderer| {
        renderer
            .lod
            .get(base_key)
            .map(|c| c.current_level)
            .unwrap_or(0)
    };

    // Near: inspect-the-object framing at 1.2× its radius (with this bake's
    // tight error metrics the coarsest switch sits at ~2x the object radius —
    // see the calibration finding in the plan doc); 10 frames to settle.
    run_frames(renderer, center, radius, 10, |_| {
        orbit_eye(center, radius, 1.2, 0.8)
    })
    .await?;
    let near_tris = renderer.meshes.visible_triangle_count();
    let near_level = level(renderer);

    // Far: step out (same view direction) until a coarser level engages.
    let mut far_tris = near_tris;
    let mut far_level = near_level;
    let mut used_factor = 0.0f32;
    for factor in [8.0f32, 20.0, 60.0, 200.0] {
        run_frames(renderer, center, radius, 10, move |_| {
            orbit_eye(center, radius, factor, 0.8)
        })
        .await?;
        far_tris = renderer.meshes.visible_triangle_count();
        far_level = level(renderer);
        used_factor = factor;
        if far_level > near_level {
            break;
        }
    }
    let detail = format!(
        "level {near_level}→{far_level} of {} (chain errors {errors:?}), visible_tris near={near_tris} far={far_tris} (far at {used_factor}× object radius {radius:.2})",
        errors.len()
    );
    if near_tris == 0 {
        return Err(anyhow!("near camera submitted zero triangles — {detail}"));
    }
    if far_level <= near_level {
        return Err(anyhow!("selection never picked a coarser level — {detail}"));
    }
    if far_tris >= near_tris {
        return Err(anyhow!(
            "level rerouted but triangle count did not drop — {detail}"
        ));
    }
    Ok(detail)
}

/// Check 5 — prefab-churn: spawn/despawn a duplicated subtree ×N via the
/// scene-loader clone primitives (`PrefabTemplate::instantiate` /
/// `PrefabInstance::teardown`), asserting object counts return to baseline and
/// geometry uploads stay shared (resource count flat while instances live).
/// Falls back to load/unload ×5 of the whole bundle if the scene exposes no
/// prefab template.
async fn prefab_churn(origin: &str) -> Result<String> {
    let bundle_base = format!("{origin}/{PREFAB_CHURN_SCENE}/bundle");
    let scene = fetch_scene(&bundle_base).await?;
    let (mut renderer, _canvas) = create_renderer(base_features()).await?;
    let assets = awsm_renderer_scene_loader::assets::HttpAssets::new(bundle_base);

    let empty = counts(&renderer);
    let loaded = load_scene_for_player(&mut renderer, &scene, &assets, |_| {}).await?;
    renderer.update_transforms();
    let (center, radius) = scene_bounds(&renderer);
    run_frames(&mut renderer, center, radius, 3, |_| {
        orbit_eye(center, radius, 2.2, 0.8)
    })
    .await?;

    let result = if let Some((root, template)) = loaded.prefabs.iter().next() {
        let baseline = counts(&renderer);
        for i in 0..PREFAB_CHURN_CYCLES {
            let instance = template.instantiate(
                &mut renderer,
                Trs {
                    translation: [1.5 * (i % 5) as f32, 0.0, 1.5 * (i / 5) as f32],
                    ..Trs::IDENTITY
                },
            )?;
            // Render one frame with the instance live, and assert the clone
            // shared the template's geometry (no new uploads).
            run_frames(&mut renderer, center, radius, 1, |_| {
                orbit_eye(center, radius, 2.2, 0.8)
            })
            .await?;
            let live = counts(&renderer);
            if live.mesh_resources != baseline.mesh_resources
                || live.geometry_bytes != baseline.geometry_bytes
            {
                return Err(anyhow!(
                    "clone re-uploaded geometry on cycle {i}: resources {}→{}, bytes {}→{}",
                    baseline.mesh_resources,
                    live.mesh_resources,
                    baseline.geometry_bytes,
                    live.geometry_bytes
                ));
            }
            instance.teardown(&mut renderer);
        }
        let after = counts(&renderer);
        if after != baseline {
            return Err(anyhow!(
                "counts leaked across {PREFAB_CHURN_CYCLES} spawn/despawn cycles: {baseline:?} → {after:?}"
            ));
        }
        Ok(format!(
            "prefab root {root:?}: {PREFAB_CHURN_CYCLES} instantiate/teardown cycles, counts flat (meshes={}, transforms={}, geometry resources={}, bytes={})",
            baseline.meshes, baseline.transforms, baseline.mesh_resources, baseline.geometry_bytes
        ))
    } else {
        // No clone surface on this bundle — fall back to whole-bundle
        // load/unload cycles and assert everything returns to the empty
        // baseline (no leak).
        loaded.teardown(&mut renderer);
        for i in 0..LOAD_UNLOAD_CYCLES {
            let reloaded = load_scene_for_player(&mut renderer, &scene, &assets, |_| {}).await?;
            renderer.update_transforms();
            run_frames(&mut renderer, center, radius, 1, |_| {
                orbit_eye(center, radius, 2.2, 0.8)
            })
            .await?;
            reloaded.teardown(&mut renderer);
            let after = counts(&renderer);
            if after.meshes != empty.meshes || after.transforms != empty.transforms {
                return Err(anyhow!(
                    "load/unload cycle {i} leaked: {empty:?} → {after:?}"
                ));
            }
        }
        Ok(format!(
            "no prefab template in bundle — {LOAD_UNLOAD_CYCLES} load/unload cycles returned to baseline (meshes={}, transforms={})",
            empty.meshes, empty.transforms
        ))
    };
    destroy_renderer(renderer);
    result
}
