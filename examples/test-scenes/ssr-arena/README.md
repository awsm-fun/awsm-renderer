# ssr-arena — the jetpack-knockout arena, frozen for SSR work

A verbatim snapshot (2026-07-13) of `games/jetpack-knockout/MEDIA/SCENE`
taken **before** the game team started iterating on the arena's artwork.
The live game scene will diverge; this copy is the permanent reflections
testbed, because it exercises every tier of the SSR stack in one frame:

- **Glossy hex floor** (roughness 0.18, **`ssr_mask` 0.7** inline on the
  floor node — the per-material receive-mask reference use).
- **Box-projected reflection probe** (`environment.probe` center [0,13,0],
  half_extents [42,14,42]) with an HDR **rgb9e5** cubemap authored in
  probe-center space with energy-conserved bands (`src/gen-assets.py` is
  the reference implementation).
- **Software-BVH off-screen reflections** (`ssr_bvh_reflections: true`) —
  floating platforms produce the classic occluder-shadow column that the
  BVH mitigates (measured decomposition in
  `docs/plans/ssr-followups.md` "Occluder-shadow diagnosis").
- Emissive-only lighting (no analytic lights except one dim directional),
  bloom, halo shells, double-sided lathe wall — the content mix that
  surfaced most of the 2026-07 SSR fixes.

## Golden

`golden.png` is captured at the gameplay verification angle used
throughout the SSR branch ("David-angle", pinned at the end of
`author.js`): platform occluder column, probe band reflections, pad
glows, and the masked floor all in frame.

## Regenerating

Unlike the other suite scenes, `author.js` imports textures from a local
asset server: serve the ORIGINAL arena `src/` directory (with its
generated PNG/KTX2 files) on **:9095** (`npx http-server --cors -p 9095
-c-1`). The generated textures are not committed here — only
`src/gen-assets.py`, which regenerates them deterministically
(`python3 gen-assets.py` in that directory). Then the standard suite
flow: replay `author.js` against the mcp-dev editor, `save_project` →
`project/`, `export_player_bundle` → `bundle/`, `screenshot_scene` →
`golden.png`.

The committed `project/` and `bundle/` are self-contained (assets baked
in) — loading them needs no asset server.
