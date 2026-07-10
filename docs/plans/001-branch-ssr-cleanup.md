# 001 — Finish + harden the `updates` branch work (SSR, bloom, MCP fixes)

**Order:** first. Everything else builds on a green, committed baseline.
**Commit checkpoints:** (1) fmt+clippy green, (2) SSR bugs fixed + browser-verified,
(3) bloom old-path removal, (4) exposure/doc gaps. Commit the pre-existing working-tree
work first so fixes are reviewable on top of it.

Audit of the uncommitted working tree, 2026-07-10. Four sweeps: SSR renderer internals, SSR
exposure/persistence, the MCP-BUGS.md fix list, and the rest of the diff, plus fmt/clippy/tests.

## Verdict in one paragraph

The SSR **production path is complete and correctly wired end-to-end**: material_opaque writes a
`reflection_descriptor` (gated by the `write_ssr_descriptor` cache-key axis), the trace pass
(linear DDA, half-res default) samples the resolved HDR and writes premultiplied reflection, and a
joint-bilateral composite additively blends it — with correct structural-vs-live-uniform handling,
lazy textures, resize, zero cost when disabled, and full persistence (scene model → project.toml →
bundle scene.toml → scene-loader apply → editor UI with dirty+undo → MCP partial-update).
`cargo test --all-features`: **all green** (700+ tests). But **CI is red** (fmt + one clippy lint),
there are **two real SSR rendering bugs** (MSAA edge descriptor gap, dead blit bind group), the
bloom migration left its old path half-removed, and a handful of exposure/doc gaps below. Hi-Z
traversal and temporal denoise are built but deliberately parked (gated off) — scaffolding, not bugs.

## P0 — CI blockers (branch does not pass `task lint`)

- [x] `cargo fmt --all` — DONE (commit 8139e24a); `task lint` fmt step green.
- [x] clippy `needless_option_as_deref` fixed (`maps.as_deref_mut()` → `maps`, + dropped the
      then-unused `mut`) — commit 8139e24a; full `task lint` exit 0.

## P0 — SSR correctness bugs (confirmed by direct inspection)

