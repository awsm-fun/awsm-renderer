# docs/plans/todo.md — consolidated overnight work (SSOT)

> **Provenance.** Consolidates `custom-vertex.md` (the headline feature),
> `multithread-build-plan.md`, and `multithread-testing.md`. `mcp-improvements.md`
> is **complete + merged to main** (summarized under "Already shipped"); its full
> record lives in git history. `nanite.md` is intentionally SEPARATE — not in scope.
>
> This is the single source of truth for the autonomous overnight run. Work the
> [Master tracker](#master-tracker) top to bottom.

## ✅ Definition of done & execution rules

1. **One item at a time.** Pick the next `TODO` row (set it `WIP`). Read that
   item's full section. Implement the FULL scope — not a slice.
2. **Verify before DONE:**
   - Rust tests (unit/roundtrip where testable) + `task lint` clean (rustfmt +
     clippy `-D warnings`, all features, tests).
   - **Live verification** via the chrome-devtools MCP — a screenshot / measured
     gate proving the actual behavior (renders, shadows match, demo gate passes).
3. **Commit per completed+verified item** on this branch (`more-mcp`), co-author
   trailer `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
   Flip the tracker row to `DONE` + record the short hash. Two-commit-per-item is
   fine (impl, then doc/tracker) — it avoids amend-hash drift.
4. **Full scope or BLOCKED — never a silent slice.** If an item is genuinely too
   large or can't be finished/verified in this run, set the row `BLOCKED` with a
   one-line reason and move on. Do NOT mark a partial DONE. (This rule exists
   because the previous round shipped slices and falsely claimed 100%.)
5. **Do NOT claim "100%"** at the end unless every row is genuinely `DONE`.
   Write an honest summary: what landed (hashes), what's BLOCKED and why.

## Scope: Chrome desktop only

This project targets **Chrome desktop only** — by decision, there is no
cross-browser or mobile work. Every item below is therefore Chrome-verifiable via
the chrome-devtools MCP, so a complete run reaches **100% DONE**. (The old
cross-browser / mobile testing items were removed for this reason.)

## Dev environment

Two dev servers (each a background task you own; logs under `/tmp/`):

- **`task mcp-dev`** → editor (trunk, `:9085`) + the awsm-scene MCP (`:9086`).
  For **Part A (custom vertex)** + any editor/renderer/MCP work. Drive the editor
  + `awsm-scene` MCP through chrome-devtools at
  `http://localhost:9085/?mcp=http://127.0.0.1:9086&pair=<CODE>`.
- **`task mt:dev`** → the multithreaded renderer demos (`:9090`, COOP/COEP).
  For **Part B/C (multithread)**. Drive `http://localhost:9090/?demo=<name>` (e.g.
  `remote`, `crowd`, `churn`, `motion`) through chrome-devtools.

**Build/restart cycle** (recurs whenever you touch the server or a crate):
- Free ports before relaunch (`lsof -ti tcp:PORT | xargs kill`): 9085/9086/9082/9083
  for mcp-dev; 9090 for mt:dev.
- Relaunch with `run_in_background: true` (NOT inline `&`). Wait for HTTP 200 +
  the "server listening" log line + a trunk "success".
- After a `task mcp-dev` server rebuild (changes under `packages/mcp` /
  `editor-protocol` / `scene` / renderer crates), reload the editor in
  chrome-devtools; **the MCP pair code rotates on restart** — the next tool call
  errors with the new code; navigate `?pair=<NEW CODE>` to re-pair.
- Editor-only changes hot-reload via trunk; a renderer/meshgen-crate-only change
  needs an editor-file touch (append+remove a comment in `node_sync.rs`) to
  trigger a trunk dep rebuild, then reload.
- Harness CACHES the MCP tool/query schema across restarts → exercise NEW
  commands via `dispatch_command {command:{cmd:...}}` and NEW queries/fields via
  `run_query {query:{query:...}}` (the harness forwards extra query fields).

**Native tests:** editor / editor-protocol via `cargo test -p awsm-editor` /
`-p awsm-editor-protocol`; renderer via `cargo test -p awsm-renderer` (validation
tests need `--features dynamic-material-validation`); renderer-core via
`cargo test -p awsm-renderer-core`; meshgen via
`cargo test -p awsm-meshgen --features authoring --lib`.

**Gotchas worth keeping in mind** (from prior rounds): reproduce before assuming a
capability is missing (several "missing" things were already-implemented-but-
undiscoverable); a long base64 in a tool param can get mangled — keep payloads
small; for custom materials read `get_material_contract` first +
`get_material_diagnostics` after. The accumulated detail lives in memory
`mcp-improvements-loop-mechanics`.

## Already shipped (merged to main — context, not work)

The `mcp-improvements` effort (PR #137 + follow-ups) is **done**: raw texture
upload, UV-transform tool+render, keyframe channels, `patch_kind` +
`get_kind_schema` (full NodeKind JSON Schema), magenta unassigned sentinel,
duplicate-id + subtree queries, facing hints, fused `paint_where`/`transform_where`
+ a reusable selection handle + read pagination, custom transparent/alpha,
particle sprites + true soft-gradient blend, the `ibl` include, vertex-color
footgun docs, `displace`-`noise()` + **displace-from-texture**, toon banding guard,
multi-line-WGSL fix, and the agent **equirect panorama environment** (skybox + a
proper SH cosine-convolved diffuse IBL + disk persistence). The ONE deliberately
deferred piece — the programmable GPU vertex stage — is **Part A** below.

## Master tracker

| id | item | status | commit |
|----|------|--------|--------|
| CV1 | Custom vertex: ABI + `apply_vertex` hook + geometry & shadow per-material pipelines + registration/cache-key + naga validation (Phase 1) | BLOCKED | see §Blocked items |
| CV2 | Custom vertex: transparent + geometry-masked + shadow-masked variants (Phase 2) | BLOCKED | depends on CV1 |
| CV3 | Custom vertex: editor 3rd WGSL window + toggle + `set_material_vertex_wgsl` MCP + contract doc + starter body (Phase 3) | BLOCKED | depends on CV1 |
| CV4 | Custom vertex: polish — normal-from-height helper, vertex texture-fetch example, skinned-mesh tests (Phase 4, optional) | BLOCKED | depends on CV1–CV3 |
| B2 | Multithread: screenshot capture path (`renderer.capture_frame`) | DONE | 4c593c02 |
| B1 | Multithread: device-loss + worker-crash recovery | BLOCKED | see §Blocked items |
| T4 | Multithread: resilience VERIFICATION (after B1) | BLOCKED | depends on B1 |
| T3 | Multithread: perf at scale + soak (Chrome) | DONE | (verification — see §Verification results) |
| T5 | Multithread: allocation / GC validation (Chrome) | DONE | (verification — see §Verification results) |
| B3 | Multithread: arena growth policy (only if T3 shows unbounded growth) | DONE (N/A) | not needed — T3 churn soak proved memory bounded |
| B4 | Multithread: bundled scene fixture for `?demo=scene` (optional) | DONE | eec614d2 |

**Suggested order:** CV1 → CV2 → CV3 → B2 → B1 → T4 → T3 → T5 → B3 (conditional)
→ B4 (optional) → CV4 (polish). Custom vertex is the headline feature and fully
Chrome-verifiable (render a displaced mesh; confirm its shadow + silhouette
match). The multithread items are smaller + self-contained. Every row is
Chrome-doable, so the target is a genuine 100%.

> **Note on scale:** CV1 (per-material geometry/shadow pipeline split) and B1
> (recovery) are the two genuinely large items — attempt them phase by phase; if a
> phase can't complete + verify in this run, BLOCK it with the specific reason
> rather than half-shipping.

## Blocked items (overnight run 2026-06-22)

Both are the two genuinely-large items the tracker flagged; each is multi-day with
unresolved design questions, so per the execution rules they are BLOCKED (not
half-shipped) with the specific reason rather than a silent slice.

- **CV1 (→ blocks CV2/CV3/CV4).** The custom-vertex feature's *full* Phase-1 scope
  can't be completed + **live-verified** in one run:
  1. **ABI not threaded.** `apply_vertex()` (`shared_wgsl/vertex/apply_vertex.wgsl`)
     and its five callers only carry `position/normal/tangent/vertex_index` (+
     instance rows). The §2 ABI also needs `uv`, `instance_id`, the `MaterialData`
     uniforms, and `FrameGlobals` — none are currently passed into `apply_vertex`,
     and the geometry/shadow passes don't even carry UVs in their vertex buffers.
     Threading the full ABI touches all five vertex entry points + their vertex
     buffer layouts.
  2. **Per-material pipelines.** The geometry masked path already proves the shape
     (a lazy per-`shader_id` pool + material/texture bind groups —
     `geometry/masked_pipeline.rs`), so the geometry side is mirrorable; but the
     shadow + shadow-masked passes need the same per-material split built from
     scratch, and all five must stay byte-identical or shadows detach.
  3. **Verification is coupled to CV3.** The DONE bar (a displaced mesh whose
     shadow + silhouette match) needs an authoring path to inject vertex WGSL —
     i.e. CV3's `set_material_vertex_wgsl` MCP setter + editor window + registration
     plumbing. Native tests can't render (no GPU). So a genuine CV1 verification
     requires CV1 **and** CV3 substantially built — multi-day combined.

- **B1 (→ blocks T4).** Device-loss + worker-crash recovery is greenfield and has
  unresolved design questions in this very doc:
  - No `GPUDevice.lost` subscription and no `renderer.rebuild_gpu()` exist; recovery
    means recreating the *entire* GPU-handle graph (buffers/textures/pipelines/bind
    groups) from the CPU mirrors. Open question (per §B1): how much rebuilds behind
    one call vs. needs a full re-`commit_load`.
  - No render-worker respawn path exists (`workers/pool.rs` `onerror` only fails the
    in-flight meshgen job; it is not the render worker). A respawn must re-transfer a
    fresh `OffscreenCanvas`, re-post the shared module+memory, and re-hand every live
    arena `SlotBinding` — and the render-worker topology (slot→key map) must be
    re-derived (open question: persist it in shared memory vs. reload the scene).
  - T4 is the *verification* of B1's code; with no code to exercise it is blocked too.

## Verification results (overnight run 2026-06-22)

Live-measured via the chrome-devtools MCP against `task mt:dev` (:9090).

**T3 — perf at scale (motion demo, render worker, 120 Hz display):**

| bodies N | movers | fps | frame time |
|---|---|---|---|
| 100 | 50 | 120 | 8.34 ms |
| 1000 | 500 | 120 | 8.34 ms |
| 2000 | 1000 | 120 | 8.34 ms |
| 5000 | 2500 | 60 | **16.68 ms** ← 16.6 ms (60 fps) budget crossover |
| 10000 | 5000 | 24 | 41.68 ms |

8.34 ms is the 120 Hz RAF cap (true GPU frame time is lower); the render worker
holds display refresh up to ~2000 movers and first misses the 16.6 ms / 60 fps
budget at **~5000 bodies (2500 movers)**, degrading to ~24 fps at 10000.

**T3 — memory soak (churn demo, ~59 min):** spawned 2447 / despawned 2435 /
**reusedSlots 2431 (99.84 % of freed slots reused)**, `invariantOk` held the whole
run (live == spawned − despawned). The arena reuses nearly every freed slot, so it
never grows beyond the live peak → **shared-memory growth is flat/bounded**.

**B3 — N/A:** T3's soak proved bounded growth, so no arena compaction/slab-reuse
policy is needed (the conditional trigger did not fire).

**T5 — allocation / GC:** the O(N) render hot path uses pooled scratch — transform
descent/pack via `Transforms::arena_pack_scratch` (`std::mem::take` + `clear()` +
restore in `descend_pack_arena`) and the cull path via `RenderFrameScratch`
(`opaque_snapshots` + `occlusion_instance_bytes`, taken/restored across `render()`).
No O(meshes) per-frame `Vec`/`Box` remains; the only residual per-frame `vec![]`
are constant-size render-pass descriptors (O(passes)). Motion under load ran a
clean 600 frames / 5.00 s with **zero dropped frames**, confirming no GC-pause jank.

---

# Part A — Custom vertex shaders (the headline feature)


> **Goal.** A custom (dynamic-WGSL) material can control its **vertices**, the same
> way it already controls **masking and color** in the fragment stage. The agent
> writes a small WGSL body; the renderer wraps it as a template hook, gates it
> into the rasterizing passes, and gives that material its **own pipeline**.
> *Straightforward in shape, not small in surface* (renderer + editor + MCP).
>
> Status: **design doc / not started.** This is the deferred "part 2" of §16
> (mcp-improvements) — explicitly NOT blocking the current MCP PR. The
> displace-from-texture data hook already shipped (`7bbca00a`); this is the
> programmable-WGSL version.

---

## 1. The key architectural fact (don't skip this)

The opaque material pass is **deferred**: a **compute shader** (`cs_opaque`)
shades from a visibility buffer. **It has no vertex stage.** Vertices are
rasterized earlier, by the **geometry (visibility) pass**, which writes
triangle-id + barycentric + world-normal into the visibility texture that the
compute kernel reads.

So the custom *fragment* hook (`custom_shade_dynamic`) lives in the deferred
**compute** kernel — but a custom *vertex* hook must live in the **raster**
passes that actually run a `@vertex` stage. There are five, and **every one
shares the same transform function** `apply_vertex()`:

| Pass | File (vertex) | Role | Must match? |
|---|---|---|---|
| **Geometry / visibility** | `render_passes/geometry/shader/geometry_wgsl/vertex.wgsl` | rasterize → visibility buffer (drives the deferred opaque shade) | **yes — this is the one** |
| **Geometry masked** | `render_passes/geometry/shader/masked_template.rs` (reuses geometry vertex) | alpha-tested cutout into the visibility buffer | yes |
| **Transparent** | `render_passes/material_transparent/shader/material_transparent_wgsl/vertex.wgsl` | forward-shaded blend | yes |
| **Shadow** | `shadows/shader/shadow_wgsl/vertex.wgsl` | depth-only into shadow maps | yes |
| **Shadow masked** | `shadows/shader/shadow_masked_wgsl/vertex.wgsl` | alpha-tested depth | yes |

All five call **`render_passes/shared/shared_wgsl/vertex/apply_vertex.wgsl`**,
which runs the canonical chain: **morph → skin → model/instance transform →
world → clip**, plus the inverse-transpose normal/tangent transform and the
billboard override.

**Correctness invariant (the whole reason this is subtle):** if a vertex is
displaced in one pass but not another, the silhouette, depth, shadows, and
masked cutout stop matching the shaded surface. **The displacement must run
identically in all five passes.** Injecting it into the single shared
`apply_vertex()` is what makes that automatic — every pass that compiles the
custom-vertex variant of `apply_vertex` gets the same displacement for free.

Your instinct was right on both counts: *"make sense in the visibility shader as
a template"* → the geometry pass + `apply_vertex`; *"custom materials get their
own pipeline since that's going to include/gate"* → see §3.

---

## 2. The WGSL hook & contract (mirror the fragment machine)

### 2a. Where it injects

Add one gated hook to `apply_vertex.wgsl`, **after morph, before skin** — so the
agent always works in a **consistent post-morph LOCAL frame** (skinned and
non-skinned alike), and skinning / instancing / the model→world transform then
deform the displaced mesh exactly as they would the base. (Injecting *after* skin
would hand the agent world-space positions for skinned meshes but local for
rigid ones — inconsistent; rejected. A post-skin/world-space variant is a later
opt-in flag, not v1.)

```wgsl
// shared_wgsl/vertex/apply_vertex.wgsl  (inside fn apply_vertex)
//   ... morph targets applied (local) ...
{% if has_custom_vertex %}
    let _d = custom_displace_vertex(VertexDisplaceInput(
        vertex.position, normal, tangent, uv0, vertex.vertex_index, instance_id, material
        {% if inc.camera %}, frame_globals {% endif %}
    ));
    vertex.position = _d.position;
    normal  = _d.normal;
    tangent = _d.tangent;
{% endif %}
//   ... skinning (deforms the displaced local frame) ...
//   ... model/instance transform → world → clip (inverse-transpose on _d.normal) ...
```

The wrapper that holds the agent's body (mirrors `custom_shade_dynamic`), emitted
into each rasterizing template under `{% if has_custom_vertex %}`:

```wgsl
struct VertexDisplaceInput {
    position: vec3<f32>,   // post-morph LOCAL position
    normal:   vec3<f32>,   // post-morph LOCAL normal
    tangent:  vec4<f32>,   // LOCAL tangent (w = handedness)
    uv:       vec2<f32>,   // uv0 (gate more sets behind an include if needed)
    vertex_index: u32,
    instance_id:  u32,     // for per-instance displacement (sentinel if non-instanced)
    material: MaterialData,// the SAME auto-generated struct as the fragment hook
    {% if inc.camera %} globals: FrameGlobals, {% endif %} // time, camera
};
struct VertexDisplaceOutput {
    position: vec3<f32>,   // displaced LOCAL position
    normal:   vec3<f32>,   // the shader's LOCAL normal (REQUIRED — see §6)
    tangent:  vec4<f32>,   // the shader's LOCAL tangent (passthrough if unchanged)
};
fn custom_displace_vertex(input: VertexDisplaceInput) -> VertexDisplaceOutput {
{{ dynamic_wgsl_vertex|safe }}
}
```

### 2b. The contract (what the agent gets / returns)

- **In:** local `position`, `normal`, `tangent`, `uv`, `vertex_index`,
  `instance_id`, the material's declared `material.*` uniforms/textures/buffers
  (so it can sample a heightmap or read a `time`/`amplitude` uniform), and —
  behind the `camera` include — `globals` (time, camera) for animated displacement.
- **Out:** displaced local `position`, `normal`, and `tangent` — the shader owns
  the whole surface frame (§6). Passthrough is the explicit "I didn't change it"
  (return the input value).
- **Available helpers:** the same auto-generated `MaterialData` struct +
  `material_data_load()` the fragment hook uses (`materials/src/dynamic_layout.rs`)
  — identical byte layout, so the vertex stage and fragment stage read the same
  uniform buffer. **Vertex texture fetch** (sampling a heightmap in the vertex
  stage) is allowed by WebGPU; it requires the material texture pool to be bound
  in the geometry pass for custom-vertex draws (see §3).

### 2c. Includes / gating

Reuse `ShaderIncludes` (`materials/src/shader_includes.rs`), but the vertex stage
wants a **narrower** set than the fragment (no lighting/IBL/shadows — those are
fragment concerns). Two options:
- **(recommended)** a `for_vertex(includes)` mask (sibling of `for_custom`) that
  forces off everything except `math` / `camera` / `textures` / `vertex_color`.
- a separate `vertex_shader_includes` list on the material def.

---

## 3. Pipelines: the per-material split (the real lift)

Today the **geometry pass uses one pipeline for all opaque meshes** — it's
material-agnostic (writes triangle-id/normal, doesn't care what shades later).
That's why it's cheap. Custom vertex displacement breaks that: a mesh whose
material displaces vertices needs its **own** geometry-pass vertex pipeline
(compiled with that material's WGSL). This is the "own pipeline" you predicted.

Concretely:
- The geometry / masked / transparent / shadow / shadow-masked **cache keys** gain
  `dynamic_vertex_shader: Option<DynamicVertexShaderInfo>` (mirror of
  `DynamicShaderInfo`: `{ struct_decl, loader_decl, wgsl_vertex, includes }`).
  `None` → the existing shared fast pipeline (zero cost for everyone else).
- The geometry-pass **draw loop** must bucket meshes by vertex-shader id: meshes
  with no custom vertex go through the one shared pipeline (as today); each
  custom-vertex material gets its own pipeline + draw. The dispatch/bucketing
  machinery already exists for the fragment side (`bucket_entries`,
  `dispatch_hash`); extend it to the vertex axis.
- Custom-vertex geometry draws must additionally **bind the material's uniform +
  texture-pool bind groups** (the shared geometry pipeline doesn't today), so the
  vertex stage can read uniforms / sample heightmaps. These bind groups are
  already global (the deferred pass uses them) — it's wiring, not new resources.
- **Shadows:** the same custom-vertex variant must compile for the shadow +
  shadow-masked pipelines, or shadows detach from the displaced silhouette.

This is the bulk of the renderer work. Risk is concentrated in the geometry-pass
draw loop and the shadow loop (per-material pipeline selection where there was
one). Budget accordingly.

---

## 4. Registration & cache key (mechanical mirror)

Mirror exactly what `alpha_wgsl` (the 2nd, mask-only window) does end-to-end:

- `MaterialRegistration` (`dynamic_materials/registry.rs`) gains
  `wgsl_vertex: Option<String>` (alongside `wgsl_fragment`, `alpha_wgsl`).
- `build_registration` (`engine/bridge/dynamic.rs`) reads the new editor window,
  and **folds `wgsl_vertex` into `wgsl_hash`** so an edit recompiles. Idempotent
  registration is keyed on `(name, layout_hash, wgsl_hash)` — unchanged shape.
- The cache-key `DynamicShaderInfo` already carries `struct_decl` / `loader_decl`
  (the auto-generated `MaterialData` + loader from `dynamic_layout.rs`) — reuse
  them verbatim for the vertex hook (same uniform layout). Add `wgsl_vertex`.
- `dispatch_hash` already invalidates pipelines when the registry changes — the
  vertex WGSL rides in `wgsl_hash`, so no new invalidation channel is needed.

---

## 5. Validation (mirror `validate_dynamic_material_wgsl`)

`renderer.rs::validate_dynamic_material_wgsl` assembles the opaque (or transparent)
template with the agent's fragment + runs naga. Add the sibling: assemble the
**geometry-pass** template with the custom-vertex hook + run naga, returning
`line: None`-style errors to the editor (the existing fragment validator already
omits naga line numbers because they index the assembled module — keep that).
This catches the agent's vertex-WGSL errors at edit time, before a GPU compile.

> NB §20 lesson: the editor's lightweight `controller::custom_material::compile_wgsl`
> pre-check (the line-by-line "missing `;`" heuristic) runs on *every* WGSL window
> before naga — make sure the vertex window routes through the continuation-aware
> version, not a fresh copy of the old heuristic.

---

## 6. Normals are the shader's responsibility (RESOLVED)

**Decision: the hook returns the surface frame (`normal` required, `tangent`
passthrough-or-recomputed). The renderer does NOT recompute normals.**

Rationale: displacing positions invalidates normals, and **perturbing the normal
is itself a primary use case** — a custom vertex shader may want to displace the
normal directly (detail/wobble/anisotropy) with or without moving the position.
A renderer-side derivative or neighbor recompute would *fight* the shader (it'd
overwrite the shader's intended normal) and is strictly less expressive. So the
contract makes the frame the shader's job, exactly like color/alpha are the
fragment shader's job.

Mechanics: the agent's returned local `normal` flows through the existing
inverse-transpose transform in `apply_vertex` and is written to the visibility
buffer (and interpolated in the transparent pass) with **zero new machinery** —
the geometry vertex shader already transforms + emits whatever normal
`apply_vertex` produces. Passthrough (`return input.normal`) is the explicit
"unchanged."

Authoring help (docs/examples, not renderer logic): for height-field
displacement the analytic normal is a few lines — sample the height at two
epsilon-offset UVs and cross the tangent deltas, or return the closed-form
gradient. The contract doc ships a worked example so authors don't have to
rediscover it. (A convenience `recompute_normal_from_height(...)` WGSL helper
could live behind an include later, but it's a helper the shader *calls*, not
something the renderer imposes.)

---

## 7. Editor changes (mirror the alpha_wgsl window)

`CustomMaterial` already has two WGSL windows (`wgsl`, `alpha_wgsl`). Add a third,
`vertex_wgsl`, plumbed identically:
- A **toggle** "custom vertex" on the material (so non-vertex materials keep the
  shared fast geometry pipeline — the default must be OFF).
- The Material-mode Studio gets a vertex WGSL editor window when the toggle is on.
- `vertex_wgsl` feeds `build_registration` + the `wgsl_hash` + the `compile_wgsl`
  pre-check + `validate_dynamic_material_wgsl` (vertex variant) for live diagnostics.
- A starter body that renders non-trivially out of the box (e.g. a gentle sine
  ripple along the normal using `globals.time`), like the default fragment body.

---

## 8. MCP changes

- `set_material_vertex_wgsl { material, wgsl }` — the typed setter (mirror
  `set_material_wgsl` / `set_material_alpha_wgsl`).
- `get_material_contract` gains a `vertex: true` mode returning the vertex ABI
  (the `VertexDisplaceInput`/`Output` structs + the include list), mirroring the
  existing `transparent: true` contract switch.
- A new `docs/dynamic-materials/contract-vertex.md` describing the ABI + the
  normal caveat + a worked example (heightmap displacement + analytic normal).

---

## 9. Phasing

1. **Core raster path.** ABI + `apply_vertex` hook + geometry & shadow per-material
   vertex pipelines + registration/cache-key + naga validation. Verify: a custom
   vertex material ripples a plane AND its shadow + silhouette match (no detached
   shadow). This is the correctness baseline.
2. **Cover the rest of the passes.** Transparent + geometry-masked + shadow-masked
   custom-vertex variants. Verify a *transparent* + *masked* custom-vertex mesh.
3. **Authoring surface.** Editor window + toggle + MCP setter + contract doc +
   starter body + live diagnostics.
4. **Polish (optional).** Derivative normal recompute (§6.2); a vertex-stage
   heightmap-sampling worked example; skinned-mesh interaction tests.

Phases 1–2 are the renderer lift; 3 is the mechanical mirror of `alpha_wgsl`; 4 is
opt-in quality.

---

## 10. Performance

- **Zero cost for materials that don't use it.** The hook is gated
  (`has_custom_vertex` defaults off); every existing mesh keeps the single shared
  geometry/shadow pipeline and the current batched draw. This must be guarded by a
  test/benchmark — the whole point is that the common path is untouched.
- **Cost is per-custom-vertex-material, and opt-in.** Each such material adds a
  geometry-pass pipeline + its own draw bucket (breaking the one-pipeline batch for
  *those* meshes only) + binding the material uniform/texture groups in the
  geometry + shadow passes. For a handful of custom-vertex materials this is
  negligible; a scene that makes *every* material custom-vertex pays the
  visibility pass becoming material-bucketed (closer to a traditional forward
  vertex cost). That's inherent and acceptable — it's the agent's choice.
- **Vertex texture fetch** (heightmap sampling in the vertex stage) is a real but
  agent-chosen cost; no different from any engine's vertex displacement.
- Compile cost: one extra pipeline per custom-vertex material per pass it appears
  in (geometry/shadow/transparent/masked), compiled once on registration (the
  `wgsl_hash`/`dispatch_hash` cache already dedups).

---

## 11. Resolved decisions

- **Normal/tangent ownership** → the shader returns the surface frame; the
  renderer never recomputes (§6).
- **Injection order** → after morph, **before skin**, in the post-morph LOCAL
  frame, consistently for skinned + rigid meshes (§2a). A post-skin/world-space
  variant is a later opt-in flag, not v1.
- **Skinned + custom-vertex** → resolved by the order above: displacement is in
  rest-pose local space and gets skinned along (a displaced character's detail
  follows the skin — the intuitive default).
- **Instanced + custom-vertex** → `instance_id` is in `VertexDisplaceInput`, so an
  author can drive per-instance displacement (index a declared buffer by it). The
  model/instance transform still applies after the hook, unchanged.
- **Tangents** → carried in the output frame (passthrough by default; the shader
  recomputes them when it does normal-mapped detail). Resolved as part of §6.
- **MSAA / masked edges** → orthogonal: the vertex hook only changes position/
  frame; the masked fragment's sample-mask AA path is untouched. Preserve it
  as-is in the masked custom-vertex variant.

## 12. Remaining implementation risk (not a design question)

The one genuinely hard part is mechanical, not a decision: the **geometry-pass +
shadow-pass draw loops** go from one shared pipeline to per-vertex-material
pipeline selection + binding the material groups for custom-vertex draws. Scope
this first (Phase 1) and gate the success criterion on **shadows + silhouette
matching the displaced surface** — that's the proof the five passes stayed in
sync.

---

# Part B — Multithreaded renderer: build items


Code work deferred from the Phase 2 hardening. These are *not* testing (that
lives in `docs/plans/multithread-testing.md`) — they are renderer/protocol
features still to be written. Architecture + the landed work: `PLAYER-GUIDE`
§9.

> Not here: the **game-API parity** work (load an exported scene in the worker +
> runtime ops) — that is being built directly, not deferred.

## B1 — Device-loss + worker-crash recovery (resilience)
Production sessions must survive a lost GPU device and a dead worker. The arena
+ the renderer's CPU mirrors are already the source of truth, so recovery is
largely "rebuild the GPU side from data we still hold."

- **GPU device loss.** Subscribe to `GPUDevice.lost`. On loss: request a fresh
  adapter/device, recreate the surface config + all GPU buffers/textures/
  pipelines, and re-upload from the existing CPU mirrors (transforms buffer,
  instance arenas, materials, mesh geometry). The scene graph / arena state in
  shared memory is untouched — only `web_sys` GPU handles are rebuilt.
  - Open question: how much of the renderer's GPU-handle graph can be rebuilt
    behind a single `renderer.rebuild_gpu()` vs. needs a full re-`commit_load`.
- **Worker crash.** Main thread watches `worker.onerror` / a heartbeat. On death:
  respawn the worker from the same bootstrap, re-transfer a fresh
  `OffscreenCanvas` (the old one is gone with the worker), re-post the shared
  module+memory, and re-hand every live arena `SlotBinding` (the sim worker's
  bindings are addresses into shared memory that *survived* — but the render
  worker that owned topology did not, so topology must be re-derived; design
  whether the sim worker or a persisted manifest re-seeds it).
  - Open question: can the render worker's topology (slot→key map) live in shared
    memory so a respawn recovers it, rather than reloading the scene?
- **Acceptance:** T4 in the testing doc (force device loss / kill the worker →
  renderer continues).

## B2 — Screenshot capture path (the platform-bounded gap)
`OffscreenCanvas.convertToBlob` is rejected by Chrome on a WebGPU canvas
(`NotReadableError` — swapchain not host-readable post-present; measured in H7).
A robust capture needs renderer support:

- Render (or blit) the final frame into an explicit color target created with
  `COPY_SRC`, then `copyTextureToBuffer` → `mapAsync` → read the bytes →
  (optionally PNG-encode) → return over the protocol's `Screenshot` →
  `ScreenshotBytes` (+ Transferable buffer, already wired).
- Expose this as a renderer API (`renderer.capture_frame() -> Vec<u8>`) so both
  the single-threaded model-viewer and the worker path share it.
- Watch row-stride alignment (`bytesPerRow` 256-byte multiple) on readback.
- **Acceptance:** `?demo=remote` `Screenshot` returns non-empty bytes whose
  decoded image matches the on-screen frame.

## B4 — (optional) A real bundled scene fixture for `?demo=scene`
The stale `assets/world/*` fixtures were deleted (pre-refactor format). If you
want `?demo=scene` to load a real scene **file** (closer to the shipped path)
rather than building a `Scene` in code, export one from the current editor as a
TOML `EditorProject`, bundle it same-origin (Trunk `copy-dir`), and have the
worker fetch + `project_to_scene` + `load_scene_for_player`. Low priority — the
in-code scene already proves the player loader runs in the worker.

## B3 — (conditional) Arena growth policy
Only if T3's churn soak shows shared-memory growth is *unbounded*: add slab
reuse / compaction so long spawn/despawn sessions keep memory flat. Default
assumption is that free-slot reuse already bounds it — confirm in T3 first.

---

# Part C — Multithreaded renderer: testing


The Phase 1 + Phase 2 architecture is landed and verified (see
`docs/PLAYER-GUIDE.md` §9). What remains before a public ship is **validation**,
not architecture. This doc is *testing only* — code work deferred from the
hardening lives in `docs/plans/multithread-build-plan.md`, not here.

Target is **Chrome desktop** (the project's only platform). All gates below run
through the chrome-devtools MCP.

## T3 — Performance at scale + soak (the one that catches slow leaks)
- **Frame-time budgets** under `?stress=N` for `motion` / `crowd` at rising N;
  record the mover-count where the render worker misses 16.6 ms.
- **Multi-minute soak with a heap trace** (`?trace=sub-frame`,
  `take_heapsnapshot`): confirm **no per-frame heap growth** in the render hot
  path (the pack/upload path is pooled by construction — verify under load).
- **Shared-memory growth is BOUNDED under sustained spawn/despawn churn**
  (`?demo=churn` over many minutes). Shared `WebAssembly.Memory` grows but never
  shrinks; confirm the arena's free-slot reuse keeps growth flat rather than
  ratcheting. **If growth is unbounded, that flips to a build item** (arena
  compaction / slab reuse policy) — note it back in the build-plan.
- **Exit:** documented N-vs-frametime curve; flat memory over a 10-min churn soak.

## T4 — Resilience **verification** (after the build-plan lands the code)
The recovery *code* is in `multithread-build-plan.md`; this is its test side:
- Force `GPUDevice` loss (devtools / `device.destroy()`) mid-session → renderer
  rebuilds and keeps rendering.
- Kill the render worker mid-session → main thread respawns it, re-hands the
  `OffscreenCanvas`, re-establishes arena bindings, scene is intact.
- Asset-fetch failure during a scene load → clean `Error` event, no hang.

## T5 — Allocation / GC validation (David's standard)
- Under `?stress` + `?trace=sub-frame`, confirm the render hot path does **zero**
  per-frame heap allocation (pooled scratch in `transforms::descend_pack_arena`;
  pre-allocated binding/bind tables in the physics workers). Catch any
  regression that reintroduces a per-frame `Vec`/`Box`.

## How to run
- All gates: chrome-devtools MCP (as used throughout Phase 2).
- Server: `task mt:dev` (port 9090, COOP/COEP).
