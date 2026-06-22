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
> ⏳ **REMAINING BLOCKER — texture-transform doesn't RENDER (deferred, deep
> renderer/prep-pass bug; §1 stays WIP).** With §11 landed I verified the
> transform path end-to-end with instrumentation: applying any transform
> (`set_node_texture_transform scale:[4,4]`, or even **identity**) makes the
> checker **vanish to flat** — the shader reads a **zero matrix** (UV→(0,0) →
> uniform corner texel). PROVEN-CORRECT on the CPU/data side: `resolve_texture`
> logs `transform_key offset=32 slot_index=1`; the transform buffer's CPU slot 1 =
> `[4,0,0,4, 0,0]` (the scale matrix); the dirty-range flush uploads it; and even a
> **synchronous full-buffer `gpu.write_buffer`** (bypassing the mapped uploader)
> still renders flat. So the upload lands but the **shader reads `texture_transforms`
> as zero for any slot ≥ 1** (slot 0 / identity works → §11). The UV transform for
> interior pixels is applied in the **prep pass** (`render_passes/material_prep`,
> the `{% if prep_present %}` branch in `material_opaque_wgsl/helpers/texture_uvs.wgsl`)
> — strong lead: the prep pass binds a stale/separate texture-transforms buffer, or
> its materialized-UV cache isn't invalidated when a transform changes live. Fix
> there, then verify scale/offset tile+shift the checker and `flow` scrolls it.
> **This same blocker gates §2** (the `texture_transform` keyframe channel needs
> transforms to render). Diagnostics were reverted; the tool + reject-loudly + the
> §11 unblock are committed (`3d0102c7`, `8c7d2264`).

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

---

## 7. 🟡 `set_frame_time` doesn't pin builtin texture `Flow`

`set_frame_time` pins `frame_globals.time` for deterministic temporal-*material*
screenshots, but builtin texture `Flow` did not respond to it (frame 0 vs 1
identical) — it appears to integrate real `delta_time` independently. So even if
Flow rendered, you couldn't deterministically screenshot a specific phase.
(Related to §1; listed separately because it also affects any delta-integrated
effect.)

---

## 8. 🟡 No facing/orientation hint per model

Project metadata says `-Z forward`, but this imported model's **face is +Z**.
Nothing in the node/asset data signals a model's facing, so placing the jetpack
"on the back" was trial-and-error (I put it on the chest first). A
bounds/normal-derived facing hint, or a documented convention check, would save
a round-trip. (Model-specific; smaller payoff and likely later in the queue —
but in scope and required, not optional.)

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

## 13. 🟠 No typed way to set builtin `alpha_mode` or base-color **alpha**

Glass needs `alpha_mode: blend` + a sub-1 base-color alpha on a builtin
material. `set_builtin_param base_color` takes **3 floats (no alpha)**, and
there's no `alpha_mode` tool for builtin materials. The only route was the full
`set_kind` escape hatch (resend the whole NodeKind with
`"alpha_mode":"blend"` + `base_color:[r,g,b,a]`). Add
`set_builtin_alpha_mode { node, mode, cutoff? }` and let `base_color` accept 4
floats.

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

## 15. 🟡 Vertex-color default is **(1,1,1,1)**, which inverts splat-weight logic

Painting snow on peaks then `mix(base, snow, vColor.r)` turned the **whole
terrain white** — unpainted verts default to `(1,1,1,1)`, not `0`, so every vert
read as full weight. Had to mark painted verts with a **zeroed channel**
(`g=0`) and invert the test. The transparent contract even documents the custom
read default as `vec4(1)` — but for *splat weights* that's a footgun. Document it
prominently in the splatting recipe, and/or offer a "paint clears to 0" baseline.

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
| 1 | Texture UV transform — typed tool + render-path apply | WIP | `3d0102c7` (code; visual gated by §11) |
| 2 | Texture offset/flow keyframe channel | TODO | |
| 3 | Machine-readable command schema + patch-style `set_kind` | DONE | `72839eb2` (patch_kind; full JSONSchema bounded-deferred — see §3) |
| 4 | Typed/patch particle emitter config | WIP | |
| 5 | Unassigned-material node: render magenta/warn + fix docs | TODO | |
| 6 | `duplicate_node` returns id(s) + `get_children`/`get_subtree` | TODO | |
| 7 | `set_frame_time` pins builtin texture `Flow` | TODO | |
| 8 | Per-model facing/orientation hint | TODO | |
| 9 | Papercuts (frame_node, screenshot msg, solve_ik root, clip-clear pose) | TODO | |
| 10 | Selection handles + pagination + fused `paint_where` | TODO | |
| 11 | Per-node texture override re-specializes variant (or rejects loudly) | DONE | `8c7d2264` |
| 12 | `alpha_mode` re-wraps custom shader; doc batch ordering | TODO | |
| 13 | `set_builtin_alpha_mode` + base_color rgba | TODO | |
| 14 | Particle realism (raw sprite upload, alpha sampled, doc `forces`) | TODO | |
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
