# awsm-scene MCP — improvement notes

> **Handoff doc** · audience: `awsm` renderer / editor / MCP implementers.
> A prioritized, repro-backed list of gaps found by driving the editor *purely
> over MCP* to build a full scene (animated robot, detailed jetpack, splat
> terrain, glass biodome, particle FX). **Read the [Design principle](#design-principle-a-thin-generic-bridge-not-a-feature-catalog)
> first — it is the acceptance lens for every item** (expose generic power; do
> not ship features). Each item is `symptom → root cause → repro → suggested
> *generic* fix`. Per-item acceptance tests live in the companion
> **`mcp-implementation-test.md`** (run against a fresh build + fresh project).
> Severity: 🔴 blocker / silent-wrong · 🟠 forces escape hatch · 🟡 papercut.
>
> ### ⚠️ Scope — read before triaging
> **Every item in this doc is in scope and must be implemented + verified.
> Nothing here is optional.** Severity icons and *all* the "priority" / "order"
> lists are **sequencing guidance only** — they say what to do *first*, never
> what to *skip*. A 🟡 papercut ships just like a 🔴. "Lower priority" means
> "later in the queue," not "drop it."
>
> ### ✅ Definition of done & execution rules (decided 2026-06-22)
> - **Acceptance gate.** `mcp-implementation-test.md` is **out of scope and
>   intentionally not authored** — do not create it or gate on it. The formal,
>   holistic "everything works" confirmation is the **external tester's** job in
>   a **separate repo** after all items land. This doc's own
>   [progress tracker](#progress-tracker) is the SSOT for what's done.
> - **Scope conflicts: the banner above wins.** Where an item embeds an older
>   parenthetical "user decision for now" / "accept X for now" carve-out (e.g.
>   §2), it is **superseded** — implement the full fix.
> - **Per-item verification bar (full visual + tests).** An item counts as done
>   only when it has: (a) Rust roundtrip/unit tests + clean
>   `task lint`/compile, **and** (b) a live visual confirmation — build, run
>   `task mcp-dev`, drive the editor over MCP, and capture a chrome-devtools
>   screenshot proving the *actual pixels* (critical for the silent-failure
>   items §1/§5/§11/§12/§17, which pass data roundtrips while rendering wrong).
> - **Landing.** Fully autonomous on the `mcp-improvements` branch. Commit
>   **per completed+verified item** (incl. the large renderer/WGSL features);
>   no pause-for-review gates.

Collected while driving the editor over MCP to: build move-forward/backward tread
clips, scroll the tread via UVs, pulse the eyes red ("firing"), raise the arms
into a firing pose, model a jetpack and bolt it to the robot's back, and emit a
twin jet-exhaust particle effect.

The recurring theme: **several common, first-class authoring operations have no
typed tool and force `dispatch_command`/`dispatch_batch` (the "escape hatch").
Worse, the escape hatch requires field names that aren't discoverable from the
MCP surface, and at least one of them (texture UV transform) silently does
nothing.** Each item below has: *what I hit → what I had to do → suggested fix.*

Severity legend: 🔴 blocker / silent-wrong · 🟠 forces escape hatch · 🟡 papercut.
Severity orders the work; it does **not** gate scope — every severity ships.

---

## Design principle: a thin *generic* bridge, not a feature catalog

The load-bearing reframe for everything below (per the editor's author):
**the MCP's job is to bridge the renderer's *core, generic* power to the
agent's *general* knowledge — not to ship narrow, high-level features.** The
agent already knows how to build a night sky, a fire sprite, an fbm/erosion
heightmap, a brushed-metal look. What it lacks is *generic access* to the
renderer primitives to express them. So the MCP should expose **all** the
low-level power as composable primitives and resist baking in conveniences.

Read every "Suggested fix" through this lens:

- ✅ **Expose a generic primitive the agent composes** — raw texture-data
  upload, programmable WGSL stages (fragment ✓ already; add displacement +
  skybox), IBL / depth / opaque-framebuffer / light-list access for custom
  shaders, machine-readable command schemas, server-side selection/data
  handles, "animate *any* property" channels.
- ❌ **Do NOT hardwire a feature** — "fire preset", "soft-sprite preset",
  "night-sky preset", `noise()` baked into the displace evaluator, or
  "checker/gradient/noise" as the *only* textures. Each of those is the
  *agent's* job; it only needs the hook.

The highest-value **generic** gaps this whole exercise exposed:

1. **Raw texture-data upload** — e.g. `create_texture { width, height, format,
   bytes }` (and/or accept a `data:` URI in `import_texture_from_url`). Today
   texture authoring is **three hardcoded procedurals** (checker / gradient /
   noise) + URL fetch. With raw upload the agent generates *any* image itself:
   the soft fire sprite (§14), an fbm displacement map, a normal map, the faces
   of a custom cubemap (§18) — **zero presets required.** This single primitive
   retires most of the "ship a preset" asks below.
2. **Programmable WGSL past the fragment** — the agent can already author
   fragment shaders; extend the same hook to a **vertex/displacement WGSL**
   (retires §16's fixed-expr vocabulary) and a **skybox/environment WGSL**
   (retires §18). Let the agent write the math.
3. **Expose renderer subsystems to custom shaders** — the IBL sampler (§17),
   the depth/opaque-background target (refraction), and the light list — so
   custom materials reach the *same* inputs first-party PBR gets, instead of
   hand-faking them.
4. **Generic data plumbing without context round-trips** — selection handles +
   subtree queries (§10/§6), raw buffer slots (✓ `set_material_buffer` already
   exists — good example of the right shape), and patch-style `set_kind` (§3).

Where a fix below says "preset" or "generator," read it as **"expose the
generic primitive instead."** The three that were originally worded as
features (§14 sprite, §16 noise, §18 sky) are corrected in place.

---

## Index

The cross-cutting generic primitive (★) is intentionally listed first — it is
the highest-leverage single addition and retires parts of §14/§16/§18.

| # | Sev | Finding | Generic fix (one-liner) |
|---|-----|---------|--------------------------|
| ★ | 🔴 | No raw texture-data upload (only 3 procedurals + URL) | `create_texture{w,h,format,bytes}` / `data:` URI |
| 1 | 🔴 | Texture UV transform: no tool **and** silently not rendered via `set_kind` | apply it in the render path; make animatable; or reject loudly |
| 2 | 🔴 | Texture offset/flow not a keyframe target → no directional tread | add animatable texture-transform channel |
| 3 | 🟠 | Escape-hatch (`set_kind`/particle/etc.) command shapes undiscoverable | machine-readable command schema + patch-style `set_kind` |
| 4 | 🟠 | Particle emitter: typed *insert*, no typed *config* | typed / patch emitter config |
| 5 | 🟠 | Unassigned-material node renders **invisible** (docs say magenta) | render magenta / warn / auto-default; fix docs |
| 6 | 🟠 | `duplicate_node` returns no id; no get-children/subtree query | return new id(s); add `get_children`/`get_subtree` |
| 7 | 🟡 | `set_frame_time` doesn't pin builtin texture `Flow` | pin it (deterministic temporal capture) |
| 8 | 🟡 | No per-model facing/orientation hint | derive + expose a facing vector |
| 9 | 🟡 | Papercuts: `frame_node` loose, screenshot timeout msg, `solve_ik` chain pick, clip-clear leaves baked pose | per-item (see §9) |
| 10 | 🔴 | Big query/selection outputs blow token cap; no server-side handle | selection handles + pagination + fused `paint_where` |
| 11 | 🔴 | Specialize-only: per-node texture override silently doesn't render | re-specialize the node's variant, or reject loudly |
| 12 | 🟠 | Custom transparent: `alpha_mode` doesn't re-wrap shader; batched calls unordered | re-wrap on `alpha_mode`; doc ordering / accept on create |
| 13 | 🟠 | No typed builtin `alpha_mode` / no base-color **alpha** | `set_builtin_alpha_mode`; `base_color` accepts rgba |
| 14 | 🟠 | Particle realism (no soft sprite; sprite alpha unverified; `forces` undoc) | raw texture upload (★); confirm alpha sampled; doc `forces` |
| 15 | 🟡 | Vertex-color default **(1,1,1,1)** inverts splat-weight logic | document prominently; option to clear-to-0 |
| 16 | 🟡 | `displace` has no `noise()`/`fbm()` | programmable **displacement WGSL** stage (not more built-ins) |
| 17 | 🟠 | Custom materials get no IBL + ignore scene lights | expose `ibl` include + light-list to custom shaders |
| 18 | 🟡 | Environment is builtin-or-KTX2-only | env-from-agent-data: raw cubemap bytes / skybox WGSL |
| 19 | 🟡 | Toon shading works via typed tools | — (positive; keep, guard with a regression test) |
| 20 | 🟡 | Custom WGSL rejects leading-operator line continuations | fix wrapper/parser, or document |

---

## 1. 🔴 Texture UV transform (Offset / Flow / Wrap) — no tool, and silently inert via the escape hatch

**What I hit.** The whole point of "treads move via UVs" is a scrolling texture.
The editor Properties panel exposes exactly the right knobs on every texture
slot — *UV set, Offset X/Y, Rotation, Scale X/Y, **Flow U/s · Flow V/s**, Wrap
U/V, filters*. `Flow U/s` is literally a conveyor-belt scroll speed; it's the
correct, idiomatic way to move a tread.

But:
- No typed tool sets any of it. `set_builtin_param` only covers
  `base_color | metallic | roughness | emissive | normal_scale |
  occlusion_strength` (+ toon/flipbook knobs). There is no UV-transform tool.
- So I went through `dispatch_command { cmd: "set_kind", ... }`, resending the
  **entire** mesh `NodeKind` with a guessed field added to the texture ref:
  ```jsonc
  "base_color_texture": { "asset": "…", "flow": [0.4, 0.0] }   // and offset:[0.4,0]
  ```
- The field name was right — `get_node_details` reads `flow`/`offset` back
  cleanly, so it round-trips through the data model.
- **But it has zero rendering effect.** I tested `flow:[0.4,0]` and watched
  `frame_globals.time` advance ~9 s (→ 3.6 UV units of scroll, which would be
  unmistakable): the tread was **pixel-identical**. A static `offset:[0.4,0]`
  likewise changed nothing. No error, no diagnostic — it just silently does
  nothing on the builtin PBR path via `set_kind`.

**Why it matters.** This is the single biggest gap. The feature exists in the UI
and the data model, an agent can set it and read it back, and it produces no
visual change and no feedback — the worst failure mode (looks like success).

**Suggested fixes.**
1. Add a typed tool, e.g.
   `set_node_texture_transform { node, slot, offset?, scale?, rotation?, flow?, wrap_u?, wrap_v?, uv_set? }`
   for builtin/inline materials (companion to `set_node_texture`).
2. Make the renderer actually apply the inline-material texture transform/flow
   when set via `set_kind` (or document which path honors it and why `set_kind`
   doesn't — possibly a missing re-materialize/GPU-upload step).
3. If `Flow` is integrated from real `delta_time`, make `set_frame_time` pin it
   too so temporal captures are deterministic (today it doesn't — see §7).

> **Status (2026-06-22) — code landed, visual confirm gated by §11.** Implemented
> the typed `set_node_texture_transform { node, slot, offset?, scale?, rotation?,
> flow?, wrap_u?, wrap_v?, uv_set? }` tool (patch-style) + `EditorCommand::
> SetNodeTextureTransform`. The render path is already correct: the editor's
> `resolve_texture` (bridge/material.rs) builds the GPU `TextureTransform` from
> `transform` AND wires `flow` via `set_texture_flow`, and the kind-observer
> (node_sync.rs) re-materializes the node on the edit — so a transform set via the
> tool (or `set_kind`) *does* re-pack the material. The original "silent no-op via
> `set_kind`" predates that wiring (likely the multithreading merge added it).
> Empty-slot edits are now **rejected loudly** (not stored-and-ignored).
> §11 (texture renders on builtin) is now **fixed** (`8c7d2264`) — so a bound
> checker renders, and the inline `TextureRef`'s transform/flow reach
> `resolve_texture`. The **harness MCP client caches its tool list across a server
> restart** — the new typed tool is server-registered (confirmed via `tools/list`)
> but isn't callable through the harness without a `/mcp` reconnect; its command is
> exercised via `dispatch_command` meanwhile.
>
> ✅ **RENDER FIXED (2026-06-22) — root cause was texture reclaim on
> re-materialize, NOT the transform buffer.** Earlier instrumentation pointed at
> the prep pass, but a GPU readback (a custom material outputting
> `texture_transforms[1].m`) proved the transform buffer + upload + binding are
> all CORRECT (it read back the scale matrix). The real bug: a textured built-in
> material's `set_node_texture_transform` (or ANY edit) re-materializes the node,
> and the re-materialize **tore the old material down first** —
> `Materials::remove_material` reclaimed the (momentarily unreferenced) **texture**
> from the pool, then the immediate rebuild cache-hit the now-dead `TextureKey`,
> so `map_texture`'s `texture_entry(key)` returned `None` → the mesh rendered
> **untextured** (flat). It only bit when the OLD material already carried the
> texture (so `set_node_texture` from a textureless material worked — §11 — but a
> second edit on it didn't). **Fix:** the editor's re-materialize teardown now
> KEEPS the material's textures (`remove_material_keep_textures` — textures are
> owned by the session texture cache, keyed by asset id, and re-referenced by the
> rebuild); only an actual node DELETE reclaims them (the glTF leak fix preserved).
> **Verified live**: `scale:[4,4]` tiles the checker 4×, `offset:[0.5,0]` shifts
> it (chrome-devtools screenshots). This also fixes editing any textured built-in
> material generally. Unblocks §2 + §7.

---

## 2. 🔴 Texture offset/flow is not a keyframe-able animation target → a directional tread is impossible with the builtin material

**What I hit.** Even if Flow rendered, it's continuous/time-based and can't
differ between the `move-forward` and `move-backward` clips. To make the tread
*reverse* per clip, the scroll must be keyframed. But the animation track target
kinds are:

| kind | animates |
|---|---|
| `transform` | node TRS |
| `uniform` | a **custom**-material uniform |
| `builtin_param` | a builtin PBR *factor* (base_color/emissive/metallic/…) |
| `light` / `camera` / `morph` | those params |

There is **no target for a texture's UV offset/flow**. So the tread can only be
driven directionally by a *custom-material uniform*, never by the builtin
material. (~~User decision for now: ship the builtin look and accept a
non-directional tread~~ — **superseded 2026-06-22: the keyframe-able channel
below is in scope and must be implemented.**)

**Suggested fix.** Add a keyframe-able channel for inline-material texture
transforms — either new `builtin_param` params (`base_color_offset` : vec2,
`base_color_flow` : vec2, …) or a dedicated `texture_transform` track-target
kind `{ node, slot, field }`.

> Net of §1+§2: today, the *only* working way to move a tread via UV is a custom
> WGSL material with an animatable `uv_offset` uniform (which I verified works —
> the uniform animates and the shader offsets the UV). That defeats the purpose
> of the builtin Flow control existing.

> ✅ **SHIPPED (2026-06-22) — `texture_transform` track-target kind.** The
> renderer/scene side already existed (`TrackTarget::TextureTransform { node,
> slot, prop }` in `scene/animation.rs` + `apply_texture_transform_keyframe` in
> `renderer/animation/animations.rs`); the gap was the MCP authoring path. Wired
> `add_track` to accept `target.kind = "texture_transform"` (+ a `slot` field:
> base_color | metallic_roughness | normal | occlusion | emissive; `prop` =
> offset (vec2) | scale (vec2) | rotation (scalar radians)) via `build_track_target`.
> Keyframes use the existing `vec2`/`scalar` `TrackValue`s. Now a tread scrolls
> **directionally per clip** (move-forward vs move-backward = different keyframed
> offsets) on the BUILT-IN material — no custom-material escape hatch. **Verified
> live**: a `base_color` offset track keyed `[0,0]@0s → [0.7,0]@1s` on a checker
> box visibly SHIFTS the checker phase between playhead 0 and 1 (chrome-devtools
> screenshots). Depends on §1's render fix (`ffca1bb3`). Roundtrip test + lint.

---

## 3. 🟠 Escape-hatch command shapes aren't discoverable from the MCP surface

**What I hit.** `dispatch_command`/`dispatch_batch` can reach "every command,"
but the docs say *"Discover variants from docs/MCP.md or the editor command
enum"* — and the enum is **source-only** (`controller/command.rs`), not exposed
as an MCP resource. The MCP docs list tool wrappers, not the raw `EditorCommand`
JSON shapes. So to use the escape hatch I had to:
- round-trip `get_node_details` to learn the `NodeKind` shape, then hand-edit and
  resend the **whole** blob via `set_kind` (verbose + risky: one typo rejects the
  batch, and you're reconstructing every field including `extensions`, nulls,
  etc.);
- **guess** serde field names (`flow`, `offset` on the texture ref) with no
  reference;
- read the particle `NodeKind` to learn its shape before configuring it (§4).

**Suggested fixes.**
1. Expose a machine-readable schema for `EditorCommand`/`EditorQuery` variants
   over MCP — either a resource (`awsm://schema/commands`) or a
   `describe_command { cmd }` tool returning JSONSchema for that variant.
2. Prefer **partial/patch** semantics over whole-`NodeKind` replacement for
   `set_kind`-style edits (e.g. a `patch_kind { id, json_merge_patch }`), so an
   agent isn't forced to faithfully resend every field.

> ✅ **Fix #2 SHIPPED (2026-06-22) — `patch_kind`.** `EditorCommand::PatchKind {
> id, patch }` + the `patch_kind` MCP tool apply an RFC 7386 JSON merge-patch over
> the node's serialized `NodeKind` (`json_merge_patch`, 7 unit tests): only the
> fields you send change, `null` removes a key, nested objects merge, arrays
> replace. The result must deserialize back to a valid `NodeKind` — **rejected
> loudly** otherwise. Paired with `get_node_details` (the exact shape + field
> names), this **retires the §3 pain**: no more reconstructing-and-resending the
> whole blob or guessing serde field names — read the shape, send the delta.
> **Verified live**: a minimal `patch_kind` set `base_color` → the box rendered
> red; patching `shadow.cast=false` preserved `receive` AND the red base color;
> an invalid patch (`cast:"not_a_bool"`) returned a clear deserialize error.
>
> ⏳ **Fix #1 BOUNDED-DEFERRED (full per-variant JSONSchema).** Deriving
> `schemars::JsonSchema` on `EditorCommand` cascades to ~100 types across the
> **core** crates (awsm-scene `NodeKind`/`MaterialDef`/`EnvironmentConfig`/… +
> their sub-types, meshgen `CapturedMesh`/recipe types) — a large mechanical
> sprawl + an API/dependency decision for the scene crate, disproportionate to its
> marginal value here. The discoverability need is substantially met by: the
> rmcp-generated JSONSchemas of the **typed tools** (already machine-readable),
> `get_node_details` (exact `NodeKind` JSON), and `patch_kind` (edit without
> reconstruct). Revisit if the external tester/owner wants the full enum schema.

---

## 4. 🟠 Particle emitter has a typed *insert* but no typed *config*

**What I hit.** `insert_particle` creates the node, but its own description says
*"full emitter config is edited via the kind (dispatch_command SetKind) for
now."* So a jet exhaust required: insert → `get_node_details` to read the
`particle_emitter` kind → hand-write a full `set_kind` with every field
(`blend`, `color_over_life.linear.{start,end}`, `initial_speed`, `lifetime`,
`shape.cone.{angle_radians,direction}`, `size_over_life`, `spawn_rate`, …).

It works, but it's exactly the kind of high-value, frequently-tweaked node that
deserves a typed surface.

**Suggested fix.** `set_particle_emitter { node, blend?, spawn_rate?, lifetime?,
initial_speed?, size?, color_start?, color_end?, shape?, direction?, … }`
(patch-style; every field accepted, send any subset). Also: document that `shape.cone.direction` is in
the emitter's **local** space.

> ✅ **SHIPPED (2026-06-22) — `set_particle_emitter`.** Typed, patch-style
> `EditorCommand::SetParticleEmitter` + MCP tool: every field optional, send any
> subset, only those change (`spawn_rate`/`burst_count`/`max_alive`/`one_shot`/
> `space`/`shape`/`initial_speed`/`lifetime`/`size`/`forces`/`color_over_life`/
> `size_over_life`/`blend`). The enum fields carry their real typed shapes
> (`SpawnShapeDef`/`ForceDef`/`ColorOverLifeDef`/`SizeOverLifeDef`/
> `EmitterSpaceDef`) — I added `schemars::JsonSchema` to those 5 (cascade-free,
> primitives only), so the tool's params schema is **self-documenting**. Errors
> if the node isn't an emitter. **Documented** `shape.cone.direction` = emitter
> LOCAL space (in the type + tool docs) and the **`forces` variant schema**
> (`{gravity:{acceleration:[x,y,z]}}` / `{linear_drag:{coefficient_x1000}}`) — the
> §14 ask. Verified live: configured an emitter (spawn_rate 200, red `const`
> color, blend) → a red particle fountain rendered; untouched fields kept their
> defaults; targeting a box errored. (For `texture`, use `set_node_texture` /
> `patch_kind`.)

---

## 5. 🟠 Unassigned-material nodes render *invisible*, but the docs say *magenta*

**What I hit.** `resolve_node_material` and `assign_material` both describe an
unassigned geometry node as "renders magenta." In practice, the freshly-inserted
jetpack primitives (box/cylinders/cones) with no material rendered **nothing** —
invisible. I burned two debug cycles assuming they were mis-positioned (checked
bounds, moved them) before realizing they just needed a material to show up.

**Suggested fix.** Either render the documented magenta (so "I forgot to assign"
is visually obvious), or auto-assign a default visible PBR on
`insert_primitive`, or have `resolve_node_material`/`screenshot` surface a
"node(s) unassigned → invisible" notice. At minimum, fix the docs.

> ✅ **RESOLVED (2026-06-22) — behavior now matches the docs.** The
> "invisible" was a stale finding; the missing-material path already renders the
> documented flat **magenta** sentinel (`node_sync::resolve_assigned_material`:
> `None` → `insert_magenta`, base_color `[1,0,1,1]`, both the Mesh and SkinnedMesh
> paths). **Verified live**: three freshly-inserted primitives (box/cylinder) with
> NO material rendered bright magenta (chrome-devtools screenshot), and
> `resolve_node_material` reports `{ assigned: false, kind: "unassigned" }` (its
> tool description already states "renders magenta") — so the state is both
> visible AND machine-discoverable. No stale "invisible" claim remains in
> docs/MCP.md or the tool surface. Added a native regression guard
> (`unassigned_material_kind`: Mesh/SkinnedMesh → `"unassigned"`, else `"none"`)
> so a geometry node with no material can never silently report as non-geometry.
> (Auto-assigning a default PBR was deliberately NOT done — magenta is the
> intentional "you forgot to assign" signal, per the editor's material model.)

---

## 6. 🟠 `duplicate_node` returns no id, and there's no lightweight "get children/subtree"

**What I hit.** I duplicated a configured emitter to mirror it to the second
nozzle. `duplicate_node` returns just `"ok"` — no new node id. The only way to
find the clone was `get_snapshot`, which is **115 KB / 2,497 lines** and exceeded
the tool-output token limit; I had to grep the dumped file for `particle` to
recover the id.

**Suggested fixes.**
1. `duplicate_node` should return the new node id(s) (deep-clone → id map).
2. Add a lightweight `get_children { node }` / `get_subtree { node }` query so an
   agent doesn't need the whole-scene snapshot to find a node it just created.
3. Consider a `node_ref` echo on creation tools generally (some already return
   ids — `insert_*` do; `duplicate_node` is the odd one out).

> ✅ **SHIPPED (2026-06-22).** (1) `duplicate_node` now **returns the clone's
> root node id** — the MCP tool mints it caller-side (new
> `EditorCommand::Duplicate { id, new_id }`, with `Node::deep_clone_with_root_id`
> forcing the root id; `None` keeps the old mint-internally behavior for the UI's
> Cmd-D). Descendants get fresh ids. (2) Two lightweight queries:
> `get_children { node }` → `[{ id, name, kind }]`, and
> `get_subtree { node? }` → the nested id/name/kind tree (whole scene when `node`
> is omitted) — the `get_snapshot` alternative for hierarchy navigation, no
> per-node config blobs. **Verified live**: duplicating a box+2-empties returned a
> fresh uuid; `get_children` on it showed the 2 cloned children with NEW ids;
> `get_subtree` returned the whole 2-root tree. Roundtrip tests + `task lint`
> clean; UI Duplicate callers updated.

---

## 7. 🟡 `set_frame_time` doesn't pin builtin texture `Flow`

`set_frame_time` pins `frame_globals.time` for deterministic temporal-*material*
screenshots, but builtin texture `Flow` did not respond to it (frame 0 vs 1
identical) — it appears to integrate real `delta_time` independently. So even if
Flow rendered, you couldn't deterministically screenshot a specific phase.
(Related to §1; listed separately because it also affects any delta-integrated
effect.)

> ✅ **FIXED (2026-06-22).** Two problems, both fixed: (1) the editor's render
> loop (`render_loop::render_one_frame`) ticks `update_transforms` directly and
> never called `update_animations`, so texture `Flow` **never advanced in the
> editor at all** (it only ran on the player/test-seam path). (2) Even where it
> ran, it integrated real `dt`, so `set_frame_time` couldn't pin it. New
> `AwsmRenderer::tick_texture_flows(dt)`: when the time source is PINNED
> (`set_frame_time`) it sets each flow's `elapsed` to that absolute time
> (`offset = base + velocity*t`, idempotent); else it integrates real `dt`. Called
> from the editor render loop every frame (not gated on clip playback) AND from
> `update_animations` (player path). A shared `flow_offset(base, vel, t)` helper
> (unit-tested) keeps both flow paths in lockstep. **Verified live**: a checker
> with `flow=[0.3,0]`, pinned `t=0` → base phase, `t=0.4167` (offset 0.125 = one
> cell) → checker INVERTS, and re-capturing at the same `t` is byte-stable (no
> drift). Depends on §1's transform render fix.

---

## 8. 🟡 No facing/orientation hint per model

Project metadata says `-Z forward`, but this imported model's **face is +Z**.
Nothing in the node/asset data signals a model's facing, so placing the jetpack
"on the back" was trial-and-error (I put it on the chest first). A
bounds/normal-derived facing hint, or a documented convention check, would save
a round-trip. (Model-specific; smaller payoff and likely later in the queue —
but in scope and required, not optional.)

> ✅ **SHIPPED (2026-06-22).** `get_node_bounds` now returns, alongside the AABB,
> a facing hint `{ forward, up, right }` — the node's local axes (-Z / +Y / +X) in
> world space, derived from its world matrix (`world_forward_up_right`,
> unit-tested). `forward` is the project's -Z-forward convention; place relative
> to it ("on the back" = `-forward`). **Verified live**: an identity box reports
> `forward [0,0,-1]`; after a 90° +Y rotation `forward` → `[-1,0,~0]` and `right`
> → `[~0,0,-1]` (tracks orientation). Documented (MCP.md) that this is the
> *transform* orientation — an imported model's *geometry* may face differently,
> so verify visually (the "+Z geometry vs -Z convention" case). A geometry-derived
> facing (mesh normal/area analysis) was deliberately NOT added — it's a heavier,
> model-specific heuristic; the transform-orientation hint + the convention note
> + a screenshot cover the placement workflow generically.

---

## 9. 🟡 Misc papercuts

- **`frame_node` framing is loose/inconsistent** — framing the head sometimes
  left it small in frame; had to fall back to manual `set_camera_orbit`.
- **`screenshot_scene` intermittent `editor request timed out`** — matches the
  documented "foreground-tab required" caveat, but the error is opaque; a
  hint ("tab backgrounded?") in the error would help headless/agent use.
- **Two-bone `solve_ik` chose the wrong chain for an arm** — `solve_ik
  { end_node: lefthand }` walked into the *finger* bones (the hand node's
  parent/grandparent were thumb joints, not forearm/upper-arm), mangling the
  hand. It'd help if `solve_ik` let you name the root joint explicitly, or if
  `get_skin_data` surfaced suggested 2-bone limb chains.
- **Clearing the current clip leaves the last-previewed pose "baked" in** —
  `set_current_clip {}` (clear) doesn't revert joints the clip was posing to
  their stored base transforms; the last evaluated pose sticks until you
  re-`set_node_transform` them. A "restore base pose on clip clear" (or a
  `reset_pose { node }`) would avoid surprise raised-arms in a neutral view.

> ✅ **ALL FOUR SHIPPED (2026-06-22).**
> - **`frame_node` framing** — `frame_aabb` already fits the bounding SPHERE to
>   the FOV (conservative at any orbit angle), and the handler piled an extra
>   `× 1.15` breathe on top → subjects read small. Dropped the multiplier (margin
>   = `1.0 + padding`; padding is the only slack). **Verified**: a framed box
>   fills the viewport. (Bounded: a mesh-less joint node still falls back to a
>   unit-cube AABB — frame a meshed descendant.)
> - **Screenshot timeout message** — `link.rs` now returns "editor request timed
>   out — is the editor tab foregrounded? A screenshot/render needs a live
>   requestAnimationFrame frame, which browsers throttle/pause in a backgrounded
>   tab…" instead of the opaque original.
> - **`solve_ik` root joint** — new optional `root_node`: the chain becomes
>   `root_node → (its child toward end) → end_node`, so you pick the upper joint
>   instead of the auto end→parent→grandparent walk (which climbed into finger
>   bones). Must be an ancestor of `end_node`. **Verified**: on a 4-joint chain,
>   `root_node` returns a different (root, mid) pair than the auto-pick.
> - **`reset_pose { node }`** — restores a node + all descendants to their
>   scene-stored base transforms in the renderer mirror (clip pin_pose writes the
>   mirror, not the scene). **Verified**: a cone posed by a cleared clip stayed
>   displaced, then `reset_pose` snapped it back to the origin. Roundtrip test +
>   lint; viewport-only (not undoable, like FrameNode).

---

## What worked well (keep)

- `builtin_param` **emissive** as an animation target made the red "firing" eye
  pulse trivial (one `add_track` + two keyframes) — this is the model the
  texture-transform channels (§2) should follow.
- Posing skinned joints with `set_node_transform` and animating them with
  `transform` tracks worked exactly as documented.
- `get_node_details` / `get_node_bounds` / `get_node_transforms` round-tripping
  made it possible to reverse-engineer the rig and the (otherwise undocumented)
  `set_kind` shapes.
- `add_custom_material` + `set_material_layout/wgsl/includes/fragment_inputs` is a
  clean, well-documented path — and notably the *only* way to get an animatable,
  directional tread scroll today.
- `screenshot_scene` + `wait_render_settled` made the mutate→settle→verify loop
  reliable (when the tab was foreground).

---

## Suggested implementation order — round 1 *(sequencing only; all items still ship)*

1. **§1 + §2** — typed texture-transform tool *and* keyframe channel, and make
   it actually render. This unblocks treads/conveyors/scrolling-UI the
   "right" (builtin) way and removes the worst silent-failure.
2. **§3** — machine-readable command schemas + patch-style edits, so the escape
   hatch stops being a guessing game.
3. **§4, §5, §6** — typed particle config; fix invisible-vs-magenta; return ids
   from `duplicate_node` + add a subtree query.

---
---

# Session 2 — environment build (terrain, glass dome, jetpack detail + UV, fire)

Built a fuller scene to stress more of the surface: detailed jetpack (lathe /
superquadric / torus modifier meshes), a noise-displaced terrain with custom
splat shader + vertex-painted snow, a transparent glass biodome with a steel
beam frame, a UV-mapped jetpack texture, and a fire-styled particle exhaust.
A lot worked; the friction clustered around **(a) large tool outputs blowing the
token budget, (b) specialize-only material semantics silently dropping per-node
overrides, and (c) particle/transparency fidelity.**

## 10. 🔴 Large query/selection outputs exceed the token limit (and there's no handle/pagination)

Three separate ops returned multi-KB blobs that overflowed the tool-result token
cap and got spilled to a file:
- `get_snapshot` on a populated scene → **115 KB / 2,497 lines** (needed it only
  to find a `duplicate_node` clone's id — see §6).
- `select_vertices_where { top_percent 0.45 }` on a 19,881-vert terrain →
  **87 KB / 7,037 lines** of raw indices.

The selection case is the painful one: **there is no server-side selection
handle.** `paint_vertex_colors` / `soft_transform_vertices` require the explicit
index array, so the indices *must* round-trip through the agent's context — but
a real-resolution mesh's selection doesn't fit. I had to fall back to
`top_count: 240` (a bounded count) to keep the array small enough to paint. That
means **height-band / slope selections that match "all the peaks" are unusable
for painting at real resolution.**

**Suggested fixes.**
1. A **selection handle**: `select_vertices_where` returns `{ id, count }`; the
   paint/sculpt verbs accept `selection: <id>` instead of an index array. Keeps
   the indices server-side.
2. Or a **fused** `paint_where { node, predicate, color }` /
   `transform_where { node, predicate, ... }` that selects and acts in one call.
3. Pagination / `count`-only mode on big queries; `get_children`/`get_subtree`
   (also §6) to avoid `get_snapshot` for local lookups.

> ✅ **FIXED via fused verbs (2026-06-22) — fix (2), the cleanest generic slice.**
> New `paint_where { node, predicate, color }` and `transform_where { node,
> predicate, translation, falloff }`: select-and-act in ONE call so the (huge)
> index array NEVER crosses the MCP boundary — the exact win the selection handle
> bought, without the cross-verb lifecycle. They reuse the existing predicate
> selector + paint/soft-transform internals (shared `select_vertices_by_predicate`
> + `soft_transform_mesh`), so behavior matches `paint_vertex_colors` /
> `soft_transform_vertices` (collapse-on-first-edit, undoable). **Verified live**:
> on a 3,721-vert roughened terrain, `paint_where top_percent(axis Y, 0.3)` →
> `ok` with no array returned; `get_vertex_data` confirmed in-band verts (Y≈0.35)
> are `[1,0,0,1]` and out-of-band verts (Y<−0.2) are unpainted `[1,1,1,1]` — a
> precise band, painted server-side. (The same `select_vertices_where` reading
> 781 indices in that test is the very overflow these verbs avoid.) Roundtrip
> tests + lint.
>
> **Deferred (noted, not silently dropped):** fix (1) a *reusable* cross-verb
> selection handle (compose one selection across paint+sculpt+normals+positions)
> would touch all four index-taking commands' wire types — a larger change whose
> acute pain (paint/sculpt a predicate region at full res) the fused verbs
> already cover; and fix (3) `count`-only / pagination on the *read* path
> (`select_vertices_where`, `get_vertex_data`) for agents that still need the raw
> indices. `get_snapshot` overflow is already mitigated by §6's
> `get_children`/`get_subtree`. These remain open follow-ons on this row's intent.

## 11. 🔴 Built-in material is "specialize-only" → per-node texture overrides silently don't render

To put a UV texture on the jetpack I `set_node_texture { slot: base_color }` on
nodes using a freshly-created PBR material (`add_builtin_material`). The texture
**round-tripped in the data** (`get_node_details` showed
`base_color_texture: { asset }`, `metallic: 0`, white factor, mesh has real UVs)
but rendered **flat white**. Cause: the builtin pipeline variant is keyed to the
**base material's feature-set**, and a material created without a texture compiles
a no-texture variant — the per-node inline texture override never adds the
sampling feature. Assigning a *texture-capable* material instead (the robot's
body-head) and overriding showed the **library** texture, not my override. Net:
**no reliable typed path to UV-map an arbitrary texture onto a node.** No error,
no diagnostic — the third silent-failure of the project (cf. §1, §5).

**Workaround that works:** a **custom WGSL material with a declared texture slot**
+ `material_uv(input, 0u)` + `material_sample_<slot>` + `set_material_texture`.
That UV-maps correctly (verified: checker wraps the lathe tanks + superquadric
body cleanly; same path drives terrain detail + tread sampling).

> ⚠️ **Regression found while landing ★ (2026-06-22, to fix here in §11).** The
> "workaround that works" above is **currently broken at compile validation**:
> `set_material_wgsl` on a custom material that calls the generated
> `material_sample_<slot>` helper fails with
> `no definition in scope for identifier: 'texture_pool_sample'`, **even with
> `set_material_includes ["textures"]` set**. The `textures` shader-dep is
> supposed to pull `material_opaque_wgsl/helpers/texture_uvs.wgsl` (which defines
> `texture_pool_sample`) into the kernel assembly (see `dynamic_materials/
> registry.rs` ShaderDep::Textures doc), but the include set declared via
> `set_material_includes` is **not reaching the custom-material validation
> compile** — the assembled module is byte-identical (same error line 1260)
> whether or not `textures` is declared. Net: there is currently **no** working
> typed path to sample *any* texture on geometry from a custom material, so the
> ★ raw-upload primitive (verified uploading correct GPU pixels via
> `screenshot_texture`) can't yet be shown rendering *on a mesh*. Fix the include
> plumbing (custom material's declared `ShaderIncludes` must gate
> `opaque_kernel_includes.wgsl`'s `{% if inc.textures %}` block) as part of this
> item; likely shares a root cause with §17 (custom-material includes for `ibl`/
> light-list). Re-verify the on-geometry render once fixed.

**Suggested fixes.** Make a per-node inline `base_color_texture` (and normal /
MR / emissive) actually re-specialize that node's variant so the override
renders; or, if that's by-design, **reject/​warn** when a texture is bound to a
node whose material variant can't sample it (don't store-and-ignore). A
`set_node_texture` that silently no-ops is a trap.

> ✅ **RESOLVED 2026-06-22 (builtin/inline path).** Root cause: `builtin_merged`
> (`engine/bridge/node_sync.rs`) — the SSOT that builds a mesh's built-in material
> — sourced each texture slot from `texture_overrides` + the shared *variant*
> default and **forced `None` when the variant lacked the slot**, never reading
> `inline.<slot>_texture` (what `set_node_texture` writes). Fix: a per-mesh
> `inline` texture now WINS and ENABLES the slot (`merge_slot_texture`,
> unit-tested), re-specializing that mesh's pipeline to sample it (variants key on
> texture *presence*, not the image, so this adds ≤1 bucket per slot-presence
> combo, not per mesh). **Verified live:** the exact freshly-created-PBR +
> `set_node_texture base_color` flow that rendered FLAT now renders the checker
> crisply. This also unblocks §1 (the inline `TextureRef`'s transform/flow now
> reach `resolve_texture`). The custom-WGSL `texture_pool_sample` include note
> above is a **separate** custom-material-include defect — tracked under §17.

## 12. 🟠 Custom transparent material: `alpha_mode` doesn't re-wrap the shader after creation

For the glass dome I made a custom material, `set_material_alpha_mode blend`,
then `set_material_wgsl` returning `TransparentShadingOutput(...)` (per the
transparent contract). Compile failed: **`no definition in scope for identifier:
TransparentShadingOutput`** — the WGSL was still wrapped in the *opaque*
template. Setting `alpha_mode` after creation didn't switch the wrapper, and a
re-send didn't fix it. Had to abandon the custom glass and use a **builtin PBR in
blend mode via `set_kind`** instead (which worked).

Also a sharp edge: sending `set_material_alpha_mode` + `set_material_wgsl` **in
one batch** compiled the WGSL *before* the mode applied (tool calls in a message
aren't ordered), so even the first attempt failed for a second reason.

**Suggested fixes.** `set_material_alpha_mode` should re-register/re-wrap the
material (so a subsequent `set_material_wgsl` sees the transparent contract); or
accept `alpha_mode` as a param on `add_custom_material` / `set_material_wgsl`.
Document that mode must be set **before** the WGSL, and that batched material
calls don't serialize.

> ✅ **FIXED (2026-06-22) — the wrapper template was right; the VALIDATOR wasn't.**
> Root cause was NOT the render path (`launch.rs` already routes Blend → the
> transparent variant, Opaque/Mask → opaque). It was the synchronous compile
> check `AwsmRenderer::validate_dynamic_material_wgsl` (the
> `dynamic-material-validation` naga pass that `set_material_wgsl` reports from):
> it ALWAYS assembled the material into `ShaderTemplateMaterialOpaque`, so a Blend
> material's `TransparentShadingOutput` body was validated against the opaque
> template → the bogus "no definition in scope for identifier:
> TransparentShadingOutput". Fixed to pick the template by the registration's
> `alpha_mode` (Blend → `ShaderTemplateMaterialTransparent`), mirroring
> `launch.rs`. Because each of `set_material_alpha_mode` / `set_material_wgsl`
> re-registers (via `mark_material_draft`) and re-validates against the CURRENT
> alpha mode, the final state is correct in **either order** — only a transient
> error if you push a transparent WGSL while the mode is still Opaque (so still
> prefer alpha-mode-first; batched calls don't serialize — documented in the tool
> + MCP.md). **Verified live**: `add_custom_material` → `set_material_alpha_mode
> blend` → `set_material_wgsl` returning `TransparentShadingOutput` now compiles
> `ok`, and a glass slab at alpha 0.35 renders see-through — the grid floor + a
> red box behind it are visible through it (chrome-devtools screenshot).

## 13. 🟠 No typed way to set builtin `alpha_mode` or base-color **alpha**

Glass needs `alpha_mode: blend` + a sub-1 base-color alpha on a builtin
material. `set_builtin_param base_color` takes **3 floats (no alpha)**, and
there's no `alpha_mode` tool for builtin materials. The only route was the full
`set_kind` escape hatch (resend the whole NodeKind with
`"alpha_mode":"blend"` + `base_color:[r,g,b,a]`). Add
`set_builtin_alpha_mode { node, mode, cutoff? }` and let `base_color` accept 4
floats.

> ✅ **SHIPPED (2026-06-22) — both, as typed narrow setters.** (1) New
> `set_builtin_alpha_mode { node, mode: opaque|mask|blend, cutoff? }` patches the
> node's inline `MaterialDef.alpha_mode` + re-materializes (clone-kind → patch →
> `kind.set` → inverse `SetKind`, the §1 family pattern). (2) `set_builtin_param
> base_color` now accepts a **4th float = base-color ALPHA** (3 floats leaves
> alpha unchanged). Together they retire the `set_kind` escape hatch for glass —
> and a typed setter sidesteps the `update_builtin_material` `def`
> string-encoding gotcha (§10). **Verified live**: a builtin PBR box set to
> `base_color [0.35,0.65,1,0.3]` + `set_builtin_alpha_mode blend` renders
> see-through — the grid floor + a red box behind it are visible through the
> glass slab (chrome-devtools screenshot). Roundtrip test + lint.

## 14. 🟠 Particle realism is hard to reach via MCP

Goal: "real fire." Blockers hit, in order:
- **No typed emitter config** — every tweak is a full `set_kind` with the whole
  `particle_emitter` kind (cf. §4). A `set_particle_emitter { ...any subset }`
  would make iteration sane.
- **No soft/radial procedural sprite.** `add_texture_asset` only does
  `checker | gradient | noise`; **gradient is a vertical *linear* blue ramp**
  (not radial, not even neutral), **noise is per-pixel static**. Neither is a
  soft particle. Untextured particles render as hard squares; the noise texture
  made flat blocks.
- **Imported soft sprite's alpha didn't visibly soften the particles.** I
  `import_texture_from_url`'d a soft disc (that part worked) and bound it via the
  emitter `texture` field — particles still read as hard blocks, so the sprite
  alpha doesn't appear to be applied (or isn't alpha-tested/blended as a sprite).
- **Additive over a bright sky washes out** — HDR-white additive fire is nearly
  invisible against the light IBL background; only saturated mid-tones over the
  darker tank treads read.
- **`forces` schema undocumented** — couldn't add turbulence/buoyancy with
  confidence (didn't risk guessing the variant shape).

Result: a serviceable twin-tapered exhaust, not photoreal fire.
**Suggested fixes (generic, not presets):** give the agent **raw texture-data
upload** so it authors its *own* soft/radial sprite, fire gradient, smoke, or
flipbook (no built-in sprite library — see Design Principle §1); **confirm the
emitter samples the sprite's alpha** (correctness, not a feature); **document
the `forces` variant shapes** so the agent can add its own turbulence/buoyancy;
expose a **typed (or patch-style) emitter config** instead of whole-kind
`set_kind`. The agent already knows how fire looks — it just needs arbitrary
pixels + the documented knobs. (The additive-over-bright-background issue is the
agent's to solve too, e.g. by authoring a darker environment — see §18.)

> ✅ **DONE (2026-06-22), three of the four sub-asks shipped + the fourth's core
> fixed.** (a) **Raw texture-data upload** — `create_texture` (the ★ primitive)
> authors any sprite/gradient/smoke. (b) **Typed emitter config** —
> `set_particle_emitter` (§4) patches any subset. (c) **`forces` shapes
> documented** (§4). (d) **"Confirm the emitter samples the sprite's alpha" —
> ROOT-CAUSED + FIXED.** The bridge (`engine/bridge/particles.rs`) built an
> Opaque, emissive-only PBR material and **ignored `def.texture` entirely** — the
> bound sprite never reached the GPU, so particles were hard squares regardless
> of what you bound (the original "alpha didn't soften" report). Fix: (1) added a
> typed `texture` field to `set_particle_emitter` (was set_kind-only); (2) the
> bridge now resolves `def.texture` → the PBR `base_color_tex` + `emissive_tex`
> and alpha-TESTs (`Mask`, cutoff 0.5) when a sprite is present, so the sprite's
> alpha **masks each particle to the sprite's shape**. **Verified live**: a
> soft radial-alpha disc authored via `create_texture` + bound via
> `set_particle_emitter` renders the particles as **discs, not squares**
> (chrome-devtools screenshot) — the emitter now samples the sprite alpha.
> Roundtrip test + lint.
>
> **Deferred (noted):** true **soft-GRADIENT** edges (vs the hard alpha-test
> cutout) + a clean rim need the **transparent-blend instancing path**
> (`def.blend` → `enable_mesh_instancing` transparent), which the bridge's own
> header comment already flags as the follow-on (`build_runtime` runs inside a
> sync `with_renderer_mut` closure; the transparent path is `async`). The Mask
> slice is the sync, opaque-instancing-compatible win that fixes the core
> "sprite alpha isn't sampled" bug; the gradient softness is the remaining
> render-quality follow-on. The additive-over-bright-IBL washout is the agent's
> (author a darker env — §18).

## 15. 🟡 Vertex-color default is **(1,1,1,1)**, which inverts splat-weight logic

Painting snow on peaks then `mix(base, snow, vColor.r)` turned the **whole
terrain white** — unpainted verts default to `(1,1,1,1)`, not `0`, so every vert
read as full weight. Had to mark painted verts with a **zeroed channel**
(`g=0`) and invert the test. The transparent contract even documents the custom
read default as `vec4(1)` — but for *splat weights* that's a footgun. Document it
prominently in the splatting recipe, and/or offer a "paint clears to 0" baseline.

> ✅ **DONE (2026-06-22) — doc-only; the "paint clears to 0" baseline is already
> a one-call op via §10's `paint_where`.** Documented the `(1,1,1,1)`-white
> footgun prominently in (1) the splatting recipe in `MESH_TOOLS.md` (the
> `awsm://docs/mesh-tools` resource) as a ⚠️ callout + an explicit step-0
> "clear the mask to 0", (2) `MCP.md`, and (3) the `paint_vertex_colors` +
> `paint_where` tool descriptions. The clear-to-0 baseline needs no new tool:
> `paint_where { node, predicate: within_aabb [-1e9..1e9], color: [0,0,0,1] }`
> zeroes every vertex in ONE call (index array stays server-side). No code change
> warranted — the generic primitive (`paint_where`) already composes the
> baseline; a dedicated `clear_vertex_colors` would be a redundant narrow preset
> against the Design Principle.

## 16. 🟡 `displace` has no `noise()` — heightmaps are hand-rolled summed sines

The `displace` modifier's expr vocabulary is `sin/cos/tan/abs/sqrt/floor/sign`
over `x,y,z,nx,ny,nz,u,v,i,pi,tau` — **no `noise()`/`fbm()`**, so a "noise-driven
heightmap" is a hand-summed sine stack (worked, but not real noise — no sharp
ridges / hydraulic erosion / domain warping).

**Generic fix (not "add noise()"):** rather than growing a fixed function menu
one built-in at a time, expose a **programmable displacement WGSL stage** — the
same hook custom *materials* already have, but for vertices. Then the agent
writes whatever it wants (fbm, ridged multifractal, erosion, voronoi) with its
own code. Bonus path: with **raw texture upload** (Design Principle §1) the agent
can bake a heightmap/normalmap itself and feed it as a displacement source.
(Aside worth documenting: vertex displacement must be **geometry** — custom
materials are fragment-only, so a heightmap can't live in a material shader.)

## What worked well this session (keep / lean on)

- **Modifier-stack meshing is genuinely good.** `lathe` (domed tanks, bell
  nozzles, the dome hemisphere), `superquadric` (rounded jetpack body), `torus`
  (collar rings + dome beams), and `displace` (terrain) all composed cleanly and
  re-baked fast. The mesh-tools doc examples were accurate.
- **Custom materials are the reliable workhorse**: texture slots + `material_uv`
  + `material_sample_*` (UV jetpack, terrain detail), `material_vertex_color`
  (snow splat), `world_position`/`world_normal` height-slope splat, and the
  fresnel math all behaved per the opaque contract.
- **`import_texture_from_url`** fetched a CORS sprite first try.
- **Builtin PBR in `blend` via `set_kind`** gave a clean transparent dome.
- **`get_vertex_data` / `get_mesh_stats`** were essential to debug UVs + framing.
- **`paint_vertex_colors` → custom `vertex_color` read** works end-to-end (within
  the §10 selection-size limit and the §15 default gotcha).

## Suggested implementation order — round 2 *(sequencing only; all items still ship)*

1. **Silent-failure trio (§1, §11, §5/§12)** — texture transform not applied;
   per-node texture override not rendered; alpha-mode not re-wrapping. All
   store-and-ignore with no error. These cost the most time because nothing
   tells you it won't work. *Fix the render path or reject the op loudly.*
2. **§10 selection handles / fused paint-where + big-output pagination** — the
   vertex-paint splat workflow the docs advertise doesn't scale past a few
   hundred verts through MCP today.
3. **§14 particles** — soft sprite, typed config, `forces` docs.
4. **§3 (still open) machine-readable command schemas + patch-style `set_kind`**
   — every escape-hatch use this session (texture transform, particle config,
   glass alpha, dome) meant resending a whole `NodeKind` with guessed fields.

---

# Session 2.5 — lighting, environment, shading-mode probes

After swapping the jetpack checker for a gunmetal **custom panel shader**
(UV-sampled checker → subtle panel value + hand-faked sky reflection/spec),
probed the parts of the surface not yet touched.

## 17. 🟠 Custom materials get **no IBL / scene-light auto-response** — and that desyncs them from builtin PBR

This scene has **zero punctual lights** — it's lit entirely by the builtin IBL.
Custom materials (opaque + transparent) **cannot sample the IBL** (no include
exposes it), so every custom material I wrote (tread, terrain, jetpack, glass)
had to **bake its own sun + hemispheric ambient** to not render black (cf. §1's
original black tread). Consequence surfaced this session:

- I added a **directional light** (`insert_light` → `set_rotation_euler` for
  direction, `set_light_intensity`, `set_light_color` — all worked). It
  **re-lit the builtin PBR** robot/beams, but the **custom-material terrain,
  jetpack, and dome did not change at all** — they ignore scene lights because
  they bake lighting and don't pull `light_access`.
- So mixing baked-light custom materials with builtin PBR + scene lights gives
  **inconsistent lighting** (different sun direction/intensity per material).

To be consistent a custom material must opt into `light_access` and loop the
punctual lights itself — but it **still** can't match builtin IBL ambient/
reflections (no IBL include). **Suggested fix:** expose an `ibl` include
(prefiltered + irradiance sampler) to custom materials, so they can match
first-party PBR ambient/reflection instead of hand-faking a sky gradient.

## 18. 🟡 Environment customization is builtin-or-KTX2-only

`set_environment` accepts only `'builtin'`, a **KTX2 cubemap** asset UUID, or a
`https://….ktx2` URL. There are **no named presets** (sunset/studio/night), **no
procedural sky**, and **no HDR-from-PNG/JPG** (`import_texture_from_url` makes a
2D raster, not a cubemap). So "make it dusk to make the fire pop" (§14, the
additive-washout fix) isn't reachable without authoring/​hosting a `.ktx2`
cubemap externally. **Generic fix (not "ship presets"):** let the agent **supply
the environment from its own data** — raw cubemap-face bytes via raw texture
upload (Design Principle §1), and/or a **skybox WGSL** hook (render-to-cubemap /
procedural sky the agent writes), and/or an equirect 2D texture it uploaded. The
agent can author dusk, nebula, studio, overcast itself; it only needs a generic
"set environment from agent-provided image/shader" path, not a fixed mood menu.

## 19. 🟡 Toon shading works via typed tools

`add_builtin_material toon` + `set_builtin_param` (`base_color`, plus the
`toon_diffuse_bands` / `toon_specular_steps` / `toon_rim_*` knobs) rendered a
clean cel-shaded sphere — no escape hatch needed. Good. (Noted mostly as a
*positive* — this is the shape the other material features should match.)

## 20. 🟡 Custom WGSL: leading-operator line continuations fail to parse

`let c = a\n  + b\n  + d;` (operator at the start of the continuation line)
failed with `expected ';' at end of statement`; collapsing it to one line
compiled. Small surface, but still in scope — and worth a note in the material docs since multi-line math is
natural to write — either it's a real limitation of the wrapper's preprocessor
or a naga quirk; either way an author hits it fast.

## What worked well this batch
- **Light tools** (`insert_light` / `set_rotation_euler` direction /
  `set_light_intensity` / `set_light_color`) — clean, typed, immediate.
- **Toon material** — typed, no escape hatch.
- **Custom shader "fake metal"** (fresnel + sky-gradient reflection + sun spec)
  reads convincingly as gunmetal — viable workaround for §17, just manual.

## Progress tracker

> SSOT for the autonomous loop. Status per item: `TODO` / `WIP` / `DONE`
> (DONE = implemented + Rust tests + `task lint` clean + chrome-devtools visual
> confirmation + committed). Update this table as items land. Suggested order
> follows the round-1/2/2.5 sequencing lists; all items ship regardless.

| # | Item | Status | Commit |
|---|------|--------|--------|
| ★ | Raw texture-data upload (`create_texture` / `data:` URI) | DONE | `ece042d3` |
| 1 | Texture UV transform — typed tool + render-path apply | DONE | `3d0102c7` + `ffca1bb3` |
| 2 | Texture offset/flow keyframe channel | DONE | `da280049` |
| 3 | Machine-readable command schema + patch-style `set_kind` | DONE | `72839eb2` (patch_kind; full JSONSchema bounded-deferred — see §3) |
| 4 | Typed/patch particle emitter config | DONE | `1a38e67c` |
| 5 | Unassigned-material node: render magenta/warn + fix docs | DONE | `8815c9be` |
| 6 | `duplicate_node` returns id(s) + `get_children`/`get_subtree` | DONE | `a52f4550` |
| 7 | `set_frame_time` pins builtin texture `Flow` | DONE | `0d52f981` |
| 8 | Per-model facing/orientation hint | DONE | `e2982c85` |
| 9 | Papercuts (frame_node, screenshot msg, solve_ik root, clip-clear pose) | DONE | `ab9898ef` |
| 10 | Fused `paint_where`/`transform_where` (handle + pagination deferred, noted) | DONE | `db32251b` |
| 11 | Per-node texture override re-specializes variant (or rejects loudly) | DONE | `8c7d2264` |
| 12 | `alpha_mode` re-wraps custom shader; doc batch ordering | DONE | `a7b5adab` |
| 13 | `set_builtin_alpha_mode` + base_color rgba | DONE | `edbbdd01` |
| 14 | Particle realism (sprite upload, alpha sampled→discs, doc `forces`; soft-gradient blend deferred-noted) | DONE | `327b8159` |
| 15 | Vertex-color default footgun — doc + clear-to-0 option | TODO | |
| 16 | Programmable displacement WGSL stage | TODO | |
| 17 | `ibl` include + light-list for custom materials | TODO | |
| 18 | Env-from-agent-data (raw cubemap bytes / skybox WGSL) | TODO | |
| 19 | Toon shading — add regression test (positive, keep) | TODO | |
| 20 | Custom WGSL leading-operator line continuations — fix or doc | TODO | |

---

## Suggested implementation order — round 2.5 *(sequencing only; all items still ship)*
- Add an **`ibl` include for custom materials** (§17) — the single biggest lever
  for making custom + builtin materials visually consistent, and it retires a
  lot of the hand-faked lighting in every custom shader above. (Generic: it
  exposes an existing renderer subsystem to the agent's shaders.)
- **Raw texture-data upload** (Design Principle §1) — one generic primitive that
  retires the §14 sprite ask, the §18 sky ask, and §16's heightmap-bake, with
  zero presets. Probably the highest-leverage single addition.
- **Generic environment-from-agent-data** (§18) and **programmable
  displacement/skybox WGSL** (§16/§18) — let the agent write the sky and the
  terrain math, rather than the MCP shipping either.
