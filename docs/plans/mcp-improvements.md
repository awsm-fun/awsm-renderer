# awsm-scene MCP — remaining scope (5 deferred sub-scopes)

> **What this is.** The original `mcp-improvements.md` had 21 items, each of which
> got a landed + verified PRIMARY fix (PR #137). But 5 of them shipped only a
> *slice* and deferred real sub-scope. The original doc's scope banner was
> explicit — *"Every item must be implemented + verified. Nothing here is
> optional."* — so those 5 sub-scopes are **owed**, not optional. This doc is the
> spec for finishing them. (The full original doc, with every completed item's
> root-cause + commit, lives in git history on the `mcp-improvements` branch.)
>
> **Honesty rule for this round (the thing that went wrong last time):** an item
> counts as DONE only when its **FULL** scope below is implemented + verified — a
> partial "highest-value slice" is **NOT** done. If an item turns out genuinely
> too large to finish in this pass, **STOP and report it as blocked-with-reason
> for a human decision** — do *not* ship a slice and mark it DONE. No silent
> scope reduction; no "deferred-and-noted" escape this time.

---

## Design principle: a thin *generic* bridge, not a feature catalog

The acceptance lens for every item (from the editor's author): **the MCP's job is
to bridge the renderer's core, generic power to the agent's general knowledge —
not to ship narrow, high-level features.** The agent already knows how to build a
nebula sky, an fbm heightmap, a soft fire sprite; it lacks *generic access* to the
renderer primitives to express them. So expose the low-level power as composable
primitives (raw data upload ✓, programmable WGSL stages, machine-readable schemas,
server-side data handles) and resist baking in presets. Judge every fix this way:
✅ a generic primitive the agent composes · ❌ a hardwired feature/preset.

## Definition of done (per item)

1. **Full scope.** Implement everything in the item's "Still owed" section — not a
   subset. The "Already shipped" context is so you don't redo work; the "Still
   owed" is the deliverable.
2. **Tests.** Rust roundtrip/unit tests where the change is testable (editor /
   editor-protocol / renderer / meshgen all run native `cargo test`; note the
   feature flags in the code map). `task lint` clean (rustfmt + clippy `-D
   warnings`, all features, tests).
3. **Live verification.** Build, run `task mcp-dev`, drive the editor over MCP, and
   capture a **chrome-devtools screenshot proving the actual pixels** for anything
   that renders (§14, §16, §18 are visual — screenshot mandatory; §3, §10 are
   data — verify the data/handle round-trips correctly, no giant array crossing
   for §10).
4. **Commit per item** on the `mcp-improvements` branch (co-author trailer). Flip
   the tracker row to DONE + record the short hash. Update `docs/MCP.md` +
   relevant tool descriptions / contract docs.
5. **If blocked / too large:** set the row to `BLOCKED` with a one-line reason and
   STOP for a human call. Do not downgrade scope to make it "fit."

---

## §3 — full machine-readable per-variant schema for `set_kind` / NodeKind

**Already shipped** (`72839eb2`): `patch_kind` (RFC 7386 JSON merge-patch over a
node's `NodeKind`, `editor-protocol/src/merge_patch.rs`) + typed-tool JSON schemas
for the dedicated tools. `get_node_details` returns a node's serialized `NodeKind`
so the agent can read the current shape and patch a delta.

**Still owed.** There is **no machine-readable schema of the `NodeKind` variants
themselves** — the agent can read an *instance* (`get_node_details`) but cannot
discover the full field shape / enum options of a variant it hasn't seen an
instance of, so authoring a fresh kind via `patch_kind`/`set_kind` is still
partly guesswork. Expose the per-variant **JSONSchema** for the `NodeKind` tree
(and the kind-config sub-types: particle defs, material defs, mesh/modifier defs,
light/camera configs) so a tool/query returns the schema the agent validates
against. Generic primitive = "here is the exact shape of every kind," not a prose
doc.

**Code map / approach.**
- The blocker that made this "bounded-deferred": deriving `schemars::JsonSchema`
  cascades across ~100 types in `packages/crates/scene/src` (particle, material,
  mesh_def, modifier, environment, …). §4 already added
  `#[cfg_attr(feature = "schemars", derive(JsonSchema))]` to 5 particle enums in
  `scene/src/particle.rs` — **follow that exact pattern** across the `NodeKind`
  tree + its sub-types, behind the existing `schemars` feature (so non-schema
  builds pay nothing).
- Then expose it: add a `get_kind_schema { kind? }` MCP tool / `EditorQuery`
  (or extend an existing discovery query) that returns `schemars::schema_for!`
  output for `NodeKind` (and named sub-types). The MCP crate already depends on
  `schemars` (every `*Params` derives it).
- Watch for: types that don't cleanly derive (e.g. ones holding `AssetId`/glam
  types — add `#[schemars(with = "...")]` shims), and the `--all-features` lint.

**Verify.** `get_kind_schema` returns a valid JSONSchema enumerating every
`NodeKind` variant + its fields/enums; round-trip test that the schema for a
known variant lists its real fields; `task lint` with all features.

---

## §10 — reusable vertex-selection handle + read-path pagination

**Already shipped** (`db32251b`): fused `paint_where` / `transform_where`
(`{ node, predicate, … }`) that select-and-act in one call so the index array
never crosses the MCP boundary. Helpers in `editor/src/controller/state.rs`:
`select_vertices_by_predicate`, `node_editable_mesh`, `soft_transform_mesh`.

> ✅ **DONE (`135cf797`).** Added a session-scoped selection store
> (`VERTEX_SELECTIONS` thread_local in `state.rs`). `select_vertices_where`
> gained `store` → returns `{ id, count }` (indices kept server-side), plus
> `count_only` / `offset` / `limit`. The four index-taking commands +
> `get_vertex_data` gained `selection: Option<u32>` (additive, `serde(default)` —
> a handle wins over `indices`, dangling handle errors loudly); `get_vertex_data`
> also gained `offset`/`limit`. **Verified live:** a 1717-vert height-band stored
> as handle `1` (only `{id,count}` returned) drove `paint_vertex_colors` THEN
> `soft_transform_vertices` THEN a paged `get_vertex_data` (`selected:1717,
> returned:3`) — the read confirms the same verts are red AND lifted, and **no
> index array ever crossed the wire**. `count_only` returns just `{count}`.

**Still owed (two parts).**
1. **Reusable server-side selection handle.** `select_vertices_where` should be
   able to return `{ id, count }` (indices kept server-side), and the
   index-taking verbs — `paint_vertex_colors`, `soft_transform_vertices`,
   `set_vertex_positions`, `set_vertex_normals` — should accept `selection: <id>`
   referencing it. This is the generic primitive the fused verbs are a
   convenience over: it lets ONE selection drive *many* ops (paint R, then paint
   a different channel, then sculpt) without re-selecting or round-tripping
   indices.
2. **Count-only / pagination on big reads.** `select_vertices_where` (raw-index
   mode) and `get_vertex_data` should support `count`-only and `offset`/`limit`
   so an agent that genuinely needs the indices/data can page them instead of
   overflowing the tool-result token cap.

**Code map / approach.**
- Selection store: a thread_local `RefCell<HashMap<u32, (AssetId /*mesh*/,
  Vec<u32>)>>` in the editor (mirrors the `TEXTURE_KEYS` session-cache pattern in
  `engine/bridge/material.rs`) + a counter. A new `EditorQuery`/`EditorCommand`
  to create a handle from a predicate (reuse `select_vertices_by_predicate`).
- Make the 4 index-taking commands accept a selection: change `indices: Vec<u32>`
  → a `VertexSelection { Indices(Vec<u32>), Handle(u32) }` enum (or add a parallel
  `selection: Option<u32>`), resolve the handle → indices in the apply. This
  touches 4 `EditorCommand` variants' wire types + their apply arms + the 4 MCP
  tools + roundtrip tests in `editor-protocol/src/transport.rs`.
- Pagination: add `offset`/`limit`/`count_only` params to `select_vertices_where`
  + `get_vertex_data` (mcp tool + the query handlers in `state.rs`).

**Verify.** On a high-res terrain (subdivided plane, see §16-era recipe): a
height-band → `{ id, count }`; paint R via the handle; sculpt via the SAME handle;
confirm via `get_vertex_data` that both ops hit the same verts and **no index
array crossed the wire**. Pagination: a `count_only` select returns just the
count; an `offset/limit` page returns a bounded slice.

---

## §14 — true soft-gradient particle blend (transparent instancing)

**Already shipped** (`327b8159`): typed `texture` field on `set_particle_emitter`
(binds a sprite) + the bridge now resolves `def.texture` → the particle PBR
material and **alpha-TESTs** (Mask, cutoff 0.5) so the sprite alpha *masks* each
particle into the sprite shape (hard-edged discs, not squares).
`engine/bridge/particles.rs::build_runtime`.

**Still owed.** True **soft-GRADIENT** edges (smooth alpha falloff, not a hard
alpha-test cutout) + a clean rim. This needs the emitter to route through the
**transparent-blend instancing** path when `def.blend` is set —
`enable_mesh_instancing` (async; builds the transparent pipeline) instead of
`enable_mesh_instancing_opaque`. The bridge's own header comment already flags
this as the follow-on.

**Code map / approach.**
- `meshes.rs::enable_mesh_instancing` (async) already routes a Blend material to
  the transparent pass (it checks `is_transparency_pass`); `enable_mesh_instancing_opaque`
  (sync, what `build_runtime` uses today) does NOT build the transparent pipeline.
- The constraint: `build_runtime` runs inside a **sync** `with_renderer_mut`
  closure (`engine/bridge/node_sync.rs::materialize_particle`). The async
  transparent instancing can't run there. Restructure: build the mesh + material
  in the sync part, then do `enable_mesh_instancing` in the async `materialize_particle`
  (a second renderer-lock acquisition), or make the particle-build path async
  end-to-end.
- Material: when `def.blend`, build a **Blend** PBR (`MaterialAlphaMode::Blend`)
  with `base_color_tex = sprite` so the sprite's per-texel alpha drives the
  standard (src.a, 1-src.a) blend → soft edges. Keep emissive for the glow. The
  per-instance color (`InstanceAttr`) fades alpha over life smoothly (no Mask pop).

**Verify.** A soft radial-alpha sprite (author via `create_texture`, PNG-encoded
is robust) + `blend: true` renders **soft-edged, smoothly-fading** particles —
NOT hard discs and NOT squares (chrome-devtools screenshot). Compare against the
Mask path (still available when `blend` is false).

---

## §16 — agent-authored displacement data (the generic "supply your own heightfield")

**Already shipped** (`9b307e8c`): `noise()` (+ `min max pow mod atan2 step clamp
fract exp log` and multi-arg calls) in the CPU `displace` expr evaluator
(`meshgen/src/expr.rs`, `authoring` feature) — the agent composes fbm / ridged /
domain-warp from `noise()` + arithmetic, evaluated at mesh-bake time.

**Still owed.** A way for the agent to supply displacement from **arbitrary data
it authored**, not just a formula — the original doc's stated generic fix. Two
legitimate generic primitives (either fully satisfies the design principle;
implement at least the first, which is the tractable + highest-leverage one):
1. **Displace-from-texture** (do this): a `displace`-family modifier that samples
   an agent-authored **heightmap texture** (from `★ create_texture` — already
   shipped) and offsets each vertex along its normal by the sampled value (with a
   `strength`, and UV or planar/triplanar mapping). This lets the agent bake ANY
   heightfield (eroded terrain, a logo, scanned data) externally and feed it —
   the fully-generic data hook, reusing the raw-upload primitive.
2. **Programmable WGSL displacement stage** (stretch / report if too large): the
   same hook custom *materials* have, but for vertices — the agent writes WGSL
   that displaces verts on the GPU (loops, texture sampling, multi-octave noise).
   This is a large new geometry-stage feature; if it proves out-of-scope for one
   pass after (1) lands, mark it BLOCKED with that reason rather than
   half-shipping.

**Code map / approach (for (1)).**
- The modifier enum + eval live in `meshgen/src/modifiers.rs` (`Modifier::Displace
  { expr } => displace(...)`) and the schema variant is in
  `editor-protocol`/`scene` (`Modifier::Displace`). Add a `DisplaceTexture`
  variant (or extend `Displace`) carrying the heightmap `AssetId` + `strength` +
  mapping. The editor resolves the texture asset → pixel data (the §10/§14 texture
  resolution + `texture_key_for` seam, or the raw decoded bytes the editor holds
  for create_texture'd assets) and samples it per vertex along the normal in the
  modifier eval pass (editor-side, since it needs the asset bytes — the meshgen
  `displace` is pure CPU).
- Per-vertex: sample heightmap at the vertex UV (or a planar projection), `pos +=
  normal * (height - 0.5) * strength`. Recompute normals after (the stack already
  does for `roughen`/`displace`).

**Verify.** A plane (subdivided) + a `DisplaceTexture` modifier referencing an
agent-authored heightmap (e.g. a gradient/fbm PNG via `create_texture`) visibly
deforms to match the heightmap (`get_mesh_stats` bbox.y grows + chrome-devtools
screenshot showing the relief matches the image).

---

## §18 — agent-authored panorama environment (equirect / cubemap)

**Already shipped** (`19b80c21`): `set_environment { zenith, nadir }` — a two-color
sky-gradient that drives both skybox + IBL, reusing the built-in's
`CubemapImage::new_sky_gradient` generator via `SkyboxConfig::SkyGradient` /
`IblConfig::SkyGradient` (`scene/src/environment.rs`, `engine/bridge/env_sync.rs`).

**Still owed.** Let the agent supply a **full arbitrary environment image** it
authored — an **equirect 2D texture** (via `create_texture`) and/or **6 cubemap
face textures** — turned into the skybox + IBL (so a custom panorama lights +
reflects the scene), not just a 2-color gradient.

**Code map / approach.**
- Renderer already has the cubemap-face update + IBL bake primitives:
  `environment.rs::update_skybox_all_faces` / `regenerate_skybox_mipmaps`,
  `lights.rs::update_ibl_prefiltered_env_all_faces` /
  `regenerate_ibl_prefiltered_env_mipmaps` / `update_ibl_irradiance_all_faces`,
  and `CubemapImage` (incl. `new_colors`). The KTX path (`apply_ibl` /
  `apply_skybox` in `env_sync.rs`) shows how a `CubemapImage` reaches the GPU.
- Equirect → cubemap: project the equirect 2D image to 6 faces (sample the
  equirect by each face direction's lat/long) → a `CubemapImage` → set as skybox +
  prefiltered-env, `regenerate_*_mipmaps` for the specular roughness mips.
  Irradiance: a cosine-convolution of the env into the 32² irradiance cubemap (a
  bake pass — check whether a runtime convolution exists or needs adding; a
  low-cost approximation is acceptable if it visibly lights consistently, but say
  so in the commit).
- Surface it: extend `set_environment` to accept an equirect texture `AssetId`
  (and/or 6 face asset ids) → new `SkyboxConfig`/`IblConfig` variants → `env_sync`
  builds the cubemap + IBL.

**Verify.** An agent-authored equirect panorama (any image via `create_texture`)
becomes the skybox AND a metallic/low-roughness sphere reflects + is lit by it
(distinct from the built-in) — chrome-devtools before/after screenshots.

---

## Progress tracker

| # | Item | Status | Commit |
|---|------|--------|--------|
| 3 | Full per-variant JSONSchema for NodeKind / kind-config types | TODO | |
| 10 | Reusable vertex-selection handle + read-path pagination | DONE | `135cf797` |
| 14 | True soft-gradient particle blend (transparent instancing) | TODO | |
| 16 | Agent displacement data: displace-from-texture (+ WGSL stage stretch) | TODO | |
| 18 | Agent panorama environment: equirect / cubemap → skybox + IBL | TODO | |
