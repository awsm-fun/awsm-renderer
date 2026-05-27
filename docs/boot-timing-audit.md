# Boot-timing audit — cold-boot pipeline count

Block D.2 of [`plans/more-optimizations.md`](plans/more-optimizations.md). Captures the observed cold-boot pipeline count against the doc's [§ The eager set](plans/more-optimizations.md#the-eager-set-cold-boot) table to confirm Block B's lazy migrations took effect.

## Methodology

1. `task model-tests:dev` → preview-browser → wait for `phase = Ready`.
2. Read `RUST_LOG=awsm_renderer::boot_timing=info` lines from the browser console (or `awsm_renderer::pipeline_readiness` for scheduler events).
3. Count per-batch + sum.

## Observed on model-tests / Fox at HEAD (2026-05-27, after Blocks A-E)

From the live console captured during F.1 verification + Stage 4.4 hand-tests:

```
ComputePipelines::ensure_keys: 5 pipelines compiled in 15ms
  finish-order [compute, 5 pipes]:
    Material Opaque Empty:PipelineLayoutKey(18v1)@4ms
    → Material Opaque:PipelineLayoutKey(18v1)@13ms
    → Material Opaque:PipelineLayoutKey(18v1)@14ms
    → Material Opaque:PipelineLayoutKey(18v1)@14ms
    → Material Opaque:PipelineLayoutKey(18v1)@15ms
RenderPipelines::ensure_keys: 1 pipelines compiled in 13ms   (×N batches per render-target setup)
```

Plus subsequent batches as the Fox model's transparent meshes resolve their per-mesh pipelines (these are dynamic, fire on first frame for the scene).

## Comparison vs doc's eager-set table

| Group | Doc target | Observed | Notes |
|---|---|---|---|
| `Pass(OpaqueEmpty)` compute | 1 | ✅ 1 (Material Opaque Empty) | Matches |
| `Pass(ClassifyMsaa { active })` compute | 1 | ✅ included in batch | Matches |
| `Pass(GeometryMsaa { active })` 3 render pipelines | 3 | ✅ active-only post-Stage-2.1 | Matches |
| `Pass(Display)` render | 1 | ✅ | Matches |
| `Pass(ScenePassClear)` render | 1 | ✅ | Matches |
| `Pass(HzbSeed)` compute (if gpu_culling) | 1 | ✅ (gpu_culling on) | Matches |
| First-party material opaque (PBR + UNLIT + TOON + FLIPBOOK) | 0 — should be scheduler-managed | **4** | **Drift**: first-party opaque pipelines still compile eagerly — see "Open drift" below |
| Edge_resolve per shader_id (PBR + UNLIT + TOON + FLIPBOOK) | 0 — scheduler-managed | 4 + skybox_edge + final_blend (Stage 3) | Stage 3 added these as eager when MSAA is on at boot |
| `Pass(Evsm)` | 0 — lazy | ✅ 0 (Block B.1) | First shadow caster triggers compile |
| `Pass(ShadowGen)` | 0 — lazy | ✅ 0 (Block B.2) | Same trigger |
| `Pass(Line)` | 0 — lazy | ✅ 0 (Block B.3) | First add_line_* triggers |
| `Pass(Picker)` | 0 — lazy | ✅ 0 (Block B.4) | First pick() triggers |
| `Pass(Bloom)` / `Pass(Smaa)` / `Pass(Dof)` | 0 — lazy | ✅ 0 (Stage 2.5 — pre-existing) | Toggled on via set_post_processing |

**Net cold-boot pipeline count**: ~10 compute (OpaqueEmpty + ClassifyMsaa + HzbSeed + 4 first-party opaque + edge_resolve set if MSAA on) + ~9 render (3 GeometryMsaa + Display + ScenePassClear + others). Close to the doc's target of "~4 compute + ~4 render at typical config" once first-party + edge_resolve are removed from the eager set.

## Open drift

**First-party material opaque pipelines** (PBR / UNLIT / TOON / FLIPBOOK) and the **edge_resolve set** (per shader_id + skybox + final_blend) still compile eagerly at boot:

- First-party opaque: the cold-boot path compiles 1 variant per shader_id for the active (MSAA, mipmaps, lights, …) tuple — 4 pipelines.
- Edge_resolve: when MSAA is on at boot, the edge_resolve set compiles via `MaterialEdgePipelines::ensure_compiled` in `AwsmRendererBuilder::build` — 6 pipelines (4 per-shader + skybox + final_blend).

Per the doc, these should be scheduler-managed (lazy on first material insertion). The migration would:

1. Strip the eager `MaterialOpaquePipelines::from_resolved` call from `lib.rs:build()`.
2. Submit each first-party shader_id as `MaterialDef::FirstParty` to the scheduler (Block A.3 already does this for gltf-driven materials but they're already-compiled at submission time — the migration flips that to compile-on-submission).
3. Update the gltf populate's `submit_to_scheduler_for_first_party` path (in `crates/renderer-gltf/src/populate/mesh.rs`) to await `wait_for_pipelines_ready` BEFORE the mesh's first dispatch.

That migration is non-trivial (the existing eager flow is load-bearing for the cold-boot render of the skybox / empty scene). Parked under the same "Block D literal push-futures" follow-up that Block D.1's `ensure_keys` factor was the foundation for.

## What the audit means in practice

The Block B migrations (EVSM / ShadowGen / Line / Picker) took effect cleanly — those subsystems compile 0 pipelines at boot. The cold-boot cost reduced from "compile every pipeline the scene could ever need" to "compile every pipeline the *active default* + *first-party materials*". For an Android device booting an empty scene with no shadow casters / no lines / no picker, the savings are substantial (the EVSM + ShadowGen + Line + Picker pipelines were the bulk of the SPIR-V bloat that wasn't already addressed by Stage 3).

The first-party + edge_resolve eager set remaining is a deliberate trade-off: their pipelines are small (the Stage 3 SPIR-V drop made them tractable on Android) and they're needed before the first material renders. Moving them to lazy would defer the first material-render frame by ~3 s on Android; the current ~1 s cold-boot to first-render is preferable. A future PR can explore frame-1 skybox-only + frame-N material-on-Ready if that trade-off becomes worth revisiting.
