# Plan: multithreading hardening (Phase 2 — make it primetime)

**Status: Phase 1 (M0–M7) landed on the `multithreading` branch — the
architecture is proven (shared-memory bootstrap, seqlock arena, render-side
pack, off-main responsiveness, Layer 1 protocol, input forwarding). This doc is
the Phase-2 *hardening* work-order: turn the validated vertical slice into a
shippable subsystem.** It runs **autonomously end-to-end** like Phase 1 — each
milestone H1–H9 has a pass/fail gate (Rust tests + Chrome DevTools MCP), is
committed on the current branch, and proceeds immediately to the next. The
single-threaded editor/model-viewer build stays green at every step. **The
final milestone (H9) deletes this doc** — its permanent home is
`docs/PLAYER-GUIDE.md` §9 + the `examples/multithreaded` reference + git history.

This supersedes the original M0–M7 plan (now implemented; see the commits).

---

## Why Phase 2

The Phase-1 review surfaced gaps that are genuine blockers to shipping a game on
this, not polish. All of them are implementation hardening on a correct
foundation — none are architectural dead-ends:

1. **Stale CPU-side bounds for sim-owned nodes/instances.** The render side
   packs physics-written transforms to the GPU but never refreshes
   `world_aabb` / the spatial index, so frustum culling, shadows, and picking
   are wrong for anything physics moves. (Phase 1 dodged it by parenting bodies
   to root and keeping motion in-frustum.) **Correctness blocker.**
2. **`foreign_write` aliasing.** Value bytes are read as `&[u8]` while a foreign
   thread writes via `*mut` — UB in Rust's model (works today only because wasm
   doesn't reorder across the seqlock fences). **Soundness blocker.**
3. **The per-frame copy at the pack step.** The descent packs into a *private*
   `Vec` mirror that `queue.writeBuffer` then reads — an extra memcpy every
   frame. The shared-memory-direct-upload path was never proven. **Perf blocker.**
4. **Spawn/despawn topology never exercised live.** `allocate`/`free` are unit
   tested but the spawn-binds-a-slot / despawn-frees-it round-trip during the
   hot loop, and arena growth under a concurrent foreign writer, are unproven.
5. **Deferred sim-state.** Lights (and morph weights / skin joints) can't be
   driven from the sim worker — many games animate lights.
6. **Layer 1 is minimal.** Procedural geometry only; no real glTF, no texture
   transfer, no scene-mutation/query commands beyond `Pick`, thin errors.
7. **Resize/DPR is a stub.** The worker `Resize` handler is a no-op, so the
   `OffscreenCanvas` renders at a fixed 800×600 and is CSS-upscaled — looks like
   broken AA (MSAA 4× is actually *on*). **Visible-quality blocker.**
8. **Main-thread boot long-task (~59 ms).** Main synchronously instantiates the
   multi-MB wasm at boot — a real >50 ms task that misrepresents responsiveness.

---

## Locked decisions (carried from Phase 1, still in force)

Shared linear memory + native atomics; the renderer stays `!Send` on the render
worker; topology is owner-only, values are foreign; the seqlock arena
(`buffer/shared_arena.rs`) is the single foreign-writable primitive; the
single-threaded build is feature-gated off and continuously validated. Phase 2
adds: **bounds are refreshed from the arena on descent**, **the value region is
`UnsafeCell`-backed**, and **the upload reads the arena/staging directly when
the platform allows**.

---

## Execution model

For each milestone: (1) do the work, (2) `cargo test` + the threaded build,
(3) serve with COOP/COEP, (4) drive Chrome DevTools MCP to verify the gate
EXACTLY as written, (5) commit on the current branch, (6) proceed immediately.
Gates are pass/fail; never advance on red; never fake a pass. Keep the
single-threaded build and every existing `?demo=` working. Stop only at H9
(done) or a genuine block needing human input.

> **MCP toolkit:** `navigate_page`, `evaluate_script`, `take_screenshot`,
> `list_console_messages`, `list_network_requests`, `performance_start_trace`/
> `performance_stop_trace`, `resize_page`, `emulate` (devicePixelRatio).
> Threaded serve: `task mt:dev` → http://127.0.0.1:9090.

---

## Milestones