- [x] **FIXED (commit 3dcebbd6): MSAA edge pixels never wrote `reflection_descriptor`.**
      Fix: `shade_sample` now textureStores the descriptor when `sample_index == 0` (the
      owning bucket writes; exclusive ownership ⇒ race-free; sky-at-sample-0 needs no store —
      trace bails on depth ≥ 1.0). naga-validated by the wgsl_validation suite; browser
      smoke-verified with MSAA on: emissive-sphere reflections on a glossy floor, structural
      SSR off→on + half↔full-res toggles, zero console errors. Original finding:
      Stores exist in `cs_opaque` (`compute.wgsl:438`) and the cs_shade interior/sample-0 arm
      (`compute.wgsl:1072`), but the EDGE ARM (`compute.wgsl:~1077-1176`) returns without a
      `textureStore` — `shade_sample` computes `ssr_reflectivity`/`ssr_spread` (`:547/:646`) and
      discards them. Since there is no clear/LoadOp on the storage texture, silhouette-edge pixels
      under MSAA read stale prior-frame reflectivity → edge shimmer/garbage reflections.
      Fix: store a descriptor in the edge arm (e.g. sample-0's, or coverage-weighted), or clear the
      texture when SSR+MSAA are both on. Then browser-verify with MSAA on
      ([[wgsl-cross-stage-interpolation-must-match]]: WGSL is runtime-only, cargo can't catch it).
- [x] **Dead `ssr_to_transparent_blit_bind_group` deleted** (commit 3dcebbd6) — field +
      creation removed from `render_textures.rs`; stale ssr-texture docstring corrected while
      there (reflection-only premultiplied + additive composite, half-res default).

## P1 — SSR exposure/persistence gaps

- [ ] **`ssr.temporal_weight` has no live control surface anywhere.** It persists and the renderer
      writes it per-frame (`render.rs:639`), but there's no editor UI row (`app.rs` only has the
      `SSR temporal` toggle) and no `ssr_temporal_weight` field in MCP `PostProcessParams` /
      `SetPostProcess`. Only reachable by hand-editing TOML. Add both (or decide it's intentional
      and document that).
- [ ] **No MCP read-back for post-process state** — `set_post_process` exists, no
      `get_post_process` (pre-existing: bloom/exposure were never readable either; SSR inherits
      it). An agent can set SSR blind but can't query current values. Worth adding while in here.
- [ ] **`set_post_process` tool description omits SSR** (`packages/mcp/src/mcp.rs:~3895`) — prose
      lists tonemapping/bloom/dof/exposure but never mentions SSR (per-field schema descriptions do
      cover it). Update the summary.
- [ ] **Stale comment** in `dispatch_post` (`packages/frontend/editor/src/app.rs:~988`): "SSR has
      no settings-drawer control yet (set via MCP)" — false; full drawer controls exist right above.

## P1 — Bloom migration: old path half-removed

The dedicated bloom mip-pyramid pass replaced the effects-pass extract/blur, but the old path is
duplicated rather than deleted:

- [ ] `effects/pipeline.rs:156-171` (`slot_inputs_for`) still compiles the `BloomPhase::Extract` +
      both `Blur` pipelines when bloom is on; `effects/render_pass.rs` only dispatches `Blend`.
      Remove the dead slots/pipelines and the old extract/blur branches in
      `effects_wgsl/helpers/bloom.wgsl:85-114` (incl. `blur_sample`).
- [ ] Dead const `BLOOM_INTENSITY` (`bloom.wgsl:3`) — unreferenced (intensity is now pre-applied in
      the bloom pass).
- [ ] `effects/render_pass.rs:69` `let _ = BLOOM_BLUR_PASSES;` leftover — drop the import.
- [ ] Blend `ping_pong` hardcoded `false` in render_pass.rs while pipeline.rs still derives
      `(1+BLOOM_BLUR_PASSES)%2==1` — they agree today; unify so they can't diverge.

## P2 — SSR polish / doc drift (stale M1 blit-design docstrings in shipped M4 code)

- [ ] `ssr/mod.rs:6-7` + `render_passes.rs:129-132` say the pass runs "before the transparent
      pass" — it runs after transparent/resolve (`render.rs:1624-1671`).
- [ ] `ssr/render_pass.rs:1-9` + `trace.wgsl:7-10` describe "base + reflection, blit back over
      composite" — actual: reflection-only premultiplied + additive composite.
- [ ] `trace.wgsl:389` writes alpha=1.0 on a MISS while `composite.rs:3` documents "alpha =
      coverage; misses write 0". Harmless today (additive blend uses rgb), but a latent trap —
      make alpha actually be coverage, or fix the doc.
- [ ] `ssr/bind_group.rs:10` calls the target "full-res" (half-res by default).
- [ ] Duplicated `ssr_reflectivity/ssr_spread` init+store copy-pasted across `cs_opaque` /
      `shade_sample` / interior arm in compute.wgsl — extract a helper to reduce drift risk.
- [ ] Per-frame `vec![]` for color attachments / descriptors in `ssr/composite.rs:316`,
      `ssr/render_pass.rs:143` — [[avoid-per-frame-allocations-standard]] applies even if
      idiomatic elsewhere in the renderer; pool or hoist.
- [ ] Stray double blank line in `render.rs` ~:1527 (fmt will catch).

## P2 — Non-SSR diff findings

- [ ] **`worker_job.rs::execute_async` parity path missed by the glTF fetch fixes** — still uses
      `get_type_from_filename(..).unwrap_or(Json)` + text fetch (`worker_job.rs:509-546`) and has
      no `bypass_http_cache`. Latent (path is A/B-gated, non-default), but it's the exact bug just
      fixed in `GltfLoader::load`; port the content-sniff + cache flag or delete the parity path.
- [ ] **`skin_bridge.rs::sync_bones_to_skin` allocates per frame** — rebuilds a
      `HashSet<TransformKey>` + `Vec` snapshot every call even when idle (~:60). The equality guard
      only skips writes, not construction. Cache/reuse ([[avoid-per-frame-allocations-standard]]).
- [ ] **Skinned duplicate: verify opaque pipeline key cloning.** `duplicate_skinned_with_new_skin`
      (meshes.rs ~1520) clones the transparent pipeline key; confirm an opaque skinned mesh
      duplicate also lands in the opaque pass's pipeline map (the non-skinned duplicate path does).
- [ ] Pool-slicing math in `duplicate_skinned_with_new_skin` assumes `[vis || attr_index ||
      attr_data]` contiguous packing — add a debug assert on offset monotonicity.
- [ ] **Stale off-by-one binding comments** in `material_opaque/bind_group.rs` after the 24→25..28
      prep-binding shift: lines ~377, ~400 ("binding 26"→27), ~412 ("27"→28), and layout-section
      comments ~841, ~872, ~889/892. Runtime is positional and correct; comments lie.
- [ ] **Docs stale vs. MCP-BUGS fixes**: `docs/ASSET_WORKFLOWS.md:23,50,56` still document only
      `"builtin"` as the env-slot reset string (code now also accepts `"built_in_default"`); the
      new `set_camera_clip` tool description says "near 0.1, far 10000" but actual manual defaults
      are `1.0/5000.0` (`editor/src/controller/state.rs:251-255`).
- [ ] No tests were added for any of the MCP-BUGS fixes (all verified manually/live). At minimum:
      a unit test for the loader content-sniff (GLB magic vs JSON) and the `/glb` upload/download
      `.glb`-suffix symmetry.

## Parked by design (tracked, not TODO for this branch)

- **Hi-Z traversal** (`ssr_minz/` + `hiz` template branch): built, naga-validated, gated OFF
  (`SsrTrace::PRODUCTION == LinearDda`, `cache_key.rs:51`) because coarse traversal has a known
  horizontal-banding defect (`cache_key.rs:33-48`, `trace.wgsl:177-185`). Zero runtime cost while
  gated. Reverse-Z is now plan 003 (runs BEFORE SSR sign-off); Hi-Z promote-or-delete is decided in plan 004 (trace assumes forward-Z, `depth>=1.0`
  = sky).
- **Temporal reprojection**: works but no neighbourhood color clamp (explicitly out of scope,
  `trace.wgsl:363-364`); default `temporal_weight=0.9` will ghost on moving reflectors. Off by
  default — fine; don't ship `temporal=true` as a default until clamped.

## Verification still owed (nothing on this branch has been browser-verified)

> **Scope note:** this plan only owes SMOKE verification — enough to prove the P0 fixes
> land (especially item 2, MSAA+SSR edges) and nothing is visibly broken. The FULL SSR
> sign-off matrix (all knobs × MSAA × res × temporal × materials, plus the Hi-Z
> promote-or-delete decision) is deliberately deferred to plan 004, AFTER reverse-Z
> (003) changes the depth convention under it — don't burn time exhaustively verifying
> a convention that's about to change.

Per [[wgsl-cross-stage-interpolation-must-match]] / [[aa-verify-in-model-viewer]], shader-side work
needs live verification:

1. SSR on/off toggle, all knobs live, on a glossy scene (black glossy dielectric probes specular
   best — white saturates).
2. **MSAA + SSR together** — the P0 edge-descriptor bug will show here; verify the fix kills edge
   shimmer.
3. Half-res upsample quality at silhouettes (composite sigma `z*0.05` heuristic,
   `composite.rs:145`).
4. Temporal on: ghosting behavior with a moving camera/reflector.
5. Editor round-trip: set SSR in drawer → save → reload project → values persist; export bundle →
   player load applies SSR.
6. MCP `set_post_process` partial update of SSR fields.
7. Bloom visual parity vs the old effects-pass bloom (threshold/knee/intensity/scatter knobs).
