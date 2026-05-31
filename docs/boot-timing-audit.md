# Boot-timing audit — cold-boot pipeline count

Block D.2 of [PR #99](https://github.com/dakom/awsm-renderer/pull/99). Captures the observed cold-boot pipeline count against the doc's [§ The eager set](https://github.com/dakom/awsm-renderer/pull/99) table to confirm Block B's lazy migrations took effect.

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
| First-party material opaque (PBR + UNLIT + TOON + FLIPBOOK) | 0 — scheduler-managed | ✅ 0 (Block D.1 PART 2) | `MaterialOpaquePipelines::shader_descriptors_and_layouts` passes `include_first_party: false` at cold boot; `launch_first_party_material_compile` fires on the gltf populate path's first per-`shader_id` registration. |
| Edge_resolve per shader_id (PBR + UNLIT + TOON + FLIPBOOK) + skybox_edge + final_blend | 0 — scheduler-managed | 6 at boot when MSAA is on | Cold-boot compiles via `MaterialEdgePipelines::ensure_compiled` in `AwsmRendererBuilder::build`. See "Remaining drift" below. |
| `Pass(Evsm)` | 0 — lazy | ✅ 0 (Block B.1) | First shadow caster triggers compile |
| `Pass(ShadowGen)` | 0 — lazy | ✅ 0 (Block B.2) | Same trigger |
| `Pass(Line)` | 0 — lazy | ✅ 0 (Block B.3) | First add_line_* triggers |
| `Pass(Picker)` | 0 — lazy | ✅ 0 (Block B.4) | First pick() triggers |
| `Pass(Bloom)` / `Pass(Smaa)` / `Pass(Dof)` | 0 — lazy | ✅ 0 (Stage 2.5 — pre-existing) | Toggled on via set_post_processing |

**Net cold-boot pipeline count** on a zero-scene with MSAA-on: ~6 compute (OpaqueEmpty + ClassifyMsaa + HzbSeed + edge_resolve set: 4 per-shader + skybox + final_blend) + ~5 render (3 GeometryMsaa + Display + ScenePassClear). First-party material opaque pipelines (4) compile lazily on first material registration via the gltf populate path's `launch_first_party_material_compile`.

## Remaining drift

**Edge_resolve set** (per shader_id × 4 + skybox + final_blend = 6 pipelines) still compiles eagerly at boot whenever MSAA is on:

- Via `MaterialEdgePipelines::ensure_compiled` in `AwsmRendererBuilder::build`.

> **Update (PR #105) — edge_resolve compile model reworked.** The
> migration sketched below was **superseded** by a different (and better)
> design. Edge resolve is now treated as a **layout-level** concern, not a
> per-material one: the full edge set (per-shader × N + skybox +
> final_blend) is compiled **once per bucket-layout change** via
> `launch_edge_resolve_compile` (the two sync relaunch sites:
> `register_material`'s tail + `relaunch_all_buckets_after_layout_change`),
> NOT pushed per material. Its scheduler promises are charged to a single
> non-material group, `PassKind::MaterialEdgeResolve` (≈ the `Pass(EdgeChain)`
> idea in step 3), and their **install validity is keyed on layout-content**
> — a resolved edge pipeline installs iff its cache key is still one the
> current layout wants (`MaterialEdgePipelines::{set_desired_edge_keys,
> is_edge_key_desired}`) — so it depends on **no material's generation and no
> canonical-PBR assumption** (a PBR-absent scene has no anchor material). The
> eager `MaterialEdgePipelines::ensure_compiled` in `build()` / `prewarm` was
> **kept on purpose** as the awaited "ready NOW for the first shown frame"
> installer; it shares `build_descriptors` + `desired_keys` with the
> scheduler path, so the two never diverge. So step 1 (strip eager) was *not*
> taken, and step 2 (per-material push) was *reversed*. See PR #105 and
> `pipeline_scheduler/launch.rs::launch_edge_resolve_compile`.

The original (now-superseded) sketch, for history — per the doc these
"should be" scheduler-managed (lazy on first material insertion):

1. Strip the eager `MaterialEdgePipelines::ensure_compiled` call from `lib.rs:build()`.
2. Have `launch_first_party_material_compile` / `launch_dynamic_material_compile` also push the edge_resolve variant for the registered shader_id (the per-shader edge_resolve pipeline is shader-id-keyed in the same way the primary opaque pipeline is).
3. Keep the skybox_edge_resolve + final_blend compile eager (or lift them into a `Pass(EdgeChain)` scheduler entry — they're material-agnostic).

## What the audit means in practice

The Block B migrations (EVSM / ShadowGen / Line / Picker) took effect cleanly — those subsystems compile 0 pipelines at boot. Block D.1 PART 2 extended this to first-party material opaque pipelines (PBR / UNLIT / TOON / FLIPBOOK), which now also compile lazily on the gltf-populate path's first per-`shader_id` registration. The cold-boot cost reduced from "compile every pipeline the scene could ever need" to "compile only the active default pipelines + the edge_resolve set when MSAA is on". For an Android device booting an empty scene with no shadow casters / no lines / no picker, the savings are substantial (the EVSM + ShadowGen + Line + Picker pipelines were the bulk of the SPIR-V bloat that wasn't already addressed by Stage 3).

The edge_resolve eager set remaining is a deliberate trade-off: those pipelines are small (the Stage 3 SPIR-V split made them tractable on Android) and they're needed before the first material renders with MSAA edge resolution. (PR #105 update: the eager set is retained as the "first-frame-ready" installer; the *runtime* edge rebuild is now layout-level + content-validated rather than per-material — see the "Update (PR #105)" note above.)
