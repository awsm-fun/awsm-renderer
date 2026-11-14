//! The checks themselves (docs/plans/007). Each prints one `PLAYER-TEST` line
//! through [`Report`]; the scene list + expectations are parametrized in
//! [`SCENES`].

use anyhow::{anyhow, Result};
use awsm_renderer::features::RendererFeatures;
use awsm_renderer::AwsmRenderer;
use awsm_renderer_scene::Trs;
use awsm_renderer_scene_loader::{load_scene_for_player, LoadedScene};

use crate::harness::{
    count_authored_nodes, create_renderer, create_renderer_with_msaa,
    create_renderer_with_specialization, destroy_renderer, fetch_scene, now_ms, orbit_eye,
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
        // Both shading paths: per-bucket edge kernels with MSAA, the plain
        // opaque kernel without.
        for msaa in [true, false] {
            set_hud(&format!(
                "player-tests: pipelines {}{}",
                scene.name,
                if msaa { "" } else { " (no MSAA)" }
            ));
            pipeline_invariants(scene.name, scene.features, &origin, &mut report, msaa).await;
        }
    }

    // ── 13. a post-process pass that cannot be compiled is turned off; the
    //        load goes on without it. ─────────────────────────────────────────
    for case in POSTPROCESS_FAILURES {
        if let Some(filter) = &filter {
            if !filter.iter().any(|f| f == case.scene) {
                continue;
            }
        }
        let name = format!("postprocess-compile-failure:{}", case.name);
        set_hud(&format!("player-tests: {name}"));
        report.emit_result(&name, postprocess_compile_failure(case, &origin).await);
    }

    let pick_enabled = filter
        .as_ref()
        .map(|f| f.iter().any(|s| s == PICK_SCENE))
        .unwrap_or(true);

    // ── 16. nothing is compiled, and waited for, ahead of the batch. ───────
    if pick_enabled {
        for case in ["blit", "ssr-composite", "brdf-lut", "upload-helpers"] {
            let name = format!("nothing-awaited-ahead-of-the-batch:{case}");
            set_hud(&format!("player-tests: {name}"));
            report.emit_result(&name, nothing_awaited_ahead_of_the_batch(case).await);
        }
    }

    // ── 14 + 15. compile failures and deferral around a committed / an
    //        uncommitted scene. ───────────────────────────────────────────────
    if pick_enabled {
        set_hud("player-tests: prep-compile-failure-keeps-rendering");
        report.emit_result(
            "prep-compile-failure-keeps-rendering",
            prep_compile_failure_keeps_rendering(&origin).await,
        );
        set_hud("player-tests: aa-before-commit-reserves");
        report.emit_result(
            "aa-before-commit-reserves",
            aa_before_commit_reserves().await,
        );
        set_hud("player-tests: aa-flip-before-commit-leaves-nothing");
        report.emit_result(
            "aa-flip-before-commit-leaves-nothing",
            aa_flip_before_commit_leaves_nothing().await,
        );
    }
    let toggle_enabled = filter
        .as_ref()
        .map(|f| f.iter().any(|s| s == "pbr-extensions"))
        .unwrap_or(true);
    if toggle_enabled {
        set_hud("player-tests: aa-toggle-pipelines-stable");
        report.emit_result(
            "aa-toggle-pipelines-stable",
            aa_toggle_pipelines_stable(&origin).await,
        );
    }

    // ── 19. a rejected SSR composite is not forgotten by a commit that
    //        failed for another reason. ───────────────────────────────────────
    let composite_retry_enabled = filter
        .as_ref()
        .map(|f| f.iter().any(|s| s == "ssr-arena"))
        .unwrap_or(true);
    if composite_retry_enabled {
        set_hud("player-tests: ssr-composite-failure-survives-a-failed-commit");
        report.emit_result(
            "ssr-composite-failure-survives-a-failed-commit",
            ssr_composite_failure_survives_a_failed_commit(&origin).await,
        );
    }

    // ── 20. PBR materials that differ only in cheap features share one
    //        kernel, and render as they do with a kernel each. Needs no
    //        bundle: the scene is built here. ─────────────────────────────────
    let feature_grid_enabled = filter
        .as_ref()
        .map(|f| f.iter().any(|s| s == FEATURE_GRID_SCENE))
        .unwrap_or(true);
    if feature_grid_enabled {
        for (name, msaa) in [
            ("pbr-cheap-features-share-a-kernel", true),
            ("pbr-cheap-features-share-a-kernel:no-msaa", false),
        ] {
            set_hud(&format!("player-tests: {name}"));
            report.emit_result(name, pbr_cheap_features_share_a_kernel(msaa).await);
        }
        // ── 21. a cheap feature gained on a running scene shows at once and
        //        costs no kernel. ──────────────────────────────────────────────
        for (name, msaa) in [
            ("pbr-cheap-feature-edit-keeps-kernels", true),
            ("pbr-cheap-feature-edit-keeps-kernels:no-msaa", false),
        ] {
            set_hud(&format!("player-tests: {name}"));
            report.emit_result(name, pbr_cheap_feature_edit_keeps_kernels(msaa).await);
        }
    }

    // ── 10. a pick that rebuilds the render textures must not strand the
    //        frame's bind groups on the old ones. ──────────────────────────────
    if pick_enabled {
        set_hud("player-tests: pick-after-mip-grow");
        report.emit_result("pick-after-mip-grow", pick_after_mip_grow(&origin).await);
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
    msaa: bool,
) {
    let suffix = if msaa { "" } else { ":no-msaa" };
    let ready_name = format!("pipelines-ready:{name}{suffix}");
    let ensure_name = format!("config-ensure-keeps-kernels:{name}{suffix}");
    let loaded = async {
        let bundle_base = format!("{origin}/{name}/bundle");
        let scene = fetch_scene(&bundle_base).await?;
        let (mut renderer, _canvas) = create_renderer_with_msaa(features(), msaa).await?;
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

/// One simulated compile failure: which pipelines the driver "rejects" while
/// `scene` loads, and what the load must look like afterwards.
struct PostprocessFailure {
    name: &'static str,
    scene: &'static str,
    /// Matched against the label of a compute pipeline's shader module.
    reject: &'static str,
    /// How many matching compiles to reject (the first N).
    limit: u32,
    expect: FailureOutcome,
}

#[derive(Clone, Copy)]
enum FailureOutcome {
    /// The load succeeds with SSR off (its pass gone) and bloom untouched.
    SsrOff,
    /// The load succeeds with bloom off (its pass gone) and SSR untouched.
    BloomOff,
    /// The load succeeds with the volumetric atmosphere off (its pass gone).
    VolumetricsOff,
    /// The load succeeds on the post-processing config the renderer was built
    /// with (the harness builds with the default).
    BuiltConfig,
    /// The load fails: not a post-process pipeline.
    LoadFails,
}

const POSTPROCESS_FAILURES: &[PostprocessFailure] = &[
    PostprocessFailure {
        name: "ssr",
        scene: "ssr-arena",
        reject: "^SSR ",
        limit: u32::MAX,
        expect: FailureOutcome::SsrOff,
    },
    // The SSR composite is a render pipeline outside the pools; it belongs to
    // the SSR pass all the same.
    PostprocessFailure {
        name: "ssr-composite",
        scene: "ssr-arena",
        reject: "^SSR Composite shader$",
        limit: u32::MAX,
        expect: FailureOutcome::SsrOff,
    },
    PostprocessFailure {
        name: "bloom",
        scene: "ssr-arena",
        reject: "^Bloom ",
        limit: u32::MAX,
        expect: FailureOutcome::BloomOff,
    },
    PostprocessFailure {
        name: "volumetrics",
        scene: "volumetrics",
        reject: "^Volumetrics ",
        limit: u32::MAX,
        expect: FailureOutcome::VolumetricsOff,
    },
    // The effects pass's bloom-BLEND variant fails (with bloom on the pass
    // compiles two variants; the blend one has the larger shader and the
    // batch is issued largest first, so it is the first "Effects" compile).
    // Only bloom dispatches it, so that costs bloom and nothing else.
    PostprocessFailure {
        name: "effects-bloom-blend",
        scene: "ssr-arena",
        reject: "^Effects$",
        limit: 1,
        expect: FailureOutcome::BloomOff,
    },
    // The effects pass's plain variant for the scene's config fails (no bloom
    // here, so it is the only one); the built config's variant, compiled next
    // as the fallback, does not.
    PostprocessFailure {
        name: "effects",
        scene: "volumetrics",
        reject: "^Effects$",
        limit: 1,
        expect: FailureOutcome::BuiltConfig,
    },
    // The control: a pipeline from the same reserved batch that is NOT
    // post-processing (a frame cannot be drawn without it) is still an error.
    PostprocessFailure {
        name: "material-prep",
        scene: "ssr-arena",
        reject: "^Material Prep$",
        limit: u32::MAX,
        expect: FailureOutcome::LoadFails,
    },
];

/// Makes `createComputePipelineAsync` / `createRenderPipelineAsync` reject the
/// first `limit` pipelines whose shader-module label matches `pattern`, the
/// way a driver that cannot build them would. Returns the (live) list of
/// labels it rejected. Undo with [`restore_compute_pipeline_compiles`].
fn reject_compute_pipeline_compiles(pattern: &str, limit: u32) -> Result<js_sys::Array> {
    let install = js_sys::Function::new_with_args(
        "pattern, limit",
        "const proto = GPUDevice.prototype;
         const re = new RegExp(pattern);
         const rejected = [];
         for (const name of ['createComputePipelineAsync', 'createRenderPipelineAsync']) {
           const saved = '__playerTests_' + name;
           if (!proto[saved]) proto[saved] = proto[name];
           const original = proto[saved];
           proto[name] = function (descriptor) {
             const stage = descriptor && (descriptor.compute || descriptor.vertex);
             const label = (stage && stage.module && stage.module.label) || '';
             if (rejected.length < limit && re.test(label)) {
               rejected.push(label);
               return Promise.reject(new GPUPipelineError(
                 'simulated compile failure: ' + label, { reason: 'internal' }));
             }
             return original.call(this, descriptor);
           };
         }
         return rejected;",
    );
    install
        .call2(
            &wasm_bindgen::JsValue::NULL,
            &pattern.into(),
            &f64::from(limit).into(),
        )
        // The hook's own array, not a copy: it fills as compiles are rejected.
        .map(wasm_bindgen::JsCast::unchecked_into::<js_sys::Array>)
        .map_err(|e| anyhow!("install compile-failure hook: {e:?}"))
}

/// Undoes every device hook this file installs.
fn restore_compute_pipeline_compiles() {
    let restore = js_sys::Function::new_no_args(
        "const proto = GPUDevice.prototype;
         for (const name of ['createComputePipelineAsync', 'createRenderPipelineAsync', 'createShaderModule']) {
           const saved = '__playerTests_' + name;
           if (proto[saved]) proto[name] = proto[saved];
         }",
    );
    let _ = restore.call0(&wasm_bindgen::JsValue::NULL);
}

/// Records every shader module and pipeline the device is asked for (label +
/// `performance.now()`), and holds back the RESOLUTION of pipelines whose
/// shader-module label matches `slow` by `delay_ms` — a compile that takes
/// that long. The pipeline is still issued at once. Undo with
/// [`restore_compute_pipeline_compiles`].
struct CompileTrace(wasm_bindgen::JsValue);

impl CompileTrace {
    fn install(slow: &str, delay_ms: u32) -> Result<Self> {
        let install = js_sys::Function::new_with_args(
            "slow, delayMs",
            "const proto = GPUDevice.prototype;
             const re = new RegExp(slow);
             const trace = { pipelines: [], modules: [] };
             for (const name of ['createComputePipelineAsync', 'createRenderPipelineAsync']) {
               const saved = '__playerTests_' + name;
               if (!proto[saved]) proto[saved] = proto[name];
               const original = proto[saved];
               proto[name] = function (descriptor) {
                 const stage = descriptor && (descriptor.compute || descriptor.vertex);
                 const label = (stage && stage.module && stage.module.label) || '';
                 trace.pipelines.push({ label, t: performance.now() });
                 const compile = original.call(this, descriptor);
                 if (delayMs > 0 && re.test(label)) {
                   return new Promise((resolve, reject) =>
                     setTimeout(() => compile.then(resolve, reject), delayMs));
                 }
                 return compile;
               };
             }
             const saved = '__playerTests_createShaderModule';
             if (!proto[saved]) proto[saved] = proto.createShaderModule;
             const original = proto[saved];
             proto.createShaderModule = function (descriptor) {
               trace.modules.push({ label: (descriptor && descriptor.label) || '', t: performance.now() });
               return original.call(this, descriptor);
             };
             return trace;",
        );
        install
            .call2(
                &wasm_bindgen::JsValue::NULL,
                &slow.into(),
                &f64::from(delay_ms).into(),
            )
            .map(Self)
            .map_err(|e| anyhow!("install compile trace: {e:?}"))
    }

    /// When the first `kind` (`"pipelines"` / `"modules"`) whose label matches
    /// `pattern` was asked for, in `performance.now()` ms.
    fn first(&self, kind: &str, pattern: &str) -> Option<f64> {
        let find = js_sys::Function::new_with_args(
            "trace, kind, pattern",
            "const re = new RegExp(pattern);
             const hit = trace[kind].find((entry) => re.test(entry.label));
             return hit ? hit.t : -1;",
        );
        find.call3(
            &wasm_bindgen::JsValue::NULL,
            &self.0,
            &kind.into(),
            &pattern.into(),
        )
        .ok()
        .and_then(|t| t.as_f64())
        .filter(|t| *t >= 0.0)
    }

    /// Every `kind` label recorded so far, sorted.
    fn labels(&self, kind: &str) -> Vec<String> {
        let labels = js_sys::Function::new_with_args(
            "trace, kind",
            "return trace[kind].map((entry) => entry.label).sort();",
        );
        labels
            .call2(&wasm_bindgen::JsValue::NULL, &self.0, &kind.into())
            .map(|labels| {
                js_sys::Array::from(&labels)
                    .iter()
                    .filter_map(|label| label.as_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// How many `kind` entries match `pattern`.
    fn count(&self, kind: &str, pattern: &str) -> usize {
        let count = js_sys::Function::new_with_args(
            "trace, kind, pattern",
            "const re = new RegExp(pattern);
             return trace[kind].filter((entry) => re.test(entry.label)).length;",
        );
        count
            .call3(
                &wasm_bindgen::JsValue::NULL,
                &self.0,
                &kind.into(),
                &pattern.into(),
            )
            .ok()
            .and_then(|n| n.as_f64())
            .unwrap_or(0.0) as usize
    }
}

/// How long the batch checks make one pipeline "take", and how soon the thing
/// that must NOT wait for it has to have happened. Far apart on purpose.
const SLOW_COMPILE_MS: u32 = 1500;
const NOT_WAITING_MS: f64 = 700.0;

/// Check 16 — `nothing-awaited-ahead-of-the-batch`. A cold load compiles as
/// one batch, issued as early as possible; a pipeline that is created and
/// AWAITED on its own before that batch delays everything behind it by its
/// whole compile. Four pipelines used to be: the two blit pipelines (awaited
/// in `build()` before the boot shader modules were even created), the SSR
/// composite (awaited in `set_post_processing`), the default BRDF LUT (awaited
/// at the top of the commit), and the sRGB / mipmap upload helpers (first
/// created, and awaited, by the upload that needs them).
///
/// Each case makes one of them take [`SLOW_COMPILE_MS`] and checks that the
/// step after it did not wait.
async fn nothing_awaited_ahead_of_the_batch(case: &str) -> Result<String> {
    let outcome = async {
        match case {
            // build(): the boot shader modules are created while the blit
            // pipelines compile, not after.
            "blit" => {
                let trace = CompileTrace::install("^Blit shader$", SLOW_COMPILE_MS)?;
                let start = now_ms();
                let (renderer, _canvas) = create_renderer(base_features()).await?;
                let built = now_ms() - start;
                let modules = trace.first("modules", "^Geometry$").map(|t| t - start);
                destroy_renderer(renderer);
                let modules = modules.ok_or_else(|| anyhow!("no Geometry module was created"))?;
                if modules > NOT_WAITING_MS {
                    return Err(anyhow!(
                        "the boot shader modules were created {modules:.0}ms into build(), after the {SLOW_COMPILE_MS}ms blit compile — build() waited for it first"
                    ));
                }
                Ok(format!(
                    "boot shader modules created {modules:.0}ms into a build() whose blit compile took {SLOW_COMPILE_MS}ms (build returned at {built:.0}ms)"
                ))
            }
            // set_post_processing before the commit returns without the
            // composite; the commit collects it.
            "ssr-composite" => {
                let (mut renderer, _canvas) = create_renderer(base_features()).await?;
                let trace = CompileTrace::install("^SSR Composite shader$", SLOW_COMPILE_MS)?;
                let mut post_processing = renderer.post_processing.clone();
                post_processing.ssr.enabled = true;
                let start = now_ms();
                let set = renderer.set_post_processing(post_processing).await;
                let setter = now_ms() - start;
                let commit = renderer.commit_load(|_| {}).await;
                let committed = now_ms() - start;
                let issued = trace.count("pipelines", "^SSR Composite shader$");
                let ssr_on = renderer.post_processing.ssr.enabled
                    && renderer.render_passes.ssr.is_some();
                let frames = run_frames(&mut renderer, glam::Vec3::ZERO, 4.0, 2, |_| {
                    glam::Vec3::new(0.0, 2.0, 6.0)
                })
                .await;
                destroy_renderer(renderer);
                set.map_err(|e| anyhow!("set_post_processing: {e}"))?;
                commit.map_err(|e| anyhow!("commit_load: {e}"))?;
                frames?;
                if issued == 0 {
                    return Err(anyhow!("no SSR composite pipeline was created"));
                }
                if setter > NOT_WAITING_MS {
                    return Err(anyhow!(
                        "set_post_processing before the commit took {setter:.0}ms — it waited for the {SLOW_COMPILE_MS}ms composite compile"
                    ));
                }
                if !ssr_on || committed < f64::from(SLOW_COMPILE_MS) * 0.9 {
                    return Err(anyhow!(
                        "the commit did not collect the composite: returned at {committed:.0}ms, SSR on with a pass = {ssr_on}"
                    ));
                }
                Ok(format!(
                    "set_post_processing returned in {setter:.0}ms with the composite still compiling; the commit collected it ({committed:.0}ms)"
                ))
            }
            // commit_load: the batch is issued while the default LUT's
            // pipeline compiles.
            "brdf-lut" => {
                let (mut renderer, _canvas) = create_renderer(base_features()).await?;
                let trace = CompileTrace::install("^BRDF Lut Shader$", SLOW_COMPILE_MS)?;
                let start = now_ms();
                let commit = renderer.commit_load(|_| {}).await;
                let committed = now_ms() - start;
                let batch = trace.first("pipelines", "^Geometry$").map(|t| t - start);
                let lut = trace.count("pipelines", "^BRDF Lut Shader$");
                destroy_renderer(renderer);
                commit.map_err(|e| anyhow!("commit_load: {e}"))?;
                let batch = batch.ok_or_else(|| anyhow!("the commit issued no Geometry pipeline"))?;
                if lut == 0 {
                    return Err(anyhow!("no BRDF LUT pipeline was created"));
                }
                if batch > NOT_WAITING_MS {
                    return Err(anyhow!(
                        "the commit issued its batch {batch:.0}ms in — after the {SLOW_COMPILE_MS}ms BRDF LUT compile, which it waited for first"
                    ));
                }
                if committed < f64::from(SLOW_COMPILE_MS) * 0.9 {
                    return Err(anyhow!(
                        "the commit returned at {committed:.0}ms without waiting for the LUT it started"
                    ));
                }
                Ok(format!(
                    "batch issued {batch:.0}ms into a commit whose BRDF LUT compile took {SLOW_COMPILE_MS}ms (commit returned at {committed:.0}ms)"
                ))
            }
            // The upload helpers start compiling when an image arrives, and
            // the plain-RGBA ones inside a cold load's batch regardless.
            "upload-helpers" => {
                use awsm_renderer::core::texture::mipmap::MipmapTextureKind;
                use awsm_renderer::core::texture::texture_pool::TextureColorInfo;
                const HELPERS: &str = "^(sRGB Conversion|Mipmap) Shader$";

                let (mut renderer, _canvas) = create_renderer(base_features()).await?;
                let trace = CompileTrace::install("^$", 0)?;
                let sampler = renderer
                    .textures
                    .get_sampler_key(&renderer.gpu, Default::default())
                    .map_err(|e| anyhow!("sampler: {e}"))?;
                // Rgba16float-free: the raw path is 8-bit RGBA, 64² so it has mips.
                let pixels = vec![200u8; 64 * 64 * 4];
                renderer
                    .textures
                    .add_image_rgba_raw(
                        &pixels,
                        64,
                        64,
                        sampler,
                        TextureColorInfo {
                            mipmap_kind: MipmapTextureKind::Albedo,
                            srgb_to_linear: true,
                            premultiplied_alpha: None,
                        },
                    )
                    .map_err(|e| anyhow!("add_image_rgba_raw: {e}"))?;
                // Two frames for the background tasks to reach the device; no
                // commit, no upload.
                crate::harness::next_frame().await;
                crate::harness::next_frame().await;
                let on_add = trace.count("pipelines", HELPERS);
                destroy_renderer(renderer);

                // And a load that adds no image at all.
                let (mut renderer, _canvas) = create_renderer(base_features()).await?;
                let commit = renderer.commit_load(|_| {}).await;
                let in_batch = trace.count("pipelines", HELPERS) - on_add;
                destroy_renderer(renderer);
                commit.map_err(|e| anyhow!("commit_load: {e}"))?;
                if on_add < 2 {
                    return Err(anyhow!(
                        "an sRGB image with mips was added and {on_add} upload-helper pipelines were started (expected the sRGB conversion and a mipmap one) — they wait for the upload"
                    ));
                }
                if in_batch < 3 {
                    return Err(anyhow!(
                        "a cold commit started {in_batch} plain-RGBA upload-helper pipelines (expected 3)"
                    ));
                }
                Ok(format!(
                    "{on_add} helper pipelines started when the image was added, {in_batch} more inside a cold commit's batch"
                ))
            }
            other => Err(anyhow!("unknown case {other}")),
        }
    }
    .await;
    restore_compute_pipeline_compiles();
    outcome
}

/// Check 13 — `postprocess-compile-failure`. A post-process pass whose
/// pipeline the driver rejects (an odd device) must cost the scene that
/// effect, not the load: the effect is turned off with a warning and
/// everything else loads and renders.
///
/// Everything a scene sets up before its commit only RESERVES pipelines, so
/// the rejection arrives inside `commit_load`'s batch. Locks the regression
/// where it failed the whole commit — and with it the scene load — where the
/// same rejection used to be a warning from `set_post_processing`.
///
/// The last case is the control: a rejected pipeline that is not
/// post-processing is still an error.
async fn postprocess_compile_failure(case: &PostprocessFailure, origin: &str) -> Result<String> {
    use awsm_renderer::post_process::{AtmosphereMode, PostProcessing};

    let bundle_base = format!("{origin}/{}/bundle", case.scene);
    let scene = fetch_scene(&bundle_base).await?;
    let (mut renderer, _canvas) = create_renderer(base_features()).await?;
    let assets = awsm_renderer_scene_loader::assets::HttpAssets::new(bundle_base);

    // Only the scene load runs against the failing driver.
    let rejected = reject_compute_pipeline_compiles(case.reject, case.limit)?;
    let loaded = load_scene_for_player(&mut renderer, &scene, &assets, |_| {}).await;
    restore_compute_pipeline_compiles();
    let rejected = rejected.length();
    if rejected == 0 {
        destroy_renderer(renderer);
        return Err(anyhow!(
            "no pipeline matched /{}/ — the check simulated nothing",
            case.reject
        ));
    }

    if let FailureOutcome::LoadFails = case.expect {
        destroy_renderer(renderer);
        return match loaded {
            Err(err) => Ok(format!(
                "{rejected} non-post-process compiles rejected: the load fails — {err:#}"
            )),
            Ok(_) => Err(anyhow!(
                "{rejected} non-post-process compiles rejected, yet the load reported success"
            )),
        };
    }
    if let Err(err) = loaded {
        destroy_renderer(renderer);
        return Err(anyhow!(
            "{rejected} compiles rejected and the whole load failed — {err:#}"
        ));
    }

    let pp = &renderer.post_processing;
    let passes = &renderer.render_passes;
    let state = format!(
        "ssr={} (pass {}), bloom={} (pass {}), atmosphere={:?} (volumetrics pass {})",
        pp.ssr.enabled,
        passes.ssr.is_some(),
        pp.bloom,
        passes.bloom.is_some(),
        pp.atmosphere.mode,
        passes.volumetrics.is_some(),
    );
    let as_expected = match case.expect {
        FailureOutcome::SsrOff => {
            !pp.ssr.enabled && passes.ssr.is_none() && pp.bloom && passes.bloom.is_some()
        }
        FailureOutcome::BloomOff => {
            !pp.bloom && passes.bloom.is_none() && pp.ssr.enabled && passes.ssr.is_some()
        }
        FailureOutcome::VolumetricsOff => {
            pp.atmosphere.mode == AtmosphereMode::Off && passes.volumetrics.is_none()
        }
        FailureOutcome::BuiltConfig => *pp == PostProcessing::default(),
        FailureOutcome::LoadFails => unreachable!("handled above"),
    };
    if !as_expected {
        destroy_renderer(renderer);
        return Err(anyhow!(
            "{rejected} compiles rejected; the load succeeded but left {state}"
        ));
    }
    // Everything else the scene needs is compiled…
    let missing = material_pipelines_missing(&renderer, &material_pipelines(&renderer));
    if !missing.is_empty() {
        destroy_renderer(renderer);
        return Err(anyhow!(
            "{rejected} compiles rejected; material pipelines not installed after the load: {missing:?}"
        ));
    }
    // …and it renders, cleanly.
    renderer.update_transforms();
    let (center, radius) = scene_bounds(&renderer);
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
    if !(error.is_null() || error.is_undefined()) {
        let message = js_sys::Reflect::get(&error, &"message".into())
            .ok()
            .and_then(|m| m.as_string())
            .unwrap_or_else(|| format!("{error:?}"));
        return Err(anyhow!(
            "{rejected} compiles rejected; the load succeeded ({state}) but rendering raised a GPU validation error: {message}"
        ));
    }
    Ok(format!(
        "{rejected} compiles rejected; loaded with {state}; 8 frames, no GPU validation error"
    ))
}

/// Check 19 — `ssr-composite-failure-survives-a-failed-commit`. The SSR
/// composite's compile is left running until the commit collects it. If the
/// driver rejects it AND that same commit fails for an unrelated reason (a
/// rejected pipeline outside post-processing is the commit's error), the
/// commit never gets to turn SSR off. The retry must still know the composite
/// failed.
///
/// Locks the bug where it did not: the composite reported its rejection once,
/// the retried commit saw nothing wrong and succeeded with SSR on, and the
/// composite — the pass's last step — silently never drew.
async fn ssr_composite_failure_survives_a_failed_commit(origin: &str) -> Result<String> {
    const SCENE: &str = "ssr-arena";
    let bundle_base = format!("{origin}/{SCENE}/bundle");
    let scene = fetch_scene(&bundle_base).await?;
    let (mut renderer, _canvas) = create_renderer(base_features()).await?;
    let assets = awsm_renderer_scene_loader::assets::HttpAssets::new(bundle_base);

    // The first commit meets a driver that rejects the composite and material
    // prep: the second makes it fail outright.
    let rejected =
        reject_compute_pipeline_compiles("^(Material Prep|SSR Composite shader)$", u32::MAX)?;
    let first = load_scene_for_player(&mut renderer, &scene, &assets, |_| {}).await;
    restore_compute_pipeline_compiles();
    let rejected: Vec<String> = rejected.iter().filter_map(|l| l.as_string()).collect();
    if !rejected.iter().any(|l| l == "SSR Composite shader")
        || !rejected.iter().any(|l| l == "Material Prep")
    {
        destroy_renderer(renderer);
        return Err(anyhow!(
            "expected both a composite and a material-prep compile to be rejected, got {rejected:?}"
        ));
    }
    if first.is_ok() {
        destroy_renderer(renderer);
        return Err(anyhow!(
            "the load succeeded although material prep was rejected — the check set nothing up"
        ));
    }

    // The driver is healthy again; the same commit is retried.
    let retry = renderer.commit_load(|_| {}).await;
    let ssr_enabled = renderer.post_processing.ssr.enabled;
    let ssr_pass = renderer.render_passes.ssr.is_some();
    let frames = match &retry {
        Ok(_) => {
            renderer.update_transforms();
            let (center, radius) = scene_bounds(&renderer);
            run_frames(&mut renderer, center, radius, 4, |_| {
                orbit_eye(center, radius, 2.2, 0.8)
            })
            .await
            .map(|_| ())
        }
        Err(_) => Ok(()),
    };
    destroy_renderer(renderer);
    retry.map_err(|e| anyhow!("the retried commit failed: {e}"))?;
    frames?;
    if ssr_enabled || ssr_pass {
        return Err(anyhow!(
            "the retried commit succeeded with ssr.enabled={ssr_enabled} and an SSR pass={ssr_pass}, although its composite was rejected — the reflection would never be composited"
        ));
    }
    Ok(
        "first commit failed on material prep with the composite rejected too; the retry committed with SSR off"
            .to_string(),
    )
}

/// Check 14 — `prep-compile-failure-keeps-rendering`. A config change that
/// re-keys the material-prep pipelines (here the SSCS step count) is compiled
/// by the next `commit_load`. On a scene that is already on screen, a compile
/// the driver rejects must leave the pass on the pipelines it was rendering
/// with: the commit reports the error and the frames go on.
///
/// Locks the regression where the new, uncompiled keys were installed before
/// the compile: after a failure the pass held keys with no pipeline behind
/// them, and every frame from then on failed.
async fn prep_compile_failure_keeps_rendering(origin: &str) -> Result<String> {
    let bundle_base = format!("{origin}/{PICK_SCENE}/bundle");
    let scene = fetch_scene(&bundle_base).await?;
    let (mut renderer, _canvas) = create_renderer(base_features()).await?;
    let assets = awsm_renderer_scene_loader::assets::HttpAssets::new(bundle_base);
    load_scene_for_player(&mut renderer, &scene, &assets, |_| {}).await?;
    renderer.update_transforms();
    let (center, radius) = scene_bounds(&renderer);
    let eye = orbit_eye(center, radius, 2.2, 0.8);
    run_frames(&mut renderer, center, radius, 3, |_| eye).await?;

    // Re-key the prep megashader, then commit against a driver that rejects it.
    let mut shadows = renderer.shadows_config().clone();
    shadows.sscs_step_count += 1;
    renderer.set_shadows_config(shadows);
    let rejected = reject_compute_pipeline_compiles("^Material Prep$", u32::MAX)?;
    let commit = renderer.commit_load(|_| {}).await;
    restore_compute_pipeline_compiles();
    let rejected = rejected.length();
    if rejected == 0 {
        destroy_renderer(renderer);
        return Err(anyhow!(
            "the SSCS change compiled no material-prep pipeline — the check simulated nothing"
        ));
    }
    let Err(commit_err) = commit else {
        destroy_renderer(renderer);
        return Err(anyhow!(
            "{rejected} material-prep compiles rejected, yet commit_load reported success"
        ));
    };

    // The scene must keep rendering through what it had.
    let device: wasm_bindgen::JsValue = renderer.gpu.device.clone().into();
    let call = |name: &str, args: &js_sys::Array| -> Result<wasm_bindgen::JsValue> {
        let f: js_sys::Function = js_sys::Reflect::get(&device, &name.into())
            .map_err(|e| anyhow!("device.{name}: {e:?}"))?
            .into();
        js_sys::Reflect::apply(&f, &device, args).map_err(|e| anyhow!("device.{name}(): {e:?}"))
    };
    call("pushErrorScope", &js_sys::Array::of1(&"validation".into()))?;
    let frames = run_frames(&mut renderer, center, radius, 8, |_| eye).await;
    let error = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::from(call(
        "popErrorScope",
        &js_sys::Array::new(),
    )?))
    .await
    .map_err(|e| anyhow!("popErrorScope: {e:?}"))?;
    if let Err(err) = frames {
        destroy_renderer(renderer);
        return Err(anyhow!(
            "frames fail after a rejected live material-prep compile — {err:#}"
        ));
    }
    if !(error.is_null() || error.is_undefined()) {
        let message = js_sys::Reflect::get(&error, &"message".into())
            .ok()
            .and_then(|m| m.as_string())
            .unwrap_or_else(|| format!("{error:?}"));
        destroy_renderer(renderer);
        return Err(anyhow!(
            "GPU validation error in the frames after a rejected live material-prep compile: {message}"
        ));
    }

    // And once the driver accepts it, the same commit goes through.
    let retry = renderer.commit_load(|_| {}).await;
    let after = match retry {
        Ok(_) => run_frames(&mut renderer, center, radius, 4, |_| eye).await,
        Err(err) => Err(anyhow!("commit_load retry: {err}")),
    };
    destroy_renderer(renderer);
    after?;
    Ok(format!(
        "{rejected} material-prep compiles rejected: commit_load returned the error ({commit_err}), 8 frames rendered cleanly on the previous pipelines, and a retry committed"
    ))
}

/// Check 15 — `aa-before-commit-reserves`. Until the first commit every
/// setter only RESERVES its pipelines, so the commit can compile the whole
/// load as one batch. `set_anti_aliasing` must not be the exception: called
/// while loading it must compile nothing — neither its own pipelines nor
/// whatever was reserved before it.
///
/// Locks the regression where it drained the reserved pool on the spot: a
/// compile wave of its own in the middle of a load, and a post-process
/// pipeline the driver rejects failed the setter instead of being turned off
/// by the commit.
async fn aa_before_commit_reserves() -> Result<String> {
    use awsm_renderer::anti_alias::AntiAliasing;

    let (mut renderer, _canvas) = create_renderer(base_features()).await?;
    let pending = |renderer: &AwsmRenderer| {
        renderer.pipelines.compute.pending_count() + renderer.pipelines.render.pending_count()
    };
    // Something reserved that the driver will reject: SSR.
    let mut post_processing = renderer.post_processing.clone();
    post_processing.ssr.enabled = true;
    renderer
        .set_post_processing(post_processing)
        .await
        .map_err(|e| anyhow!("set_post_processing: {e}"))?;
    let reserved_before = pending(&renderer);

    // The pass's POOL pipelines. (Its composite is outside the pools and is
    // deliberately issued — not awaited — as soon as the pass is built.)
    let rejected =
        reject_compute_pipeline_compiles("^SSR (Trace|Resolve|Temporal|BVH Trace)$", u32::MAX)?;
    let set_aa = renderer
        .set_anti_aliasing(AntiAliasing {
            msaa_sample_count: None,
            ..Default::default()
        })
        .await;
    let compiled_by_setter = reserved_before.saturating_sub(pending(&renderer));
    let rejected_by_setter = rejected.length();
    // The commit is where the batch compiles — and where SSR is turned off.
    let commit = renderer.commit_load(|_| {}).await;
    restore_compute_pipeline_compiles();
    let rejected_total = rejected.length();
    let ssr_enabled = renderer.post_processing.ssr.enabled;
    let left_reserved = pending(&renderer);
    destroy_renderer(renderer);

    if let Err(err) = set_aa {
        return Err(anyhow!(
            "set_anti_aliasing before the first commit failed (it compiled the reserved pool: {rejected_by_setter} SSR compiles rejected) — {err}"
        ));
    }
    if rejected_by_setter > 0 || compiled_by_setter > 0 {
        return Err(anyhow!(
            "set_anti_aliasing before the first commit compiled pipelines ({compiled_by_setter} reserved slots filled, {rejected_by_setter} rejected) instead of leaving them to the commit"
        ));
    }
    commit.map_err(|e| anyhow!("commit_load: {e}"))?;
    if rejected_total == 0 || ssr_enabled || left_reserved > 0 {
        return Err(anyhow!(
            "after the commit: {rejected_total} SSR compiles rejected, ssr.enabled={ssr_enabled}, {left_reserved} slots still reserved (expected >0, false, 0)"
        ));
    }
    Ok(format!(
        "set_anti_aliasing compiled nothing with {reserved_before} slots reserved; the commit compiled the batch and turned SSR off ({rejected_total} compiles rejected)"
    ))
}

/// Check 17 — `aa-flip-before-commit-leaves-nothing`. A renderer is built
/// for one MSAA mode, and `build()` reserves that mode's pipelines. An
/// embedder that switches mode before the first commit must end up compiling
/// what a renderer BUILT in the new mode compiles — not that plus the
/// pipelines of the mode it left.
///
/// Locks the leftover: the old mode's nine geometry pipelines and HZB seed
/// stayed reserved and the commit compiled them for nothing.
async fn aa_flip_before_commit_leaves_nothing() -> Result<String> {
    use awsm_renderer::anti_alias::AntiAliasing;

    let pools = |renderer: &AwsmRenderer| {
        (
            renderer.pipelines.compute.len(),
            renderer.pipelines.render.len(),
        )
    };
    let mode = |msaa: bool| if msaa { "MSAA" } else { "no MSAA" };
    let mut detail = Vec::new();
    for to_msaa in [false, true] {
        // Built in the target mode.
        let trace = CompileTrace::install("^$", 0)?;
        let (mut built, _canvas) = create_renderer_with_msaa(base_features(), to_msaa).await?;
        let commit = built.commit_load(|_| {}).await;
        let expected = pools(&built);
        destroy_renderer(built);
        let expected_compiles = trace.labels("pipelines");
        commit.map_err(|e| anyhow!("commit_load (built in the target mode): {e}"))?;

        // Built in the other mode, switched before the commit.
        let trace = CompileTrace::install("^$", 0)?;
        let (mut flipped, _canvas) = create_renderer_with_msaa(base_features(), !to_msaa).await?;
        let set = flipped
            .set_anti_aliasing(AntiAliasing {
                msaa_sample_count: to_msaa.then_some(4),
                ..Default::default()
            })
            .await;
        let commit = match set {
            Ok(()) => flipped.commit_load(|_| {}).await.map(|_| ()),
            Err(err) => Err(err),
        };
        let got = pools(&flipped);
        let frames = run_frames(&mut flipped, glam::Vec3::ZERO, 4.0, 2, |_| {
            glam::Vec3::new(0.0, 2.0, 6.0)
        })
        .await;
        destroy_renderer(flipped);
        let mut extra = trace.labels("pipelines");
        restore_compute_pipeline_compiles();
        commit.map_err(|e| anyhow!("set_anti_aliasing + commit_load: {e}"))?;
        frames?;
        // What the switched renderer compiled that the built one did not.
        for label in &expected_compiles {
            if let Some(at) = extra.iter().position(|l| l == label) {
                extra.remove(at);
            }
        }
        if got != expected || !extra.is_empty() {
            return Err(anyhow!(
                "built {} then switched to {} before the commit: {} compute + {} render pipelines in the pools, but a renderer built {} has {} + {}; compiled on top of what that one compiles: {extra:?}",
                mode(!to_msaa),
                mode(to_msaa),
                got.0,
                got.1,
                mode(to_msaa),
                expected.0,
                expected.1
            ));
        }
        detail.push(format!(
            "{} -> {}: {} compute + {} render, same as built {}",
            mode(!to_msaa),
            mode(to_msaa),
            got.0,
            got.1,
            mode(to_msaa)
        ));
    }
    Ok(detail.join("; "))
}

/// Check 18 — `aa-toggle-pipelines-stable`. On a committed scene both MSAA
/// modes are kept once compiled, so toggling back and forth must stop
/// creating pipelines after the first round trip: the second and third are
/// cache hits.
///
/// Locks the leak where every toggle rebuilt every transparent mesh's
/// pipeline and left the previous one in the pool — the transparent shaders
/// were wrongly swept as "stale" at each re-layout, so the same pipeline came
/// back under a new shader key. The editor grew by six render pipelines per
/// MSAA toggle on a scene with three transparent meshes.
async fn aa_toggle_pipelines_stable(origin: &str) -> Result<String> {
    use awsm_renderer::anti_alias::AntiAliasing;
    const SCENE: &str = "pbr-extensions";

    let bundle_base = format!("{origin}/{SCENE}/bundle");
    let scene = fetch_scene(&bundle_base).await?;
    let (mut renderer, _canvas) = create_renderer(base_features()).await?;
    let assets = awsm_renderer_scene_loader::assets::HttpAssets::new(bundle_base);
    load_scene_for_player(&mut renderer, &scene, &assets, |_| {}).await?;
    renderer.update_transforms();
    let (center, radius) = scene_bounds(&renderer);
    let eye = orbit_eye(center, radius, 2.2, 0.8);
    let transparent = renderer
        .meshes
        .iter()
        .filter(|(_, mesh)| renderer.materials.is_transparency_pass(mesh.material_key))
        .count();
    if transparent == 0 {
        destroy_renderer(renderer);
        return Err(anyhow!(
            "{SCENE} has no transparent mesh — the check exercises nothing"
        ));
    }

    let census = |renderer: &AwsmRenderer| {
        (
            renderer.pipelines.compute.len(),
            renderer.pipelines.render.len(),
            renderer.shaders.len(),
        )
    };
    let mut rounds = Vec::new();
    for _ in 0..3 {
        for msaa in [None, Some(4)] {
            renderer
                .set_anti_aliasing(AntiAliasing {
                    msaa_sample_count: msaa,
                    ..Default::default()
                })
                .await
                .map_err(|e| anyhow!("set_anti_aliasing({msaa:?}): {e}"))?;
            renderer
                .commit_load(|_| {})
                .await
                .map_err(|e| anyhow!("commit_load after set_anti_aliasing({msaa:?}): {e}"))?;
            run_frames(&mut renderer, center, radius, 2, |_| eye).await?;
        }
        rounds.push(census(&renderer));
    }
    destroy_renderer(renderer);
    if rounds[1] != rounds[0] || rounds[2] != rounds[0] {
        return Err(anyhow!(
            "{transparent} transparent meshes; (compute, render, shaders) after each MSAA off/on round trip: {rounds:?} — later round trips still create pipelines"
        ));
    }
    Ok(format!(
        "{transparent} transparent meshes; (compute, render, shaders) = {:?} after each of three MSAA off/on round trips",
        rounds[0]
    ))
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

/// The whole canvas as 8-bit RGBA, row-major from the top-left. Same rule as
/// [`read_canvas_pixel`]: call it in the task of the `render()` that drew it.
fn read_canvas(canvas: &web_sys::HtmlCanvasElement) -> Result<Vec<u8>> {
    let read = js_sys::Function::new_with_args(
        "canvas",
        "const c = document.createElement('canvas');
         c.width = canvas.width; c.height = canvas.height;
         const g = c.getContext('2d', { willReadFrequently: true });
         g.drawImage(canvas, 0, 0);
         return g.getImageData(0, 0, c.width, c.height).data;",
    );
    let data = read
        .call1(&wasm_bindgen::JsValue::NULL, canvas.as_ref())
        .map_err(|e| anyhow!("read canvas: {e:?}"))?;
    Ok(js_sys::Uint8ClampedArray::new(&data).to_vec())
}

/// The name `?scenes=` knows the `pbr-cheap-features-share-a-kernel` scene by
/// (it is built in code, not loaded from a bundle).
const FEATURE_GRID_SCENE: &str = "pbr-feature-grid";

/// One material of the `pbr-cheap-features-share-a-kernel` scene.
struct FeatureCase {
    name: &'static str,
    /// Whether the material carries an EXPENSIVE feature (here: clearcoat),
    /// i.e. keeps a kernel apart from the cheap-only ones by default.
    expensive: bool,
    edit: fn(&mut awsm_renderer::materials::pbr::PbrMaterial),
}

/// Ten opaque PBR materials: a bare one, one per cheap feature that an
/// opaque material can carry (transmission routes a material to the
/// transparent pass, so it cannot), all of those together, and two with
/// clearcoat. Ten distinct feature sets.
fn feature_cases() -> Vec<FeatureCase> {
    use awsm_renderer::materials::pbr::{
        PbrMaterial, PbrMaterialClearCoat, PbrMaterialDispersion, PbrMaterialEmissiveStrength,
        PbrMaterialIor, PbrMaterialSpecular, PbrMaterialVertexColorInfo, PbrMaterialVolume,
    };
    fn emissive_strength(m: &mut PbrMaterial) {
        m.emissive_factor = [0.10, 0.04, 0.0];
        m.emissive_strength = Some(PbrMaterialEmissiveStrength { strength: 4.0 });
    }
    fn ior(m: &mut PbrMaterial) {
        m.ior = Some(PbrMaterialIor { ior: 2.4 });
    }
    fn specular(m: &mut PbrMaterial) {
        m.specular = Some(PbrMaterialSpecular {
            tex: None,
            // No specular at all: the light's highlight goes.
            factor: 0.0,
            color_tex: None,
            color_factor: [1.0, 0.35, 0.1],
        });
    }
    fn volume(m: &mut PbrMaterial) {
        m.volume = Some(PbrMaterialVolume {
            thickness_tex: None,
            thickness_factor: 0.5,
            attenuation_distance: 1.0,
            attenuation_color: [0.2, 0.8, 0.4],
        });
    }
    fn dispersion(m: &mut PbrMaterial) {
        m.dispersion = Some(PbrMaterialDispersion { dispersion: 0.5 });
    }
    fn vertex_color(m: &mut PbrMaterial) {
        m.vertex_color_info = Some(PbrMaterialVertexColorInfo { set_index: 0 });
    }
    fn clearcoat(m: &mut PbrMaterial) {
        m.clearcoat = Some(PbrMaterialClearCoat {
            tex: None,
            factor: 1.0,
            roughness_tex: None,
            roughness_factor: 0.1,
            normal_tex: None,
            normal_scale: 1.0,
        });
    }
    vec![
        FeatureCase {
            name: "bare",
            expensive: false,
            edit: |_| {},
        },
        FeatureCase {
            name: "emissive_strength",
            expensive: false,
            edit: emissive_strength,
        },
        FeatureCase {
            name: "ior",
            expensive: false,
            edit: ior,
        },
        FeatureCase {
            name: "specular",
            expensive: false,
            edit: specular,
        },
        FeatureCase {
            name: "volume",
            expensive: false,
            edit: volume,
        },
        FeatureCase {
            name: "dispersion",
            expensive: false,
            edit: dispersion,
        },
        FeatureCase {
            name: "vertex_color",
            expensive: false,
            edit: vertex_color,
        },
        FeatureCase {
            name: "all-cheap",
            expensive: false,
            edit: |m| {
                emissive_strength(m);
                ior(m);
                specular(m);
                volume(m);
                dispersion(m);
                vertex_color(m);
            },
        },
        FeatureCase {
            name: "clearcoat",
            expensive: true,
            edit: clearcoat,
        },
        FeatureCase {
            name: "clearcoat+cheap",
            expensive: true,
            edit: |m| {
                clearcoat(m);
                emissive_strength(m);
                specular(m);
                vertex_color(m);
            },
        },
    ]
}

/// What one policy made of the [`feature_cases`] scene.
struct FeatureSceneRender {
    /// The bucket (shader id) each case's material resolved to, in case order.
    buckets: Vec<awsm_renderer::materials::MaterialShaderId>,
    /// The frame, 8-bit RGBA.
    frame: Vec<u8>,
    /// The canvas pixel inside each case's quad, in case order.
    samples: Vec<[u8; 4]>,
}

/// The [`feature_cases`] scene, committed, in a live renderer.
struct FeatureScene {
    renderer: AwsmRenderer,
    canvas: web_sys::HtmlCanvasElement,
    /// Each case's material, in case order.
    keys: Vec<awsm_renderer::materials::MaterialKey>,
    /// Each case's quad centre (world space), in case order.
    centers: Vec<glam::Vec3>,
}

impl FeatureScene {
    const HALF: f32 = 0.42;
    const EYE: glam::Vec3 = glam::Vec3::new(0.0, 0.0, 7.0);
    const RADIUS: f32 = 3.0;

    /// Renders `frames` frames and reads the last one back: the frame, and
    /// the canvas pixel inside each case's quad.
    async fn render(&mut self, frames: usize) -> Result<(Vec<u8>, Vec<[u8; 4]>)> {
        use glam::{Mat4, Vec3};
        let center = Vec3::ZERO;
        run_frames(
            &mut self.renderer,
            center,
            Self::RADIUS,
            frames.saturating_sub(1),
            |_| Self::EYE,
        )
        .await?;
        // The last frame is read back in the task that drew it.
        crate::harness::next_frame().await;
        crate::harness::set_camera(&mut self.renderer, Self::EYE, center, Self::RADIUS)?;
        self.renderer.update_transforms();
        self.renderer
            .render(None)
            .map_err(|e| anyhow!("render: {e}"))?;
        let frame = read_canvas(&self.canvas)?;

        // Where each quad lands: the harness camera is a 45° perspective
        // looking down -Z. Sample off the quad's centre, inside its first
        // triangle.
        let (width, height) = (self.canvas.width() as usize, self.canvas.height() as usize);
        let view_proj = Mat4::perspective_rh(
            45.0_f32.to_radians(),
            width as f32 / height as f32,
            0.1,
            100.0,
        ) * Mat4::look_at_rh(Self::EYE, center, Vec3::Y);
        let samples = self
            .centers
            .iter()
            .map(|c| {
                let clip = view_proj
                    * (*c + Vec3::new(Self::HALF * 0.4, -Self::HALF * 0.4, 0.0)).extend(1.0);
                let x = ((clip.x / clip.w * 0.5 + 0.5) * width as f32) as usize;
                let y = ((1.0 - (clip.y / clip.w * 0.5 + 0.5)) * height as f32) as usize;
                let at = (y.min(height - 1) * width + x.min(width - 1)) * 4;
                [frame[at], frame[at + 1], frame[at + 2], frame[at + 3]]
            })
            .collect();
        Ok((frame, samples))
    }
}

/// Builds the [`feature_cases`] scene — a 5 × 2 grid of camera-facing quads
/// with per-vertex colours, one material each, one directional light — in a
/// fresh renderer under `specialization`, and commits it once.
async fn build_feature_scene(
    msaa: bool,
    specialization: awsm_renderer::materials::pbr::PbrSpecialization,
) -> Result<FeatureScene> {
    use awsm_renderer::lights::Light;
    use awsm_renderer::materials::pbr::PbrMaterial;
    use awsm_renderer::materials::{Material, MaterialAlphaMode};
    use awsm_renderer::raw_mesh::RawMeshData;
    use awsm_renderer::transforms::Transform;
    use glam::Vec3;

    const COLUMNS: usize = 5;
    const SPACING: f32 = 1.0;
    const HALF: f32 = FeatureScene::HALF;

    let cases = feature_cases();
    let (mut renderer, canvas) =
        create_renderer_with_specialization(base_features(), msaa, specialization).await?;
    renderer
        .insert_light(
            Light::Directional {
                color: [1.0, 1.0, 1.0],
                intensity: 2.0,
                // Nearly along the view axis, so every quad carries the
                // light's specular highlight.
                direction: [-0.05, -0.08, -1.0],
                volumetric_intensity: 0.0,
            },
            None,
        )
        .map_err(|e| anyhow!("insert_light: {e}"))?;

    let rows = cases.len().div_ceil(COLUMNS);
    let mut keys = Vec::with_capacity(cases.len());
    let mut centers = Vec::with_capacity(cases.len());
    for (index, case) in cases.iter().enumerate() {
        let mut pbr = PbrMaterial::new(MaterialAlphaMode::Opaque, false);
        pbr.base_color_factor = [0.15, 0.15, 0.15, 1.0];
        pbr.metallic_factor = 0.0;
        pbr.roughness_factor = 0.35;
        (case.edit)(&mut pbr);
        let key = renderer.materials.insert(
            Material::Pbr(Box::new(pbr)),
            &renderer.textures,
            &renderer.dynamic_materials,
            &renderer.extras_pool,
        );
        let center = Vec3::new(
            ((index % COLUMNS) as f32 - (COLUMNS as f32 - 1.0) * 0.5) * SPACING,
            ((rows - 1) as f32 * 0.5 - (index / COLUMNS) as f32) * SPACING,
            0.0,
        );
        let transform = renderer.transforms.insert(
            Transform {
                translation: center,
                ..Transform::IDENTITY
            },
            None,
        );
        // Every quad carries the same vertex colours; only a material with
        // the vertex-colour feature reads them.
        let quad = RawMeshData {
            positions: vec![
                [-HALF, -HALF, 0.0],
                [HALF, -HALF, 0.0],
                [HALF, HALF, 0.0],
                [-HALF, HALF, 0.0],
            ],
            normals: Some(vec![[0.0, 0.0, 1.0]; 4]),
            uv_sets: vec![vec![[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]]],
            colors: Some(vec![
                [1.0, 0.1, 0.1, 1.0],
                [0.1, 1.0, 0.1, 1.0],
                [0.1, 0.1, 1.0, 1.0],
                [1.0, 1.0, 0.1, 1.0],
            ]),
            indices: vec![0, 1, 2, 0, 2, 3],
            ..Default::default()
        };
        renderer
            .add_raw_mesh(quad, transform, key)
            .map_err(|e| anyhow!("add_raw_mesh({}): {e}", case.name))?;
        keys.push(key);
        centers.push(center);
    }
    renderer
        .commit_load(|_| {})
        .await
        .map_err(|e| anyhow!("commit_load: {e}"))?;
    renderer.update_transforms();
    Ok(FeatureScene {
        renderer,
        canvas,
        keys,
        centers,
    })
}

/// Builds the [`feature_cases`] scene under `specialization`, renders it and
/// reports the buckets its materials resolved to.
async fn render_feature_scene(
    msaa: bool,
    specialization: awsm_renderer::materials::pbr::PbrSpecialization,
) -> Result<FeatureSceneRender> {
    let mut scene = build_feature_scene(msaa, specialization).await?;
    let (frame, samples) = scene.render(5).await?;
    let buckets = scene
        .keys
        .iter()
        .map(|key| scene.renderer.materials.shader_id(*key))
        .collect();
    destroy_renderer(scene.renderer);
    Ok(FeatureSceneRender {
        buckets,
        frame,
        samples,
    })
}

/// Check 20 — `pbr-cheap-features-share-a-kernel`. An opaque PBR kernel is
/// ~240 KB of WGSL and the features a material uses are a few hundred bytes
/// to a few kilobytes of it, so a kernel per exact feature set costs a cold
/// load one kernel for every combination its materials happen to use. The
/// default policy ([`PbrSpecialization::ExpensiveFeatures`]) compiles the
/// cheap features into every kernel instead. Over ten materials with ten
/// different feature sets (see [`feature_cases`]):
///
/// - by default the eight that differ only in cheap features resolve to ONE
///   bucket and the two with clearcoat to another — two kernels, not ten;
/// - `with_pbr_specialization(EveryFeature)` gives every set its own bucket
///   again (the opt-out works); and
/// - the two renders agree pixel for pixel: a material shades the same
///   through the shared kernel as through one compiled for exactly its
///   features, whether it has a given cheap feature or not.
///
/// Locks both directions: cheap features splitting buckets again, and a
/// shared kernel that drops or invents a feature for some material.
///
/// [`PbrSpecialization::ExpensiveFeatures`]: awsm_renderer::materials::pbr::PbrSpecialization::ExpensiveFeatures
async fn pbr_cheap_features_share_a_kernel(msaa: bool) -> Result<String> {
    use awsm_renderer::materials::pbr::PbrSpecialization;
    use std::collections::HashSet;

    /// Largest per-channel difference tolerated between the two renders. The
    /// shared kernel reads from the material what the exact one has as a
    /// compile-time constant, so a driver may fold a value differently in the
    /// last float bit; a feature gone missing is tens of levels.
    const MAX_CHANNEL_DIFF: u8 = 1;

    let cases = feature_cases();
    let shared = render_feature_scene(msaa, PbrSpecialization::ExpensiveFeatures).await?;
    let exact = render_feature_scene(msaa, PbrSpecialization::EveryFeature).await?;

    // ── bucket counts. ──
    let distinct = |r: &FeatureSceneRender| r.buckets.iter().collect::<HashSet<_>>().len();
    let cheap_buckets: HashSet<_> = cases
        .iter()
        .zip(&shared.buckets)
        .filter(|(case, _)| !case.expensive)
        .map(|(_, id)| *id)
        .collect();
    let expensive_buckets: HashSet<_> = cases
        .iter()
        .zip(&shared.buckets)
        .filter(|(case, _)| case.expensive)
        .map(|(_, id)| *id)
        .collect();
    if cheap_buckets.len() != 1 {
        let split: Vec<String> = cases
            .iter()
            .zip(&shared.buckets)
            .filter(|(case, _)| !case.expensive)
            .map(|(case, id)| format!("{}→{}", case.name, id.as_u32()))
            .collect();
        return Err(anyhow!(
            "materials that differ only in cheap features were split over {} buckets (a ~240 KB kernel each): {}",
            cheap_buckets.len(),
            split.join(", ")
        ));
    }
    if expensive_buckets.len() != 1 || !cheap_buckets.is_disjoint(&expensive_buckets) {
        return Err(anyhow!(
            "the two clearcoat materials must share one bucket of their own; got {expensive_buckets:?} against the cheap bucket {cheap_buckets:?}"
        ));
    }
    if distinct(&exact) != cases.len() {
        return Err(anyhow!(
            "with_pbr_specialization(EveryFeature) must give each of the {} feature sets its own bucket; got {}",
            cases.len(),
            distinct(&exact)
        ));
    }

    // ── the scene is really there, and the features really show. ──
    let sample = |r: &FeatureSceneRender, name: &str| {
        let index = cases.iter().position(|c| c.name == name).unwrap_or(0);
        r.samples[index]
    };
    let differs = |a: [u8; 4], b: [u8; 4]| (0..3).any(|i| a[i].abs_diff(b[i]) > 12);
    let bare = sample(&shared, "bare");
    for name in ["emissive_strength", "ior", "specular", "vertex_color"] {
        if !differs(sample(&shared, name), bare) {
            let all: Vec<String> = cases
                .iter()
                .zip(&shared.samples)
                .map(|(case, rgba)| format!("{} rgba{rgba:?}", case.name))
                .collect();
            return Err(anyhow!(
                "the {name} quad renders like the bare one (rgba{:?} against rgba{bare:?}) — the scene shows nothing, so the comparison proves nothing; quads: {}",
                sample(&shared, name),
                all.join(", ")
            ));
        }
    }

    // ── the two renders agree. ──
    if shared.frame.len() != exact.frame.len() {
        return Err(anyhow!("the two renders differ in size"));
    }
    let mut max_diff = 0u8;
    let mut over = 0usize;
    for (a, b) in shared
        .frame
        .chunks_exact(4)
        .zip(exact.frame.chunks_exact(4))
    {
        let diff = (0..3).map(|i| a[i].abs_diff(b[i])).max().unwrap_or(0);
        max_diff = max_diff.max(diff);
        if diff > MAX_CHANNEL_DIFF {
            over += 1;
        }
    }
    if over > 0 {
        let worst: Vec<String> = cases
            .iter()
            .enumerate()
            .filter(|(i, _)| {
                (0..3).any(|k| {
                    shared.samples[*i][k].abs_diff(exact.samples[*i][k]) > MAX_CHANNEL_DIFF
                })
            })
            .map(|(i, case)| {
                format!(
                    "{}: rgba{:?} shared against rgba{:?} exact",
                    case.name, shared.samples[i], exact.samples[i]
                )
            })
            .collect();
        return Err(anyhow!(
            "the shared kernels render differently from one kernel per feature set: {over} pixels differ by more than {MAX_CHANNEL_DIFF}/255 (max {max_diff}); {}",
            if worst.is_empty() {
                "no quad centre differs".to_string()
            } else {
                worst.join("; ")
            }
        ));
    }
    let shown: Vec<String> = cases
        .iter()
        .zip(&shared.samples)
        .map(|(case, [r, g, b, _])| format!("{} {r}/{g}/{b}", case.name))
        .collect();
    Ok(format!(
        "{} feature sets: {} buckets by default ({} with EveryFeature); renders agree (max channel diff {max_diff}/255 over {} pixels); quads: {}",
        cases.len(),
        distinct(&shared),
        distinct(&exact),
        shared.frame.len() / 4,
        shown.join(", ")
    ))
}

/// Check 21 — `pbr-cheap-feature-edit-keeps-kernels`. With the cheap features
/// in every opaque PBR kernel, a material that GAINS one on a running scene
/// (`update_material`) is already in the right bucket:
///
/// - the feature shows on the next frame, with no commit (the kernel has the
///   code; the edit only gave the material's payload a value for it); and
/// - the next `commit_load` changes nothing: same buckets, same kernels,
///   nothing recompiled.
///
/// With a kernel per exact feature set the material stayed in a kernel
/// without the feature until a commit moved it, and that commit could grow
/// the bucket set, which recompiles every kernel.
///
/// The edit is IOR 1.5 → 2.4 on the bare material, which nothing but the
/// IOR feature's code can show.
async fn pbr_cheap_feature_edit_keeps_kernels(msaa: bool) -> Result<String> {
    use awsm_renderer::materials::pbr::{PbrMaterialIor, PbrSpecialization};
    use awsm_renderer::materials::Material;

    let mut scene = build_feature_scene(msaa, PbrSpecialization::default()).await?;
    let bare = feature_cases()
        .iter()
        .position(|case| case.name == "bare")
        .unwrap_or(0);
    let key = scene.keys[bare];
    let (_, before) = scene.render(5).await?;
    let pipelines_before = material_pipelines(&scene.renderer);
    let missing = material_pipelines_missing(&scene.renderer, &pipelines_before);
    if !missing.is_empty() {
        destroy_renderer(scene.renderer);
        return Err(anyhow!("not installed after the commit: {missing:?}"));
    }
    let bucket_before = scene.renderer.materials.shader_id(key);
    let compute_before = scene.renderer.pipelines.compute.len();

    scene.renderer.update_material(key, |material| {
        if let Material::Pbr(pbr) = material {
            pbr.ior = Some(PbrMaterialIor { ior: 2.4 });
        }
    });
    // No commit: the feature must show by itself.
    let (_, edited) = scene.render(3).await?;
    let shows = (0..3).any(|i| edited[bare][i].abs_diff(before[bare][i]) > 30);
    if !shows {
        destroy_renderer(scene.renderer);
        return Err(anyhow!(
            "IOR set on a running scene does not show without a commit: the quad is rgba{:?}, was rgba{:?} — its kernel has no IOR code",
            edited[bare],
            before[bare]
        ));
    }

    scene
        .renderer
        .commit_load(|_| {})
        .await
        .map_err(|e| anyhow!("commit_load after the edit: {e}"))?;
    let pipelines_after = material_pipelines(&scene.renderer);
    let bucket_after = scene.renderer.materials.shader_id(key);
    let compute_after = scene.renderer.pipelines.compute.len();
    let (_, committed) = scene.render(3).await?;
    destroy_renderer(scene.renderer);
    if bucket_after != bucket_before {
        return Err(anyhow!(
            "the commit moved the material from bucket {} to {} for a cheap feature",
            bucket_before.as_u32(),
            bucket_after.as_u32()
        ));
    }
    if pipelines_after != pipelines_before || compute_after != compute_before {
        return Err(anyhow!(
            "the commit after a cheap-feature edit changed the material pipelines: {} → {} buckets, {} → {} compute pipelines, kernels changed: {}",
            pipelines_before.buckets,
            pipelines_after.buckets,
            compute_before,
            compute_after,
            pipelines_before.kernels != pipelines_after.kernels
        ));
    }
    if (0..3).any(|i| committed[bare][i].abs_diff(edited[bare][i]) > 1) {
        return Err(anyhow!(
            "the commit changed how the edited material renders: rgba{:?} → rgba{:?}",
            edited[bare],
            committed[bare]
        ));
    }
    Ok(format!(
        "IOR set with no commit: quad rgba{:?} → rgba{:?} on the next frames; the following commit kept all {} buckets and {} kernels",
        before[bare],
        edited[bare],
        pipelines_after.buckets,
        pipelines_after.kernels.len()
    ))
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
