# awsm-renderer-codec-meshopt

`EXT_meshopt_compression` bufferView decode (and the encode surface for the
editor bake path, via the re-exported `meshopt` crate) over the **official
meshoptimizer C library** — linked straight into the app wasm. No worker, no
Emscripten.

## wasm32 build glue

- The `meshopt` crate (0.6.2) ships its own wasm32 support: vendored
  meshoptimizer sources + stripped stub headers (`include_wasm32/`) compiled
  with `-isystem`, so **no allocator shim and no libc stubs are needed on our
  side**. Decode writes into caller-provided memory; encode allocates through
  the C++ default which the crate's wasm build resolves.
- Apple clang has **no wasm backend**; the workspace `.cargo/config.toml`
  points `cc` at Homebrew LLVM (`brew install llvm`) for the
  `wasm32-unknown-unknown` target only (`CC_wasm32-unknown-unknown` under
  `[env]`).

## Running the tests on wasm

Tests are dual-attribute (`#[test]` native / `#[wasm_bindgen_test]` on wasm).
The runner version must match the workspace `wasm-bindgen` (see the pinned
version comment in the root `Cargo.toml`) — grab the matching release tarball
(`wasm-bindgen-<ver>-aarch64-apple-darwin.tar.gz`) or
`cargo install wasm-bindgen-cli --version <ver>`:

```sh
CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=/path/to/wasm-bindgen-test-runner \
  cargo test -p awsm-renderer-codec-meshopt --target wasm32-unknown-unknown
```

Runs in node (pure compute, no browser APIs).

## Fixture-gated tests

`tests/fixture_decode.rs` decodes every meshopt bufferView of the paid
`fixtures/local/police-meshopt.glb` (gitignored, never committed). build.rs
only compiles it when the file exists, so the suite stays green on machines
without the fixture.

Gotcha encoded in `tests/roundtrip.rs`: the index codec is lossless only up to
per-triangle rotation (a,b,c → b,c,a, winding preserved) — never compare raw
index sequences after a roundtrip.
