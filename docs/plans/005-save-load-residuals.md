# 005 — Save/Load roundtrip: remaining items

**STATUS: ✅ COMPLETE (2026-07-10)** — §1 shipped+browser-verified, §2 not
reproducible (evidence below), §3 shipped with native tests, §4 real bug found
+ fixed + determinism pinned, §5 remains watch-only (nothing to build).

**Order:** fifth. Small, self-contained, and later plans (test scenes are saved
projects + baked bundles) depend on trusting the roundtrip completely.

The big save/load fidelity effort is **done and shipped** (see git history of the
retired `save-load-roundtrip.md` plan): root cause was an interruptible fire-and-forget
async save (fixed: blocking save modal + per-file write-verify + `check_save_complete`
guard + `SaveCensus`); authored tangents, texture color-space (`color_kind`), external-URI
texture bytes, KTX2/HDR env bytes, and custom-material Studio reload all roundtrip. The
law stands: **Save and Load are each ONE transaction; a save that cannot be lossless
fails loudly.** What's left:

## 1. `VerifyRoundtrip` end-to-end regression command (the missing harness half)
The shipped `SaveCensus` (`persistence.rs:903`, `editor_query_json({"query":"save_census"})`)
counts caches; the full self-test was never built. Add debug
`EditorCommand::VerifyRoundtrip`: `serialize_inmem` → clear **ALL** byte caches
**including `mesh_cache`** (the old self-test deliberately skipped it — exactly where the
historical drift hid) → `apply_inmem` → assert per-subsystem counts + byte-equality
(every `AssetSource::Mesh` non-empty, every raster texture has bytes, clip/track/keyframe
counts, rig/bind/cluster/KTX present) → JSON census out. Drive it over chrome-devtools
against fixtures: static multi-mesh, skinned, morph, sculpted, custom-material, nanite,
KTX env. This becomes the standing regression net for every later plan that touches
persistence.

### §1 RESULT (2026-07-10): **SHIPPED + browser-verified**
`EditorCommand::VerifyRoundtrip` (also an MCP tool `verify_roundtrip` + query
`verify_roundtrip_report`): census → `serialize_inmem` → clear EVERY byte cache
(mesh_cache INCLUDED — the historical hiding spot — plus texture/buffer/rig/
bind-pose/cluster/KTX/skin-joints + untracked renderer resources + texture-key
registry) → `apply_inmem` → census again → `{before, after, equal,
after_complete, lossless}`. Driven over a live fixture (box + sphere + 2 pbr
material variants + procedural checker bound to base_color + clip/track/2
keyframes): report `lossless: true`, all counts identical, and the rendered
frame is BYTE-IDENTICAL before vs after (scene PNG 224,802 B both sides).
Bonus: the run exposed the §4 stale-texture-key bug below — exactly the class
of drift the harness exists to catch.

## 2. Reloaded captured meshes aren't editable until re-imported
Known minor gap: a cold-loaded **captured** mesh renders but `get_mesh_data` /
`get_vertex_data` read 0 verts, so sculpt/vertex tools are dead on it until re-import.
Find where the reload path skips populating the editable-mesh side (the render path and
the edit path diverge on load) and fix; add a fixture to the VerifyRoundtrip run
(load → query vertex count > 0 → perform a vertex edit → save → reload → edit survives).

### §2 RESULT (2026-07-10): **NOT REPRODUCIBLE on HEAD — no code change**
Browser repro attempted on `updates` HEAD (editor :9085): insert sphere →
`convert_to_editable_mesh {node, mesh: <new-asset-id>}` → `get_mesh_data` reads
**221 verts / 384 tris** → `reload_project_in_memory` → re-query reads the SAME
221/384. The editable side is fully populated after reload; the historical gap
was evidently closed by the shipped save/load fidelity work (`restore_mesh_bytes`
re-seeds `mesh_cache` before `apply_project`). Covered permanently by the §1
VerifyRoundtrip census (mesh assets asserted non-empty after an all-cache-clear
reload).

