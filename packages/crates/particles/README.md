# awsm-renderer-particles

Pure-CPU particle simulator. No `awsm-renderer` dep.

## What's here

- `Emitter` — spawn rate, burst count, max-alive cap, `one_shot`, `EmitterSpace` (`World` / `Local`), lifetime / initial-speed / size ranges, `forces`, etc.
- `SpawnShape { Point, Sphere, Cone }`.
- `Force { Gravity, LinearDrag }`.
- Particle state held **struct-of-arrays** internally (positions / velocities / ages / lifetimes / base sizes), matching the layout a GPU compute backend would write.
- Per-life parameters via `ColorOverLife` and `SizeOverLife` enums (`Const` / `Linear`, sampled over normalized age `t`). Alpha is folded into `color_over_life`'s `.a` channel — there is no separate `alpha_over_life`.
- `Simulator::tick(dt, emitter, emitter_world_pos)` advances state and repacks the `Vec<InstanceAttr>` (`position(3) | size(1) | color(4)`, 32 bytes) that the renderer's per-instance attribute path consumes directly.

## Stance

- **Eye-candy only.** Particles never feed gameplay state. There is no host-bridge surface for particles in the player's per-game module API.
- **Non-deterministic.** Runs at the render tick rate. Replays will not reproduce particle visuals frame-exact.
- **CPU at v1, GPU-shape layout.** Going GPU compute later is a renderer-internal swap — no API, schema, or editor change.

## Why CPU with GPU-shape

CPU iteration speed, debuggability, and editor preview at current particle counts (< 10k simultaneous) outweigh GPU scale. The SoA layout means when we do swap, the buffer the compute shader writes is bit-identical to the buffer the CPU writes today.