### H1 — Resize / DPR correctness (the "MSAA" red herring)
The worker-hosted canvas must render at **display-size × devicePixelRatio**, not
a fixed 800×600 stretched to the window.
- Size the canvas to the window in `index.html`; main forwards `Resize { css_w,
  css_h, dpr }`; the worker sets the `OffscreenCanvas` backing size to
  `round(css*dpr)`, has the renderer rebuild size-dependent render textures, and
  updates the camera aspect. Add an `AwsmRenderer` surface-resize API if one
  doesn't exist (reconfigure context + recreate render textures at the new size).
- Wire it across the worker-hosted demos (render/motion/crowd/input).
- **Gate:** `evaluate_script` → `canvas.width === round(innerWidth*dpr)`;
  `take_screenshot` shows crisp (non-upscaled) edges; `resize_page` to a new
  size → re-screenshot crisp + correct aspect (no stretch); `emulate` dpr=2 →
  backing size doubles. Console clean. **Commit; continue.**

### H2 — Kill the main-thread boot long-task
No >50 ms task on the main thread at startup.
- Shrink main's synchronous footprint: main should do the *minimum* (spawn
  workers, transfer canvas, forward input). Move heavy wasm work off the main
  thread's synchronous boot — e.g. async/streaming instantiation, deferring
  non-critical init to a post-paint task, or a thin JS-only main bootstrap that
  spawns workers before main's wasm fully initialises.
- **Gate:** `performance_start_trace`(reload)/`stop_trace` over boot shows the
  longest main-thread task < 50 ms (target < 20 ms); the input demo's worst rAF
  gap settles < 30 ms. All demos still boot. **Commit; continue.**

### H3 — Sim-owned bounds + culling correctness
Physics-driven transforms/instances get correct CPU-side world bounds.
- On the descent, for each updated sim-owned node, recompute `world_aabb` from
  the arena world matrix × the mesh's local geometry bounds; sync the spatial
  index and note caster-moved (so shadows/culling/picking are right). For
  instanced meshes, recompute the combined AABB from the per-instance arena.
- **Gate:** `cargo test` — AABB derived from an arena world matrix equals the
  direct (single-threaded) compute. Browser: a mover travels a wide arc fully
  off-screen and back; expose a "drawn mesh count"; with the fix it is culled
  only when fully outside the frustum and never pops while partly visible
  (`take_screenshot` t0/t1 + the count). **Commit; continue.**

### H4 — `foreign_write` memory-model soundness
No Rust aliasing UB on the shared value region.
- Back the arena value region with `UnsafeCell<u8>` (or word-atomic access).
  All value reads/writes go through raw pointers synchronised solely by the
  seqlock; never hand out a `&[u8]`/`&mut [u8]` that can alias a concurrent
  write. Encapsulate so safe callers can't construct a conflicting reference.
- **Gate:** `cargo +nightly miri test` on the `shared_arena` value-access +
  seqlock logic reports no UB (if miri can't model the cross-thread path, run it
  on the single-threaded access path and record the soundness argument in the
  module docs). Browser `?demo=arena` still: `tornAccepted=0`. **Commit; continue.**

### H5 — Live spawn / despawn topology transaction
Bodies spawn and despawn during the hot loop without races or leaks.
- A command-channel round-trip binds a slot at spawn (owner-only) and frees it
  at despawn; foreign writers only ever touch bound slots; arena growth happens
  owner-side between frames (addresses stay stable — decision B). Reconcile the
  free-list; a freed-then-reused slot must not deliver a stale value.
- **Gate:** a demo continuously spawns + despawns bodies while the physics
  worker writes. Over ≥10 s: zero torn-accepted, no crash/leak, live-count ==
  spawned − despawned, slot reuse verified, zero per-frame `postMessage` on the
  write path. `take_screenshot` shows the churn. **Commit; continue.**

### H6 — Lights (+ morph weights / skin joints) as sim-state
Animated lights and deforms drivable from the sim worker.
- Give punctual lights a stable-slot arena (the dense-repack → stable-slot
  refactor the original plan flagged as a prerequisite); foreign-write light
  world transforms / params; render descends + repacks. Apply the same
  arena-backed foreign-write path to morph weights and skin joint matrices.
- **Gate:** a physics worker animates a light's position — `take_screenshot`
  t0/t1 shows the illuminated region sweeping across a static surface; an
  animated morph/skin deform is visible; zero per-frame `postMessage`. The
  single-threaded path for lights/morph/skin is unchanged (regression
  screenshot). **Commit; continue.**

