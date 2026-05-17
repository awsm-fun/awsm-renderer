# awsm-curves

Pure-CPU curve math. No `awsm-renderer` dep. No `web-sys` dep.

## What's here

- `Curve3` — 3D path curves (Catmull-Rom + Bezier), sampling, tangents, Frenet/parallel-transport frames.
- `Curve1<T>` — 1D parameter curves over `[0, 1]` with output types `f32`, `Vec3`, color (`[f32; 4]`). Used for color-over-life / size-over-life / etc.
- Curve-aware geometry helpers (`nearest_point_on_curve`, `curve_length_between`).

## Why it's its own crate

Gameplay code (lap detection, AI pathing, camera-rail behavior) wants to operate on curves without dragging WebGPU in. The renderer + editor consume curves; the player WIT bridge exposes them to per-game modules. None of those callers should pull WebGPU.

## What's not here

- Animation curves over real time. `Curve1<T>` is over a normalized `[0, 1]` parameter, not a timeline.
- Renderer/editor visualization of curves — that's the renderer's job. This crate only does math.

## Companion crates

- `awsm-geometry` — non-curve CPU geometry (AABB, ray/triangle, frustum). No dep here on it.
- `awsm-meshgen` — depends on this crate; sweep generators consume curves.
