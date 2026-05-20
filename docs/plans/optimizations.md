# Renderer Optimisations — Backlog

A working file for performance / robustness / feature items that
surfaced during the shadow audits but were intentionally deferred. The
shadow subsystem is at the "ships correctly under expected scenes"
quality bar; this doc tracks the next layer of polish without forcing
it into the critical path.

Treat these as **shovel-ready specs**, not commitments. Each item has
enough detail to be picked up cold, including a rough sense of scope
and what unlocks it. Many cluster around the same underlying problem
(global culling + spatial structure), so re-read the cluster headers
before grabbing a single item — the right move may be to bundle.

---

## Cluster 1 — Per-view culling + spatial structure

The biggest single cost lever in the renderer's foreseeable future.
Currently every shadow view (cascades, spots, cube faces) iterates
every mesh in the scene and does an AABB-vs-frustum test against the
light-space view-projection. Point lights multiply this by 6, so a
scene with 8 shadowed point lights + 4 directional cascades is 8*6 +
4 = 52 views, each scanning every mesh. At 100 meshes that's 5200
checks per frame, all CPU-side.

A scene-wide spatial structure benefits not just shadows but the
geometry pass, picking, light culling, and any future visibility
query. Owning it once at scene level pays off for every consumer.

### 1.1 Scene-level BVH (or octree, or sorted-by-X bucket)

Replace the per-view linear sweep at
[`shadows/render_pass.rs::record`](../crates/renderer/src/shadows/render_pass.rs)
(search for `Frustum::from_view_projection`) with a BVH query. Build
the BVH lazily on first query per frame, invalidate on mesh
add / remove / transform-change.

**Effort:** medium-high. New `crate::scene_bvh` module, integration with
mesh add/remove and transform write paths. Sahkov SAH-builder is
~300 lines; or wrap an existing crate (`bvh`, `rstar`).

**Unlocks:**
- Per-view shadow caster cull → `O(log N · views)` instead of
  `O(N · views)`.
- Faster `Shadows::write_gpu` caster-AABB sweep (item 1.2).
- Faster geometry pass culling.
- Cheap visibility queries for light culling (item 2.1).

**Risk:** the per-mesh AABB needs to stay accurate as transforms
animate — that's already true today but the BVH's tolerance for
in-place updates vs rebuild needs design.

### 1.2 Cached caster-AABB list

`Shadows::write_gpu` currently rebuilds `caster_aabbs_scratch` every
frame by iterating every mesh and filtering on
`cast_shadows && !hidden && !hud` ([`shadows/state.rs`](../crates/renderer/src/shadows/state.rs),
search for `caster_aabbs_scratch`). For mostly-static scenes this is
pure waste.

Dirty-track a `casters: Vec<(MeshKey, Aabb)>` on `Shadows` (or on
`Meshes`, with `Shadows` subscribing). Invalidate on
`Mesh::set_cast_shadows` / `Mesh::set_hidden` / world-AABB change.
Trivial to implement *after* the BVH lands (the BVH already maintains
the relevant per-mesh metadata).

**Effort:** small (after BVH); medium standalone.

**Unlocks:** measurable per-frame CPU win on static scenes with many
non-casting meshes.

### 1.3 Cube-face per-axis culling verification

The render pass at [`shadows/render_pass.rs`](../crates/renderer/src/shadows/render_pass.rs)
already does `Frustum::from_view_projection(view.view_projection)`
once per cube face, so each face's 90° frustum naturally excludes the
other 5 faces' geometry. This is "free" given the per-view structure.
Worth profiling to confirm it's actually skipping work (it might be —
the per-mesh AABB-vs-frustum test rejects meshes outside the cone),
or extending the cull to a fast-path major-axis test before the full
frustum check.

**Effort:** trivial diagnostic; medium if the fast-path is needed.

---

## Cluster 2 — Light + render scheduling

### 2.1 Light culling pass

Not yet implemented. Receivers currently shade every active light,
which is fine for ≤8 lights but doesn't scale. A tile-based or
cluster-based light culling pass (compute shader producing a per-tile
list of relevant lights) would let us scale to 100s of lights.