### H7 — Full Layer 1 protocol surface + real assets
The complete D4 command/event set.
- Add `SetLocal`, `SetMeshMaterial`, `UpdateCamera`, light/env/decal updates,
  `Bounds` (reply), `Screenshot` (reply, Transferable bytes), Transferable
  **texture** payloads, real **glTF** load (pull in `renderer-gltf` /
  `scene-loader`), and robust `Error` + load backpressure.
- **Gate:** a DOM driver loads a real glTF over the protocol (progress bar
  paints from `Loading` events); `SetMeshMaterial` changes a material
  (`take_screenshot` before/after); `UpdateCamera` orbits; `Bounds` returns an
  AABB; `Screenshot` returns pixels; `Pick` hits. Console clean. **Commit; continue.**

### H8 — Zero-copy GPU upload (settle writeBuffer-from-shared-memory)
Remove the intermediate private-`Vec` copy on the transform/instance upload
path — *if the platform allows*.
- Empirically test `queue.writeBuffer` from a shared-memory-backed `TypedArray`.
  **If accepted:** pack 64 → 112 directly into a shared (or mapped-staging)
  buffer the uploader reads, eliminating the per-frame copy. **If rejected by
  Chrome:** keep the copy, but capture the exact error + a written rationale —
  this is a platform constraint, NOT a fake pass.
- **Gate:** `evaluate_script` records the writeBuffer-from-shared result. If
  supported: the transform path no longer copies into a private `Vec`
  (verified in code + an identical `take_screenshot` + a `?trace` bench showing
  the copy gone). If unsupported: the result + rationale are recorded and the
  scene still renders identically. **Commit; continue.**

### H9 — Per-frame allocation audit + finalize + delete this doc
Hot path allocation-free; everything green; doc retired.
- Audit + pool any remaining per-frame heap allocation on the descent / pack /
  upload / spawn paths (David's avoid-allocations standard); finalize the
  `?stress=N` + `?trace=sub-frame` benches; fold all hardened behavior into
  `docs/PLAYER-GUIDE.md` §9 and the example README.
- Prove both builds from one tree: editor (single-threaded, `task editor:dev`)
  and the threaded example (`task mt:dev`) run and screenshot correctly.
- **Then delete `docs/plans/multithreading.md`** (content now lives in the
  guide + example + git history).
- **Gate:** `?stress=N` + `?trace=sub-frame` shows no per-frame heap growth in
  the render hot path; `cargo fmt --all -- --check` + `cargo clippy --all
  --all-features --tests -- -D warnings` + `cargo test` all green; editor +
  example both run/screenshot; `docs/plans/multithreading.md` removed.
  **Commit. Done.**

---

## Autonomous `/loop` prompt

Paste this as the `/loop` task (self-paced; runs unattended).

> Implement `docs/plans/multithreading.md` (Phase 2 hardening) fully and
> autonomously, milestones H1→H9 in order. Do ALL work on the current git branch
> — do NOT create new branches; commit each milestone as its own commit. The
> Phase-1 foundation already exists, so there is NO go/no-go gate: proceed
> through every milestone WITHOUT stopping for review. For each milestone do the
> code work, run `cargo test` + the threaded build (`task mt:dev` flags), serve
> with COOP/COEP, and verify that milestone's gate EXACTLY as written via
> chrome-devtools MCP (`evaluate_script` for assertions/DPR/writeBuffer probes,
> `take_screenshot` for visual/motion/quality proof, `resize_page`/`emulate` for
> resize+DPR, `performance_start_trace`/`stop_trace` for the main-thread
> long-task check, `list_console_messages`/`list_network_requests` for clean
> console + no per-frame postMessage); when GREEN, commit and proceed IMMEDIATELY
> to the next milestone. If a gate is RED but fixable, iterate and re-verify.
> Where a milestone says "implement if the platform allows, else document the
> constraint" (H8), do exactly that — capture the real result, never fake a pass.
> STOP and summarize ONLY when: H9 is green and `docs/plans/multithreading.md`
> has been deleted, OR a gate stays red after several genuine fix attempts with
> no progress, OR a milestone is blocked on something needing my input. Never
> skip a gate, never advance past a red gate, never fake a pass. Keep the
> single-threaded editor/model-viewer build AND every existing `?demo=` working
> at every step. Start with H1.
