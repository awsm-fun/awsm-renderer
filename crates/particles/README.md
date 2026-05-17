# awsm-particles

Pure-CPU particle simulator. No `awsm-renderer` dep.

## What's here

- `Emitter` — spawn rate, burst count, max-alive cap, loop vs one-shot, world-space vs emitter-local, lifetime range, initial speed, etc.
- `SpawnShape { Point, Sphere, Cone }`.
- `Force { Gravity, LinearDrag }`.
- `Particle` data laid out **struct-of-arrays** to match the layout a GPU compute backend would write.
- Per-life parameter curves via `awsm-curves::Curve1<T>`: `color_over_life`, `size_over_life`, `alpha_over_life`.
- `Simulator::tick(dt)` advances state and exposes a packed `[InstanceAttr]` slice that the renderer's per-instance attribute path consumes directly.

## Stance

- **Eye-candy only.** Particles never feed gameplay state. There is no host-bridge surface for particles in the player's per-game module API.
- **Non-deterministic.** Runs at the render tick rate. Replays will not reproduce particle visuals frame-exact.
- **CPU at v1, GPU-shape layout.** Going GPU compute later is a renderer-internal swap — no API, schema, or editor change.

## Why CPU with GPU-shape

CPU iteration speed, debuggability, and editor preview at current particle counts (< 10k simultaneous) outweigh GPU scale. The SoA layout means when we do swap, the buffer the compute shader writes is bit-identical to the buffer the CPU writes today.
