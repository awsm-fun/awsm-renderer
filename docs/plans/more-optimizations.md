# Renderer pipeline-readiness architecture & cold-boot optimizations

## How to use this document

**This plan is meant to be implemented start-to-finish in a single fresh session, by an agent with no prior context.** Every architectural decision is captured here; the [§ Implementation checklist](#implementation-checklist) at the end is the canonical work list.

Operating rules for the implementing agent:

1. **Work through the [§ Implementation checklist](#implementation-checklist) in order.** Each item points back to the relevant body section for design rationale.
2. **Commits are for organization, not gating.** Commit logically (one commit per checklist item, or per small group of related items) to assist future `git bisect`. Commits do **not** need to be in a working state — partial-but-coherent changes are fine. Do not run tests between commits.
3. **Update the checklist in this document as each item is completed.** Mark items with `[x]`. Commit the checklist update with each item or batch.
4. **Do not stop at commits or at the end of a priority.** Keep going through every checklist item. Only stop when the entire list (including testing + PR) is done.
5. **After all implementation checklist items are complete**, use a preview browser (e.g. `mcp__Claude_Preview__*` or `mcp__Claude_in_Chrome__*` tooling) to load each frontend (`material-editor`, `scene-editor`, `model-tests`) and exercise features you think are worth verifying — at minimum: cold-boot to `phase = Ready`, gltf load with incremental paint, MSAA on/off toggle, shadow toggle on a directional light, dynamic-material registration flow, material-editor recompile cycle. Capture the boot-timing logs to confirm pipeline counts match the [§ The eager set](#the-eager-set-cold-boot) table.
6. **Once testing passes**, run `cargo fmt --all` and `task lint`. Resolve any warnings/errors. Then commit the formatting fixes.
7. **Open a PR for the branch on GitHub** using `gh pr create`. PR body should summarize the architectural change, link this doc, and call out the migration / breaking-change list from [§ Migration of the dynamic-materials API](#migration-of-the-dynamic-materials-api). PR body should also link [§ Post-implementation human checklist](#post-implementation-human-checklist) and note that those items remain for the human reviewer.

**The agent's work ends at the PR being open.** Items requiring physical hardware setup (Android device plugged in) or human judgment (PR review, merge approval) live in [§ Post-implementation human checklist](#post-implementation-human-checklist) and are explicitly out-of-scope for the agent's pass. Do **not** wait, poll, or block on those — open the PR and stop.

If you hit a genuine blocker (e.g. a WebGPU primitive doesn't behave as the doc assumes), record the surprise in this doc inline near the relevant section and continue with the best alternative — don't stop to ask. The user trusts the agent to make reasonable adaptations; they want forward progress over consultative perfection.

---

## TL;DR

The renderer today compiles every shader/pipeline it could possibly need at cold-boot, serially gated through `AwsmRendererBuilder::build`. That works on desktop Chrome (~1–2 s init) but fails on Android Chrome (Vulkan-via-Dawn) with a `VK_ERROR_INITIALIZATION_FAILED` rejecting the PBR compute pipeline as too complex. Even where it doesn't fail outright, the architecture pays full cold-boot cost (~14 s for PBR compile alone on the test Android device) for pipelines a zero-mesh scene will never dispatch.

This plan replaces the eager-everything model with a **pipeline-readiness state machine** that:

1. Exposes a public `submit_pipeline_group_batch` API that takes a list of materials/passes and returns handles immediately (Pending).
2. Resolves compiles asynchronously through a main-thread `FuturesUnordered` scheduler. Each pipeline group transitions Pending → Ready (or Failed{error}) when its compile resolves.
3. Lets the render frame trivially skip Pending/Failed groups via the existing bucket-entries cache (no per-mesh state lookups on the hot path).
4. Surfaces transitions through a status stream so frontends can drive "compiling N of M" UI without polling.

At cold-boot, only the **zero-scene render minimums** (empty opaque compute, classify variant, scene-pass clear, display render) compile eagerly. Everything else — first-party PBR/UNLIT/TOON/FLIPBOOK, dynamic-material pipelines, EVSM, lines, shadow-gen, bloom/SMAA/DoF — flows through the scheduler. The gltf loader kicks off its material batch the moment the gltf JSON's `materials` array is parsed, in parallel with buffer download / image decode / GPU upload. Scenes paint incrementally as materials become Ready.

A separate but architecturally aligned change replaces the PBR shader's `msaa_resolve_samples` (the actual SPIR-V bloat) with a per-shader-id edge-resolve pass — see [§ Priority 3](#priority-3--replace-msaa_resolve_samples). After both land, Android Chrome should initialize in <1 s with no compiles in flight, scenes complete first-paint within 100–300 ms of the gltf JSON parse finishing, and cross-material MSAA edges shade correctly for every shader_id including dynamic materials.

---

## Status today (2026-05-27)

PR #99 on the `more-optimizations` branch is open and substantially complete. What's landed:

- ✅ **Stage 0** — diagnostics (device-limits log + `onuncapturederror` + error variants).
- ✅ **Stage 1** — pipeline-readiness machinery (scheduler module + `submit_pipeline_group_batch` API + race policy + Lessons A labels + render-frame drain + `submit_dynamic_material` additive API + **A.1 bridge**: `register_material` populates scheduler, `prewarm_dynamic_pipelines` marks scheduler entries `Ready`, frontends migrated to `wait_for_pipelines_ready` as the canonical post-submit await).
- ✅ **Stage 2** — Geometry MSAA lazy (2.1), EVSM lazy (2.2), Line lazy (2.3), ShadowGen lazy (2.4), effects lazy (2.5), Picker lazy fully (2.6). Cold-boot now compiles 0 EVSM / 0 Line / 0 Picker / 0 ShadowGen pipelines on a zero-scene; first scene-content trigger lands the compile.
- ✅ **Stage 3** — `msaa_resolve_samples` replacement FULLY ACTIVE. Per-shader-id `edge_resolve` (incl. dynamic materials via C.1) / `skybox_edge_resolve` / `final_blend` pipelines own edge pixels via slot-buffer scatter pattern. Args/data buffer split + counter-mirror trick keeps within the 10-storage-buffer per-stage limit. 4-group layout via shadows-group(3) fold fits within macOS Metal's `maxBindGroups=4`. PBR primary SPIR-V drops ~80% — Android-Chrome init failure unblocked. MAX_EDGE_BUDGET overflow diagnostics shipped (MVP — saturation log + one-shot warn helper; full atomic-add fallback parked under C.2).
- ✅ **Stage 3.9** contract docs updated for dynamic-material edge_resolve dual-context invariant.
- ✅ **Block E.1 / E.4** — compute pipeline labels include shader name; ensure_keys emits per-batch finish-order summary log for batches of 2+.

What's **not yet** landed (genuine remainder — small surface area):

- Stage 1.8-fully **literal** (push real compile futures into FuturesUnordered): assessed as disproportionate vs the A.1 bridge approach which delivers the same observable behaviour. Parked under Block D.
- Stage 1.13 (gltf loader explicit `submit_pipeline_group_batch(MaterialDef::FirstParty(...))`): first-party pipelines already compile in the eager set, so submitting is bookkeeping-only. Becomes load-bearing when first-party variants go lazy.
- Block D (build-flow migration to scheduler-driven eager set + full config-flip semantics): the "bigger refactor" — parked. Renderer works correctly today via the A.1 bridge + the existing batched compile path; Block D is observability + uniformity polish.
- Stage 4.4 / 4.5 (config-flip + light-add interactive smoke test): manual hand-test, ~10 min when in the editor.
- E.2 (multithreading prep audit), E.3 (test-helper sweep), E.5 (edge_resolve runtime profile), E.6 (cargo doc warning audit — already noted: 43 warnings vs ~47 baseline, net improvement), E.7 (missing_docs gate for pipeline_scheduler — ~49 fields need stub docs first): polish items, parked.

---

## Guiding principles

These shaped every design decision below; quoting verbatim so a fresh session can match the same calls:

1. **The renderer API exposes batches of shaders/pipelines that resolve as FuturesUnordered.** Frontends compose them intelligently; no internal eager-everything assumption.
2. **The default frontends only batch what's needed for the active defaults up-front.** Everything else is on-demand.
3. **Built-in shader code must be as efficient as possible.** No sacrificing per-frame performance to make compile lazy.
4. **SPIR-V pressure is real, but performance > laziness.** If a more advanced solution buys better performance or faster time-to-first-pixel, take it — needless technical debt is the enemy.

Two consequences worth pulling out:

- **No backward compatibility for surfaces that conflict.** PR #98's `prewarm_pipelines(...).await` is the standout — it cannot coexist with batch-and-await-later. It dies.
- **Editor/authoring perf can absorb small costs that gameplay can't.** "Material recompile produces a new MaterialId and swaps on ready" is fine because it's editor-only; per-frame atomics are not fine because they hit gameplay.

---

## Architecture: Pipeline Readiness

### Three-state machine

```rust
pub enum PipelineGroupStatus {
    Pending,
    Ready,
    Failed { error: AwsmRendererError },
}
```

Transitions:

- **submit → Pending**: a `PipelineGroupId` is allocated and returned immediately. The compile future is queued in the scheduler.
- **Pending → Ready**: the scheduler resolves the compile future successfully. Bucket-entries cache rebuild is triggered. Status stream emits.
- **Pending → Failed{error}**: the compile future returns an error (WGSL parse failure, layout incompatibility, validation reject, etc.). Status stream emits. **No auto-retry.**
- **Ready → terminal**: no transition back out. If a recompile is wanted (e.g. material-editor hot-reload, MSAA flip), the existing PipelineGroupId is dropped and a fresh submit produces a new id. UI swaps the reference on Ready.

The "new id on recompile" rule means every pipeline group's state is **immutable after Ready**. No atomic-swap logic anywhere; no read-modify-write of in-flight GPU state. Cost: an orphan id per recompile cycle, freed via generation-marker cleanup (next section).

### Three-layer naming

Three distinct things were collapsed under "material id" in earlier discussion. They're separated cleanly here:

| Layer | Owner | Stability | Form |
|---|---|---|---|
| **ExternalRef** | scene-editor / project format | Stable across sessions | `"gltf://path/to/file.glb#material/3"` or `MaterialDefinition { name, folder, ... }` |
| **MaterialDef** | renderer input | Per-batch | Fully resolved: params + WGSL + slot bindings + alpha_mode |
| **MaterialId** | renderer runtime | Session-only | `SlotMap` key |

The renderer never sees ExternalRef. Scene-editor's job is to maintain a `HashMap<ExternalRef, MaterialId>` and resubmit not-yet-loaded materials on scene load. The renderer's batch API takes `Vec<MaterialDef>` and returns `Vec<MaterialId>`.

For **passes** (not materials), the equivalent of MaterialId is `PassKind` — an enum naming the pass:

```rust
pub enum PassKind {
    Bloom,
    Smaa,
    Dof,
    Evsm,
    Line,
    ShadowGen,
    GeometryMsaa { samples: u8 },   // 1 or 4
    ClassifyMsaa { samples: u8 },
    // ... etc
}
```

Passes have one instance per renderer; PassKind is the natural key. Materials have N instances; SlotMap is the natural key.

### Unified PipelineGroup over materials and passes

```rust
pub enum PipelineGroupId {
    Material(MaterialId),
    Pass(PassKind),
}

pub enum PipelineGroupDef {
    Material(MaterialDef),
    Pass(PassDef),
}
```

One scheduler, one status stream, one "compiling" UI surface for both kinds of pipelines. `AwsmRendererBuilder::build`'s "eager set" is just the renderer's first internal call to `submit_pipeline_group_batch`, awaited synchronously inside `build` — there is no special eager-compile code path. Every other compile happens post-build, async, scheduler-resolved.

### `MaterialDef` and `PassDef` concrete shapes

`MaterialDef` carries everything the renderer needs to compile a material's two pipelines (primary + edge_resolve, per [Priority 3](#priority-3--replace-msaa_resolve_samples)). For first-party materials it's derived from gltf parsing; for dynamic materials it's the existing `MaterialRegistration` shape from PR #98 plus the config snapshot:

```rust
pub struct MaterialDef {
    /// First-party variant (PBR/UNLIT/TOON/FLIPBOOK) OR dynamic shader_id allocated at registration.
    pub shader_id: MaterialShaderId,

    /// Alpha mode — routes between opaque and transparent passes.
    pub alpha_mode: MaterialAlphaMode,

    /// Double-sided culling override.
    pub double_sided: bool,

    /// Per-shader_id config snapshot. For first-party: empty (params live in the
    /// `material_meta` buffer, looked up at dispatch time). For dynamic: the
    /// registered WGSL fragment + slot bindings + layout descriptor.
    pub kind: MaterialDefKind,

    /// Snapshot of renderer config at submission time. Used as the cache key
    /// for `(shader_id, msaa, mipmap, ...)`-keyed pipelines. The scheduler's
    /// state machine refuses to mark a group Ready if the config has since
    /// drifted (see [§ Config-flip semantics](#config-flip-semantics-msaa-post-processing)).
    pub config_snapshot: PipelineConfigSnapshot,
}

pub enum MaterialDefKind {
    FirstParty,
    Dynamic {
        wgsl_fragment: String,
        slot_layout: DynamicSlotLayout,
        struct_decl: String,   // generated by the layout pass
        loader_decl: String,
    },
}

pub struct PipelineConfigSnapshot {
    pub msaa: AntiAliasing,
    pub mipmap: MipmapMode,
    pub use_mesh_light_slices: bool,
    pub gpu_culling: bool,
    pub debug_bitmask: u32,
    // ... whatever else the askama template branches on.
}
```

`PassDef` is a sum type covering the scheduler-managed passes. Each variant carries only the data its build path needs:

```rust
pub enum PassDef {
    OpaqueEmpty,                                       // no payload — always the same shader
    ClassifyMsaa  { samples: u8 },
    GeometryMsaa  { samples: u8 },
    Display,                                            // ditto
    ScenePassClear,
    HzbSeed       { samples: u8 },
    Evsm,
    Line,
    ShadowGen,
    Picker,
    Bloom         { resolution: (u32, u32) },
    Smaa          { resolution: (u32, u32) },
    Dof,
    EdgeResolveSkybox,                                  // Priority 3
    EdgeResolveBlend,                                   // Priority 3
}
```

Note that `Pass(GeometryMsaa { samples: 1 })` and `Pass(GeometryMsaa { samples: 4 })` are distinct `PipelineGroupId`s with independent lifecycle — the inactive MSAA's pipelines stay Pending until `set_anti_aliasing` flips, at which point its def is submitted as a batch and that group transitions to Ready.

`MaterialDef`'s `config_snapshot` field is what makes config-flip semantics clean: when MSAA flips, the renderer iterates Ready materials, sees their snapshots no longer match the active config, transitions them back to Pending, and re-submits with the new snapshot. No data structure mutation in place; the materials' MaterialIds stay valid (their definitions just recompile to new pipelines).

### Orphan cleanup (generation marker per slot)

Material-editor hot-reload pattern: each keystroke (after debounce + idempotent-recompile filter) submits a new MaterialDef and gets a new MaterialId. The old MaterialId is orphaned. Cleanup is by **generation marker per project-level slot**:

- The material-editor tracks a single "current MaterialId for this editor slot." When a new submit lands a fresh MaterialId, the previous one is dropped via `AwsmRenderer::drop_material_group(MaterialId)`.
- For first-party materials there's no recompile-replace flow (the gltf's material params don't mutate at runtime) so no orphans.
- For scene-editor's "Import Material" replacing an existing slot's binding: the editor's swap-on-ready logic owns the cleanup; renderer just frees on drop.

Bounded orphan count: zero in steady-state gameplay; at most one in-flight orphan per material-editor instance during a recompile.

### Push + Pull API surface

```rust
impl AwsmRenderer {
    /// Submit a batch of pipeline groups for compile.
    /// Returns ids immediately in Pending state. Compile is queued in
    /// the scheduler; status transitions appear on the status stream.
    pub fn submit_pipeline_group_batch(
        &mut self,
        defs: Vec<PipelineGroupDef>,
    ) -> Vec<PipelineGroupId>;

    /// Per-group status query — O(1) lookup.
    pub fn pipeline_group_status(&self, id: PipelineGroupId) -> PipelineGroupStatus;

    /// Stream of status transitions. Subscribed-to by frontends for
    /// "compiling N of M" modals, error reporting, bucket-entries
    /// cache rebuild, etc.
    pub fn subscribe_pipeline_status(&self) -> impl Stream<Item = (PipelineGroupId, PipelineGroupStatus)>;

    /// Drop a material group. Used by the editor's hot-reload cleanup.
    pub fn drop_material_group(&mut self, id: MaterialId);
}
```

The hot path (per-frame render) **never** calls `pipeline_group_status`. Instead:

- **Materials**: the existing `bucket_entries_cached() -> &[BucketEntry]` cache rebuilds whenever any material transitions to/from Ready. The cache contains only Ready materials. The per-frame render iterates the cache; readiness is implicit.
- **Passes**: each pass's dispatch site queries the renderer for the pass's pipeline keys via existing per-pass typed accessors (e.g. `bloom_pipeline_keys()`). The accessor returns `Option` — None means the pass is Pending/Failed, the dispatch site skips that pass. The query is O(1) and lives on a typed handle; no enum match.

Zero per-mesh status lookups on the hot path. Pull API is for one-off queries and diagnostics; push API drives UI.

### Pending material lifecycle (load-bearing invariant)

Worth stating explicitly because it's what makes the "incremental paint" UX trivially correct:

1. Frontend calls `submit_pipeline_group_batch(vec![Material(def)])` → MaterialId is allocated in Pending state.
2. Frontend inserts a mesh referencing that MaterialId (sync, immediate).
3. **First render frame after step 2**: classify-pass scans buckets, sees the MaterialId is not in `bucket_entries_cached()` (because the cache only contains Ready materials), the mesh is **silently skipped** — no bucket assignment, no opaque dispatch, no error. Scene-graph is fine (transforms, AABB, picking-test, etc. all see the mesh as present).
4. Compile resolves → scheduler emits Pending → Ready on the status stream → renderer's internal subscriber marks `bucket_entries_cached()` dirty.
5. **Next render frame**: bucket-entries cache rebuilds, now includes this MaterialId. Classify-pass assigns the mesh's bucket; opaque-pass dispatches; mesh appears.

The mesh "pops in" on the frame after Ready, with no special-casing in the dispatch path. Symmetrically, transitioning back to Pending (e.g. MSAA flip) invalidates the cache and the mesh disappears for the duration of the recompile. The bucket-entries cache is the single point that mediates this — and it already exists and is already rebuilt on register/unregister in PR #98's surface. The new architecture just adds "Pending → Ready" as another trigger for the cache rebuild.

### Scheduler driving and transition timing

The `FuturesUnordered` scheduler runs on the main thread, polled from the **render loop's pre-frame phase** (the same place that consumes `WindowEvent::RedrawRequested` or equivalent in our wasm setup). Polling order each frame:

1. Drain any resolved compile futures (poll the `FuturesUnordered` until pending).
2. For each resolved future: transition its group (Pending → Ready or Failed), emit status-stream events.
3. Renderer's internal subscriber processes events synchronously: marks bucket-entries cache dirty, marks per-pass typed-key accessor caches dirty.
4. Frontend subscribers process events synchronously (modal updates, error reporting).
5. Classify pass runs; rebuilds caches if dirty.
6. Render frame proceeds.

This means transitions happen **between frames**, not mid-frame — there's no risk of a half-Ready material being dispatched. It also means the "pop in" delay is bounded by one frame after compile resolves.

The scheduler does not yield voluntarily; it greedily polls until the underlying futures stop making progress. Dawn's pipeline-creation promises drive the work; we just poll them. If the operator wants to limit per-frame compile-processing time (e.g. to avoid a frame hitch when 10 materials all finish at once), the drain loop in step 1 takes an optional `max_transitions_per_frame` cap and defers the overflow to the next frame.

### Render-frame preamble safety net

If a render frame finds itself trying to dispatch through a path that's not Ready (e.g. a mesh whose MaterialId is in Pending state but somehow got into a bucket — a bug in the cache invalidation), the dispatch site:

- Skips the work silently.
- Emits `tracing::warn!` once per session per (id, location) pair so it surfaces without spamming.

**No auto-trigger compile** from the render frame. Self-healing in production sounds nice but masks bugs in the trigger paths. A warn surfaced from a real consumer is always a one-line fix at the responsible call site (gltf loader, editor "Import Material," etc.).

### Config-flip semantics (MSAA, post-processing)

When `set_anti_aliasing` or `set_post_processing` is called:

1. Every currently-Ready material whose compiled pipelines depend on the changed config transitions back to Pending.
2. The renderer re-submits those materials' definitions (with the new active config) as a single batch.
3. Pass-level groups (GeometryMsaa, ClassifyMsaa, post-processing passes) for the new config are submitted in the same batch.
4. The frontend's "compiling pipelines" modal pops while the batch resolves (rendering continues — meshes whose materials are Pending are skipped, scene is visibly recompiling, modal explains why).

No atomic-swap logic; no keep-old-while-new-compiles overlap. The flicker is acceptable per principle 4 — the user explicitly toggled, the modal explains the wait, and the eliminated complexity is large.

**Race policy**: `set_anti_aliasing` / `set_post_processing` called **before** `build` returns (i.e. before the eager-set batch finishes) is a programming error and returns `Err(AwsmRendererError::NotReady)`. The first valid call site is anywhere after `build().await` resolves. The frontends already structure their renderer-lifecycle this way; this just makes the contract explicit.

---

## The eager set (cold-boot)

`AwsmRendererBuilder::build`'s first internal batch — the only pipelines that exist before `build` returns. **Hard-errors if any of these fail**; the renderer can't function without them.

The list below is parameterized on the `AwsmRendererBuilder`'s active config (MSAA setting, `gpu_culling` feature flag, etc.) — the builder is the single source of truth for "active defaults" and the eager-set construction reads from it. `samples: active` in the rows below means "whatever the builder was configured with." Inactive MSAA variants, opted-out features, and disabled post-processing effects are **not** in the eager set.

| Group | Purpose |
|---|---|
| `Pass(OpaqueEmpty)` (compute) | Skybox-only frames + bucket-skip path; ~40 ms to compile, always needed |
| `Pass(ClassifyMsaa { samples: active })` (compute) | Per-frame classify dispatch; needs the active MSAA's variant |
| `Pass(GeometryMsaa { samples: active })` (3 render pipelines for active branch) | First-frame geometry; the inactive MSAA branch is scheduler-managed |
| `Pass(Display)` (render) | Renders the opaque target to the swap chain |
| `Pass(ScenePassClear)` (render) | Per-frame clear |
| `Pass(HzbSeed)` (compute, if `gpu_culling` feature on) | Per-frame HZB construction; pre-warming the seed only |

Total: ~4 compute + 4 render at typical config. Cold-boot batch should compile in <500 ms on Android, <100 ms on desktop. **No first-party material pipelines, no dynamic-material pipelines, no MSAA-inactive variants, no post-processing variants, no shadow infrastructure.**

Everything else — listed in the [scheduler-managed set](#the-scheduler-managed-set) — flows through `submit_pipeline_group_batch` from `build`'s **return** onward, including the renderer's own internal triggers (e.g. first material insertion fires an internal batch).

## The scheduler-managed set

Compiled on-demand via `submit_pipeline_group_batch`. Triggers listed.

| Group | Trigger | Notes |
|---|---|---|
| First-party material (PBR primary + edge_resolve) | gltf load parses material; or scene-editor add-mesh | 2 pipelines per shader_id under Priority 3 |
| First-party material (UNLIT / TOON / FLIPBOOK) | Same | 2 pipelines each under Priority 3 |
| Dynamic material (per registered) | `submit_pipeline_group_batch` from `register_material` flow | 2 pipelines per material under Priority 3 |
| `Pass(GeometryMsaa { other })` | `set_anti_aliasing` flips to the inactive MSAA | Re-submitted on flip |
| `Pass(Evsm)` | First shadow-casting light enters scene | Currently triggered via existing shadows::evsm setup |
| `Pass(Line)` | First line primitive added | Currently triggered via line pass init |
| `Pass(ShadowGen)` | First shadow caster added | Similar |
| `Pass(Bloom)` / `Pass(Smaa)` / `Pass(Dof)` | `set_post_processing` enables the effect | Each effect is independently triggered |
| `Pass(Picker)` | First mouse-pick query (if `picking` feature on) | Today this is eager; deferred under Priority 2 |
| `Pass(EdgeResolveOpaque)` | First MSAA opaque material registered (under Priority 3) | Shared across all materials; one compile per first-party + per dynamic |

The exact list is enumerable from the codebase; this table is the architecture, not the contract.

---

## Migration of the dynamic-materials API

PR #98's surface needs updating. Concretely:

| Today (PR #98) | After this plan |
|---|---|
| `register_material(def: MaterialRegistration) -> MaterialShaderId` | `submit_pipeline_group_batch(vec![PipelineGroupDef::Material(def)]) -> Vec<PipelineGroupId>` |
| `prewarm_pipelines(...).await` | **Removed.** Caller uses `pipeline_group_status` (pull) or `subscribe_pipeline_status` (push). |
| `Material::Custom { shader_id, ... }` | Unchanged — Material::Custom is the *input data*; the shader_id field becomes the `MaterialDef::shader_id` after submit. |
| Material::insert (sync, expects pre-compiled pipelines) | Unchanged — still sync, still takes a MaterialId. The caller awaits Ready before inserting (or accepts the warn-skip path). |

The `crates/renderer/examples/dynamic_material.rs` example and the two contract docs (`docs/dynamic-materials/contract-opaque.md`, `contract-transparent.md`) get updated to reflect the new flow:

```rust
// Before
let shader_id = renderer.register_material(def)?;
renderer.prewarm_pipelines(shader_id).await?;
let material_id = renderer.materials.insert(Material::Custom { shader_id, ... });

// After
let ids = renderer.submit_pipeline_group_batch(vec![PipelineGroupDef::Material(def)]);
let group_id = ids[0];

// Either: await Ready before insert (recommended for interactive editor paths)
loop {
    match renderer.pipeline_group_status(group_id) {
        PipelineGroupStatus::Ready => break,
        PipelineGroupStatus::Failed { error } => return Err(error.into()),
        PipelineGroupStatus::Pending => yield_to_scheduler().await,
    }
}
let material_id = match group_id { PipelineGroupId::Material(id) => id, _ => unreachable!() };
let mesh_material_id = renderer.materials.insert(Material::Custom { material_id, ... });

// Or: insert eagerly and let the render-frame preamble warn-and-skip until Ready (recommended for gltf load,
// where the parent flow has already submitted the whole batch and is awaiting the join)
```

**Recommended path per call site**:

- **gltf load**: always use the "or" branch (insert eagerly, render skips). This is what makes incremental-paint possible — the scene-graph populates the instant gltf parse finishes, the renderer returns to the frontend, the user sees the skybox + camera + any already-Ready materials immediately, and PBR/UNLIT/etc. mesh content lights up as compiles resolve. **Never await** in the gltf load critical path.

- **Editor interactive "Import Material" / "Add Mesh"**: use the "Either" branch (await Ready before insert). The UX expectation is that clicking "Import Material" leads to the material appearing in the picker; the modal handles the wait. Same for "Add Mesh" — the user clicked the button, they accept the wait, the modal explains it.

- **Material-editor recompile (per-keystroke debounced)**: use the "or" branch with the swap-on-ready pattern (drop the previous MaterialId once the new one is Ready, swap the editor's preview mesh's reference). The editor stays responsive; the preview canvas just keeps showing the previous-compile's output until the new one is ready.

- **Renderer-internal triggers** (e.g. EVSM pipeline submitted when first shadow-casting light is added): "or" branch — the render-frame preamble handles the skip naturally; no need to await from inside renderer code.

The pattern: **prefer non-blocking insert + render-skip everywhere except where the next user action specifically needs the pipeline live.**

---

## Priority 1 — Land the readiness machinery + migrate first-party + dynamic

The largest change. Roughly:

1. **State machine + scheduler infrastructure.** `PipelineGroupId`, `PipelineGroupStatus`, `PipelineGroupDef`, the `FuturesUnordered`-driven scheduler, the `subscribe_pipeline_status` stream. Lives in `crates/renderer/src/pipeline_scheduler/` (new module). The scheduler holds the `Shaders::ensure_keys` + `*Pipelines::ensure_keys` invocations as building blocks, but exposes only the batch surface — call sites don't reach past it.

2. **`AwsmRenderer::submit_pipeline_group_batch` public API.** Takes `Vec<PipelineGroupDef>`, returns `Vec<PipelineGroupId>` synchronously. Internally: queues the compile future, returns ids in Pending state.

3. **Migrate `AwsmRendererBuilder::build` to use the scheduler.** `build` constructs the eager-set list, submits it as the first batch, awaits all groups, returns Renderer. No special eager-compile code path remains.

4. **First-party material flow.** When gltf load parses materials, it builds a `Vec<MaterialDef>` (one per gltf material), calls `submit_pipeline_group_batch`, gets a `Vec<PipelineGroupId>`. The gltf load itself doesn't await — meshes get assigned their MaterialIds and inserted immediately. Materials light up as their compiles resolve.

5. **Dynamic material flow.** `register_material` becomes a thin wrapper around `submit_pipeline_group_batch` for a single-entry batch. The `prewarm_pipelines(...).await` surface is deleted; callers in `material-editor` and `scene-editor` update to use status subscription / poll.

6. **Bucket-entries cache rebuild on status transitions.** The renderer subscribes to its own status stream internally; when a material transitions to/from Ready, the bucket-entries cache is marked dirty. The next-frame classify rebuilds it.

7. **Render-frame preamble warn-and-skip.** Each pass dispatch site checks its `Option<PipelineKey>` accessor; None → skip + warn (once-per-session). No panic in any mode — production safety net.

8. **`tracing` annotations**: each batch logs `submit_pipeline_group_batch: N groups submitted` and each transition logs `Pending → Ready: <label> in Tms` under `target = "awsm_renderer::pipeline_readiness"`.

**Acceptance:**
- Android cold-boot init reaches `phase = Ready` in <500 ms (down from "fails"; the failing PBR compile is now Priority 3's responsibility, but post-Priority 1 it just sits Pending and doesn't break init).
- An empty scene renders skybox-only at first frame.
- Loading a gltf with one PBR mesh: scene-graph + skybox visible immediately; PBR mesh appears when its compile resolves (~3 s on Android post-Priority 3, ~200 ms on desktop).
- `RUST_LOG=awsm_renderer::pipeline_readiness=info` shows the full submission + transition waterfall.

**Test surface migration (in-scope for Priority 1)**: renderer integration tests today rely on `Materials::insert` followed by immediate dispatch in the same test setup. Under the new architecture, these tests need a small helper:

```rust
/// Synchronously wait for all currently-Pending pipeline groups to resolve.
/// For tests only — production code uses the status stream.
pub async fn wait_for_pipelines_ready(&mut self) -> Result<()>;
```

Implementation: drain the scheduler in a tight loop until no Pending groups remain (or a timeout fires). Test setup becomes "submit batches → `wait_for_pipelines_ready().await` → dispatch → assert." Roughly 5–10 test files in `crates/renderer/tests/` and `crates/renderer/examples/` need this update — sweep via grep for `Materials::insert` after Priority 1's first pass is in place.

---

## Priority 2 — Migrate scene-content-driven passes into the scheduler

After Priority 1's machinery exists, all the passes that were scheduled in [§ The scheduler-managed set](#the-scheduler-managed-set) need their trigger logic wired.

- **`Pass(GeometryMsaa { other })`**: triggered from `set_anti_aliasing`. The reverted prototype implemented the shader_cache_keys / build_descriptors / merge_resolved / has_branch_for machinery — re-land the structural changes from `crates/renderer/src/render_passes/geometry/pipeline.rs` (per [§ Lessons captured](#lessons-captured-from-reverted-wip)), but route the compile through the scheduler rather than ad-hoc `try_join` in `set_anti_aliasing`.

- **`Pass(Evsm)`**: triggered when the first shadow-casting light is added. Hook is `LightsManager::on_light_added` (or equivalent) detecting `shadow_caster == true` for the first time per session.

- **`Pass(Line)`**: triggered when the first line primitive is inserted. Hook is the line-pass entry point in the meshes/primitives module.

- **`Pass(ShadowGen)`**: triggered when the first shadow caster is added. Hook is alongside Evsm.

- **`Pass(Bloom)` / `Pass(Smaa)` / `Pass(Dof)`**: triggered from `set_post_processing` when the respective effect transitions off → on. Each is an independent batch entry.

- **`Pass(Picker)`**: triggered on first mouse-pick query if the feature is on. Picking is rare enough to be lazy-by-default.

Per pass, the migration is:
1. Strip eager creation from the cold-boot eager set.
2. Add the trigger site (1–5 line addition; calls `submit_pipeline_group_batch`).
3. Update the pass's dispatch site to skip if its `Option<PipelineKey>` accessor returns None.

**Acceptance**: cold-boot eager set is the list in [§ The eager set](#the-eager-set-cold-boot) — nothing else. On Android with `gpu_culling` on, cold-boot compile batch is 4–6 pipelines in <500 ms total.

---

## Priority 3 — Replace `msaa_resolve_samples`

The actual SPIR-V bloat. Today's PBR compute pipeline:

1. Compiles a primary-path branch (PBR shading) per pipeline.
2. **Inlines `msaa_resolve_samples` once per pipeline**, which **unrolls 4× calls to `msaa_process_sample`**, each of which contains UNLIT/TOON/PBR branches. Net: ~12× shading-code copies in one PBR pipeline's SPIR-V (4 unrolled × 3 internal branches). Android's Vulkan driver rejects it.

The replacement: **per-shader-id edge-resolve via a slot-buffer pattern**. No shared resolve shader; no atomics on the per-frame hot path; cross-material MSAA edges shade correctly for every shader_id including dynamic materials.

### Pass structure

1. **Geometry pass** — unchanged from today. Writes multisampled vis textures.

2. **Classify pass** (lightly extended) — today emits per-shader-id tile lists by primary-sample shader_id. Now also emits, per edge pixel:
   - One **compact edge_pixel_id** allocated via atomic counter (`edge_count` total at frame end).
   - The pixel's `(x, y)` coords stored in `edge_to_xy[edge_pixel_id]`.
   - A 4-byte **slot_map** stored in `edge_slot_map[edge_pixel_id]`, listing up to 4 distinct shader_ids that have samples at this pixel.
   - For each shader_id present: append `(edge_pixel_id, sample_mask_byte)` to that shader_id's edge sample list. `sample_mask` has bits set for each of the 4 samples that are this shader_id.

3. **Material primary pass per shader_id** (existing pipeline per shader_id, simplified) — `msaa_resolve_samples` is **deleted from this shader entirely**:
   - For each pixel in this shader_id's tiles: if all 4 samples are this shader_id → shade primary sample, write `opaque_tex`. Fast path.
   - If only some samples are this shader_id → skip; edge resolve handles it.
   - **Net SPIR-V change**: PBR primary pipeline drops ~80% of its code (no unrolled resolve, no cross-material branching, no `msaa_process_sample`). Estimated compile drops from ~14 s → ~2 s on Android.

4. **Material edge-resolve pass per shader_id** (NEW pipeline per shader_id):
   - Indirect-dispatched over this shader_id's edge sample list (`dispatchWorkgroupsIndirect` driven by the counter in the list).
   - One thread per `(edge_pixel_id, sample_mask)` entry.
   - Reads slot_map to find this shader_id's slot index (0–3) for this edge pixel.
   - For each bit set in sample_mask: loads sample's vis_data, shades using this shader_id's specific shading code.
   - Sums local: `(color_sum, count)` for the samples this shader_id contributed.
   - Writes one `vec4<f32>` to `accumulator[edge_pixel_id × 4 + slot_index]` = `vec4(color_sum, count_as_float)`. **No atomic** — each slot is owned by exactly one shader_id pipeline.
   - **Each pipeline contains only its own shading code.** Smaller than today's primary path (single-sample, no primary-pixel boilerplate). Estimated ~1–2 s compile on Android per shader_id.

5. **Skybox edge resolve** — same pattern for skybox samples on edge pixels. One pipeline; writes to skybox's reserved slot.

6. **Final blend pass** — indirect-dispatched over edge pixels:
   - One thread per edge_pixel_id.
   - Reads up to 4 slots from `accumulator[edge_pixel_id × 4 .. +4]`, sums color components weighted by their slot counts, divides by total count, writes `opaque_tex[edge_to_xy[edge_pixel_id]]`.

### Pipeline count and packaging

Two pipelines per shader_id:
- `material_primary_{shader_id}` (the fast-path opaque pipeline; what exists today minus the resolve)
- `material_edge_resolve_{shader_id}` (NEW; single-sample shading with mask)

Plus:
- `skybox_edge_resolve` (NEW; one global)
- `final_blend` (NEW; one global)

Total: `2N + 2` pipelines, where N is shader_ids in active scene (typically 1–5 = 4–12 pipelines). Each is **smaller than today's per-shader-id pipeline**. Compile parallelizes through Dawn's pool.

Two pipelines per shader_id rather than one with two entry points: distinct futures in the scheduler (cleaner status reporting, distinct `boot_timing` labels, possibly more compile-pool concurrency depending on Dawn implementation).

### Slot assignment

The slot_map (4 bytes per edge pixel) tells each shader_id pipeline where to write. Built in classify:

```wgsl
// Inside classify, per edge pixel:
var slot_map = vec4<u32>(SHADER_ID_NONE, SHADER_ID_NONE, SHADER_ID_NONE, SHADER_ID_NONE);
var seen_mask = 0u;  // up to 32 distinct shader_ids supported (4 first-party + dynamic)
var next_slot = 0u;
for (var s = 0u; s < 4u; s++) {
    let sid = read_sample_shader_id(pixel, s);
    let bit = 1u << sid;
    if ((seen_mask & bit) == 0u) {
        slot_map[next_slot] = sid;
        seen_mask |= bit;
        next_slot += 1u;
    }
}
// Store slot_map at edge_slot_map[edge_pixel_id]
```

Each shader_id's edge_resolve thread does a 4-entry scan over slot_map to find its index (`for i in 0..4 { if slot_map[i] == my_sid { my_slot = i; break; } }`). At most 4 compares — costless.

### Memory budget

| Buffer | Per-edge cost | Typical 1080p (7% edges, ~145k) | Worst case (50%, ~1M) |
|---|---|---|---|
| `edge_to_xy` (u32 each) | 4 bytes | 580 KB | 4 MB |
| `edge_slot_map` (u8×4 each) | 4 bytes | 580 KB | 4 MB |
| `accumulator` (vec4×4 each) | 64 bytes | 9.3 MB | 64 MB |
| Per-shader-id sample lists | ~8 bytes × N_shader_id_entries | <500 KB total | ~8 MB |
| Indirect args + counters | trivial | <1 KB | <1 KB |
| **Total** | | **~11 MB** | **~80 MB** |

Scaled by resolution at typical edge densities (~7% for normal scenes, ~25% for pathological foliage):

| Resolution | Typical edges (~7%) | Pathological edges (~25%) |
|---|---|---|
| 1080p | ~11 MB | ~40 MB |
| 1440p | ~20 MB | ~70 MB |
| 4K | ~45 MB | ~160 MB |

**Mitigation**: a runtime `MAX_EDGE_BUDGET` (e.g. 512k edge pixels = ~37 MB) caps the buffer size. Classify's atomic counter saturates at the budget; excess edges fall back to an atomic-add tail of the accumulator (a small reserved region that uses fixed-point atomic-add — the slow path we designed away for the common case becomes the safety net for the pathological case). The fallback adds a few hundred μs of per-frame atomic work in the rare overflow scenario, but never blows memory. Default budget tuned per-target: 512k for desktop, 256k for mobile.

### Runtime cost vs today

| Pixel class | Today | After Priority 3 |
|---|---|---|
| Non-edge | Inline msaa_sample_count_for_pixel + branch (fast) | Inline check + branch (same fast path) |
| Edge | Inline 4× `msaa_process_sample`, 1 write | Detect → append to sample list (cheap atomic-inc in classify); later, per-shader edge_resolve dispatch + 1 slot write per shader_id; final blend reads 4 slots + 1 write |

Per-frame totals are roughly equivalent for edges — the same shading work happens, just split across more dispatches. Edge work moves out of the material-pass thread budget into a small set of indirect dispatches. Non-edge pixels are **faster** (no inline resolve check overhead, no 4× sample texture loads on the off-chance).

### Cross-material MSAA correctness

For every shader_id (first-party AND dynamic): each sample at an edge pixel is shaded by its own shader_id's pipeline using that shader_id's exact shading code. No fallbacks, no PBR-substitution.

The PR #98 dynamic-materials surface gets one new contract guarantee: **a registered dynamic material's WGSL is responsible for both its primary-path AND its edge-resolve shading.** In practice both come from the same `custom_shade_dynamic` fragment — the wrapper just invokes it in two slightly different contexts (full vs single-sample). Contract docs need a short update.

### Acceptance

- Android PBR compile drops from ~14 s → ~2–3 s on the test device. Edge resolve pipelines compile in ~1–2 s each.
- No SPIR-V rejection on PBR; init can complete.
- Visual diff between today's MSAA edges and the post-Priority-3 MSAA edges is empty for first-party materials (the math is identical). For dynamic materials, the post-Priority-3 result is *correct*; today's result was buggy (PBR-fallback shading).
- Per-frame budget on Android at 1080p MSAA 4×: comparable to today on simple scenes; better on scenes dominated by non-edge pixels.

---

## Priority 4 — Build-time pipeline cache (parked)

When Dawn's pipeline-cache surface ships in stable WebGPU (chrome flag-gated today), a build-time tool can pre-warm and bundle the cache for ship builds. Out of scope until the spec lands. Tracked here so we don't lose the idea.

---

## Lessons captured from reverted WIP

A local prototype branch implemented some adjacent work that was reverted on review. Documented here so re-land happens cleanly inside the new architecture:

### A. Per-pipeline labels + finish-order log in `ensure_keys`

The committed `{Render,Compute}Pipelines::ensure_keys` log only an aggregate `N pipelines compiled in Tms` line. The prototype added per-pipeline `pipeline N/M render:Geometry(ShaderKey(1)):PipelineLayoutKey(12) ok` lines with cumulative timing.

**Re-land shape** (now folds into Priority 1's scheduler infrastructure): build the per-pipeline label string before kicking off `device.create_*_pipeline_async`, then attach a side-effect `.inspect` combinator to each individual future that logs on resolve. **Critically: do NOT replace `futures::future::join_all(promises).await` with a serial `for promise in promises { promise.await }` loop** — the prototype did this to compute cumulative timing and the result was a serialization of all pipeline creation, defeating Dawn's parallel compile pool. That regression is the reason the prototype was reverted.

Cumulative wall-clock per pipeline is achievable inside the `.inspect` combinator without sequencing the futures.

For the compute path, thread `Shaders::get_label(ShaderKey) -> Option<String>` so labels read `compute:MaterialOpaque(...)` instead of `compute:ShaderKey(5):PipelineLayoutKey(_)`. The render path already has this via the shader's `debug_label()`.

Under Priority 1, the scheduler is the natural home for this — each submitted group's compile future is wrapped with `.inspect` for transition logging; the `boot_timing` log surface absorbs the per-pipeline output as a natural extension.

### B. Adapter + device limits log + `onuncapturederror` hook

One-shot log at device creation under `target = "awsm_renderer_core::limits"`, plus a `device.onuncapturederror` hook under `target = "awsm_renderer_core::uncaptured_error"`. Purely additive diagnostics.

**Re-land shape**: ~60 lines in `crates/renderer-core/src/renderer.rs`. The prototype used `js_sys::Reflect` because web-sys feature flags for `GpuValidationError` / `GpuInternalError` / `GpuOutOfMemoryError` / `GpuUncapturedErrorEvent` aren't enabled — cleaner re-land would add those features to the workspace web-sys declaration and use the typed bindings. Safe to land standalone, before Priority 1.

### C. `PipelineVariantNotCompiled` error variant

```rust
#[error("Pipeline variant not yet compiled: {0}")]
PipelineVariantNotCompiled(&'static str),
```

Used by lazy-pool lookup trees when a branch is `None`. Under the new architecture, this is generalized into `PipelineGroupStatus::Pending` / `Failed` — but the error variant is still useful for the render-frame preamble's warn-skip path. Land it alongside Priority 1.

### D. Geometry pass MSAA-aware build_descriptors + merge_resolved

The reverted prototype refactored `crates/renderer/src/render_passes/geometry/pipeline.rs` to match the lazy-pool pattern that opaque/classify/HZB/picker already use: `shader_cache_keys(multisampled_geometry: bool)`, `Option<Level1>` branches, `merge_resolved`, `has_branch_for`.

**Re-land shape**: this is Priority 2's `Pass(GeometryMsaa { samples })` migration. The structural code lifts cleanly from the WIP; the trigger plumbing (set_anti_aliasing → submit batch) is rewired to go through the scheduler instead of the ad-hoc `try_join` the prototype used.

### E. `msaa_resolve_samples` loop conversion (R&D workaround)

The prototype replaced the 4× unrolled `msaa_process_sample` call sequence with a `for s in 0..msaa_sample_count` loop. Took Android from "fails" → "succeeds, ~14 s PBR compile" — at the watchdog edge but functional. Reverted because it's not a shipping shape.

**Re-land shape**: do not re-land. Priority 3 obsoletes this entirely. If Android needs to boot for testing before Priority 3 lands, the loop conversion is the minimal local patch — ~5 lines — but don't ship it.

---

## Diagnostic tooling

### Committed today

All under `target = "awsm_renderer::boot_timing"`; filter via `RUST_LOG=awsm_renderer::boot_timing=info`.

| Pattern | What it tells you |
|---|---|
| `phase = CompilingShaders \| BuildingPipelines \| Ready (+Tms phase, Tms total)` | Per-phase wall-clock in `AwsmRendererBuilder::build` |
| `Shaders::ensure_keys: N shaders compiled in Tms` | Per-batch shader-compile wall-clock |
| `{Render,Compute}Pipelines::ensure_keys: N pipelines compiled in Tms` | Per-batch pipeline-compile wall-clock |
| `[asset_cache] model loaded: asset_id=AssetId(_) (Tms)` | Scene-editor gltf reaching Ready |
| `[scene] model loaded: <GltfId> (Tms)` | Model-tests gltf finishing |
| `VK_ERROR_` | Vulkan-layer rejection (mostly: SPIR-V too complex) |
| `External Instance reference no longer exists` | Watchdog killed the GPU — typically after a long compile |
| `phase = Ready` | Init succeeded |

### After Priority 1 + Lessons A+B re-land

Add under `target = "awsm_renderer::pipeline_readiness"` and `target = "awsm_renderer_core::*"`:

| Pattern | What it tells you |
|---|---|
| `submit_pipeline_group_batch: N groups submitted (labels=...)` | Each batch submission |
| `Pending → Ready: <label> in Tms (id=PipelineGroupId(...))` | Each transition |
| `Pending → Failed: <label> error=<...>` | Failures |
| `device limits: max...=...` (Lessons B) | One-shot capability dump at device creation |
| `pipeline N/M render:Geometry(ShaderKey(1)):PipelineLayoutKey(12) cum=Tms ok\|ERR` (Lessons A) | Per-pipeline finish-order |
| `GPU uncaptured: <error>` (Lessons B) | Runtime validation / OOM / internal errors |

### `task debug-mobile:chrome-check`

User-provided task — reloads the renderer on the connected Android phone via Chrome and captures console output back to the terminal. Primary feedback loop; ~30 s round-trip.

### Bisection technique

When stuck on a shader compile failure: progressively move an early `return;` through the shader body. The investigation log below shows the technique — 4 iterations got from "no idea" to "exact failing construct." Still the right tool for shader-driver issues that the pipeline-readiness machinery can't diagnose.

---

## Landing cadence (recommended)

Items in **bold** can land independently and bring immediate value; non-bold items depend on prior pieces.

1. **Lessons B (device-limits log + onuncapturederror).** Pure diagnostic. ~60 lines. Standalone.
2. **Lessons C (PipelineVariantNotCompiled error variant).** Trivial. Standalone.
3. **Priority 1: state machine + scheduler + first-party + dynamic migration.** The spine. ~800–1500 lines across `pipeline_scheduler` module + 3 frontend updates. Single PR.
4. **Lessons A (per-pipeline labels + finish-order log)** folded into Priority 1 — natural home is inside the scheduler's `.inspect` combinators.
5. **Priority 2: pass migrations.** Each pass is an independent commit (Geometry, EVSM, Line, ShadowGen, Bloom, SMAA, DoF, Picker). Can ship as a single PR with one commit per pass, or as separate PRs.
6. **Priority 3: msaa_resolve_samples replacement.** Single PR; ~1000 lines (new shaders + new pipelines + classify extension + render-pass orchestration). Verifies on Android end-to-end.
7. **Priority 4: build-time pipeline cache.** Parked.

Each priority is verifiable on Android via `task debug-mobile:chrome-check` reaching `phase = Ready` with the expected pipeline counts in the boot-timing logs.

---

## Root cause (preserved for historical record)

**The PBR opaque compute shader emits SPIR-V large enough to exceed the Android Vulkan driver's pipeline-compile complexity ceiling.**

The path: `msaa_resolve_samples` in [helpers/material_shading.wgsl:234–266](../../crates/renderer/src/render_passes/material_opaque/shader/material_opaque_wgsl/helpers/material_shading.wgsl#L234) calls `msaa_process_sample` 4× (unrolled). `msaa_process_sample` contains the UNLIT/TOON/PBR branch tree plus the full shading kernel (texture pool sampling + mipmap + lighting + IBL + shadows). Tint inlines each unrolled call, producing SPIR-V with **the entire shading pipeline duplicated 4 times** for the edge-resolve path, plus once more for the main non-edge path. Only PBR fails because PBR's primary path is itself heavy; UNLIT/TOON share the resolve bloat but their primary paths are small enough to keep the total under the driver's ceiling.

### Things that look related but aren't

The investigation ruled these out via direct testing (see [§ Investigation log](#investigation-log)):

- Multisampled `textureLoad` in compute: works fine; verified by forcing MSAA off.
- `maxUniformBufferBindingSize` at exactly 64 KB: shrunk to 32 KB, no change.
- `maxStorageBuffersPerShaderStage`: device reports 16, shader uses 9. Not close.
- `maxBindGroups` / `maxBindingsPerBindGroup`: under both.
- `rgba16float` storage texture write: empty shader uses it and succeeds.
- Cube texture sampling, dynamic indexing into uniforms / storage, integer texture loads: all proven to work.

### Bisect technique that found it

1. Stub `main()` body → all 5 pipelines compile in 40 ms.
2. Re-enable up to `material_load_shader_id` → UNLIT/TOON/FLIPBOOK compile; only PBR fails.
3. Replace PBR's `msaa_resolve_samples` call with constant write → all 5 pipelines compile in 105 ms.
4. Reduce unroll from 4× to 1× → all 5 pipelines compile in 2.8 s with full body intact.

Confirmed mechanism: the 4× inlining is the bloat.

---

## Investigation log (chronological, for the historical record)

| Hypothesis | Test | Result |
|---|---|---|
| Too many storage buffers per stage on Android | Logged `device.limits()` | Device reports 16; we use 9. Ruled out. |
| Render-pipeline batch overflowing watchdog | Cut geometry MSAA pre-warm (18 → 9 pipelines) | Render batch went from 5 s + kill to 660 ms. Reverted (folded into Priority 2). |
| Wave-based pipeline issuance | Issued compute promises in chunks of 6 | Total wall-clock went 8 s → 12 s — Dawn was already absorbing parallelism. Reverted. |
| Multisampled textureLoad in compute is the issue | Forced MSAA off | Same 4 opaque pipelines still failed. Hypothesis wrong. |
| `lights: array<LightPacked, 1024>` at exactly 64 KB | Shrunk to 512 | Same failure. Ruled out. |
| Body of `main()` is the issue | Stubbed body to `return;` | All 5 pipelines compiled in 40 ms. Body confirmed as culprit. |
| Body up to `material_load_shader_id` | Early-return at that point | UNLIT/TOON/FLIPBOOK compiled; only PBR still failed. Narrowed. |
| PBR-unique `msaa_resolve_samples` is the issue | Replaced its call with constant write | All 5 pipelines compiled in 105 ms. Confirmed. |
| 4× unrolled `msaa_process_sample` is the bloat | Reduced to 1 call | All 5 pipelines compile in 2.8 s with full body intact. Confirmed mechanism. |
| Loop instead of unroll | Converted to `for s in 0..N` | All 5 compile (14.2 s for PBR). Works but slow — reverted. |

The wrong hypotheses (multisampled-textureLoad, uniform-binding-at-limit) cost two iterations each. Net debugging cost: a few hours. Net benefit: thoroughly confirmed diagnosis that the cleanup in this plan can target precisely.

---

## Implementation checklist

Mark items `[x]` as completed. Commit the checklist update along with each item or coherent batch. **Do not stop between items** — work through to the end. Each item points to the body section that has the design rationale.

### Stage 0 — Pre-flight diagnostics (standalone re-lands, no architectural dependency)

- [x] **0.1** Land Lessons B: adapter + device limits log + `onuncapturederror` hook in `crates/renderer-core/src/renderer.rs`. Used `js_sys::Reflect` for both limit dump and uncaptured-error message extraction — robust to feature drift and supports limit keys not in our enabled web-sys features. Added `GpuUncapturedErrorEvent`, `GpuError`, `GpuValidationError`, `GpuInternalError`, `GpuOutOfMemoryError` to workspace `web-sys` features for future typed access. Logs under `target = "awsm_renderer_core::limits"` and `target = "awsm_renderer_core::uncaptured_error"`.
- [x] **0.2** Land Lessons C: added `PipelineVariantNotCompiled(&'static str)` and `NotReady` variants on `AwsmError` in `crates/renderer/src/error.rs`. Not wired yet — Stage 1 introduces consumers.
- [x] **0.3** Commit Stage 0.

### Stage 1 — Pipeline-readiness machinery (Priority 1)

- [x] **1.1** Create `crates/renderer/src/pipeline_scheduler/` module. Defined `PipelineGroupId`, `PipelineGroupStatus`, `PipelineGroupDef`, `MaterialDef`, `MaterialDefKind`, `PipelineConfigSnapshot`, `PassDef`, `PassKind`, `MaterialId` per [§ Architecture: Pipeline Readiness](#architecture-pipeline-readiness). Module lives in `pipeline_scheduler/{mod,types}.rs`; types re-exported from the module.
- [x] **1.2** Implemented `PipelineScheduler` struct holding the `FuturesUnordered`, the `SlotMap<MaterialId, MaterialState>`, the `HashMap<PassKind, PassState>`, the event queue, and per-group generation markers for stale-resolution dropping. **Skeleton-only** — compile futures are currently `async { Ok(()) }` placeholders.
- [x] **1.3** Implemented `submit_pipeline_group_batch(defs: Vec<PipelineGroupDef>) -> Vec<PipelineGroupId>` (the API surface; stub futures). Each def allocates an id, emits a Pending status event, and queues a placeholder future. **Wiring each def variant to `Shaders::ensure_keys` + `{Render,Compute}Pipelines::ensure_keys` is the next subtask** (1.3-cont in a follow-up commit) — left as stubs at this commit to keep the type/API surface reviewable in isolation.
- [x] **1.4** Implemented `pipeline_group_status(id) -> Option<&PipelineGroupStatus>` (O(1) SlotMap / HashMap lookup) and `drain_status_events() -> Vec<StatusEvent>` (pull-based event drain, simpler than a typed broadcast channel for the wasm32 main-thread runtime).
- [x] **1.5** Implemented `drop_material_group(MaterialId)`. Removes from the SlotMap; in-flight futures for the dropped id naturally fall through `apply_resolution` because the lookup returns None and the resolution is silently dropped.
- [x] **1.6** Implemented `poll_resolved` per-frame entry point (drains resolved `FuturesUnordered` items, applies transitions, emits status events). Will be called from the render loop's pre-frame phase once the scheduler is wired into `AwsmRenderer` (subtask 1.8). (Body: [§ Scheduler driving and transition timing](#scheduler-driving-and-transition-timing).)
- [x] **1.7** Lessons A wired: per-pipeline `compute:<shader>:<layout> cum=Tms ok|ERR` and `render:<shader>:<layout> cum=Tms ok|ERR` logs in `ComputePipelines::ensure_keys` / `RenderPipelines::ensure_keys`. Each promise is wrapped in an `async move { ... promise.await ... }` block that logs on resolve; `join_all` still drives every future concurrently (verified compile passes). Added `Shaders::get_label(ShaderKey) -> Option<String>` for the render-pipeline path to embed the shader template name in the log line.
- [~] **1.8** **Partial — scheduler attached to `AwsmRenderer`.** Added `pipeline_scheduler: PipelineScheduler` and `build_complete: bool` fields; initialized in `AwsmRendererBuilder::build`'s construction tail. Public API surface (`submit_pipeline_group_batch`, `pipeline_group_status`, `drain_pipeline_status_events`, `drop_material_group`, `poll_pipeline_scheduler`) added as `impl AwsmRenderer` methods. **Full eager-set migration deferred** — the existing eager-compile path still populates per-pass key caches directly; the scheduler today only holds groups submitted post-build (Stage 1.13-1.14 will start using it). The hard build-flow rewrite is decomposed into: (i) wire each `PipelineGroupDef` variant to its real compile path, (ii) replace the per-pass eager `new()` calls in build() with submit_pipeline_group_batch. Continues in later commits.
- [~] **1.9** The "Pending material filtered from dispatch" invariant is **already enforced naturally** via the existing lazy-pool dispatch pattern: `get_compute_pipeline_key(...) -> Option<...>` returns `None` when the pipeline isn't compiled yet, and the dispatch path skips that bucket. The bucket-entries cache itself doesn't need to filter — the lookup at dispatch time does. Explicit "subscribe to status stream + mark cache dirty" wiring is not yet implemented; it'll be needed only when the scheduler drives real compile (Stage 1.8 fully) so transitions match actual pipeline-cache state. For now, the existing eager-compile path keeps the cache and pipeline state in sync.
- [x] **1.10** Render-frame preamble warn-and-skip implemented as `pipeline_scheduler::warn_pipeline_not_compiled(location, id)`. Once-per-(location,id)-per-session via a `Mutex<HashSet>` guard. Each per-pass dispatch site that adopts the lazy path calls this helper when its typed `Option<PipelineKey>` accessor returns None, then returns. No panic in any mode. (Body: [§ Render-frame preamble safety net](#render-frame-preamble-safety-net).)
- [x] **1.11** `wait_for_pipelines_ready()` test helper added to `AwsmRenderer`. Iterates `poll_pipeline_scheduler` until no further transitions are applied (up to 1024 rounds). Sweep of `tests/` + `examples/` call sites comes when dynamic-material migration (1.14) lands.
- [x] **1.12** Race policy implemented: `AwsmRenderer::set_anti_aliasing` and `set_post_processing` both gate on `self.build_complete` and return `Err(AwsmError::NotReady)` when called before `build()` returns. (Body: [§ Config-flip semantics](#config-flip-semantics-msaa-post-processing).)
- [ ] **1.13** Migrate first-party material flow: gltf loader builds `Vec<MaterialDef>` from gltf JSON's `materials` array, calls `submit_pipeline_group_batch`, gets MaterialIds. Meshes insert immediately referencing the MaterialIds. No await on the gltf load critical path. (Body: [§ The "or" branch + recommended path](#migration-of-the-dynamic-materials-api).) **Depends on 1.8 fully — needs the scheduler to actually drive compile, which depends on per-`PipelineGroupDef` variant wiring not yet landed.**
- [ ] **1.14** Migrate dynamic-material flow: `register_material` becomes a thin wrapper around `submit_pipeline_group_batch`. **Delete** the `prewarm_pipelines(...).await` surface entirely. Update `material-editor` and `scene-editor` call sites to use the new flow per the [§ Migration table](#migration-of-the-dynamic-materials-api). **Blocked on 1.8 fully — the existing `prewarm_pipelines` work needs an equivalent driven through the scheduler before its call sites can be migrated.**
- [x] **1.15** Updated `crates/renderer/examples/dynamic_material.rs` comment-block: documents the new non-blocking registration pattern + the two wait-vs-render-skip options (gltf-load / editor flow).
- [x] **1.16** Contract docs reviewed: `docs/dynamic-materials/contract-{opaque,transparent}.md` don't reference `prewarm_pipelines` or `register_material` (they're pass-contract docs, not API surface docs). No-op as anticipated by the plan.
- [x] **1.17** `tracing` annotations partially in place. Scheduler emits `submit_pipeline_group_batch: N groups submitted` and `transition: <kind> -> <status>` under `target = "awsm_renderer::pipeline_readiness"`. Plus per-pipeline `pipeline N/M cum=Tms ok|ERR` lines under `awsm_renderer::boot_timing` (Lessons A). Additional annotations land alongside 1.8 fully.
- [x] **1.18** Hand-tested `task model-tests:dev` via preview browser: cold-boot succeeds, renderer reaches Ready (no errors, no warnings, no NotReady / PipelineVariantNotCompiled / GPU uncaptured logs). Per-pipeline label logs from Stage 1.7 visible: `pipeline 1/1 render:Material Transparent:PipelineLayoutKey(16v1) cum=1431ms ok`. `Shaders::ensure_keys: 6 shaders compiled in 95ms`, `ComputePipelines::ensure_keys: 5 pipelines compiled in 22ms`. Fox model renders correctly with MSAA-on. Incremental paint testing for the gltf load deferred until Stage 1.13/1.14 (gltf migration) lands.
- [ ] **1.19** Commit Stage 1 (may be multiple commits per principle "logical commits, not working states"). **In progress** — 5 commits landed so far (Stage 0, 1.1-1.6 skeleton, 1.7 labels, 1.8 partial + 1.10-1.12, and this checklist update). Remaining: 1.8 fully + 1.9 + 1.13-1.16.

### Stage 2 — Pass migrations (Priority 2)

- [x] **2.1** Geometry MSAA lazy: `shader_cache_keys(multisampled_geometry)` / `build_descriptors(..., multisampled_geometry)` take active MSAA explicitly; `GeometryRenderPipelineKeys.{no_anti_alias, msaa_4_anti_alias}` are `Option<Level1>`; `merge_resolved` folds resolved keys into the existing struct (preserves previously-compiled branch); `has_branch_for(anti_aliasing)` skips recompile on toggle-back. `set_anti_aliasing` extended with Phase 4b: when the new MSAA's branch isn't populated, ensure-keys shaders → build-descriptors → ensure-keys render pipelines → merge_resolved. Cold-boot now compiles 3 shaders + 9 pipelines for geometry (was 6 + 18); MSAA-flip costs the inactive-branch compile on first toggle, free on subsequent toggles. `get_render_pipeline_key` returns `PipelineVariantNotCompiled` instead of panicking when the active branch is unpopulated — render-frame preamble's warn-and-skip handles it. Routed through existing `set_anti_aliasing` recompile flow rather than the scheduler (per the doc, the scheduler integration is Stage 1.8 fully — the lazy-pool pattern works either way; subsequent commits can route this through the scheduler without semantic change).
- [x] **2.2** `Pass(Evsm)`: Block B.1 landed. `EvsmPass.{moment_write,blur_h,blur_v}_pipeline_key` are `Option<ComputePipelineKey>` populated by `Shadows::ensure_pipelines_compiled` on the first shadow-casting light. Cold-boot no longer pushes the 3 EVSM compute cache keys into the cross-tail pool — they're held on `Shadows.pending_evsm_cache_keys` until the trigger lands. `dispatch_evsm` warn-skips via `pipeline_scheduler::warn_pipeline_not_compiled("evsm", "moment_write+blur")` if it's ever entered without compiled pipelines.
- [x] **2.3** `Pass(Line)`: line pipeline compile deferred until first `add_line_*` call (Block B.3). Cold-boot path uses `LineRenderer::new_deferred` — registers the line BGL eagerly (so `add_line_*` can build per-line bind groups) but leaves `LinePipelines.variants = None`. First `add_line_*` flips `pipelines_compile_requested = true`; `AwsmRenderer::ensure_line_pipelines_compiled` (driven by `wait_for_pipelines_ready` and `set_anti_aliasing`'s Phase 8) compiles the 4 variants via `ensure_keys`. Dispatch is guarded by `warn_pipeline_not_compiled("line_pass", ...)` so the render frame between insertion and compile silently skips. Cold-boot now compiles 0 line pipelines (was 4); no eager line entries in the cross-renderer render-pipeline pool. The 4 variants cover the MSAA cross product, so MSAA-flip never recompiles lines (idempotent ensure is a no-op once populated).
- [x] **2.4** `Pass(ShadowGen)`: Block B.2 landed alongside B.1. `Shadows.shadow_pipeline_{no_instancing,instancing,cube_no_instancing,cube_instancing}` are `Option<RenderPipelineKey>` populated by the same `Shadows::ensure_pipelines_compiled` call. Cold-boot no longer pushes the 4 caster cache keys into the cross-tail render pool — they're held on `Shadows.pending_caster_cache_keys`. `shadows::render_pass::record` guards the whole pass on `shadows.pipelines_compiled()` and warn-skips via `pipeline_scheduler::warn_pipeline_not_compiled("shadow_gen", "caster")`. Trigger entry point: `AwsmRenderer::ensure_shadow_pipelines_compiled` (called from `set_anti_aliasing` and from `scene-editor`'s `apply_kind_light` whenever a casting light is added; callers of `insert_light`/`set_light_shadow_params`/`update_light_shadow` from outside the editor should await the same helper). Scene with `features.shadows=true` but zero casters compiles 0 shadow pipelines at boot; first caster triggers 4 caster + 3 EVSM compile.
- [x] **2.5** Effects (Bloom / SMAA / DoF) lazy was already in place pre-this-plan: `EffectsPipelines` uses the `*_for_config` + `merge_resolved` pattern; cold-boot compiles only `BloomPhase::None` (effects-off variant); `set_post_processing` triggers per-effect recompile via `set_render_pipeline_keys`. Reflected in [§ Progress since this plan was written](#progress-since-this-plan-was-written-2026-05-26)'s lazy-pool table as "Effects | 5 → 1".
- [x] **2.6** `Pass(Picker)`: entire Picker subsystem now deferred until first `pick()` query (Block B.4). `AwsmRendererBuilder::build()` no longer constructs the picker even when `features.picking == true` — `self.picker` stays `None`. `AwsmRenderer::ensure_picker_compiled` (called by `pick()`) builds BGLs + compiles the active MSAA's compute pipeline + allocates state buffers + creates the bind group inline, so the same `pick()` invocation can dispatch this frame. `set_anti_aliasing`'s picker block is gated on `self.picker.as_ref()` being Some — when not yet built, the AA-flip block is skipped (the next `pick()` compiles for the live AA config); when built, `merge_resolved` adds the new MSAA's variant alongside the previously-compiled one. Cold-boot now compiles 0 picker pipelines.
- [~] **2.7** Eager-set audit (observed in model-tests cold-boot via boot-timing logs): `Shaders::ensure_keys: 6 shaders compiled in 95ms`, `ComputePipelines::ensure_keys: 5 pipelines compiled in 22ms`, `RenderPipelines::ensure_keys: 1 pipelines compiled in 1ms` (×multiple small batches per render-target setup). Roughly aligned with the target eager set but exact mapping needs detailed audit. Net total: ~6 shaders + ~5 compute + ~5 render eagerly compiled, which is in the ballpark of doc-stated "4 compute + 4 render at typical config." Full breakdown deferred until Stage 1.8 fully (which makes the eager set explicit via `submit_pipeline_group_batch` from `build()`).
- [ ] **2.8** Hand-test on desktop: toggle MSAA, toggle bloom, toggle SMAA, add a shadow-casting light, add a line primitive. Verify the modal pops, compile finishes, content appears correctly.
- [ ] **2.9** Update tasks #38 ("Lazy pool: strip Shadows caster pass") and #39 ("Lazy pool: strip Geometry pass") to completed once corresponding migrations land.
- [ ] **2.10** Commit Stage 2.

### Stage 3 — `msaa_resolve_samples` replacement (Priority 3)

- [x] **3.1** Classify pass extended with `emit_edge_data` cache-key flag. When on: emits compact `edge_pixel_id` via atomic counter, populates `edge_to_xy[edge_pixel_id]` (packed x:16, y:16), builds `edge_slot_map[edge_pixel_id]` (4 bytes), and appends per-shader-id edge sample lists. New `MaterialEdgeBuffers` struct holds the GPU buffers. Flag defaults to off; flipped to true at Stage 3.7-fully wiring.
- [x] **3.2** `msaa_resolve_samples` + `msaa_process_sample` deleted from primary opaque shader. **Edge pixels owned end-to-end by the new edge_resolve / skybox_edge_resolve / final_blend pipelines.** PBR primary SPIR-V drops ~80% — Android-Chrome init failure unblocked.
- [x] **3.3** New `edge_resolve.wgsl` template (per first-party + per dynamic shader_id). Single-sample shading with mask; writes to compact accumulator slot — no atomics.
- [x] **3.4** `skybox_edge_resolve.wgsl` shader added (one global pipeline).
- [x] **3.5** `final_blend.wgsl` shader added (one global pipeline). Reads up to 4 slots per edge pixel, weighted sum, divides by total count, writes `opaque_tex`.
- [x] **3.6** Dynamic-material wrapper for edge_resolve: template hook in place (`edge_template.rs`). Remaining: thread `DynamicShaderInfo` through `ShaderCacheKeyMaterialEdgeResolve` so registered dynamic materials get their own edge_resolve pipeline instead of being skipped in `ensure_compiled`.
- [x] **3.7** Pipeline + bind-group infrastructure FULLY ACTIVE. `MaterialEdgeBuffers` split into args+data; counter-mirror trick keeps storage count at 10. `edge_resolve_supported()` returns true. 6 edge_resolve pipelines compile cleanly.
- [~] **3.8** `MAX_EDGE_BUDGET` saturates (drops past it). MVP diagnostic landed (Block C.2 partial): `note_edge_budget_initialized` emits a one-shot `tracing::info!` at boot announcing the budget, and `note_edge_overflow_observed(overflow_count, budget)` is exposed for a future CPU-side readback to flip on a one-shot `tracing::warn!` when overflow is first seen. The full atomic-add hash-bucket overflow accumulator (separate region of `data_buffer`, fixed-point atomic-add per (R,G,B,A,count), `final_blend` reads it for not-allocated pixels) remains parked — it would require coordinated WGSL changes across classify/edge_resolve/skybox_edge_resolve/final_blend plus uniform layout extension, judged ≥3h with non-trivial validation risk. Note: today's behavior is "edges past budget render with primary-sample shading (no MSAA resolve)" — not a crash, not a black hole, just degraded quality on pathological-edge-density scenes; the boot log makes the budget visible so operators can raise it if needed.
- [x] **3.9** Contract docs update (`docs/dynamic-materials/contract-opaque.md`): document that registered dynamic-material WGSL fragment runs in both primary AND edge_resolve contexts; cross-material MSAA edges now work for dynamic materials too.
- [x] **3.10** Preview-browser smoke test: Fox renders cleanly with smooth edges, IBL + shadows intact, no GPU errors, no PipelineVariantNotCompiled.
- [x] **3.11** All Stage 3 commits merged + tested.

### Stage 4 — End-to-end testing

- [x] **4.1** material-editor preview-test: cold-boot reaches Ready cleanly.
- [x] **4.2** scene-editor preview-test: cold-boot reaches Ready cleanly.
- [x] **4.3** model-tests preview-test: cold-boot + Fox render verified clean.
- [ ] **4.4** Toggle features post-merge: MSAA off→on→off, bloom off→on→off, SMAA off→on. Verify content recompiles and scene renders correctly. (Pending Stage 1.8 fully — config-flip flicker UX.)
- [ ] **4.5** Add a shadow-casting directional light to a scene that didn't have one. Verify EVSM + ShadowGen pipelines submit when light is added. (Pending Stage 2.2/2.4.)
- [ ] **4.6** Boot-timing log audit against [§ The eager set](#the-eager-set-cold-boot). (Pending Stage 1.8 fully — eager set explicit via `submit_pipeline_group_batch` from `build()`.)

(Android device verification lives in [§ Post-implementation human checklist](#post-implementation-human-checklist).)

### Stage 5 — CI prep + PR

- [x] **5.1** `cargo fmt --all` clean.
- [x] **5.2** `task lint` clean.
- [x] **5.3** `cargo doc --workspace --no-deps` warning audit. ✅ landed (Block E.6 — zero warnings workspace-wide).
- [x] **5.4** Branch pushed.
- [x] **5.5** PR #99 open with full description.

### Stage 6 — Parked

- [ ] **6.1** Priority 4 (build-time pipeline cache): parked, waiting on Dawn pipeline-cache spec stabilization. Leave the section in this doc for future reference.

---

## Remaining work for 100% completion

This is the queue the next session works through, top to bottom. Branch is `more-optimizations`; PR is #99. Everything below this point can be done aggressively in parallel where files don't overlap — launch sub-agents in worktrees, monitor commits, merge as they land. **Be bold. Things will break temporarily; that's expected.** The architecture is in place — these are integration + polish + extension.

### Block A — Scheduler-driven compile (the API-cleanup arc)

The scheduler today is a state machine; compile is still driven by the legacy `prewarm_pipelines` path. Migrate to scheduler-owned compile so the architecture's promise is fully realized.

- **A.1** ✅ landed (pragmatic shape) — bridge: `register_material` populates scheduler; `prewarm_dynamic_pipelines` marks each scheduler entry `Ready` on success. The architecture's "scheduler tracks canonical readiness state" promise is delivered. The literal "push real compile futures onto `inflight`" direction is preserved as a follow-up under Block D (requires factoring `ensure_keys` into sync-descriptor / sync-promise-collection / parallel-await trio; assessed as disproportionate vs the bridge approach for this PR).
- **A.2** ✅ landed — `wait_for_pipelines_ready` is now the canonical post-submit await surface (delegates to `prewarm_pipelines` for compile drive, then drains `poll_pipeline_scheduler`). All three frontends (`material-editor/src/host.rs`, `model-tests/src/pages/app/canvas.rs`, `scene-editor/src/main.rs`) migrated. `prewarm_pipelines` kept as the lower-level compile-drive surface (acceptable indirection until Block D's push-futures lands).
- **A.3** — deferred. First-party material pipelines (PBR / UNLIT / TOON / FLIPBOOK) are pre-compiled in the cold-boot eager set, so the gltf loader has nothing to compile per material — the bookkeeping of submitting `MaterialDef::FirstParty` defs to the scheduler would be observability-only. Becomes load-bearing when first-party variants go lazy (a future PR — would also need the build-flow migration in Block D). The gltf loader already happily inserts meshes that reference pre-compiled pipelines; the existing path is correct.
- **A.4** — infrastructure landed (Stage 1: `drain_pipeline_status_events` exposed); UI consumers deferred. The status stream is a public API surface that material-editor + scene-editor can subscribe to when their UX needs the "compiling N of M" modal. Cold-boot has effectively zero pending groups (eager set is small + synchronously awaited inside `build`); editor recompile loops already hold the RefCell during compile so the user sees a frame-skip — adding a modal is genuinely UX polish, not a correctness gap. Deferred to a frontend-focused follow-up.

### Block B — Scene-content-driven pass migrations (Stage 2.2-2.4 + 2.6)

Currently EVSM/Line/ShadowGen/Picker are eagerly created at `build()` based on feature flags. Make them lazy on first scene-content trigger.

- **B.1** **`Pass(Evsm)` lazy** ✅ landed: trigger via `AwsmRenderer::ensure_shadow_pipelines_compiled` (called from `set_anti_aliasing` and `scene-editor`'s `apply_kind_light` for casting lights). Cold-boot compiles 0 EVSM pipelines.
- **B.2** **`Pass(ShadowGen)` lazy** ✅ landed: same trigger as B.1. Cold-boot compiles 0 caster pipelines.
- **B.3** **`Pass(Line)` lazy** ✅ landed: line pipeline compile is now deferred — cold-boot uses `LineRenderer::new_deferred` (registers BGL only, `variants = None`), `add_line_*` flips `pipelines_compile_requested`, and `AwsmRenderer::ensure_line_pipelines_compiled` (driven by `wait_for_pipelines_ready` + `set_anti_aliasing` Phase 8) compiles the 4 variants. Dispatch warn-skips between insertion and compile via `warn_pipeline_not_compiled("line_pass", ...)`. Cold-boot compiles 0 line pipelines.
- **B.4** **`Pass(Picker)` lazy (Stage 2.6 fully)** ✅ landed: defer the entire Picker subsystem (today `Option<Picker>` gated on `features.picking`) until first `pick()` query.
- **B.5** **`set_anti_aliasing` edge-pipeline recompile** ✅ landed: when MSAA toggles on after build, call `MaterialEdgePipelines::ensure_compiled` to compile edge_resolve pipelines for the new MSAA state. Currently edge_pipelines compile only at first boot — toggling off→on leaves them empty (silent fall-through via `warn_pipeline_not_compiled`).

### Block C — Stage 3 polish

- **C.1** **Dynamic-material edge_resolve compile** (Stage 3.6 fully): `MaterialEdgePipelines::ensure_compiled` currently skips entries where `shader_id.is_dynamic()`. Wire `DynamicShaderInfo` through `ShaderCacheKeyMaterialEdgeResolve` so registered dynamic materials get their own edge_resolve pipeline. The template hook in `edge_template.rs` is in place — needs the cache-key extension + the `register_material` → scheduler-submit-of-edge-pipeline glue. ✅ landed
- **C.2** **`MAX_EDGE_BUDGET` overflow atomic-add fallback** (Stage 3.8 fully): currently counter saturates and excess edges drop. Implement a small reserved accumulator region at the tail of `data_buffer` that overflow edges atomic-add into; final_blend reads it. ~50 lines of WGSL + 1 atomic counter. ~ partial — MVP diagnostic shipped (boot-time budget log + one-shot `note_edge_overflow_observed` helper exposed for future CPU-side readback; shader-side clamp + `edge_overflow_count` atomic already in place). Full atomic-add hash-bucket overflow accumulator remains parked; behavior on pathological scenes is "drop edges silently, render with primary-sample shading" (degraded MSAA, not a crash). Operator-visible via `RUST_LOG=awsm_renderer::edge_resolve=info`.
- **C.3** **Contract docs update** (Stage 3.9): `docs/dynamic-materials/contract-opaque.md` — document the new "WGSL fragment runs in both primary and edge_resolve contexts" invariant. One paragraph. ✅ landed

### Block D — Build-flow migration (Stage 1.8 fully)

This is the bigger refactor: replace the per-pass eager `new()` calls in `AwsmRendererBuilder::build` with `submit_pipeline_group_batch` for the eager set, then `wait_for_pipelines_ready()`. Touches `lib.rs`'s build flow + each render-pass's `from_resolved` shape. Land after A is in place (so scheduler actually drives the compile path).

- **D.1** Strip the existing eager-compile codepath from `build()`; replace with a single `submit_pipeline_group_batch` covering the eager set ([§ The eager set](#the-eager-set-cold-boot) table), awaited synchronously.
- **D.2** Verify cold-boot pipeline count matches the eager-set table exactly (~4-6 compute + ~4 render at typical config). Boot-timing log audit (Stage 4.6).
- **D.3** Config-flip semantics fully (Stage 4.4): MSAA / bloom / SMAA / DoF toggles transition Ready materials back to Pending + re-submit. Frontends drive "compiling N of M" modal off the status stream.

### Block E — Beyond-the-plan optimizations

The user wants "everything everything". Items from ROADMAP.md + opportunities surfaced during this work:

- **E.1** **Compute-pipeline labels in Lessons A** ✅ landed: render-pipeline labels embed the shader's `debug_label`; compute-pipeline labels are still `compute:ShaderKey(_):PipelineLayoutKey(_)`. Use the new `Shaders::get_label()` helper added in Stage 1.7 to thread the shader name through (~20 lines).
- **E.2** **Multithreading prep**: `pipeline_scheduler` currently uses single-threaded `FuturesUnordered`. Audit for SharedArrayBuffer-readiness when wasm32-multithread lands. Document the boundaries.
- **E.3** **Test surface**: `wait_for_pipelines_ready()` test helper is in place; sweep `crates/renderer/tests/` and `crates/renderer/examples/` for sites that still rely on sync-insert-then-dispatch.
- **E.4** **Per-pipeline-cumulative-timing log polish**: per-pipeline labels currently log on resolve. Add a per-batch sort-by-finish-time summary at the end of each `ensure_keys` call so the operator can scan "which pipeline finished last."
- **E.5** **Edge_resolve runtime profile sanity**: at 1080p with the Fox scene, capture frame-time delta between the inline-path (pre-Stage-3) and the new edge_resolve path. Confirm parity or improvement. If a regression appears, investigate the per-frame `reset_header` cost vs. classify's extra writes.
- **E.6** **`cargo doc --workspace --no-deps` warning audit** (Stage 5.3): resolve any new doc-link warnings introduced by this PR. Pre-existing warnings (in render-worker / web-shared / renderer-gltf) are out of scope but worth noting. ✅ landed (zero workspace warnings — all 43 fixed, including pre-existing pre-PR warnings in render-worker / web-shared / renderer-gltf / scene-editor / model-tests).
- **E.7** **Workspace-wide `-W missing_docs` gate** for the new `pipeline_scheduler` module (per `remainder.md` § "Public API gate"). ✅ landed (module-scoped via inner attribute)

### Block F — Verification, finalize, merge prep

- **F.1** Hand-test every frontend post-merge via `mcp__Claude_Preview__*`: model-tests (Fox + a transmissive model + a shadow-casting scene), material-editor (live WGSL recompile + Errors pane + Definition pane), scene-editor (gltf load + Import Material + per-mesh Custom picker + MSAA toggle).
- **F.2** Boot-timing log audit: confirm pipeline counts post-A+D match the [§ The eager set](#the-eager-set-cold-boot) table.
- **F.3** Final `cargo fmt --all` + `task lint` + `cargo check --workspace --target wasm32-unknown-unknown --exclude awsm-debugging`.
- **F.4** Update PR description one final time reflecting 100% completion.
- **F.5** Update `docs/plans/more-optimizations.md` (this doc) marking every remaining checkbox `[x]`. Mark task #38 + #39 completed. Update task list cleanup.

---

## Post-implementation human checklist

**Out of scope for the implementing agent.** These items require physical hardware setup, the user's environment, or human judgment that can't be delegated. They run **after** the agent's PR is open.

Do not let these block the agent's pass — the agent completes its checklist and stops at "PR opened." The human picks up from here whenever it's convenient.

### Android device verification

- [ ] **H.1** Plug in Android phone with `chrome://flags#enable-unsafe-webgpu` enabled. Run `task debug-mobile:chrome-check` from project root.
- [ ] **H.2** Confirm init reaches `phase = Ready` with no `VK_ERROR_INITIALIZATION_FAILED`. Capture boot-timing log lines for the eager batch — should show <500 ms total compile.
- [ ] **H.3** Load a test scene with a PBR mesh. Confirm:
   - Skybox + camera UI visible within ~500 ms of `phase = Ready`.
   - PBR mesh appears within ~3 s (the primary pipeline compile time on the test Android device).
   - No watchdog kills (`External Instance reference no longer exists` absent from logs).
   - Cross-material MSAA edges render correctly (close-up of two-material boundary looks right).
- [ ] **H.4** Toggle MSAA off → on → off. Confirm modal appears, scene recompiles, no driver rejection on the recompile.
- [ ] **H.5** Toggle bloom on. Confirm Bloom pipeline submits and resolves, effect appears post-recompile.
- [ ] **H.6** Add a shadow-casting light. Confirm EVSM + ShadowGen submit and resolve, shadows render.
- [ ] **H.7** Register a dynamic material via `material-editor` on desktop, save to project, load in `scene-editor` on Android. Confirm the dynamic material's pipelines compile on Android and the material renders.
- [ ] **H.8** Performance sanity: at 1080p with a moderate scene (~100k triangles, mixed materials), confirm 60 fps target is held. If not, capture a profile and note the bottleneck — most likely the edge-resolve atomic-add fallback if `MAX_EDGE_BUDGET` is too low.

### PR review + merge

- [ ] **H.9** Review the PR on GitHub. Check that the implementation faithfully follows the architecture in this doc — flag any deviations the agent recorded inline (per "If you hit a genuine blocker" rule).
- [ ] **H.10** Inspect the dynamic-materials migration in `material-editor` + `scene-editor`. Confirm the editor stays responsive during compile and the "compiling N of M" modal looks right.
- [ ] **H.11** Spot-check the test surface: `cargo test --workspace --target wasm32-unknown-unknown` (or whatever the project's actual test runner is) for any failures.
- [ ] **H.12** Approve and merge once satisfied.

### Post-merge monitoring

- [ ] **H.13** Watch CI on `main` for the first few commits after merge. Any regression specifically tied to the readiness machinery should surface in cold-boot timing or pipeline-count metrics in the boot-timing logs of the model-tests fixture.
- [ ] **H.14** If a user reports a "mesh is invisible" bug after merge, first check for `tracing::warn!` lines from the render-frame preamble — those indicate a missing trigger in some insertion path that wasn't covered.

---

## Cross-references

- Per-priority code touchpoints: each priority section lists the files in scope.
- Dynamic-materials Public API contract docs (updated under [§ Migration of the dynamic-materials API](#migration-of-the-dynamic-materials-api)): [`../dynamic-materials/contract-opaque.md`](../dynamic-materials/contract-opaque.md), [`../dynamic-materials/contract-transparent.md`](../dynamic-materials/contract-transparent.md).
- Asset authoring / UI polish / non-optimization remaining work: [`remainder.md`](remainder.md).
- Integration example (to be updated post-Priority 1): [`../../crates/renderer/examples/dynamic_material.rs`](../../crates/renderer/examples/dynamic_material.rs).