## 3. Multi-track morph blending (runtime feature, was P2-A)
Multiple simultaneous morph tracks targeting the same mesh at different indices stomp
each other (`animation.rs:159` — per-index masked blending deferred). The morph DATA
roundtrips fine; this is a runtime mixing feature. Implement per-index masked blending;
fixture: 2 tracks × 2 indices animating independently. This also feeds the
animation-blend test scene in plan 006.

### §3 RESULT (2026-07-10): **SHIPPED — masked at the renderer blend layer**
`VertexAnimation` gains `mask: Option<u64>` (bit i drives morph index i;
`None` = whole-vector, the unchanged glTF path). Editor morph tracks lower via
`VertexAnimation::new_single(index, weight)`; `weights_blend_replace` +
additive Vertex blending skip undriven indices (rest-seeded accumulator keeps
them); both morph write paths use the clamped `apply_mut` (also fixes a latent
unclamped `copy_from_slice` panic in the loose player path). No per-frame
allocations added; single-track/glTF hot path identical. 5 new native tests
green, incl. the headline 2-tracks-1-target compose in a real
`AnimationClipGroup` (order-independent) and an editor host test proving
`Track::lower` emits `mask == Some(0b01)/Some(0b10)`.

## 4. Procedural-texture roundtrip — VERIFY, don't build (old P1-C claim is stale)
The old plan claimed procedural textures were "GPU-only, no recipe" — no longer true:
they are Checker/Gradient/Noise (`ProceduralKind` → `ProceduralTextureDef`,
`editor-protocol/src/command.rs:36`), CPU-generated in
`editor/src/engine/bridge/material.rs:646 procedural_rgba`, the def IS the recipe (a
serialized `TextureDef` variant in project.toml), and bundle export already
regenerates deterministically (`controller/export.rs:924-949`). Remaining work:
- Verify the **reload** path regenerates them (not just export) — fixture in the
  VerifyRoundtrip run.
- Verify determinism byte-for-byte across regen (Noise seed especially); pin with a
  native test on `procedural_rgba`.
- Only if verification exposes a non-deterministic or non-regenerating case: fall back
  to bake-on-save into texture_cache for that case. Otherwise this item is
  documentation + tests only.

### §4 RESULT (2026-07-10): **REAL BUG FOUND + FIXED — stale texture-key registry**
- **Determinism: PINNED.** Native test `generators_are_byte_deterministic` in
  `meshgen/src/procedural_texture.rs` (checker/gradient/seeded-noise ×2 identical
  bytes; different noise seed ≠). Green.
- **Reload bug (fixed):** a procedural checker bound to a box's pbr variant came
  back WHITE after every reload — while the binding DATA survived perfectly
  (`node_kind_details` still showed `base_color_texture: {asset}`; the asset def
  round-tripped; even unbind→rebind stayed white). Root cause: the session
  `TEXTURE_KEYS`/`TEXTURE_KEYS_ANY` registries (asset → renderer `TextureKey`)
  were NEVER cleared on project teardown — removing the old scene's meshes
  releases their pooled GPU textures, so `resolve_texture`'s cache-hit fast path
  returned a DANGLING key and the slot silently rendered untextured. Raster
  assets were masked (restore_textures re-uploads and overwrites their entries);
  procedural assets have no restore step (lazy regen at bind time), so the stale
  hit shadowed the regen path entirely. Fix: `material::clear_texture_keys()`
  called at all four teardown sites (NewProject, LoadPlayerBundle,
  ReloadProjectInMemory, VerifyRoundtrip). Verified: full fixture round-trip now
  renders BYTE-IDENTICAL scene PNGs before/after (224,802 B), census lossless.
  Note the misleading breadcrumbs for posterity: the asset panel's "Used by: 0
  objects" shows 0 even for a live builtin-inline binding (red herring), and the
  panel's checker preview renders from the def, not the GPU key.

## 5. Field backstop (watch, no code)
The picked-directory truncation hypothesis is instrumented (`fs.rs::write_bytes`
re-read-verify + `save_to_dir` "wrote N/total" log). Nothing to build — but if any
loop-session save ever logs a verify failure, stop and surface it immediately.
