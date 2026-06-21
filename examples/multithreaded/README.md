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

- **M0** — shared-memory smoke (`src/smoke.rs`): two workers attach to one
  memory; worker A increments a native `AtomicU32`, worker B observes the
  increments across the thread boundary with zero `postMessage` on the
  shared-state path. Confirms `crossOriginIsolated` + cross-thread shared
  linear memory.

The single-threaded editor / model-viewer builds are untouched and keep
using the stable toolchain.
