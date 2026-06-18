# prep-only — make the shared prep pass the single opaque shading path

**Goal (David):** there is no "prep vs non-prep". The deferred/**opaque** path has ONE way — the opaque
wrapper (`cs_opaque` no-MSAA / `cs_shade` MSAA) reads the shared **prep** buffers (UV/vcolor arrays +
per-pixel + per-edge-sample shadow + per-edge-sample UV/vcolor) and calls the templated material color fn.
No on/off flag, no recompute branch in the opaque module, no second opaque variant.

**Why the duality exists today:** prep was rolled out behind an A/B flag (`PrepPassConfig.enabled` /
`with_prep_pass`) for safe, byte-parity-gated incremental landing. That flag is the cruft — kill it.

**Boundary — NOT in scope (architectural, not the flag):** the **transparent** pass is *forward*-shaded;
it has no deferred visibility buffer, so there is physically no prep buffer for it to read. Transparent
materials therefore recompute UV/vcolor/shadow inline — a different rendering model, not the prep flag. The
material *color* templates are ALREADY unified (one `wgsl_fragment` body; opaque + transparent are two thin
wrappers). So the shared recompute WGSL (`{% if prep_present %} read {% else %} recompute {% endif %}` in
`apply_lighting.wgsl` / `vertex_color_attrib.wgsl` / texture_uv helpers) STAYS — it is rendered only for the
transparent module (which passes `prep_present=false`); the opaque module never emits it once opaque is
always `prep_present=true`. (A true "deferred transparency" unification is a separate, much larger project.)

**Branch:** `follow-on` (off merged main / PR #129). Stage ONLY explicit renderer src paths; NEVER `git add
-A`. NO backticks in `git commit -m` (zsh substitutes them). After every commit: `cargo test -p awsm-renderer
-p awsm-materials -p awsm-scene-loader --lib` GREEN. Do NOT touch uber-shader.md / start the uber-shader.

**GPU byte-parity gate (Part-A discipline):** prep-ON is already verified byte-identical to prep-OFF, so P1
just makes the verified path the only one — confirm model-tests still render + (cross-check) prep output ==
the saved baseline anchors on MetalRoughSpheres + SheenChair, MSAA. P2 is the real parity risk (new buffer +
offset). Method: model-tests :9080 (touch lib.rs; wait for `Compiling awsm-renderer` + new `✅ success`),
chrome `/app/model/<Name>`, sleep ~14s, screenshot, python3 PIL diff excluding sidebar x<215, max-diff 0.
(Re-create the baseline anchors first — they were deleted; capture current HEAD prep-on as the reference,
since prep-on IS the shipping behavior.)

---

**VERIFICATION BAR (corrected, per David):** this is a deliberate shader-architecture rewrite, NOT a
behavior-preserving refactor — so do NOT gate on byte-parity vs the old shaders. The bar is **correct
visible result**: the models render properly (no black frames, no artifacts/seams, no pipeline-create
errors), shading looks right. Sub-pixel deltas (a channel off by 1 on a handful of pixels, from FP-op
reordering when the shader text changes) are EXPECTED and fine — ignore them. Only chunky/structural visual
differences or breakage are real failures. Use the PIL diff only as a coarse signal (huge diff = look at it;
a few ±1 pixels = fine), never as a hard max-diff-0 gate.

## P1 [DONE] — remove the prep on/off flag (opaque = prep-only)
Removed `PrepPassConfig.enabled` + `with_prep_pass` + all the flag threading; prep pass + buffers are now
unconditional; opaque always `prep_present=true` (the prep variant axis collapsed → one opaque variant per
(msaa,mips,shader_id)); transparent keeps `prep_present=false` (forward). 257+34+27 green, warning-free.
Renders correctly (MetalRoughSpheres + SheenChair, MSAA); the only delta vs the old prep-on build is an
invisible ±1 on 93 sheen pixels (FP-op reordering from the changed shader text under whitespace-minimize) —
not a regression, ignored per the corrected bar above. size_regression ceilings re-set to the prep-on sizes.

## P1 (orig) — remove the prep on/off flag (opaque = prep-only)

- Delete `PrepPassConfig.enabled` + `AwsmRenderer(Builder)::with_prep_pass` + the `prep_enabled`/`enabled`
  threading (renderer.rs, render.rs RenderContext, anti_alias.rs, textures.rs, render_textures.rs, the
  opaque `pipeline.rs` + `shader/cache_key.rs` + `shader/template.rs`, classify if it reads it). KEEP the
  rest of `PrepPassConfig` (the `max_shadow_casters_per_pixel` / K sizing knob + `shadow_visibility_layers`).
- Make the prep pass + its buffers UNCONDITIONAL: `render_textures` always allocates the prep UV/vcolor +
  shadow-visibility arrays; `render.rs` always dispatches `cs_prep` (+ `cs_prep_edge` under MSAA); the
  `material_prep` render pass is always `Some`.
- Opaque shader: always render with `prep_present = true` (the opaque cache key/template no longer carries a
  prep axis → one opaque variant per (msaa, mips, shader_id), not two). Transparent still passes
  `prep_present = false` (forward). The shared `.wgsl` `{% if prep_present %}` forks are untouched — opaque
  simply only ever emits the read branch.
- size_regression: prep-on opaque sizes become the baseline; re-tighten ceilings.
- Verify: 320 tests green; model-tests render; prep-only opaque == baseline anchors (MSAA, MetalRoughSpheres
  + SheenChair). Commit P1 [DONE].

## P2 [RESOLVED — WON'T DO, by design] — 5b-attrs: edge samples read prep
After analysis (+ David), the per-edge-sample UV/vcolor buffer is a **pessimization**, not
a win: the edge shading arm already holds the per-sample triangle + barycentric in-register,
so recomputing UV/vcolor there is a few reads — cheaper than computing the same in
`cs_prep_edge`, writing it, reading it back, plus ~16–48 MB VRAM, to evict ~10 lines of code.
Shadows earn their edge buffer (expensive sampling + evicts ~50 KB); UV/vcolor don't. This is
the same call as world-position (also recomputed, never prepped). Edge UV/vcolor recompute is
KEPT and documented as a deliberate boundary. The rule ("prep the expensive common work,
re-derive the trivially-cheap work") is now written into `material_prep/buffers.rs`,
`helpers/texture_uvs.wgsl`, `helpers/vertex_color_attrib.wgsl`, `material_prep/.../compute.wgsl`,
`README.md`, `packages/crates/renderer/README.md`, and `docs/SHADER_GUIDELINES.md`.

## P2 (orig) — 5b-attrs: edge samples read prep too (delete the last opaque recompute)

- Add a packed per-edge-sample UV0/vcolor0 buffer mirroring `EdgeShadowBuffer` (`material_prep/buffers.rs`):
  an `Rgba8unorm`/fp16-packed `texture_2d_array` (a texture, not an 11th storage buffer — dodge the macOS
  10-storage cap), sized `max_edge_budget × MAX_EDGE_SHADOW_SAMPLES`, keyed identically to the shadow buffer
  (`edge_pixel_id * samples + sample`).
- `cs_prep_edge` fills it (per edge-sample UV/vcolor, same loop that fills the shadow buffer).
- `cs_shade` EDGE arm reads it via `PrepReadContext` EDGE mode → DELETE the EDGE-mode RECOMPUTE for
  UV/vcolor in `apply_lighting.wgsl` / `vertex_color_attrib.wgsl` / the texture_uv helper (opaque only —
  the transparent `prep_present=false` branch is unaffected). After P2 the opaque module has ZERO recompute.
- VRAM: packed, so ~comparable to the ~8 MB shadow buffer (NOT the ~48 MB fp32 estimate that motivated the
  original deferral).
- Verify: byte-parity MSAA + (now-unconditional) prep on SheenChair (multi-material + multi-UV + edges) +
  MetalRoughSpheres + MultiUv, max-diff 0. Commit P2 [DONE].

## Cleanup
Remove §5 from `followup.md` (both items now done). If any opaque-only recompute helper is now genuinely
unreferenced (NOT shared with transparent), delete it; otherwise leave the shared source. Update naga tests.

When P1+P2 done + green + byte-parity: STOP, post a before/after summary (prep flag gone; opaque variant
axis collapsed; opaque recompute eliminated; transparent forward boundary documented), confirm uber-shader
still awaits David.
