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

- [x] DONE (commit 718181bd) — `ssr_temporal_weight` added to the SetPostProcess command
      (undoable), the editor drawer ("SSR temporal weight" row, clamped 0..1), and MCP
      `PostProcessParams`. Browser-verified round trip (dispatch 0.42 → query reads it back).
      Original finding: **`ssr.temporal_weight` has no live control surface anywhere.** It persists and the renderer
      writes it per-frame (`render.rs:639`), but there's no editor UI row (`app.rs` only has the
      `SSR temporal` toggle) and no `ssr_temporal_weight` field in MCP `PostProcessParams` /
      `SetPostProcess`. Only reachable by hand-editing TOML. Add both (or decide it's intentional
      and document that).
- [x] DONE (commit 718181bd) — read-only `get_post_process` MCP tool via new
      `EditorQuery::PostProcess` (full PostProcessConfig JSON incl. the ssr block).
      Original finding: **No MCP read-back for post-process state** — `set_post_process` exists, no
      `get_post_process` (pre-existing: bloom/exposure were never readable either; SSR inherits
      it). An agent can set SSR blind but can't query current values. Worth adding while in here.
- [x] DONE (commit 718181bd) — description now documents the whole ssr_* surface, live-vs-
      structural split, and points at get_post_process. Original finding:
      **`set_post_process` tool description omits SSR** (`packages/mcp/src/mcp.rs:~3895`) — prose
      lists tonemapping/bloom/dof/exposure but never mentions SSR (per-field schema descriptions do
      cover it). Update the summary.
- [x] DONE (commit 718181bd) — comment rewritten to point at dispatch_ssr. Original finding:
      **Stale comment** in `dispatch_post` (`packages/frontend/editor/src/app.rs:~988`): "SSR has
      no settings-drawer control yet (set via MCP)" — false; full drawer controls exist right above.

## P1 — Bloom migration: old path half-removed

The dedicated bloom mip-pyramid pass replaced the effects-pass extract/blur, but the old path is
duplicated rather than deleted:

- [x] DONE (all four, one cut): `BloomPhase` reduced to `None | Blend` (Extract/Blur variants
      deleted), the `ping_pong` axis removed from the effects cache key / templates /
      bind-groups (B bind group deleted — blend can no longer diverge from the compiled
      pipeline by construction), `BLOOM_BLUR_PASSES` + `BLOOM_INTENSITY` + `blur_sample` +
      `bloom_threshold` + the extract/blur WGSL branches all deleted; bloom.wgsl is now just
      the blend fn. Bloom-on compiles 2 slots instead of 5. Browser-verified: emissive sphere
      + bloom on → wide pyramid glow, zero console errors; bloom-off path unchanged.

## P2 — SSR polish / doc drift (stale M1 blit-design docstrings in shipped M4 code)

- [x] `ssr/mod.rs` + `render_passes.rs` placement/staging docs rewritten (post-resolve,
      before bloom; shipped-vs-gated state).
- [x] `ssr/render_pass.rs` + `trace.wgsl` head docs rewritten to the shipped reflection-only
      + additive-composite design.
- [x] Alpha IS coverage now: `let coverage = select(0.0, 1.0, hit)` stored in both the out
      and temporal-history writes — matches composite.rs's documented contract. Browser-verified
      output-identical (additive blend uses rgb).
- [x] `ssr/bind_group.rs` binding-5 doc corrected (half-res by default).
- [x] `ssr_pbr_descriptor()` helper extracted; all three arms (cs_opaque / shade_sample /
      interior) call it. naga-validated + browser-verified identical reflections.
- [x] `SsrComposite` render-pass descriptor now built once in `recreate()` and reused every
      frame (allocation-free render()); the trace dispatch had no per-frame vec after the
      baseline. Browser-verified.
- [x] Handled by the fmt pass (commit 8139e24a).

## P2 — Non-SSR diff findings

- [x] `worker_job.rs::execute_async` ported to the single-binary-fetch + `parse_gltf_lenient`
      content-sniff (extension never consulted; Draco rejected up front). Cache-bypass wiring
      deferred with a doc note — the path has ZERO dispatch sites today (register-only A/B
      scaffold); wire `bypass_http_cache` through `GltfParseInput` when 006 axis 2 decides its
      fate (promote or delete).
- [x] `sync_bones_to_skin` now reuses thread-local scratch (Vec + HashSet, clear keeps
      capacity) — zero steady-state allocations.
- [x] VERIFIED NO GAP: the opaque pass has no per-mesh pipeline map to clone — opaque
      pipelines are bucket-driven (classify by shader_id; the duplicate's material meta routes it
      to the same bucket). Both duplicate fns clone exactly the transparent per-mesh key, and the
      skinned fn mirrors the static fn line-for-line (meshes.rs:38-58 vs :77-99).
- [x] debug_asserts added on offset monotonicity + section-length bounds in both match arms.
- [x] All seven stale binding-number comments corrected (25/26/27/28).
- [x] `ASSET_WORKFLOWS.md` now documents all three reset aliases; `set_camera_clip` description
      corrected (AUTO default; manual 1.0/5000).
- [x] Tests added: `loader.rs::content_sniff_parses_glb_and_json_bytes` (builds a minimal GLB
      container + raw JSON, both parse via the content sniff) and
      `http.rs::glb_id_suffix_symmetry` (upload/download now share `canonical_glb_id`, extracted
      so they can't drift apart again). Both green.

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
