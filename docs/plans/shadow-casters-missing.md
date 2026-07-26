# Shadow casters missing — `shadows-all` fails its own verify criteria

Status: **reproduced, not yet root-caused** (2026-07-26). Found by dogfooding
the renderer against LOCKSTEP-GAMES' dance-off stage.

## Reproduction (30 seconds)

```
task test-scenes          # :9084
task mcp-dev -- 9086      # editor :9085
```
`load_project_from_url http://localhost:9084/shadows-all/project`, then the
scene's own recipe: `set_camera_orbit {yaw:0.5, pitch:0.6, radius:13,
look_at:[0,0.3,0]}`, `wait_render_settled`, `screenshot_scene`.

## What happens

`examples/test-scenes/shadows-all/verify.md` lists under **fail**: *"a caster
with NO shadow"*. On this build three of the four casters hit exactly that:

| Caster | Expected | Actual |
|---|---|---|
| `tall-box` (tan, left) | solid directional shadow, contact-tight base | **no shadow at all** |
| `sphere` (blue, centre) | round soft contact shadow beneath | **no shadow** (only faint darkening) |
| `lowered-box` (green, fg) | contact-tight, no donut/ring | **no shadow** |
| `thin-bar` (salmon, right) | long thin sliver | **renders** |

The warm SPOT pool renders correctly, so the spot itself is lit and aimed. The
one caster that *does* shadow (`thin-bar`) sits inside that pool — consistent
with "the spot's shadow works where its cone reaches, the DIRECTIONAL cascade
contributes no shadow at all."

This is on `volumetrics`, which is **0 commits ahead of `main`** — so it is
current `main` at 0.26.0, not a branch regression.

## Ruled out

- **Not the mesh flags.** Every caster has `shadow.cast = true`.
- **Not the blend-caster exclusion** (`NodeFilter::shadow_caster()`'s
  `exclude_blend_casters`, from `dbe51590 "alpha-blend materials no longer cast
  shadows by default"`) — every material in the dance-off repro is
  `alpha_mode = "opaque"`.
- **Not importance//tier starvation.** `importance.rs` returns High/2048 for
  directional unconditionally, and the dance-off spots score
  `intensity/(1+dist²) = 240/65 ≈ 3.7` → High.
- **Not the spot view-projection.** `state.rs`'s spot branch derives `eff_range`
  from `influence_radius`, picks a non-degenerate `up`, and builds
  `perspective_finite(2*outer_angle, …)` — all consistent with the receivers
  being inside the frustum.
- **Not SSCS.** Toggling `sscs_enabled` changes the image not at all.

## Corroborating case (dance-off)

A four-spot stage, all lights `shadow.cast = true`, spots at intensity 240,
range 8, ~3.6 m above a non-emissive floor with the dancers inside the cones:
**no cast shadow on the floor**. The *directional* in that same scene does throw
the dancers onto the backdrop — so directional casting is not universally dead
there, which cuts against the simplest "shadows are off" explanation and
suggests something receiver- or cascade-selection-shaped.

## Next steps

1. Read back the shadow atlas (`atlas_size` 4096) after a frame — is depth being
   written for the directional cascades at all, or is the failure on the
   receiver/sample side?
2. Check cascade selection: the spot descriptor sets `split_far = f32::MAX` to be
   picked unconditionally; confirm the directional cascades' `split_far` walk
   actually selects a descriptor for these receivers.
3. Bisect `shadows-all` against an older tag to find where it started failing —
   the scene exists precisely to guard this, so it presumably passed once.