Pre-requires the BVH from 1.1 if we want CPU-side per-light AABB
visibility too.

**Effort:** large. New compute pass, new uniform buffer layout, edits
to the lighting shader to walk per-tile lists.

**Reference:** Olsson, Persson, Doggett 2012 ("Clustered Deferred and
Forward Shading"). [`shared_wgsl/lighting/lights.wgsl`](../crates/renderer/src/render_passes/shared/shared_wgsl/lighting/lights.wgsl)
is the consumer that would change.

### 2.2 2D shadow per-tile clearing

Today every 2D atlas view is forced to render every frame because
`LoadOp::Clear` is attachment-wide. The throttle logic at
[`shadows/state.rs`](../crates/renderer/src/shadows/state.rs) (search
for `is_cube`) explicitly comments that 2D throttling is disabled for
this reason.

Two paths:
- **(a)** Per-cascade attachment views via a 2D texture array — each
  cascade gets its own array layer; clear only the layers whose
  cascade is due to re-render this frame.
- **(b)** Manual tile-local clear: a one-pixel-thick clear quad
  rendered into the cascade's rect before the depth pass.

(a) is cleaner; (b) is smaller. Both unlock the existing throttle
machinery so far-distance cascades + spot lights with
`update_period > 1` actually skip work.

**Effort:** medium for (a) — touches render pass + attachment
allocation; small for (b).

### 2.3 EVSM cascade batching / skip-unchanged

`EvsmPass` runs moment-write + blur for every queued cascade each
frame. Combined with 2D throttling (2.2), unchanged cascades could
skip both the depth render AND the EVSM compute. Today even an
unchanging far cascade re-runs the blur every frame.

**Effort:** small (after 2.2 lands).

### 2.4 Coarse light-space binning

A simpler alternative to per-light spatial culling for shadow casters:
hash mesh AABBs into world-space cells, query the cells overlapping
each shadow view's world frustum. Faster to query than a BVH, slower
to build, OK for ~1000 meshes.

Probably not worth implementing if 1.1 (BVH) lands.

---

## Cluster 3 — Authoring + debug UX

### 3.1 Shadow debug views

Useful for diagnosing "why doesn't my light cast a shadow?":
- **Atlas occupancy**: render the 2D shadow atlas to a viewport
  overlay, with cascade/spot tile borders drawn.
- **Cube-slot ownership**: per-light cube-pool slot index + per-face
  age (frames since last render).
- **Per-light descriptor index**: hover a light in the inspector, see
  its `descriptor_base` and `cascade_count`.
- **Cascade splits**: line widgets showing the world-space PSSM split
  planes around the camera, plus an option to draw each cascade's
  frustum corners.
- **Throttled vs. rendered**: this-frame badge on each shadow view in
  a sidebar list (green = rendered, grey = throttled).

The existing `debug_cascade_colors` flag is the precedent — extending
that pattern is straightforward.

**Effort:** medium. Mostly editor UI work; the renderer already has
the data.

### 3.2 Cascade split editor overlay

Sub-item of 3.1 but worth calling out: a viewport overlay showing
each cascade's near/far world-space distance from the camera, with a
draggable handle for `cascade_split_lambda`. Authors currently tweak
the slider blind.

**Effort:** small.

### 3.3 Stable texel-snap controls

The cascade fit at [`shadows/cascade.rs`](../crates/renderer/src/shadows/cascade.rs)
(`fit_cascade`) does texel-snap stable-fit. Currently the texel size
is implicit (`diameter / resolution`). Exposing a "snap quantum"
slider for fine-tuning when shadows still swim under specific camera
motion would help author tuning, but the current behaviour is correct
for typical scenes.

**Effort:** trivial. Mainly a UI affordance.

---

## Cluster 4 — Product / quality features

### 4.1 Quality tiers

A `ShadowQualityTier::{Low, Medium, High, Ultra}` enum that caps
multiple knobs at once:
- atlas size
- max cascades
- PCF tap count (today 16, the `Soft` path; could drop to 8 or 4 on
  Low)
- max point shadows
- EVSM atlas size (or Off entirely)
- SSCS on/off

Useful for mobile vs desktop deployment. Today each knob is
configured independently which is correct but tedious for the
"reasonable defaults for hardware tier" use case.

**Effort:** medium. Mostly schema + UI + the per-tier preset table.

### 4.2 Importance-based per-light budgets

Auto-scale `resolution` / `cascade_count` / `cube_face_update_rate`
based on the light's screen-space contribution. Off-screen lights
drop to lower resolution; lights covering most of the screen get full
quality. Today everything is authored explicitly.

**Effort:** large. Needs a screen-space contribution metric (camera
frustum intersection + light bounds + intensity falloff). Lands
better after 1.1 (BVH) is in place.

### 4.3 True cube PCSS

[`docs/SHADOWS.md` "Known limits"](../docs/SHADOWS.md) currently calls
out that cube `Pcss` is a widened-`Soft`, not true PCSS, because
`texture_depth_cube_array` doesn't expose raw depth reads. The honest
fix is to add a 2D-array depth view of the cube pool, write the
face-projection math in WGSL, and do a real blocker search.

**Effort:** medium-large. ~150 lines of WGSL + new bind group entry +
view creation. Marginal visual win at typical point-light scales
(range 1–30 m); only worth doing if the current approximation becomes
visibly insufficient.

### 4.4 Spot-light PCSS per-light scale tuning

The existing PCSS path works for spots but uses one global
`pcss_penumbra_scale` that's authored per-light. A "scale by cone
half-angle" auto-adjustment would make spot PCSS look right by
default; today wide-cone spots have visually-too-narrow PCSS unless
the user bumps the scale.

**Effort:** small. Multiply scale by `tan(outer_angle * 0.5)` or
similar.

---

## Cluster 5 — Robustness scaffolding

### 5.1 Shadow render-pass integration tests

The unit tests in `shadows/record.rs::tests` exercise the allocation
transaction but don't touch the actual render pass. A headless GPU
test (using `wgpu` directly, no editor) that drives `Shadows::write_gpu`
+ `record` end-to-end with synthetic light + mesh fixtures would
catch the "descriptor pointer mismatch" / "view pointer mismatch"
class of bugs before they reach the browser.

**Effort:** medium. Requires a `wgpu` testing harness that can run on
CI runners (or `wgpu_lite` / `naga` validation only).

### 5.2 Validation: descriptor + view bookkeeping

`active_descriptor_count` and `active_view_count` are maintained by
hand inside `write_gpu`. A `debug_assertions`-only invariant check at
function exit ("`records.values().map(|r| r.views.len()).sum() ==
active_view_count`") would catch off-by-one regressions immediately.

**Effort:** trivial.

### 5.3 Lights / Shadows desynchronisation guard

The recent API audit gated `Lights::insert`, `Lights::remove`,
`Lights::clear` behind `AwsmRenderer::insert_light` /
`remove_light` / `clear_lights` to keep the two crates' state
synchronised. There's still one bypass: `Lights::update` can change
the light's KIND (Directional → Point), which moves it out of the
shadow allocator's cube-slot expectations. If a user mutates kind in
place, the cube_slot_for_light cache may point at a slot the light
no longer owns.

Two options:
- Forbid kind changes via `update` (panic-debug, warn-release).
- Detect the kind change and call `Shadows::on_light_removed` +
  re-register.

**Effort:** small. The detection logic is ~10 lines in
`update_light`.

---

## Picking order

If/when this list gets prioritised:

1. **1.1 (BVH)** is the keystone — it unlocks 1.2, makes 2.1
   tractable, and is a one-time scene-level investment that benefits
   every pass. Worth doing first when scene complexity demands it.
2. **2.1 (light culling)** is the corollary — same time horizon.
3. **3.1 (debug views)** is the highest authoring-quality-per-effort
   item; can ship independently of 1.1.
4. **4.1 (quality tiers)** is the highest product-quality-per-effort
   item.

Everything else is filler — pick up when adjacent work makes it cheap.

The "cluster 5" robustness items are background polish; do them
opportunistically when touching the relevant code.
