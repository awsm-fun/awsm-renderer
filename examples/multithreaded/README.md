# awsm-renderer — multithreaded reference app

A standalone, copyable example of running the renderer across **real wasm
threads** over a shared `WebAssembly.Memory`. It is built milestone by
milestone alongside `docs/plans/multithreading.md`; the full
end-user guide lives in `docs/PLAYER-GUIDE.md`.

## Run it

```sh
task mt:dev      # trunk serve on http://127.0.0.1:9090 (COOP/COEP enabled)
```

Then open the URL in a browser that supports WebGPU + `SharedArrayBuffer`.

## Demos (`?demo=`)

Each milestone of `docs/plans/multithreading.md` is a selectable demo; the
default is `input`.

| `?demo=` | What it shows | Source |
|---|---|---|
| `smoke`  | two workers share one `WebAssembly.Memory` (an `AtomicU32` crosses the thread boundary) | `src/smoke.rs` |
| `arena`  | the seqlock arena under a high-rate foreign writer — zero torn values accepted | `src/arena_test.rs` |
| `render` | the full renderer hosted in a worker over the shared **transform arena** (A/B vs `?arena=0`) | `src/render_demo.rs` |
| `motion` | a physics worker moving node transforms via shared memory (`?stress=N`) | `src/motion_demo.rs` |
| `crowd`  | instanced transforms **and** attributes driven by the physics worker (`?stress=N`) | `src/crowd_demo.rs` |
| `remote` | the Layer 1 `RenderCommand`/`RenderEvent` protocol — DOM driver loads a model + picks | `src/protocol.rs`, `src/remote_demo.rs` |
| `input`  | full input forwarding (pointer/wheel/key/resize) + a main-thread responsiveness meter | `src/input_demo.rs` |

The end-user guide for opting a game into this is **`docs/PLAYER-GUIDE.md` §9**.

## The threaded build profile (why it's different)

A normal wasm build has a private, non-shared linear memory — workers can't
share state through it. Three pieces, together, produce a bundle that
imports one **shared** memory all threads attach to:

1. **`rust-toolchain.toml`** pins nightly + `rust-src` (needed for
   `-Z build-std`).
2. **RUSTFLAGS + build-std** (see `taskfiles/examples/multithreaded.yml`):
   - `-C target-feature=+atomics,+bulk-memory,+mutable-globals`
   - `-C link-arg=--shared-memory --import-memory --max-memory=…`
   - `-C link-arg=--export=__heap_base/__tls_base/__tls_size/__tls_align/__wasm_init_tls`
     (wasm-bindgen's thread transform needs these symbols)
   - `-Z build-std=std,panic_abort` (recompiles `std` with atomics)
3. **COOP/COEP headers** on serve (`Trunk.toml`): `Cross-Origin-Opener-Policy:
   same-origin` + `Cross-Origin-Embedder-Policy: require-corp`. Without them
   `crossOriginIsolated` is false and `SharedArrayBuffer` is unavailable.

Each worker attaches to the shared memory via the bootstrap in `src/bootstrap.rs`:
post `{ wasm_module, memory }` to the worker, which calls
`init({ module_or_path: wasm_module, memory })`.

## Status

Complete (M0–M7): cross-origin isolation + shared-memory smoke; the
`shared_arena` seqlock primitive; arena-backed node transforms with the
render-side 64→112 pack; a physics worker driving node transforms; instanced
transforms + attributes; the Layer 1 remote-renderer protocol; full input
forwarding + a main-thread responsiveness proof. The shared-memory sim-state
primitive lives in the renderer crate (`buffer/shared_arena.rs`); this app is
the copyable consumer.

The single-threaded editor / model-viewer builds are untouched and keep using
the stable toolchain — `cargo check --workspace` stays green throughout.
