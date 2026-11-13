//! The checks themselves (docs/plans/007). Each prints one `PLAYER-TEST` line
//! through [`Report`]; the scene list + expectations are parametrized in
//! [`SCENES`].

use anyhow::{anyhow, Result};
use awsm_renderer::features::RendererFeatures;
use awsm_renderer::AwsmRenderer;
use awsm_renderer_scene::Trs;
use awsm_renderer_scene_loader::{load_scene_for_player, LoadedScene};

use crate::harness::{
    count_authored_nodes, create_renderer, create_renderer_with_msaa, destroy_renderer,
    fetch_scene, now_ms, orbit_eye, run_frames, scene_bounds, url_flag_value, url_has_flag,
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
    /// Minimum GPU pool-textures the scene must bind on-device after load. 0 for
    /// procedural/untextured scenes; > 0 locks the sprite/decal/particle
    /// silent-drop bug class (a dropped texture leaves the pool empty).
    expected_min_textures: usize,
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

/// Decals need their feature gate on (matching kitchen-sink), else the loader
/// cleanly skips the projected-decal texture and the texture-binding assert
/// would be a no-op.
fn decals_features() -> RendererFeatures {
    RendererFeatures {
        decals: true,
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
        // The bundle-player tier's reference scene (player goldens through
        // authored cameras) — the structural sweep here just locks its load:
        // 8 nodes (floor + 4 shapes + light + TWO Camera nodes).
        name: "player-cameras",
        min_nodes: 8,
        expect_meshes: true,
        expected_min_textures: 0,
        features: base_features,
        extra: Extra::None,
    },
    SceneSpec {
        name: "kitchen-sink",
        min_nodes: 10,
        expect_meshes: true,
        expected_min_textures: 0,
        features: kitchen_sink_features,
        extra: Extra::None,
    },
    SceneSpec {
        name: "anim-skinned",
        min_nodes: 3,
        expect_meshes: true,
        expected_min_textures: 0,
        features: base_features,
        extra: Extra::None,
    },
    SceneSpec {
        name: "lights-many",
        min_nodes: 40,
        expect_meshes: true,
        expected_min_textures: 0,
        features: base_features,
        extra: Extra::None,
    },
    SceneSpec {
        name: "lod-classic",
        min_nodes: 4,
        expect_meshes: true,
        expected_min_textures: 0,
        features: lod_features,
        extra: Extra::LodTriDrop,
    },
    SceneSpec {
        name: "lod-nanite",
        min_nodes: 3,
        expect_meshes: true,
        expected_min_textures: 0,
        features: nanite_features,
        extra: Extra::NaniteStreaming,
    },
    SceneSpec {
        // The A2 open-boundary lock: a cluster mesh with genuine open rims +
        // punched holes must render crack-free through the cut at every orbit
        // radius (the native lock is lod-bake's
        // open_mesh_cut_preserves_authored_boundaries_only; this exercises the
        // on-device GPU cut + streaming residency over the same bundle).
        name: "lod-nanite-open",
        min_nodes: 2,
        expect_meshes: true,
        expected_min_textures: 0,
        features: nanite_features,
        extra: Extra::NaniteStreaming,
    },
    SceneSpec {
        name: "instancing-stress",
        min_nodes: 4,
        expect_meshes: true,
        expected_min_textures: 0,
        features: base_features,
        extra: Extra::Instancing,
    },
    SceneSpec {
        // Isaac Sim robots (USD → awsm-renderer-isaac-export → the MuJoCo
        // sidecar seam): two sim instances, 82 geom nodes + floor + light.
        // Locks that a USD-sourced bundle loads through the player path, and
        // that ANYmal's 14 OmniPBR albedo maps (sidecar texture slots) reach
        // the texture pool.
        name: "isaac-robots",
        min_nodes: 80,
        expect_meshes: true,
        expected_min_textures: 14,
        features: base_features,
        extra: Extra::None,
    },
    SceneSpec {
        name: "prefab-skinned-morph",
        min_nodes: 3,
        expect_meshes: true,
        expected_min_textures: 0,
        features: base_features,
        extra: Extra::None,
    },
    SceneSpec {
        // Texture-binding lock: the decals bundle carries an opaque KTX2 (the
        // projected label sheet). It MUST transcode + bind on-device (the
        // opaque BC1/ETC2-RGB transcode rung); a silent drop leaves the pool
        // empty. Needs the decals feature or the loader skips the decal.
        name: "decals",
        min_nodes: 2,
        expect_meshes: true,
        expected_min_textures: 1,
        features: decals_features,
        extra: Extra::None,
    },
    SceneSpec {
        // Load-path + counts lock for the GPU particle emitter (procedural,
        // no external texture → expected_min_textures 0). Locks that a particle
        // node loads + materializes a mesh without tripping the census/leak
        // asserts.
        name: "particles",
        min_nodes: 2,
        expect_meshes: true,
        expected_min_textures: 0,
        features: base_features,
        extra: Extra::None,
    },
    SceneSpec {
        // The SPRITE half of the sprite/decal/particle texture-binding lock: a
        // particle emitter that binds a billboard SPRITE texture (baked to KTX2
        // in the bundle). The sprite texture MUST transcode + bind on-device —
        // a silent drop of the emitter's sampled texture leaves the pool short.
        name: "sprite",
        min_nodes: 2,
        expect_meshes: true,
        expected_min_textures: 1,
        features: base_features,
        extra: Extra::None,
    },
];

/// Scenes for the compiled-pipeline invariants (`pipelines-ready` +
/// `config-ensure-keeps-kernels`). Each one grows the material bucket set past
/// the first-party five (a custom material and/or a PBR feature-set variant),
/// which is the precondition for both bug classes these lock; the SSR-on ones
/// additionally kick a material compile BEFORE the load's commit.
const PIPELINE_SCENES: &[PipelineScene] = &[
    PipelineScene {
        name: "ssr-arena",
        features: base_features,
    },
    PipelineScene {
        name: "kitchen-sink",
        features: kitchen_sink_features,
    },
    PipelineScene {
        name: "dynamic-materials",
        features: base_features,
    },
    PipelineScene {
        name: "pbr-extensions",
        features: base_features,
    },
];

/// One scene of the compiled-pipeline invariant sweep.
struct PipelineScene {
    name: &'static str,
    features: fn() -> RendererFeatures,
}

/// The scene the `pick-after-mip-grow` check loads (any small one with no
/// transmissive material will do).
const PICK_SCENE: &str = "player-cameras";

/// The scene whose bundle carries `prefab = true` roots — the clone-surface
/// scene for the prefab-churn check.
const PREFAB_CHURN_SCENE: &str = "prefab-static";
const PREFAB_CHURN_SKINNED_SCENE: &str = "prefab-skinned-morph";

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

    // ── 8 + 9. compiled-pipeline invariants after ONE commit. ───────────────
    for scene in PIPELINE_SCENES {
        if let Some(filter) = &filter {
            if !filter.iter().any(|f| f == scene.name) {
                continue;
            }
        }
        set_hud(&format!("player-tests: pipelines {}", scene.name));
        pipeline_invariants(scene.name, scene.features, &origin, &mut report).await;
    }

    // ── 10. a pick that rebuilds the render textures must not strand the
    //        frame's bind groups on the old ones. ──────────────────────────────
    let pick_enabled = filter
        .as_ref()
        .map(|f| f.iter().any(|s| s == PICK_SCENE))
        .unwrap_or(true);
    if pick_enabled {
        set_hud("player-tests: pick-after-mip-grow");
        report.emit_result("pick-after-mip-grow", pick_after_mip_grow(&origin).await);
    }

    // ── 11. a material turned alpha-tested on a running scene cuts out
    //        without a commit. ─────────────────────────────────────────────────
    if pick_enabled {
        for (name, msaa) in [
            ("mask-without-commit", true),
            ("mask-without-commit:no-msaa", false),
        ] {
            set_hud(&format!("player-tests: {name}"));
            report.emit_result(name, mask_without_commit(&origin, msaa).await);
        }
    }

    // ── 12. a material assigned on a running scene shades without a commit. ─
    if pick_enabled {
        for (name, msaa) in [
            ("material-without-commit", true),
            ("material-without-commit:no-msaa", false),
        ] {
            set_hud(&format!("player-tests: {name}"));
            report.emit_result(name, material_without_commit(&origin, msaa).await);
        }
    }

    // ── 5. prefab-churn — spawn/despawn a duplicated subtree, no leak. ──────
    let churn_enabled = filter
        .as_ref()
        .map(|f| f.iter().any(|s| s == PREFAB_CHURN_SCENE))
        .unwrap_or(true);
    if churn_enabled {
        set_hud("player-tests: prefab-churn");
        report.emit_result(
            "prefab-churn",
            prefab_churn(&origin, PREFAB_CHURN_SCENE, false).await,
        );
    }

    // ── 5b. prefab-churn-skinned — same churn over a SKINNED template, which
    // additionally exercises the per-instance cloned skeleton (each spawn mints
    // a TransformKey per joint; teardown must free them or transforms leak). ──
    let churn_skinned_enabled = filter
        .as_ref()
        .map(|f| f.iter().any(|s| s == PREFAB_CHURN_SKINNED_SCENE))
        .unwrap_or(true);
    if churn_skinned_enabled {
        set_hud("player-tests: prefab-churn-skinned");
        report.emit_result(
            "prefab-churn-skinned",
            prefab_churn(&origin, PREFAB_CHURN_SKINNED_SCENE, true).await,
        );
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
    /// GPU pool-texture count (`Textures::resource_counts().0`). A scene that
    /// authors textures (decals KTX2, sampled materials, sprite sheets) must
    /// materialize them here — a silent-drop bug leaves this at 0.
    pool_textures: usize,
}

fn counts(renderer: &AwsmRenderer) -> Counts {
    let (pool_textures, _cubemaps, _samplers) = renderer.textures.resource_counts();
    Counts {
        meshes: renderer.meshes.len(),
        mesh_resources: renderer.meshes.resource_count(),
        geometry_bytes: renderer.meshes.geometry_pool_used_bytes(),
        transforms: renderer.transforms.len(),
        render_pipelines: renderer.pipelines.render.len(),
        compute_pipelines: renderer.pipelines.compute.len(),
        shaders: renderer.shaders.len(),
        pool_textures,
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
        // Scene-qualified: two nanite scenes (closed helmets + the A2 open
        // sheet) run this extra, and report lines must stay unique.
        Extra::NaniteStreaming => Some(format!("nanite-streaming:{}", spec.name)),
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
    let (pool_textures, cubemaps, _samplers) = renderer.textures.resource_counts();
    let detail = format!(
        "nodes={nodes} (authored={authored}, expected ≥{}), renderer meshes={meshes}, visible_tris={tris}, pool_textures={pool_textures} (expected ≥{}), cubemaps={cubemaps}",
        spec.min_nodes, spec.expected_min_textures
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
    // Texture-binding lock: a scene that authors textures must materialize them
    // in the GPU pool. A silent drop (sprite/decal/particle bug class) leaves
    // the pool short of what the bundle declares.
    if pool_textures < spec.expected_min_textures {
        return Err(anyhow!(
            "texture pool under-bound — a declared texture was silently dropped — {detail}"
        ));
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
/// Calibration note (RESOLVED): the bake's raw QEM-sqrt errors are ≈RMS
/// deviation and so small (helmet: 4e-4..4.6e-3 object units, radius 1.27)
/// that projecting them directly put the level-1/2 switches (~0.3u/1.0u)
/// INSIDE the mesh — LOD0/1 could never display from an exterior camera.
/// Selection now multiplies the baked error by
/// `AwsmRenderer::LOD_ERROR_CONSERVATISM` (16.0), putting the helmet switches
/// at ~4.6u/16u/53u ≈ 3.6/12.6/42 radii — the inspect-range camera below sees
/// the base mesh.
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

    // Near: inspect-the-object framing at 1.2× its radius — with the
    // calibrated selection (LOD_ERROR_CONSERVATISM) this sits well inside the
    // level-1 switch (~3.6× radius for the helmet bake), so the BASE mesh
    // draws here; 10 frames to settle.
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
/// prefab template — unless `require_skinned` is set, in which case a missing
/// template (or a template with no cloned skeleton) is a FAIL: the skinned
/// variant exists precisely to lock the per-instance joint-transform lifecycle.
async fn prefab_churn(origin: &str, scene_name: &str, require_skinned: bool) -> Result<String> {
    let bundle_base = format!("{origin}/{scene_name}/bundle");
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
            if require_skinned && instance.skin_joints.is_empty() {
                return Err(anyhow!(
                    "prefab template {root:?} in {scene_name} is not skinned (no cloned joints) — the skinned churn check is not exercising the joint lifecycle"
                ));
            }
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
    } else if require_skinned {
        Err(anyhow!(
            "{scene_name} exposes no prefab template — mark one duplicate prefab=true in its author.js and regenerate the bundle"
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

/// The material-pass pipelines the live scene dispatches, as pool keys: the
/// classify pipeline, the MSAA final blend, and one shading kernel per bucket
/// a live material routes to (plus the SKYBOX bucket at index 0).
#[derive(Clone, PartialEq, Debug)]
struct MaterialPipelines {
    buckets: usize,
    classify: Option<PoolKey>,
    final_blend: Option<PoolKey>,
    /// `(bucket index, kernel)` for every USED bucket; `None` = not installed.
    kernels: Vec<(usize, Option<PoolKey>)>,
    /// Bucket indices of FIRST-PARTY buckets no material resolves to that hold
    /// a kernel anyway. Must be empty after a cold load: nothing shades
    /// through them, so compiling their kernels is pure load time.
    idle_first_party_kernels: Vec<usize>,
}

/// A compute pipeline's handle in the renderer's shared pool.
type PoolKey = awsm_renderer::pipelines::compute_pipeline::ComputePipelineKey;

fn material_pipelines(renderer: &AwsmRenderer) -> MaterialPipelines {
    let aa = &renderer.anti_aliasing;
    let msaa = aa.msaa_sample_count;
    let entries = renderer.dynamic_materials.bucket_entries_cached();
    let classify = renderer
        .render_passes
        .material_classify
        .pipeline_cache
        .borrow()
        .get(&(renderer.dynamic_materials.dispatch_hash_cached(), msaa))
        .copied();
    let opaque = &renderer.render_passes.material_opaque;
    // Shader ids opaque-routed materials currently resolve to.
    let used: std::collections::HashSet<_> = renderer
        .materials
        .keys()
        .filter(|key| !renderer.materials.is_transparency_pass(*key))
        .map(|key| renderer.materials.shader_id(key))
        .collect();
    let kernel_of = |entry: &awsm_renderer::dynamic_materials::BucketEntry| {
        if msaa.is_some() {
            opaque
                .edge_pipelines
                .get_shade_pipeline_key(aa, entry.shader_id)
        } else {
            opaque
                .pipelines
                .get_compute_pipeline_key(aa, entry.shader_id)
        }
    };
    let kernels = entries
        .iter()
        .enumerate()
        .filter(|(index, entry)| *index == 0 || used.contains(&entry.shader_id))
        .map(|(index, entry)| (index, kernel_of(entry)))
        .collect();
    // A registered custom material always compiles (that is how its WGSL
    // errors surface before it is assigned), so only first-party buckets count.
    let idle_first_party_kernels = entries
        .iter()
        .enumerate()
        .filter(|(index, entry)| {
            *index != 0
                && !used.contains(&entry.shader_id)
                && renderer.dynamic_materials.get(entry.shader_id).is_none()
                && kernel_of(entry).is_some()
        })
        .map(|(index, _)| index)
        .collect();
    MaterialPipelines {
        buckets: entries.len(),
        classify,
        final_blend: if msaa.is_some() {
            opaque.edge_pipelines.final_blend_pipeline_key
        } else {
            None
        },
        kernels,
        idle_first_party_kernels,
    }
}

/// Every pipeline in the snapshot is installed AND compiled (a reserved pool
/// slot does not count).
fn material_pipelines_missing(renderer: &AwsmRenderer, p: &MaterialPipelines) -> Vec<String> {
    let compiled =
        |key: Option<PoolKey>| key.is_some_and(|k| renderer.pipelines.compute.get(k).is_ok());
    let mut missing = Vec::new();
    if !compiled(p.classify) {
        missing.push("classify".to_string());
    }
    if renderer.anti_aliasing.msaa_sample_count.is_some() && !compiled(p.final_blend) {
        missing.push("final_blend".to_string());
    }
    for (index, key) in &p.kernels {
        if !compiled(*key) {
            missing.push(format!("kernel[bucket {index}]"));
        }
    }
    missing
}

/// Checks 8 + 9 for one scene.
///
/// **8 `pipelines-ready`** — after the load's single `commit_load`, with no
/// further commit and before any frame, the classify pipeline, the final
/// blend and the kernel of every used bucket are installed — and NO kernel
/// exists for a first-party bucket nothing shades through (the canonical PBR
/// bucket, an unused Toon / FlipBook one). Locks the bug
/// where a classify compile kicked before the commit (an SSR-on scene's
/// `set_post_processing`) was dropped as stale by the commit's own layout
/// reset and never relaunched, so the scene drew nothing shaded until some
/// later commit happened to recompile it.
///
/// **9 `config-ensure-keeps-kernels`** — a standalone
/// `ensure_config_pipelines()` on a committed scene (what the editor's
/// shadow-denoise toggle does) leaves those pipelines untouched. Locks the bug
/// where it re-installed first-party kernels compiled for the FIRST-PARTY-ONLY
/// bucket layout over the live ones, so on a scene with more buckets the sky /
/// canonical buckets read the classify tile lists at the wrong offsets.
async fn pipeline_invariants(
    name: &str,
    features: fn() -> RendererFeatures,
    origin: &str,
    report: &mut Report,
) {
    let ready_name = format!("pipelines-ready:{name}");
    let ensure_name = format!("config-ensure-keeps-kernels:{name}");
    let loaded = async {
        let bundle_base = format!("{origin}/{name}/bundle");
        let scene = fetch_scene(&bundle_base).await?;
        let (mut renderer, _canvas) = create_renderer(features()).await?;
        let assets = awsm_renderer_scene_loader::assets::HttpAssets::new(bundle_base);
        load_scene_for_player(&mut renderer, &scene, &assets, |_| {}).await?;
        anyhow::Ok(renderer)
    }
    .await;
    let mut renderer = match loaded {
        Ok(renderer) => renderer,
        Err(err) => {
            report.emit(&ready_name, false, &format!("load failed — {err:#}"));
            report.emit(&ensure_name, false, "skipped — load failed");
            return;
        }
    };

    // ── 8. straight after the one commit. ──
    let before = material_pipelines(&renderer);
    let first_party = awsm_renderer::dynamic_materials::first_party_bucket_entries().len();
    let missing = material_pipelines_missing(&renderer, &before);
    let detail = format!(
        "{} buckets ({first_party} first-party), {} used kernels, msaa={:?}",
        before.buckets,
        before.kernels.len(),
        renderer.anti_aliasing.msaa_sample_count
    );
    if before.buckets <= first_party {
        report.emit(
            &ready_name,
            false,
            &format!("scene no longer grows the bucket set, so it locks nothing — {detail}"),
        );
    } else if !before.idle_first_party_kernels.is_empty() {
        report.emit(
            &ready_name,
            false,
            &format!(
                "kernels compiled for first-party buckets no material uses: {:?} — {detail}",
                before.idle_first_party_kernels
            ),
        );
    } else if missing.is_empty() {
        report.emit(&ready_name, true, &detail);
    } else {
        report.emit(
            &ready_name,
            false,
            &format!("not installed after the commit: {missing:?} — {detail}"),
        );
    }

    // ── 9. a standalone config ensure must not disturb them. ──
    let result = async {
        // Settle first, so this check is independent of check 8: one more
        // commit installs anything the first one left out.
        renderer
            .commit_load(|_| {})
            .await
            .map_err(|e| anyhow!("second commit_load: {e}"))?;
        let settled = material_pipelines(&renderer);
        let missing = material_pipelines_missing(&renderer, &settled);
        if !missing.is_empty() {
            return Err(anyhow!("still missing after a second commit: {missing:?}"));
        }
        renderer
            .ensure_config_pipelines()
            .await
            .map_err(|e| anyhow!("ensure_config_pipelines: {e}"))?;
        let after = material_pipelines(&renderer);
        if after != settled {
            let changed: Vec<usize> = settled
                .kernels
                .iter()
                .zip(&after.kernels)
                .filter(|(a, b)| a != b)
                .map(|(a, _)| a.0)
                .collect();
            return Err(anyhow!(
                "ensure_config_pipelines replaced live material pipelines: kernels of buckets {changed:?} changed (classify changed: {}, final_blend changed: {})",
                settled.classify != after.classify,
                settled.final_blend != after.final_blend
            ));
        }
        // And the scene still renders.
        renderer.update_transforms();
        let (center, radius) = scene_bounds(&renderer);
        run_frames(&mut renderer, center, radius, 2, |_| {
            orbit_eye(center, radius, 2.2, 0.8)
        })
        .await?;
        Ok(format!(
            "{} material pipelines unchanged across a standalone ensure_config_pipelines",
            settled.kernels.len() + 2
        ))
    }
    .await;
    report.emit_result(&ensure_name, result);
    destroy_renderer(renderer);
}

/// A canvas pixel showing a plain opaque PBR mesh of [`PICK_SCENE`], found by
/// picking: `(x, y, mesh, double_sided)`. The live-edit checks swap that
/// mesh's material and watch the pixel.
async fn opaque_pbr_pick_target(
    renderer: &mut AwsmRenderer,
    canvas: &web_sys::HtmlCanvasElement,
) -> Result<(i32, i32, awsm_renderer::meshes::MeshKey, bool)> {
    use awsm_renderer::materials::{Material, MaterialAlphaMode};
    use awsm_renderer::picker::PickResult;

    let (width, height) = (canvas.width() as f32, canvas.height() as f32);
    for fy in [0.5_f32, 0.4, 0.6, 0.3, 0.7] {
        for fx in [0.5_f32, 0.4, 0.6, 0.3, 0.7] {
            let (x, y) = ((width * fx) as i32, (height * fy) as i32);
            let PickResult::Hit(mesh_key) = renderer
                .pick(x, y)
                .await
                .map_err(|e| anyhow!("pick: {e}"))?
            else {
                continue;
            };
            let Ok(mesh) = renderer.meshes.get(mesh_key) else {
                continue;
            };
            let Ok(Material::Pbr(pbr)) = renderer.materials.get(mesh.material_key) else {
                continue;
            };
            if !mesh.instanced
                && matches!(pbr.alpha_mode(), MaterialAlphaMode::Opaque)
                && !pbr.has_transmission()
            {
                return Ok((x, y, mesh_key, pbr.double_sided()));
            }
        }
    }
    Err(anyhow!(
        "no pick over {PICK_SCENE} landed on an opaque PBR mesh — the check has nothing to edit"
    ))
}

/// The canvas pixel at `(x, y)` as 8-bit RGBA. Must be called in the same
/// task as the `render()` that drew it — a WebGPU canvas only holds the frame
/// until it is presented.
fn read_canvas_pixel(canvas: &web_sys::HtmlCanvasElement, x: i32, y: i32) -> Result<[u8; 4]> {
    let read = js_sys::Function::new_with_args(
        "canvas, x, y",
        "const c = document.createElement('canvas');
         c.width = canvas.width; c.height = canvas.height;
         const g = c.getContext('2d', { willReadFrequently: true });
         g.drawImage(canvas, 0, 0);
         return Array.from(g.getImageData(x, y, 1, 1).data);",
    );
    let rgba = read
        .call3(
            &wasm_bindgen::JsValue::NULL,
            canvas.as_ref(),
            &x.into(),
            &y.into(),
        )
        .map_err(|e| anyhow!("read canvas pixel: {e:?}"))?;
    let rgba = js_sys::Array::from(&rgba);
    let channel = |i: u32| rgba.get(i).as_f64().unwrap_or(0.0) as u8;
    Ok([channel(0), channel(1), channel(2), channel(3)])
}

/// How long `material-without-commit` gives a newly assigned material to
/// shade. On a renderer that compiles its kernel lazily that is a cold kernel
/// compile; the bug this locks is "never".
const MATERIAL_WITHOUT_COMMIT_DEADLINE_MS: f64 = 30_000.0;

/// Check 12 — `material-without-commit`. A kernel (the per-bucket opaque
/// shading pipeline) is compiled only for the buckets some material shades
/// through; `commit_load` settles that for everything loaded through it. A
/// running scene can also be handed a material of a bucket that has no kernel
/// yet, with no commit — an embedder that inserts a material and assigns it
/// with `set_mesh_material`:
///
/// - the scene's first material of a family (here Unlit), and
/// - any newly inserted PBR material, which shades through the canonical PBR
///   bucket until a commit routes it to its feature-set variant.
///
/// Both must shade, on their own, within a kernel compile.
///
/// Locks the regression where they did not: the mesh's pixels were left
/// unshaded until the next `commit_load`.
async fn material_without_commit(origin: &str, msaa: bool) -> Result<String> {
    use awsm_renderer::materials::pbr::PbrMaterial;
    use awsm_renderer::materials::unlit::UnlitMaterial;
    use awsm_renderer::materials::{Material, MaterialAlphaMode};

    let bundle_base = format!("{origin}/{PICK_SCENE}/bundle");
    let scene = fetch_scene(&bundle_base).await?;
    let features = RendererFeatures {
        picking: true,
        ..base_features()
    };
    let (mut renderer, canvas) = create_renderer_with_msaa(features, msaa).await?;
    let assets = awsm_renderer_scene_loader::assets::HttpAssets::new(bundle_base);
    load_scene_for_player(&mut renderer, &scene, &assets, |_| {}).await?;
    renderer.update_transforms();
    let (center, radius) = scene_bounds(&renderer);
    let eye = orbit_eye(center, radius, 2.2, 0.8);
    run_frames(&mut renderer, center, radius, 3, |_| eye).await?;
    let (x, y, mesh_key, double_sided) = opaque_pbr_pick_target(&mut renderer, &canvas).await?;

    // Unlit magenta, then PBR emitting pure green: each is unmistakable at the
    // pixel whatever the scene's lighting and tone mapping do.
    let mut unlit = UnlitMaterial::new(MaterialAlphaMode::Opaque, double_sided);
    unlit.base_color_factor = [1.0, 0.0, 1.0, 1.0];
    let mut pbr = PbrMaterial::new(MaterialAlphaMode::Opaque, double_sided);
    pbr.base_color_factor = [0.0, 0.0, 0.0, 1.0];
    pbr.emissive_factor = [0.0, 1.0, 0.0];
    fn is_magenta([r, g, b, _]: [u8; 4]) -> bool {
        r > g.saturating_add(60) && b > g.saturating_add(60)
    }
    fn is_green([r, g, b, _]: [u8; 4]) -> bool {
        g > r.saturating_add(60) && g > b.saturating_add(60)
    }
    type Shows = fn([u8; 4]) -> bool;
    let steps: [(&str, Material, Shows); 2] = [
        ("unlit magenta", Material::Unlit(unlit), is_magenta),
        ("PBR emissive green", Material::Pbr(Box::new(pbr)), is_green),
    ];

    let mut detail = Vec::new();
    for (label, material, shows) in steps {
        let key = renderer.materials.insert(
            material,
            &renderer.textures,
            &renderer.dynamic_materials,
            &renderer.extras_pool,
        );
        renderer
            .set_mesh_material(mesh_key, key)
            .map_err(|e| anyhow!("set_mesh_material: {e}"))?;

        let start = now_ms();
        let mut frames = 0usize;
        let mut pixel = [0u8; 4];
        let mut shaded = false;
        while now_ms() - start < MATERIAL_WITHOUT_COMMIT_DEADLINE_MS {
            crate::harness::next_frame().await;
            crate::harness::set_camera(&mut renderer, eye, center, radius)?;
            renderer.update_transforms();
            renderer.render(None).map_err(|e| anyhow!("render: {e}"))?;
            frames += 1;
            pixel = read_canvas_pixel(&canvas, x, y)?;
            if shows(pixel) {
                shaded = true;
                break;
            }
        }
        let elapsed_ms = now_ms() - start;
        if !shaded {
            destroy_renderer(renderer);
            return Err(anyhow!(
                "{label}: assigned with no commit and still not shaded after {frames} frames ({:.1}s) — pixel ({x},{y}) is rgba{pixel:?}",
                elapsed_ms / 1000.0
            ));
        }
        detail.push(format!(
            "{label} shaded after {frames} frames ({elapsed_ms:.0}ms), rgba{pixel:?}"
        ));
    }
    destroy_renderer(renderer);
    Ok(format!("no commit: {}", detail.join("; ")))
}

/// How long `mask-without-commit` waits for the lazily compiled masked
/// variants. A cold compile of them is a second or two; the bug this locks is
/// "never".
const MASK_WITHOUT_COMMIT_DEADLINE_MS: f64 = 30_000.0;

/// Check 11 — `mask-without-commit`. The masked (alpha-tested) geometry and
/// shadow pipelines are compiled only for the built-in families that have an
/// alpha-tested material; `commit_load` does that for everything loaded
/// through it. A running scene can also GAIN its first one with no commit —
/// an embedder editing materials live (`update_material`, or a new material
/// assigned with `set_mesh_material`). The render loop must then compile that
/// family's variants itself and cut the mesh out once they land.
///
/// Locks the regression where it did not: the mesh drew solid until the next
/// `commit_load`, however long that took.
///
/// End to end, through the picker: a visible opaque PBR mesh is given a
/// material that is alpha-tested and fully below its cutoff, so cut out means
/// gone — the pick at its pixel must stop returning it.
async fn mask_without_commit(origin: &str, msaa: bool) -> Result<String> {
    use awsm_renderer::materials::pbr::PbrMaterial;
    use awsm_renderer::materials::{Material, MaterialAlphaMode, MaterialShaderId};
    use awsm_renderer::picker::PickResult;

    let bundle_base = format!("{origin}/{PICK_SCENE}/bundle");
    let scene = fetch_scene(&bundle_base).await?;
    let features = RendererFeatures {
        picking: true,
        ..base_features()
    };
    let (mut renderer, canvas) = create_renderer_with_msaa(features, msaa).await?;
    let assets = awsm_renderer_scene_loader::assets::HttpAssets::new(bundle_base);
    load_scene_for_player(&mut renderer, &scene, &assets, |_| {}).await?;
    renderer.update_transforms();
    let (center, radius) = scene_bounds(&renderer);
    let eye = orbit_eye(center, radius, 2.2, 0.8);
    run_frames(&mut renderer, center, radius, 3, |_| eye).await?;

    let msaa = match renderer.anti_aliasing.msaa_sample_count {
        Some(4) => Some(4u32),
        _ => None,
    };
    let has_geometry_variant = |renderer: &AwsmRenderer| {
        renderer
            .render_passes
            .geometry
            .masked_pipelines
            .has_variant(msaa, MaterialShaderId::PBR)
    };
    if has_geometry_variant(&renderer) {
        return Err(anyhow!(
            "{PICK_SCENE} already has an alpha-tested PBR material — the check needs to introduce the first one itself"
        ));
    }

    let (x, y, mesh_key, double_sided) = opaque_pbr_pick_target(&mut renderer, &canvas).await?;

    // Alpha-tested, and everywhere below its cutoff. No commit follows.
    let mut cutout = PbrMaterial::new(MaterialAlphaMode::Mask { cutoff: 0.5 }, double_sided);
    cutout.base_color_factor = [1.0, 1.0, 1.0, 0.0];
    let cutout = renderer.materials.insert(
        Material::Pbr(Box::new(cutout)),
        &renderer.textures,
        &renderer.dynamic_materials,
        &renderer.extras_pool,
    );
    renderer
        .set_mesh_material(mesh_key, cutout)
        .map_err(|e| anyhow!("set_mesh_material: {e}"))?;

    let device: wasm_bindgen::JsValue = renderer.gpu.device.clone().into();
    let call = |name: &str, args: &js_sys::Array| -> Result<wasm_bindgen::JsValue> {
        let f: js_sys::Function = js_sys::Reflect::get(&device, &name.into())
            .map_err(|e| anyhow!("device.{name}: {e:?}"))?
            .into();
        js_sys::Reflect::apply(&f, &device, args).map_err(|e| anyhow!("device.{name}(): {e:?}"))
    };
    call("pushErrorScope", &js_sys::Array::of1(&"validation".into()))?;

    let start = now_ms();
    let mut frames = 0usize;
    let mut cut_out = false;
    while now_ms() - start < MASK_WITHOUT_COMMIT_DEADLINE_MS {
        run_frames(&mut renderer, center, radius, 2, |_| eye).await?;
        frames += 2;
        let picked = renderer
            .pick(x, y)
            .await
            .map_err(|e| anyhow!("pick: {e}"))?;
        if !matches!(picked, PickResult::Hit(hit) if hit == mesh_key) {
            cut_out = true;
            break;
        }
    }
    let elapsed_ms = now_ms() - start;
    // A few more frames with the masked pipelines live, still under the scope.
    run_frames(&mut renderer, center, radius, 4, |_| eye).await?;
    let error = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::from(call(
        "popErrorScope",
        &js_sys::Array::new(),
    )?))
    .await
    .map_err(|e| anyhow!("popErrorScope: {e:?}"))?;

    let geometry_variant = has_geometry_variant(&renderer);
    let shadow_variant = renderer
        .render_passes
        .shadow_masked
        .pipelines
        .has_variant(MaterialShaderId::PBR);
    destroy_renderer(renderer);

    if !cut_out {
        return Err(anyhow!(
            "mesh still drawn solid {frames} frames ({:.1}s) after its material turned alpha-tested with no commit (PBR masked variants compiled: geometry={geometry_variant}, shadow={shadow_variant})",
            elapsed_ms / 1000.0
        ));
    }
    if !geometry_variant || !shadow_variant {
        return Err(anyhow!(
            "mesh cut out but the PBR masked variants are incomplete: geometry={geometry_variant}, shadow={shadow_variant}"
        ));
    }
    if !(error.is_null() || error.is_undefined()) {
        let message = js_sys::Reflect::get(&error, &"message".into())
            .ok()
            .and_then(|m| m.as_string())
            .unwrap_or_else(|| format!("{error:?}"));
        return Err(anyhow!(
            "GPU validation error while the masked variants compiled and landed: {message}"
        ));
    }
    Ok(format!(
        "cut out {frames} frames ({elapsed_ms:.0}ms) after the material turned alpha-tested, no commit, no GPU validation error"
    ))
}

/// Check 10 — `pick-after-mip-grow`. The render textures are rebuilt lazily by
/// whoever asks for their views first after something that shapes them changed:
/// the surface size, the first HUD mesh, or — the case here — the first
/// transmissive material, which grows the opaque target's mip chain. The render
/// preamble is one such caller, and it rebuilds every dependent bind group when
/// that happens. The picker is the other: its first `pick()` compiles the
/// picker and reads the views itself.
///
/// Locks the bug where the picker getting there first swallowed the "views
/// were recreated" signal: the next frame saw nothing new, kept every bind
/// group on the previous textures, and three frames later — once those were
/// destroyed — every submit failed validation ("Destroyed texture "Opaque"
/// used in a submit"), forever. The editor hit it at boot whenever its warm-up
/// pick raced a project load that brought in a transmissive material. (A
/// surface resize takes the same path but is healed by accident when GPU
/// culling is on: the HZB resize marks the same bind groups.)
async fn pick_after_mip_grow(origin: &str) -> Result<String> {
    use awsm_renderer::materials::pbr::{PbrMaterial, PbrMaterialTransmission};
    use awsm_renderer::materials::{Material, MaterialAlphaMode};

    let bundle_base = format!("{origin}/{PICK_SCENE}/bundle");
    let scene = fetch_scene(&bundle_base).await?;
    let features = RendererFeatures {
        picking: true,
        ..base_features()
    };
    let (mut renderer, _canvas) = create_renderer(features).await?;
    let assets = awsm_renderer_scene_loader::assets::HttpAssets::new(bundle_base);
    load_scene_for_player(&mut renderer, &scene, &assets, |_| {}).await?;
    renderer.update_transforms();
    let (center, radius) = scene_bounds(&renderer);
    // Settle first, so the render textures exist in their no-transmission shape.
    run_frames(&mut renderer, center, radius, 3, |_| {
        orbit_eye(center, radius, 2.2, 0.8)
    })
    .await?;
    if renderer.materials.has_seen_transmission() {
        return Err(anyhow!(
            "{PICK_SCENE} already has a transmissive material — the check needs to introduce the first one itself"
        ));
    }

    // The first transmissive material (no mesh needs to use it: the flag is
    // what reshapes the opaque target) — then pick BEFORE the next frame, so
    // the picker's compile is the first reader of the render-texture views.
    let mut glass = PbrMaterial::new(MaterialAlphaMode::Opaque, false);
    glass.transmission = Some(PbrMaterialTransmission {
        tex: None,
        factor: 1.0,
    });
    renderer.materials.insert(
        Material::Pbr(Box::new(glass)),
        &renderer.textures,
        &renderer.dynamic_materials,
        &renderer.extras_pool,
    );
    renderer
        .pick(4, 4)
        .await
        .map_err(|e| anyhow!("pick: {e}"))?;

    // Enough frames for the retired textures to be destroyed, under a
    // validation error scope so a rejected submit is observable.
    let device: wasm_bindgen::JsValue = renderer.gpu.device.clone().into();
    let call = |name: &str, args: &js_sys::Array| -> Result<wasm_bindgen::JsValue> {
        let f: js_sys::Function = js_sys::Reflect::get(&device, &name.into())
            .map_err(|e| anyhow!("device.{name}: {e:?}"))?
            .into();
        js_sys::Reflect::apply(&f, &device, args).map_err(|e| anyhow!("device.{name}(): {e:?}"))
    };
    call("pushErrorScope", &js_sys::Array::of1(&"validation".into()))?;
    run_frames(&mut renderer, center, radius, 8, |_| {
        orbit_eye(center, radius, 2.2, 0.8)
    })
    .await?;
    let error = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::from(call(
        "popErrorScope",
        &js_sys::Array::new(),
    )?))
    .await
    .map_err(|e| anyhow!("popErrorScope: {e:?}"))?;
    destroy_renderer(renderer);
    if error.is_null() || error.is_undefined() {
        return Ok(
            "8 frames after a pick-triggered render-texture rebuild: no GPU validation error"
                .to_string(),
        );
    }
    let message = js_sys::Reflect::get(&error, &"message".into())
        .ok()
        .and_then(|m| m.as_string())
        .unwrap_or_else(|| format!("{error:?}"));
    Err(anyhow!(
        "GPU validation error in the frames after a pick rebuilt the render textures: {message}"
    ))
}
