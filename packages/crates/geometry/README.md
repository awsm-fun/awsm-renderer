# awsm-renderer-geometry

Pure-CPU non-curve geometry utilities. No `awsm-renderer` dep, **no `awsm-renderer-curves` dep**, no `web-sys` dep.

## What's here

- `Aabb` + `point_in_aabb`, `aabb_overlap`, `aabb_union`.
- `Ray` + `ray_aabb`, `ray_triangle` (Möller–Trumbore), `ray_plane`.
- Frustum predicates: `aabb_in_frustum`, `point_in_frustum`.

## Why it's its own crate (and why no curves dep)

Gameplay code wants these for collision-flavored checks, pick-tests, range queries. Pulling WebGPU is unacceptable. Pulling `awsm-renderer-curves` is also unacceptable — keeps each crate single-purpose; consumers who want curve queries explicitly depend on `awsm-renderer-curves` too.

Curve-aware geometry helpers (`nearest_point_on_curve`, `curve_length_between`) live in `awsm-renderer-curves`, where curves live.
