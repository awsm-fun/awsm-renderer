# Scene Loader — running editor-authored scenes in a game runtime

Status: **planning + phase 1 in progress.** This doc scopes what it takes for a
*game* (not the editor) to load a scene authored in `awsm-editor` and drive it —
especially its **animations** — in the game's own runtime, and records the
design decisions (notably: what can be unopinionated vs. what cannot).

## The goal

The editor exists to author scenes that **games load and drive at runtime in
their own players**. A game should be able to:

1. Deserialize a saved project,
2. Materialize its nodes / meshes / materials / lights / cameras into the
   renderer,
3. Load the authored animation clips + mixer, and
4. Drive them every frame (`renderer.update_animations(dt)`), with full
   blending — exactly the runtime the editor itself uses.

## Where we are today (audit)

| Piece | State |
|---|---|
| Runtime animation engine (`AnimationClipGroup`, `AnimationMixer`, `update_animations`, samplers) | ✅ public + clean in `renderer` (`animation/mod.rs`) — a game *can* drive it. |
| Authored data format (`StoredAnimation`, `StoredTrack`, `Keyframe`, `MixerDoc`, `TrackTarget`, `TrackValue`) | ✅ in **shared** `scene-schema` (`animation.rs`) — a game *can* deserialize it. |
| Persisted project (`EditorProject` with `editor_animations` + `anim_mixer`) | ✅ in `scene-schema` (`project.rs`) — a game *can* deserialize it. |
| **Lowering**: `StoredAnimation` → playable `AnimationClipGroup` with **resolved** targets | ❌ **editor-only.** Lives in `editor/controller/animation.rs::Track::lower` + `editor/engine/bridge/animation_sync.rs::{resolve_target,lower_clip,lower_mixer}`. No reusable version. |
| Scene materialization (nodes/meshes/materials → renderer + the id→key maps) | ⚠️ editor-coupled (the `node_sync` bridge). The renderer's `scene-schema` feature already converts *shadows* + *materials*; nodes/transforms/meshes are not yet a reusable path. |
| Non-editor consumer that plays authored animations | ❌ none. `model-tests` plays **glTF** animations only (a *different* runtime path: glTF → loose `AnimationKey` player, not the clip-group/mixer path). |

**Net:** the runtime engine and the data are reusable; the **glue that turns the
data into something the engine can play is locked inside the editor.** A game
cannot "just load and drive" today.

This traces to a deliberate original decision — *animations persist as
editor-side data; the core scene format stays untouched.* That kept the editor
shippable but left the runtime-export path unbuilt. This doc is that path.

## Opinion: what can be unopinionated, and what cannot

**The animation lowering is unopinionated — and should be shared.** Turning a
`StoredTrack` into an `AnimationChannel` is a pure function of the data plus a
*resolver*: `Fn(&TrackTarget) -> Option<AnimationTarget>`. The editor already
factors it exactly this way (`Track::lower(&resolve)`). The resolver is the only
opinionated input, and it's a closure the caller supplies. So the value/sampler
lowering + clip/mixer assembly belong in **`renderer` behind the existing
`scene-schema` feature** (alongside the shadow/material `From<scene_schema::*>`
conversions). A game enables the feature and gets it. **No new crate.**

**The scene materialization is inherently opinionated — and cannot be fully
unopinionated.** Building the `NodeId → TransformKey` and `AssetId → MaterialKey`
maps that the resolver needs depends on choices only the game can make:

- Does the game keep the renderer's transform store as its scene graph, or its
  own (and mirror)?
- Which `NodeKind`s does it support? (A game may ignore editor-only kinds.)
- How does it map editor materials to its own material/shader system?
- Skinned meshes, instancing, LODs — game-specific policy.

You cannot hand a game one `load_scene(project) -> Scene` and have it be right
for everyone. **But you don't have to.** The split that keeps the *reusable* core
unopinionated while making the opinionated part *pluggable*:

```
   pure data lowering            caller-provided closures           optional reference
   (unopinionated, in renderer)  (opinionated, game supplies)       (opinionated default)
   ─────────────────────────     ──────────────────────────────     ──────────────────────
   StoredAnimation → channels    resolve: TrackTarget→AnimationTarget   a "materialize an
   MixerDoc → AnimationMixer      clip_key: AssetId→AnimationClipKey      EditorProject into
   value/sampler conversion       mask: &[NodeId],desc → TargetMask       the renderer" helper
                                                                          games use or replace
```

