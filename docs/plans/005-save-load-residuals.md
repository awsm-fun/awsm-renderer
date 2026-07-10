# 005 — Save/Load roundtrip: remaining items

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

## 2. Reloaded captured meshes aren't editable until re-imported
Known minor gap: a cold-loaded **captured** mesh renders but `get_mesh_data` /
`get_vertex_data` read 0 verts, so sculpt/vertex tools are dead on it until re-import.
Find where the reload path skips populating the editable-mesh side (the render path and
the edit path diverge on load) and fix; add a fixture to the VerifyRoundtrip run
(load → query vertex count > 0 → perform a vertex edit → save → reload → edit survives).

## 3. Multi-track morph blending (runtime feature, was P2-A)
Multiple simultaneous morph tracks targeting the same mesh at different indices stomp
each other (`animation.rs:159` — per-index masked blending deferred). The morph DATA
roundtrips fine; this is a runtime mixing feature. Implement per-index masked blending;
fixture: 2 tracks × 2 indices animating independently. This also feeds the
animation-blend test scene in plan 006.

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

## 5. Field backstop (watch, no code)
The picked-directory truncation hypothesis is instrumented (`fs.rs::write_bytes`
re-read-verify + `save_to_dir` "wrote N/total" log). Nothing to build — but if any
loop-session save ever logs a verify failure, stop and surface it immediately.