The loader never decides *how* a node becomes a `TransformKey`; it only consumes
the resolver. That's the line between unopinionated and opinionated, and it's the
same closure pattern the editor's bridge already uses.

## Plan

### Phase 1 — Reusable animation loader

**Done:** `renderer::animation::scene_loader` (behind `feature = "scene-schema"`)
ships `lower_stored_clip` + `lower_stored_mixer` with the value/sampler/morph
conversion as shared internals, taking caller-provided resolver closures.
Covered by CPU-only unit tests that lower a `StoredAnimation` and sample it to
the exact interpolated pose (no GPU, no editor). A game enables the feature and
calls it (see the module's doc example).

**Remaining:** dogfood it from the editor — have `animation_sync` convert its
live clip → `StoredAnimation` and call the shared lowering, so there is one
source of truth and the runtime path is exercised on every editor relower.
(Today the editor still has its own live `Track::lower`; the two are kept in
sync by hand until this lands. Low-risk but touches the verified relower path, so
deferred to a focused change.)

The public API:

```rust
/// Lower one authored clip into a playable group. `resolve` maps each track's
/// abstract target (node-id/material-id + property) to a concrete renderer
/// AnimationTarget; tracks that don't resolve are skipped.
pub fn lower_stored_clip(
    clip: &StoredAnimation,
    resolve: impl Fn(&TrackTarget) -> Option<AnimationTarget>,
) -> AnimationClipGroup;

/// Build a mixer from the authored doc. `clip_key` looks up the inserted key for
/// a clip AssetId; `mask` resolves a layer's node set into a TargetMask.
pub fn lower_stored_mixer(
    doc: &MixerDoc,
    clip_key: impl Fn(AssetId) -> Option<AnimationClipKey>,
    mask: impl Fn(&[NodeId], bool /*include_descendants*/) -> TargetMask,
) -> AnimationMixer;
```

The value/sampler/morph conversion (`track_value_to_data`,
`morph_scalar_to_vertex`, sampler construction) moves here as the shared
internals. **The editor then dogfoods this** (its bridge converts live →
`StoredAnimation` and calls the shared lowering) so there is one source of truth
and the runtime path is exercised on every editor relower. Covered by renderer
unit tests that lower a `StoredAnimation` and sample it (CPU-only, no GPU).

After Phase 1 a game can play authored clips **given** a resolver — i.e. given
that it has already materialized the scene and has the id→key maps.

### Phase 2 — Reference scene materializer (the opinionated default)

A `renderer` helper (scene-schema feature) that walks `EditorProject.nodes`,
inserts transforms/meshes/lights/cameras, applies materials (reusing the
existing material/shadow `From` conversions), and returns the
`NodeId→TransformKey` + `AssetId→MaterialKey` maps — i.e. the resolver inputs.
This is the *opinionated* default; a game with its own scene graph skips it and
supplies its own maps. Skinned meshes reuse the same baked-joint path the editor
relies on (see the `#2` skin fix).

### Phase 3 — One-call game API

`renderer::load_project(project, &mut renderer) -> LoadedScene` that runs Phase 2
then Phase 1 (lower clips + mixer using the materializer's maps) and returns
clip keys + the mixer, so the common case is one call while the closures remain
available for games that need control. Prove it with a non-editor consumer
(extend `model-tests` to load an `EditorProject` and play an authored clip).

## Open questions

- **glTF vs authored runtime paths.** glTF import yields loose `AnimationKey`
  players; authored clips yield `AnimationClipGroup`s. They coexist in
  `update_animations` but never converge. Long-term, importing glTF *through the
  editor* already normalizes to clip-groups; a pure-runtime game using raw glTF
  stays on the loose path. Decide whether the runtime loader should also offer a
  "glTF → clip-group" path for uniformity, or leave glTF on its native player.
- **Material/shader portability.** A game's material system may not match the
  editor's dynamic-WGSL materials; `Uniform`/`BuiltinParam` tracks need the
  game's material-key + slot mapping. The resolver covers this, but the
  reference materializer (Phase 2) has to take an opinion.
- **Versioning.** `EditorProject` is the on-disk contract; once games depend on
  it, additive `#[serde(default)]` evolution (already the convention) becomes
  load-bearing.
