# Compression — meshopt + quantization (first-class) + KTX2/Basis textures

Status: designed (2026-07-13), not started. Branch: `compression`.

Goal: a real compression story with **no Draco and no Emscripten toolchain**.

1. **`EXT_meshopt_compression` + `KHR_mesh_quantization` are FIRST-CLASS** in our
   mesh pipeline — natively loaded from our player bundle, and natively **loaded
   AND encoded** in the editor — via the **official meshoptimizer C library** (the
   `meshopt` FFI crate) cross-compiled to `wasm32-unknown-unknown`. No Emscripten,
   no worker — it links straight into the app wasm. This is our bundle's mesh
   format going forward.
2. **Textures stay GPU-block-compressed into VRAM.** The core renderer change: a
   KTX2/Basis texture transcodes at load to a native GPU block format
   (BC/ASTC/ETC2) and uploads compressed, never expanded to RGBA8 on device.
   Textures dominate asset size, so this is the biggest single win.

## Decisions (locked with David — do not relitigate)

1. **Draco is DROPPED** — no decode, no encode, nowhere. It's the only piece with
   no pure-Rust path (Emscripten-in-a-worker) and only bought us mesh import.
2. **Mesh codec = `EXT_meshopt_compression` + `KHR_mesh_quantization`,
   first-class, both directions**, via the **official `meshopt` crate** (David's
   call — gwihlidal's FFI over zeux/meshoptimizer via `meshopt-sys` + `cc`), NOT
   the pure-Rust `meshopt-rs`. This gives the authoritative, current bitstream that
   decodes `gltfpack` output natively (no version-compat gamble) and SIMD-capable
   encode. Editor ENCODES our bundle meshes (quantize → meshopt-encode) and DECODES
   on import; the player DECODES in-Rust on load (no worker). Reverses the old
   "bundle meshes stay uncompressed" call. The tradeoff — cross-compiling the C
   lib to wasm — is the Phase-1 spike.
3. **KTX2/Basis textures transcode at load on the PLAYER** (three.js / Babylon
   model) — bundle stores Basis-supercompressed KTX2 (one device-agnostic
   artifact); player transcodes to the adapter's block format and uploads
   compressed. Target is **desktop + mobile** WebGPU (BC desktop / ASTC+ETC2
   mobile / RGBA8 last resort). Player ships the small Basis **transcoder** only.
4. **Texture ENCODE is editor/bake-time**, off the main thread in a Web Worker,
   via the Basis **encoder** module.
5. **KTX2 becomes the bundle default** for `TextureExport` (WebP-lossless / lossy
   / Source as opt-outs, plus KTX2 **source-passthrough** for already-KTX2
   imports). ⚠ Re-bakes every existing project's textures — accepted migration.
6. **Basis codec libs are prebuilt, vendored, and hosted in a Web Worker**,
   isolated from the Rust wasm module (transferable `ArrayBuffer`s). Vendor the
   prebuilt `basis_transcoder` (three.js ships it) + `basis_encoder` — **no
   Emscripten build, no COOP/COEP**. The worker/transfer/hardening patterns in
   `~/Downloads/draco-browser-rust-wasm-final-plan.md` still apply to the Basis
   worker; ignore its Draco-specific and build-from-source parts.

### What ships where

| Module | Editor | Player |
|---|---|---|
| meshopt + quantization codec (`meshopt` C-FFI, linked in-process) | ✅ import + bake-encode | ✅ decode (no worker) |
| Basis **transcoder** (vendored, in a worker) | ✅ import preview | ✅ runtime load |
| Basis **encoder** (vendored, in a worker) | ✅ bake | ❌ |
| Draco | ❌ | ❌ |

## The two acceptance assets (paid — do NOT check into the repo)

- `…/ROBOTS/ROBOT-FAB-POLICE/BLENDER/police-opt.glb`
- `…/ROBOTS/ROBOT-FAB-ASTRABOT/BLENDER/astrabot-opt.glb`

(Under `/Users/dakom/Documents/LOCKSTEP/MEDIA-MASTERS/ARTWORK/GAMES/COMMON`.)

**David is re-exporting both with `EXT_meshopt_compression` + `KHR_mesh_quantization`
(replacing Draco)** — i.e. standard `gltfpack` output. They live in the
**gitignored** `fixtures/local/` (scaffolded; `.gitignore` + README tracked, bytes
never committed) as:
- `fixtures/local/police-meshopt.glb`
- `fixtures/local/astrabot-meshopt.glb`

**Provide one early** — the Phase-1 spike must prove the `meshopt` C-FFI decode
path (once cross-compiled to wasm) reads real `gltfpack` output.

They are skinned (`JOINTS_0/WEIGHTS_0` — NOT quantized by the extension), have no
TANGENTs (regenerated via existing MikkTSpace), use `KHR_texture_transform`
(supported) + `KHR_materials_specular` (supported). Texture encoding, which sets
our KTX2 defaults: **base-color / roughness / metallic / emissive = ETC1S**,
**normal maps = UASTC** (higher quality).

## Architecture — meshopt + quantization

Renderer is **browser-only WebGPU via `web-sys`** (wasm32, no `wgpu` crate).

**Decode order on import/load** (per meshopt bufferView, then per accessor):
1. `EXT_meshopt_compression`: read the ext's own `buffer/byteOffset/byteLength`
   (the parent bufferView points at a `fallback:true` buffer — do NOT read that as
   data). meshopt-decode by `mode` (ATTRIBUTES / TRIANGLES / INDICES), then apply
   `filter` (OCTAHEDRAL for normals/tangents, QUATERNION, EXPONENTIAL, or NONE) →
   reconstruct the logical `byteStride×count` bufferView bytes.
2. Accessors then read those bytes — but with `KHR_mesh_quantization` the
   component types are quantized ints: POSITION byte/ubyte/short/ushort (norm or
   not); NORMAL/TANGENT byte/short **normalized**; TEXCOORD byte/short.
3. **Dequantization is via transforms, not per-accessor scale:** normalized ints
   → f32 by the standard divisor (c/127, c/255, c/32767, c/65535); UNnormalized
   position ints → left as-is and mapped by the transform; either way the **node
   TRS** (non-skinned) or **`inverseBindMatrices`** (skinned — the robots) then
   maps to world space, and UVs via `KHR_texture_transform`. Our import already
   applies node TRS, IBMs, and tex-transform, so correct geometry falls out once
   accessors accept the quantized types.
   Verified actual gltfpack output for both robots: POSITION = **VEC3 `short`
   normalized**, NORMAL = VEC3 `byte` normalized (OCTAHEDRAL-filtered bufferView),
   TEXCOORD_0 = VEC2 `ushort` normalized, JOINTS_0 = VEC4 `ubyte`, WEIGHTS_0 =
   VEC4 `ubyte` normalized, INDICES = `ushort`. Note POSITION is *normalized*
   short → BOTH the /32767 normalize AND the node/IBM transform apply (don't
   assume unnormalized). meshopt bufferViews: mode ATTRIBUTES + one TRIANGLES
   (indices), filters NONE + OCTAHEDRAL (normals); a `fallback:true` buffer is
   present — decode the ext buffer, ignore the fallback.

**Renderer attribute handling.** Today `buffers/attributes.rs` promotes
U8/I8/U16/I16 → F32 (WGSL vertex path). First-class correctness = accept the new
quantized component types + normalized dequant here. **First-class GPU-optimal
(memory win)** = keep quantized formats through the visibility/geometry packing
(WebGPU has `unorm8/snorm8/unorm16/snorm16` vertex formats) instead of expanding
to f32 — a deeper change to `pack_vertex_attributes` / the mesh-buffer layout.
Sequence correctness first (Phase 4/5); GPU-quantized formats is the Phase-6
optimization, called out explicitly as the "true first-class" memory payoff.

**Encode (editor bake).** Quantize f32 attributes to ints at chosen bit depths
(pure Rust; positions→short, normals/tangents→octahedral-filtered byte/short,
UVs→short) and record the dequant transform into node TRS / IBMs / tex-transform;
then meshopt-encode (ATTRIBUTES + filters, TRIANGLES for indices). `glb-export`
writes `KHR_mesh_quantization` accessors + `EXT_meshopt_compression` bufferViews
(with the `fallback:true` buffer) — both listed in `extensionsRequired`.

**Build risk (spike first):** the `meshopt` crate compiles zeux/meshoptimizer
(C++ in a C-like, STL-free, exception-free subset) via `meshopt-sys` + `cc`.
Cross-compiling to `wasm32-unknown-unknown` needs: clang targeting wasm32
(`-fno-exceptions -fno-rtti`), the meshoptimizer allocator routed to Rust's
allocator via `meshopt_setAllocator` (or an `operator new/delete` shim — no
libc++ on this target), and stubs for any unresolved libc symbols (`assert`,
stray `math.h`). meshoptimizer is deliberately wasm-portable (zeux ships a wasm
build), so this is bounded glue, not open-ended. Bitstream compatibility is a
non-issue — this IS the library gltfpack uses. Fallback if the C build proves
intractable: pure-Rust `meshopt-rs` (v0.15 bitstream — reintroduces a compat
check) or the emscripten decoder module.

## Architecture — KTX2/Basis textures

Big leg-up: **`renderer-core/src/cubemap/ktx.rs` already uploads block-compressed
KTX2** (`map_ktx_format`, `is_block_compressed`, `block_dims`,
`calculate_bytes_per_row`, per-mip `write_texture`) — for NON-supercompressed env
cubemaps. Lift its block-layout core into a shared mod for materials.

Transcode ladders (Khronos KTX Developer Guide; pick by caps + source codec +
color-vs-data slot):
- **UASTC:** ASTC-4x4 → BC7 → ETC2-RGBA → BC3/BC1 → **RGBA8**
- **ETC1S color:** ETC2-RGBA / ETC1-RGB → BC7 → BC3/BC1 → **RGBA8**
- **RG two-channel (later opt):** EAC-RG11 / BC5 → RG8

Basis outputs `cTF{ASTC_4x4,BC7,ETC2,BC3,BC1,BC5}_*` / `cTFRGBA32`; web-sys
`TextureFormat` already has `Astc4x4Unorm(Srgb)`, `Bc7RgbaUnorm(Srgb)`,
`Etc2Rgba8Unorm(Srgb)`, `Bc3/Bc1RgbaUnorm(Srgb)`, `Bc5RgUnorm`, `EacRg11Unorm`.

**Color space (double-sRGB / normal dark-patch history):** color slots →
`*UnormSrgb` block format, SKIP the `srgb_to_linear` compute pass (invalid on
compressed anyway); linear slots (normal/MR/occlusion) → non-srgb block format.
**Normals first cut = full BC7/ASTC** (RGB), no shader change; two-channel
BC5/EAC-RG (in-shader Z reconstruct) is a Phase-6 opt. **Block dims multiple of
4**: else WebP-lossless (encode) / RGBA8 (runtime) fallback.

---

## Phase 0 — Vendor prebuilt Basis artifacts

- [x] Vendor prebuilt, hash-versioned `basis_transcoder.{js,wasm}` (three.js
      `examples/jsm/libs/basis/`, KTX2+Zstd) for editor+player, and
      `basis_encoder.{js,wasm}` (Binomial web build) for the editor only. Vendor
      licenses + `BUILD-METADATA.json` (upstream source, version, SHA-256s).
      Non-pthread → no COOP/COEP. Serve from both apps' static dirs,
      `application/wasm` MIME, immutable cache.
- **Exit:** a standalone page instantiates both Basis modules from vendored
  assets. (No Emscripten, no Draco.) ✅ **DONE 2026-07-13** — vendored at
  `web/vendor/basis/` (transcoder = three.js r185, 527KB wasm; encoder =
  basis_universal v2_1_0r non-pthread, 2.77MB wasm; SHA-256s in
  `BUILD-METADATA.json`). Served via `data-trunk rel="copy-file"` →
  `vendor/basis/` in editor (all 4 files) + player-tests + model-tests
  (transcoder pair ONLY — encoder stays editor-only). Browser-verified via
  `web/vendor/basis/smoke-test.html`: both modules instantiate, `KTX2File` +
  `BasisEncoder` exports present. ⚠ Both JS builds export the SAME global
  factory name `BASIS` — never load both in one scope; scope per-worker or
  capture-and-delete. Immutable cache headers = Cloudflare Pages config at
  deploy time (noted in vendor README).

## Phase 1 — meshopt+quant spike + Basis worker subsystem

- [x] **meshopt spike (de-risk FIRST):** add the official `meshopt` crate; get
      `meshopt-sys` cross-compiling to `wasm32-unknown-unknown` (cc→clang wasm32,
      `-fno-exceptions -fno-rtti`; route allocation via `meshopt_setAllocator` to
      Rust's allocator; stub `assert`/unresolved libc). Confirm the linked app
      wasm resolves cleanly and **decode a real re-exported robot's meshopt
      bufferViews** in-Rust. If the C build is intractable, record it and switch to
      the `meshopt-rs` fallback (then a bitstream check is back on the table).
      ✅ **GATE PASSED 2026-07-13** — far easier than budgeted: `meshopt` 0.6.2
      has NO separate meshopt-sys anymore; it vendors meshoptimizer + ships its
      own wasm32 build glue (`include_wasm32/` stub headers for
      assert/limits/math/string — "no stdlib, LLVM intrinsics"). **No allocator
      shim, no libc stubs needed on our side.** Only real fix: Apple clang has
      no wasm backend → `.cargo/config.toml [env]` points cc at Homebrew LLVM
      (`CC_wasm32-unknown-unknown=/opt/homebrew/opt/llvm/bin/clang`; requires
      `brew install llvm`). New crate `packages/crates/codec-meshopt`
      (`decode_buffer_view(data,count,stride,mode,filter)` + re-exported
      `meshopt` for encode; stride/count/filter guards + 256MB output cap).
      Proof, all ON wasm32 (wasm-bindgen-test in node, runner 0.2.118 matched
      to the workspace pin) AND native: (1) police-meshopt.glb — **82 meshopt
      bufferViews decoded, 2,942,542 compressed → 7,478,280 logical bytes**
      (1 TRIANGLES + 81 ATTRIBUTES; filters NONE + OCTAHEDRAL; octahedral
      output spot-checked unit-length); (2) encode→decode round-trips (vertex,
      index, octahedral filter) — encode allocates inside the C lib, so this
      also proves the allocator story on wasm. Fixture tests auto-skip via
      build.rs cfg when the gitignored fixture is absent. Gotcha: the index
      codec is lossless only up to per-triangle rotation — never
      byte-compare round-tripped index streams.
- [x] Basis worker: `web/workers/basis-worker.js` hosting the vendored modules;
      versioned protocol; request-id routing; init caching; structured errors;
      restart-on-fatal. Rust client crate `packages/crates/codec-basis` — async
      `transcode` (+ `encode` behind editor-only `encoder` feature); transferable
      fast path + owned convenience path. Feature-gate: player = transcoder only.
- **Exit:** meshopt round-trips in-Rust (wasm) AND decodes real gltfpack output;
  Basis worker transcodes a fixture off the main thread (verify BROWSER console).
  ✅ **PHASE 1 COMPLETE 2026-07-13.** Worker shipped (protocol v1: init/ping/
  transcode/encode, structured `{code,message}` errors, per-request ids;
  module URLs passed at init; target names resolved against the embind enum at
  runtime, no hardcoded ints). Client crate `awsm-renderer-codec-basis`:
  `transcode_js` (transferred ArrayBuffer levels) + owned `transcode`, encode
  behind `encoder` feature (default OFF; player never enables), restart-on-
  fatal drains in-flight requests and respawns lazily. Worker copied to all 3
  app dists (`workers/`). Browser-verified (console): khronos_basecolor.ktx2
  (ETC1S 12.4KB) → **bc7 349,584B / astc-4x4 349,584B / rgba32 1,398,108B, 11
  mips, 1.9–4.4ms each**; encode 32×32 RGBA → ETC1S KTX2 800B → transcode-back
  gradient preserved (6 mips). Gotchas: worker `var BASIS` global is
  non-configurable (assign `undefined`, don't `delete`); Chrome disk-caches
  worker scripts hard (smoke page uses a cache-buster; dev pages should too);
  encoder v2_1_0r dropped `setKTX2SRGBTransferFunc` (worker feature-detects
  optional setters).

## Phase 2 — Core renderer: block-compressed material textures (THE core change)

- [x] Request `texture-compression-{bc,etc2,astc}` on device create, gated on
      `adapter.features().has(..)` (mirror `indirect-first-instance`,
      `renderer-core/src/renderer.rs:263-306`). Expose the supported set.
      ✅ 2026-07-13 — all three families requested when the adapter has them;
      exposed as `AwsmRendererWebGpu::texture_compression() ->
      TextureCompressionSupport { bc, etc2, astc }` (+ `.none()` for the RGBA8
      last-resort check); one-shot tracing diagnostic at device create.
      Browser-verified in model-tests: device creates cleanly, console prints
      `texture compression support: bc=true etc2=true astc=true` (Apple
      Silicon/Metal exposes all three). Bonus verified: the Phase-0 trunk
      copy-file wiring really lands in the dist (transcoder + worker 200 on
      :9080; encoder correctly ABSENT from player dists — its "200" is trunk's
      SPA index.html fallback, a gotcha to remember when curl-probing dists).
      `compatibility.rs` untouched on purpose: compression is optional
      (RGBA8 fallback), never a compat gate.
- [x] Lift `cubemap/ktx.rs` block-layout + format-map helpers into a shared
      `renderer-core` module used by cubemaps AND materials.
      ✅ 2026-07-13 — new `renderer-core/src/texture/block_format.rs`:
      `is_block_compressed` / `block_dims` / `bytes_per_pixel` /
      `tight_bytes_per_row` / `aligned_bytes_per_row` / `rows_per_image` /
      `mip_level_byte_size` / `map_ktx_format`, with unit tests. Pure lift —
      `cubemap/ktx.rs` now delegates (605→203 lines), behavior identical.
      One addition for the materials path: `tight_bytes_per_row` — the
      256-byte row alignment is only a *buffer*-copy requirement;
      `queue.writeTexture` takes tight rows, so the Phase-2 upload path can
      skip the cubemap loader's padding staging entirely.
- [x] Compressed upload path in `renderer-core/src/texture/texture_pool.rs`
      (`write_gpu`): block `write_texture` + pre-supplied mips; **bucket the
      texture-array pool by compressed format**; skip staging, `srgb_to_linear`
      compute, and compute mipgen (invalid on compressed). Add a `Compressed`
      `ImageData` variant.
      ✅ 2026-07-13 — `ImageData::Compressed(Arc<CompressedImage>)` (format +
      tight per-level byte buffers; `validate()` checks chain length & exact
      per-mip byte sizes so bad transcoder output fails with a message, not a
      GPU validation error; `write_to_texture_layer` uploads tight rows via
      `writeTexture` — no 256-align staging). Pool: arrays auto-detect
      compressed from format (key also carries the pre-supplied mip-chain
      length so 1-level and full-chain images never share an array);
      `write_gpu_compressed` = createTexture(mips=N, TEXTURE_BINDING|COPY_DST
      [+COPY_SRC under texture-export]) + per-layer writeTexture. No encoder,
      no staging textures, no external-image copy, no sRGB compute (warn if
      requested — sRGB rides the `*UnormSrgb` format), no mipgen.
      `ImageData::create_texture` also handles Compressed (single-texture
      path). Uncompressed path untouched — browser-verified no-regression
      (Fox renders textured in model-tests). On-device compressed
      verification lands with the Phase-2 exit after format selection.
- [x] Format selection: caps + source codec + slot color-space + color-vs-normal
      → ladders above; RGBA8 last resort; multiple-of-4 guard.
      ✅ 2026-07-13 — `codec-basis::selection`: `TranscodeCaps` (mirror of
      renderer-core's `TextureCompressionSupport`; crate stays independent),
      `SourceCodec::{Etc1s,Uastc}`, `select_transcode_target[_checked]`
      (UASTC: ASTC→BC7→ETC2→RGBA8; ETC1S: ETC2→BC7→ASTC→RGBA8 — ETC1S stays
      in-family where possible, every rung RGBA-capable),
      `dims_block_compatible` (base level must be multiple-of-4 in WebGPU;
      guard folds into `_checked` → RGBA8), `texture_format_for_target`
      (sRGB rides the format; two-channel BC5/EAC-RG are linear-only →
      `None` on srgb=true; ETC1 uploads as ETC2-RGB superset). Unit-tested
      (desktop/mobile/Apple/none caps matrices). Normal maps = full-RGB
      first cut per plan.
- **Exit:** a Basis KTX2 texture uploads in a native block format (GPU-readback /
  assert no RGBA8 fallback on BC-capable desktop) and renders correctly.
  ➡ Exit proof FOLDED INTO PHASE 3 (recorded 2026-07-13): all Phase-2 pieces
  are in and individually verified, but nothing constructs
  `ImageData::Compressed` until the scene-loader KTX2 path lands — Phase 3's
  browser verification (tracing shows the selected block format; assert no
  RGBA8 on this BC+ETC2+ASTC machine; visual check) IS the Phase-2 exit,
  avoiding a throwaway harness.

## Phase 3 — Player runtime KTX2 load (scene-loader)

- [x] Replace the `TextureEncoding::Ktx2 =>` reject-stub
      (`scene-loader/src/texture.rs:189`): fetch → hand whole KTX2 to the Basis
      transcoder worker (parses container + Zstd + transcodes) with caps target +
      slot color-space + normal flag → upload compressed via Phase 2.
      ✅ 2026-07-13 — `sniff_basis_ktx2` (new, codec-basis: 48-byte header
      sniff → codec + dims, no transcode round-trip needed for target
      selection; native KTX2 = warn+unbound) → `select_transcode_target_checked`
      → thread-local `BasisWorkerClient` (player default config, encoder
      impossible) → `DecodedImage::Compressed` kept **sRGB-agnostic**; the
      binding slot picks `*Unorm`/`*UnormSrgb` at `load_texture` time (one
      asset can serve color AND data slots). `TextureCache::new` now takes the
      renderer for caps.
- **Exit:** a bundle with Basis KTX2 loads in player-tests, textures compressed on
  GPU, correct visuals, transcode off the main thread, no perf regression.
  ✅ **VERIFIED 2026-07-13** (also discharges the folded Phase-2 exit) — method:
  kitchen-sink bundle COPY (repo scene untouched) with both webp textures
  re-encoded as ETC1S KTX2 (node + vendored encoder; 362KB→67KB and tiny→5KB),
  `texture_encoding = "ktx2"`, served on :9096; player-tests
  `?bundles=…&scenes=kitchen-sink`. Console: `transcoded → Etc2Rgba, 10/11
  mips` + `binding compressed texture as Etc2Rgba8unormSrgb (srgb=true)` — a
  NATIVE BLOCK FORMAT, **no RGBA8 fallback**; ETC1S ladder correctly prefers
  ETC2 on this bc+etc2+astc device. `PLAYER-TESTS COMPLETE: 3/3` (35 nodes, 7
  meshes, 9,296 visible tris), load-transaction 273ms (vs 393ms on the
  first webp-era run — no regression; transcode is in the worker, off-main).
  🐛 REAL BUG found & fixed by this run: compressed `writeTexture` copies must
  use the PHYSICAL (block-rounded) size for sub-block tail mips — Dawn rejects
  a 2×2 copy on a 4×4-block format ("copySize.width (2) is not a multiple of
  block width (4)"); `CompressedImage::write_to_texture_layer` now rounds the
  copy extent up to whole blocks. Zero GPU validation errors after the fix.
  ⚠ Residual: a human-eyeball PIXEL check wasn't capturable here (WebGPU
  canvas reads back black post-present; the 1.2s run outruns screenshot RTT) —
  the definitive visual acceptance rides Phase 4's robot screenshots and
  Phase 5's golden round-trip, both mandatory anyway.

## Phase 4 — Editor import: meshopt + quantization + KHR_texture_basisu

- [x] Add `EXT_meshopt_compression`, `KHR_mesh_quantization`, `KHR_texture_basisu`
      to `RENDERER_SUPPORTED_EXTENSIONS` (`renderer-gltf/src/loader.rs:30`).
      Verify the `gltf` crate accepts quantized accessor component types
      (POSITION=short etc.); add lenient handling if it rejects them. Remove dead
      `GltfFileType::Draco` scaffolding.
      ✅ 2026-07-13 — trio added. Fixture-gated test (build.rs cfg, same
      pattern as codec-meshopt) proves the real robot parses: **quantized
      accessors need NO extra leniency** (gltf-json doesn't validate the
      semantic↔component-type matrix; POSITION=i16-normalized accepted as-is;
      82 meshopt bufferViews visible via raw `extension_value`). The gap was
      `KHR_texture_basisu` instead: those textures OMIT core `texture.source`
      (gltf-json sentinel Index(u32::MAX)) → validation "Missing".
      `parse_gltf_lenient` now lifts the extension's `source` into the core
      field, so basisu textures point at their KTX2 image entry like any PNG
      texture — the image DECODE path (later task) branches on payload.
      Draco fully removed (enum variant, `.drc` sniff, both reject branches,
      worker `FileTypeHint::Draco`); `GltfLoader::load`'s file_type param kept
      for API stability but no longer consulted (content sniffing decides).
      Gotcha: renderer-core's `map_ktx_format` needed `#[cfg(feature="ktx")]`
      — renderer-gltf pulls renderer-core without `ktx`, which the Phase-2
      lift had silently broken for that combination.
- [x] meshopt bufferView decode pass (pure-Rust crate) BEFORE accessor decode:
      reconstruct `byteStride×count` bytes from the ext buffer per `mode`+`filter`;
      ignore the `fallback:true` buffer. Feed reconstructed data to the existing
      accessor path.
      ✅ 2026-07-13 — `renderer-gltf/src/meshopt.rs`. Design: the fallback
      buffer is ALLOCATED ZEROED (never fetched — it would otherwise fall into
      the `Source::Bin` arm and steal/miss the GLB blob) and the decode pass
      writes each view's logical bytes into its parent range there, so the
      ENTIRE downstream accessor path reads through unchanged. Runs at the end
      of `import_buffer_data` in BOTH the main-thread loader and the worker
      parse job. Structured errors on missing/invalid ext fields + bounds
      checks on both source and destination ranges; per-model tracing line
      (views, compressed→logical bytes). Fixture test decodes the robot's 82
      views through the real loader plumbing and sanity-checks accessor-level
      results (max index < vertex count per prim; quantized POSITION regions
      non-zero).
- [x] Quantized accessors in `buffers/attributes.rs`/`accessor.rs`: accept the new
      component types; normalized → f32 dequant; unnormalized positions left for
      node-TRS / IBM dequant (verify skinned IBM path). Regenerate tangents
      (MikkTSpace). Confirm `KHR_texture_transform` UVs.
      ✅ 2026-07-13 — pleasant surprise: the conversion matrix in
      `attributes.rs` ALREADY covers every quantized type (u8/i8/u16/i16,
      normalized→f32 with the standard divisors + snorm clamp, unnormalized→
      integer promotion), `accessor_to_bytes` already destrides (meshopt's
      stride%4 padding around VEC3<i16>/VEC3<i8> handled), `skin.rs` already
      takes ubyte JOINTS + ubyte-normalized WEIGHTS, `index.rs` takes ushort.
      The REAL gap found & fixed: **AABBs read accessor min/max raw** — for
      normalized-i16 POSITION those are integers up to ±32767, inflating
      bounds ~32,767× (culling/LOD radii/framing). New shared
      `aabb::position_min_max` dequantizes per component type; both readers
      (`aabb.rs` + `populate/mesh.rs try_position_aabb`) use it; fixture test
      locks the robot's document AABB to model scale. Skinned-IBM /
      MikkTSpace-tangent / KHR_texture_transform verification = the
      acceptance run (next task), on-device.
- **Exit:** the two robots (meshopt+quant) import and render correctly in the
  editor — skinned, textured, GPU-compressed. Screenshot-verify both.
  ✅ **PHASE 4 ACCEPTANCE MET 2026-07-14** — BOTH robots import + render +
  screenshot in the editor (`task mcp-dev`, driven over the MCP HTTP link):
  police (105 nodes, 16 materials, 65 joints) and astrabot — full armor,
  correct textures (police chest emblem, astrabot's emissive blue eyes),
  correct skinned pose. Console: `meshopt decode pass: 82 bufferViews`,
  `KTX2 image (Etc1s …) → Etc2Rgba` color + `(Uastc …) → Astc4x4` normals —
  the per-slot ladder working end-to-end in the editor. Fixes the run forced:
  (1) editor mesh-capture (glb-export `extract.rs`) + the thin-shell
  heuristic (`populate/mesh.rs`) used the gltf crate's TYPED readers, which
  assert F32 and PANIC on quantized accessors → new quantization-aware
  readers (`glb-export/src/quant.rs` + a local VEC3 helper);
  (2) `KHR_texture_basisu` import decode: new `renderer-gltf/src/ktx2_image.rs`
  (sniff → ladder → worker transcode → `ImageData::Compressed` under the
  LINEAR format; `populate/material` swaps in the sRGB sibling per slot;
  `block_format::srgb_variant`); caps come from a documented
  `latest_texture_compression()` thread-local snapshot (loader has no device
  handle; machine-constant value);
  (3) **renderer skins store re-keyed**: IBMs were global per-JOINT and
  errored `JointAlreadyExistsButDifferent` — gltfpack emits multiple skins
  sharing one skeleton with different per-skin dequant-baked IBMs (police: 3
  skins × same 65 joints, different IBM accessors), which glTF allows. IBMs
  now live per-SKIN (`SecondaryMap<SkinKey, Vec<Mat4>>`), conflict check
  deleted.
  Screenshot gotcha for next time: `frame_node` on group/joint nodes framed
  degenerate joint AABBs (blank shots + uniform `canvas_stats` luma);
  framing a `skinned_mesh` node works.

## Phase 5 — Export: meshopt+quant bundle meshes + KTX2 texture default

- [x] Mesh encode (editor bake, pure-Rust): quantize attributes (positions→short,
      normals/tangents→octahedral, UVs→short; dequant transform into node TRS /
      IBMs / tex-transform) → meshopt-encode. `glb-export` writes
      `KHR_mesh_quantization` accessors + `EXT_meshopt_compression` bufferViews
      (+ `fallback:true` buffer), both in `extensionsRequired`. Player/editor
      decode via the Phase-4 path — round-trip must be lossless within the chosen
      quantization tolerance.
      ✅ 2026-07-14 — `glb-export/src/compress.rs`: `compress_glb(&[u8])`, a
      POST-PASS over the finished GLB (writer untouched, composable anywhere).
      POSITION→i16-norm stride 8, NORMAL→oct-i8 stride 4, TANGENT→oct-i8 vec4
      (w=handedness), UV∈[0,1]→u16-norm; dequant = UNIFORM scale+translation
      (normals never skew) carried by a fresh `dequant` WRAPPER child node for
      static meshes (the original node may be an animation target — its TRS is
      never touched) or folded into the skin's IBMs (per-skin transform =
      union of its meshes' bounds; a mesh spanning >1 skin or skin+static
      skips quantization). Every mesh stream meshopt-encodes (ATTRIBUTES /
      TRIANGLES); IBM/animation/morph accessors + image views pass through
      raw. Guards: morph-target meshes skip quantization (deltas untreated);
      non-[0,1] UVs stay f32 (a per-prim KHR_texture_transform remap would
      collide with authored transforms) — both still meshopt-encode.
      Wired into the editor bundle bake (`controller/export.rs`) for the
      per-mesh `assets/<id>.glb` bakes — dedup stays on uncompressed bytes,
      compression failure falls back to plain glb with a warn (never fails a
      bake); per-mesh size line traced. NOT yet applied to rig glbs
      (skinned save-format) or the standalone scene-glb exports — deliberate,
      revisit after the exit round-trip. Round-trip test (always-on,
      synthetic grid, renderer-gltf dev-dep on glb-export): encode →
      parse_gltf_lenient → decode pass → positions reproduce through the
      wrapper TRS within s/32767×2, normals within dot>0.98, triangles
      rotation-normalized equal, UVs within 2/65535; extensionsRequired
      checked on the WIRE (the lenient parser strips them in-memory).
- [x] Textures — authoring: `TextureExport::Ktx2 { profile }` + KTX2
      source-passthrough (`editor-protocol/src/assets.rs`); inspector option +
      `dispatch_texture_export` (`scene_mode/inspector.rs`); MCP
      `set_texture_export`. Bake arm (`editor/src/controller/export.rs` ~243):
      encode via Basis encoder worker — **ETC1S color / UASTC normal** by
      material-slot + color-space; record `TextureEncoding::Ktx2`; passthrough
      ships original KTX2 verbatim; non-4-multiple → WebP-lossless + `log()`. Make
      `Ktx2` the default when `texture_export` is `None` (document re-bake).
      ✅ 2026-07-14 — `TextureExport::Ktx2 { profile: Ktx2Profile }`
      (`Auto`/`Etc1s`/`Uastc`; Auto = UASTC for assets bound to a normal slot
      — base or clearcoat, collected from merged material defs — ETC1S
      otherwise) and **Default now = Ktx2(Auto)** (doc'd re-bake migration on
      the enum). Bake: editor-only Basis ENCODER worker client (thread-local,
      `with_encoder`; editor Cargo pulls codec-basis `features=["encoder"]` —
      player stays transcoder-only); decode via `image` crate → %4 guard →
      encode (mips on, zstd for UASTC, ETC1S q190) → `TextureEncoding::Ktx2`;
      failures cascade KTX2 → WebP-lossless → source, always logged, a bake
      never drops a texture. Passthrough: `ImageMime::Ktx2` added end-to-end
      (glb-export enum + both extract capture arms + editor caches), Source
      arm and Ktx2 arm both ship verbatim + record Ktx2; scene-glb exports
      embedding a KTX2 image now declare `KHR_texture_basisu`
      (used+required) with the ext-source on the texture. Inspector gains a
      "KTX2" segment (Auto profile); MCP `set_texture_export` gains
      `ktx2 | ktx2_etc1s | ktx2_uastc` and its description/default text
      updated. On-device bake verification (bundle export → ktx2 files →
      player load) = the Phase-5 exit, next iteration.
- **Exit:** export a scene (meshopt+quant + KTX2 defaults) → load in player →
  matches editor; imported-KTX2 passthrough round-trips byte-identical;
  round-trip/golden tests green.
  ✅ **PHASE 5 EXIT VERIFIED 2026-07-14** (mcp-dev, police scene):
  `export_player_bundle` → bundle inspected on disk: **20/21 mesh glbs carry
  EXT_meshopt_compression + KHR_mesh_quantization** (~6× smaller each, e.g.
  1,494,184→251,064B; console logs per-mesh deltas); **all 78 assets/*.ktx2
  byte-identical (sha256) to the fixture's embedded sources** — passthrough
  round-trips exactly; scene.toml records `texture_encoding = "ktx2"` for
  all 78, zero webp. Then `load_player_bundle` (in-memory bake → reset →
  reload through populate_awsm_scene, the RUNTIME path): scene-loader
  transcoded **74/74 unique KTX2 textures** (ETC1S→Etc2Rgba, UASTC→Astc4x4
  per slot), zero errors, and the robot renders correctly from the reload
  (screenshot: armor + police chest emblem + proportions match the
  authored render). Round-trip tests green (synthetic grid + full suite).
  📌 Residuals recorded: (1) ✅ RESOLVED 2026-07-14 (David: "lock it in") —
  the 27.9MB rig was 20.5MB uncompressed f32 accessors + **7.3MB embedded
  KTX2 images the player decodes and never uses** (78 wasted transcodes/
  load: rig materials are overridden by scene.toml via
  `GltfMaterialSource::Single`; the bundle already ships the same textures
  as assets/*.ktx2 — verified double-shipping). Bundle rigs now go through
  `strip_materials_and_images` (new in glb-export: drops materials/
  textures/images/samplers + per-primitive material refs; stale
  basisu/materials extension declarations scrubbed) + `compress_glb`
  (whose passthrough now drops orphaned bufferViews so image BYTES leave
  the BIN). SAVE-format rigs untouched; LOD bake still sees original
  bytes; fallback ships the original on error. Round-trip test locks it
  (512KB fake image → gone; geometry still decodes). Real-world rig size
  after: measure at the next on-device bundle export. (2) `load_player_bundle` gotchas for
  future runs: it takes NO url (bakes the CURRENT project); the editor tree
  is empty after it (runtime objects only — `frame_node` can't target them;
  use `reset_camera` + viewport zoom); its "textures are follow-ons" doc
  text is now stale (they load).

## Phase 6 — Hardening, tests, perf, GPU-quantized formats

- [ ] **True-first-class GPU optimization:** keep quantized vertex formats through
      the visibility/geometry packing (WebGPU `unorm/snorm 8/16`) instead of
      expanding to f32 — measure the VRAM win; browser-verify the vertex-format
      change.
      📋 **SCOPED 2026-07-14, implementation deferred behind the rest of
      Phase 6** — recon verdict: NOT a contained change. The geometry pool is
      consumed by BOTH fixed-function vertex fetch (~7 pipeline layouts; the
      easy half — snorm16x4/snorm8x4/unorm16x2 convert natively) AND raw
      `array<f32>` storage-buffer vertex pulling hardcoded in ~6 WGSL helpers
      across four passes (positions.wgsl, material_color_calc.wgsl,
      material_load_helpers.wgsl, masked_alpha.wgsl, material_prep compute —
      each needs bitcast/unpack2x16snorm word-offset math), plus packers
      (mesh_pack.rs ×3, buffer_info.rs size constants, scene-loader
      build_slot_geometry, cluster_lod), plus per-mesh dequant constants in
      MaterialMeshMeta since object-space positions have arbitrary range.
      Slice order when implemented: (A) UVs unorm16x2 in the custom-attribute
      buffer — most contained but needs per-mesh format flags in meta (UVs
      outside [0,1] can't quantize); (B) normals+tangents snorm8x4 oct in the
      56-byte visibility record; (C) positions snorm16x4 + meta dequant.
      Estimated win: visibility record 56→~32B (~1.75×) on the largest
      geometry pool. Multi-session; do NOT attempt as one loop iteration.
- [x] meshopt decode: bounds/limit checks in our FFI wrapper before handing
      untrusted buffers to the C lib (max vertex/index/byte counts; validate
      mode/filter/stride). Basis worker: limits, watchdog, cancellation, restart,
      leak test (thousands of transcode/encode cycles).
      ✅ 2026-07-14 — meshopt: stride/mode/filter validation + 256MB decoded
      cap existed since Phase 1; added a 256MB COMPRESSED-input cap
      (`MAX_ENCODED_BYTES`) so a hostile container is rejected before the C
      lib sees it. Basis worker: input limits (KTX2 ≤64MB, dimensions ≤16384,
      encode ≤4096² px) with structured `too-large` errors; client-side
      WATCHDOG (`request_timeout_ms`, default 120s) — a hung wasm can't be
      interrupted, so timeout terminates the worker, fails all in-flight
      requests, and the next call respawns (restart-on-fatal already
      existed); cancellation = drop the future (pending entry reaped on
      reply/timeout). LEAK SOAK browser-verified: 1000 transcode+encode
      cycles (`basis-worker-smoke.html?cycles=N`), worker stable, **GC floor
      3.8MB → 3.3MB (Δ −0.5MB)** — measure the GC FLOOR (min of first vs
      last quarter), not instantaneous heap: the sawtooth (105MB peaks →
      15MB reclaims) fails naive thresholds while proving health.
- [x] Golden fixtures: meshopt+quant round-trip (cube / normals+uv / skinned);
      Basis transcode goldens; the two robots as **local** (gitignored) fixtures
      in model-tests / player-tests.
      ✅ 2026-07-14 — always-on round-trip goldens in renderer-gltf
      (`meshopt.rs`): cube (exact corners through the wrapper TRS), grid
      (normals+uv, pre-existing), **skinned** (dequant folds into IBMs, no
      wrapper node, `IBM′×v_quant` reproduces bind-pose positions), plus an
      IBM-less-skin guard test. 🐛 The skinned golden FOUND A REAL BUG:
      `compress_glb` quantized meshes of skins that have NO
      inverseBindMatrices accessor (identity IBMs) with nowhere to fold the
      dequant → corrupt geometry; such meshes now skip quantization. Basis
      transcode goldens: sha-256 of level-0 output for the in-repo Khronos
      fixture baked into `basis-worker-smoke.html` (bc7 159aa349…, astc-4x4
      c3980339…, rgba32 596fd0f7…) — browser-verified MATCH ×3; a mismatch
      after a transcoder re-vendor means outputs changed and goldens must be
      re-baked knowingly. Robots as local fixtures: police (parse + decode
      pass + AABB, since Phase 4) + NEW astrabot decode-pass/accessor-sanity
      test, all fixture-gated via build.rs cfgs (auto-skip when absent;
      never committed). Note: the fixture tests live in renderer-gltf (the
      import pipeline) rather than the model-tests/player-tests browser
      harnesses — same coverage, native speed; the on-device robot runs
      remain the Phase-4/5 acceptance records.
- [ ] Optional: two-channel normals (BC5/EAC-RG) with in-shader Z reconstruction.
      ⏸ DEFERRED 2026-07-14 alongside the GPU-quantized vertex formats — both
      are shader-touching opts of the same family (the ladder + formats
      already support BC5/EAC-RG linear-only; what's missing is the in-shader
      Z reconstruct + per-slot two-channel selection). Bundle them into the
      sliced follow-up above.
- [x] Verify 4–8× texture-memory reduction + bundle geometry shrink; transcode +
      meshopt-decode never on the render hot path; no per-frame allocations added.
      ✅ 2026-07-14 — **Texture VRAM: exactly 4.0×** on the real police
      bundle (78 textures: 63×1024² + 15×512²): 356MB as RGBA8 → 89MB as
      ETC2-RGBA/ASTC-4x4 (both 1 B/px; RGBA8 is 4 B/px; full-mip ×4/3 in
      both). The 8× end of the plan's range needs the deferred opaque-only
      (ETC2-RGB/BC1, 0.5 B/px) and two-channel rungs — our ladder
      deliberately picks RGBA-capable rungs today. Wire size: 362KB WebP →
      67KB ETC1S KTX2 (5.4×) on the kitchen-sink probe.
      **Bundle geometry: 14.3MB → 2.6MB (5.5×)** summed over the police
      scene's 20 static mesh glbs (per-mesh numbers logged at bake).
      **Hot path: clean** — every `decode_buffer_view` /
      `decode_meshopt_buffer_views` call site lives in renderer-gltf's
      load-time buffer import (main + worker paths); every Basis
      `transcode` call site lives in load-time texture decode
      (renderer-gltf ktx2_image, scene-loader texture). The entire
      compression range (17 commits, 63 files) touches ZERO
      render_passes/WGSL/pipeline_scheduler files (`git diff
      9e36120d^..HEAD --stat` audit). **Per-frame allocations: none
      added** — the one per-frame-adjacent edit (skins `update_transforms`
      per-skin IBM lookup) swaps a map-by-joint for map-by-skin + Vec
      index over Copy types; upload paths (`write_gpu_compressed`) are
      dirty-gated load-time.
      ⚠ DoD caveat for sign-off: "desktop + mobile-representative matrix"
      is verified on this machine's bc+etc2+astc adapter + unit-tested cap
      matrices (desktop-BC-only / mobile-ETC2+ASTC / none); no run on
      PHYSICAL mobile hardware yet — `taskfiles/debugging/mobile.yml`
      exists for that when wanted.
- **Exit (Definition of Done):** both robots import+render; player loads KTX2
  compressed + meshopt+quant geometry across a desktop+mobile-representative
  matrix; encode is editor-only, off-main-thread (Basis) / cheap in-Rust
  (meshopt); resources deterministically freed; no sustained leak; malformed input
  fails predictably; no player perf regression.

---

## Files this touches (map)

- Device features / caps: `renderer-core/src/renderer.rs`, `compatibility.rs`.
- Shared block upload + format map (lift from cubemap): `renderer-core/src/cubemap/ktx.rs` → new shared mod; `renderer-core/src/texture/{texture_pool.rs,image.rs,texture.rs}`.
- Player KTX2 load: `scene-loader/src/texture.rs`.
- Import extensions + meshopt decode + quantized accessors + basisu: `renderer-gltf/src/loader.rs`, `buffers/{accessor.rs,attributes.rs,index.rs,mesh.rs}`, `populate/material.rs`; editor `engine/bridge/gltf.rs`.
- Mesh encode (quantize + meshopt) + extension writing: `glb-export/src/{lib.rs,write.rs}`, editor `controller/export.rs`; new `meshopt`/`meshopt-sys` FFI dep + wasm build glue (`build.rs`/allocator shim).
- Texture export/authoring: `editor-protocol/src/{assets.rs,command.rs}`, editor `controller/{export.rs,state.rs}`, `scene_mode/inspector.rs`, `mcp/src/mcp.rs`.
- GPU-quantized vertex formats (Phase 6): `renderer-gltf/src/buffers/mesh/visibility.rs`, mesh-buffer packing in `renderer`.
- Runtime enums (exist): `scene/src/{assets.rs,mesh.rs}`.
- New: `packages/crates/codec-basis`; vendored `web/vendor/basis/`, `web/workers/basis-worker.js`.

## Working rules (from docs/plans/README.md)

- `task lint` + `cargo test --all-features` green at every commit; never weaken a
  test. Update checkboxes per commit; delete the file when done.
- Shader-interface / WGSL edits are runtime-only → always browser-verify. Renderer
  `tracing` surfaces in the BROWSER console, not the editor log buffer.
- Exactly ONE dev task: `task mcp-dev` (editor :9085 + mcp :9086); probe ports
  first; never run `editor-dev` and `mcp-dev` together.
- No player performance regressions, ever; editor-only costs stay editor-only
  (Basis encoder + worker are editor-only; player carries only the small Basis
  transcoder + in-Rust meshopt+quant decode).
- Verify with the real robots — import + screenshot both — before Phase 4 is done.

---

# FOLLOW-UP QUEUE (locked with David, 2026-07-14) — implement in a fresh session

All decisions below are settled; do not relitigate. Working rules at the top of
this file still apply (lint+test green per commit, browser-verify shader
changes, `task mcp-dev`, never commit fixtures/local bytes).

## F1. Bundle export options

Progress:
- [x] Codec layer: `CompressOptions { meshopt, quantization: Off|Always|Smart{threshold_mm} }`
      in glb-export; `compress_glb_with`; plain-view quantize-without-meshopt path
      (normals/tangents → direct i16-normalized since OCT is a meshopt filter);
      Smart demotion by grid step (skin-union aware); `KHR_mesh_quantization` /
      `EXT_meshopt_compression` declared independently; both-off = passthrough.
      Option-matrix roundtrip tests in renderer-gltf.
- [x] `BundleOptions` in editor-protocol (`bundle_options` in project.toml, serde
      defaults); reactive `scene.bundle_options`; `EditorCommand::SetBundleOptions`
      takes a `BundleOptionsPatch` (ShadowsPatch pattern, undoable); wired through
      `bake_player_bundle` — base meshes + rigs + coarse LOD glbs (LOD levels now
      compress at emission; session cache keeps uncompressed levels so option flips
      can't serve stale bytes) + texture Off ⇒ WebP-lossless default.
- [x] MCP `set_bundle_options` + per-call overrides on `export_player_bundle`
      (`Request::ExportPlayerBundle{overrides}` merges the patch onto persisted
      options without modifying them); parity allowlist + docs/mcp-parity.md row.
- [x] Pre-export modal ("Export player bundle…" menu → options modal → dir
      picker; Export click = the picker's user gesture; Smart threshold shown
      only under Smart; persists via SetBundleOptions AND hands the options
      straight to the bake so it can't race the dispatch). DOM-level check
      rides the F4 mcp-dev run.

F1 complete pending F4 on-device verification.

`BundleOptions` in editor-protocol, **persisted in project.toml** (serde
defaults; no back-compat constraints — David), edited via a **pre-export
modal** (options appear when relevant, remembered in the project), plus MCP
`set_bundle_options` tool AND optional per-call overrides on
`export_player_bundle`:

- `mesh_compression: Off | Meshopt` — default `Meshopt`.
- `mesh_quantization: Off | Always | Smart` — default `Smart`.
  - Structural guards (morph targets, multi-skin/mixed-use meshes, IBM-less
    skins, out-of-[0,1] UVs) are CORRECTNESS, not policy — they apply even
    under `Always`.
  - `Smart` = structurally possible AND quantization step (max-half-extent /
    32767, i.e. extent/65534) ≤ `smart_threshold_mm`.
- `smart_threshold_mm: f32` — default `0.1`, advanced field in the modal.
- `texture_compression: Off | Ktx2` — default `Ktx2`. `Off` = lossless WebP
  (pixel-exact), never raw source dumps. Per-texture prefs override the global
  either way (precedence: per-USE override > per-texture pref > slot-based
  Auto > global).
- The knobs are INDEPENDENT: `quantization` without `meshopt` is valid
  (KHR_mesh_quantization alone) — needs a new `compress_glb` path emitting
  quantized accessors into plain views (today quantize+meshopt are one pass).
- Scope: applies to base mesh glbs, bundle rig copies, and **coarse LOD chain
  glbs** — lod1–3 currently ship UNCOMPRESSED and larger than their compressed
  base (police: lod1 504KB vs base 251KB); route them through the same
  strip(no-op)/compress under the same options. The player already decodes
  them via `from_glb_bytes` → decode pass, so no loader work.
  NOT clusters.bin (recorded in docs/nanite-lod.md as its own follow-up).
- Rig stripping (`strip_materials_and_images`) stays UNCONDITIONAL — dead
  bytes, nothing to configure.

## F2. Per-USE-SITE texture encoding (fixes an aliasing bug in Auto)

Progress:
- [x] Data model + resolution + bake: `TextureRef.export_profile`
      (`Option<TextureUseProfile>`, custom-Deserialize extended);
      `MaterialDef::for_each_texture_use_mut` (slot → `TextureColorKind`,
      drift-guarded against `texture_refs()`); pure `resolve_texture_use`
      precedence chain host-tested in editor-protocol; bake walks the BAKED
      scene (built-in inline + custom `texture_overrides` w/ slot kinds from
      the material library), groups uses by (asset, uastc, srgb), primary
      encoding keeps the original id, variants mint DETERMINISTIC ids
      (`AssetId::derive_variant`) + baked asset-table entries, refs rewritten,
      per-use override stripped from the baked doc. Encode sRGB flag is now
      slot-correct per use (MR/occlusion encode linear — was `!normal` per
      asset).
- [x] Inspector: "· Bundle codec" (Inherit/ETC1S/UASTC) row on every bound
      texture slot — built-in core + extension slots (rides `edit_slot`) AND
      custom-material texture overrides; asset inspector's KTX2 mode gains a
      Profile select (Auto/ETC1S/UASTC — was hardcoded Auto).
- [x] MCP: `set_texture_use_profile` tool + `EditorCommand::SetTextureUseProfile`
      (builtin slot names or custom slot names; extension slots via patch_kind
      on the ref's `export_profile`; loud reject when unbound; undoable).
      Parity allowlist + docs/mcp-parity.md row.

F2 complete pending F4 on-device verification.

Noted while implementing (pre-existing, NOT touched): sprite/decal
`Option<TextureRef>` fields are never collected by the bundle bake — their
textures don't ship in bundles at all today. Separate follow-up.

David's case: one texture asset used as a NORMAL map by a PBR material and as
something else by a custom/Dynamic material. Today `Ktx2Profile::Auto` marks
the whole ASSET normal if ANY use is a normal slot — wrong for the other use.

- Profile resolution moves from asset-level to **use-level**: each material
  texture reference resolves (use-site override > per-texture pref >
  slot-based Auto > global). Add an optional profile override on the texture
  reference type (editor-protocol `TextureRef` and the custom-material
  binding equivalent), exposed in the editor at the MATERIAL slot UI and via
  MCP.
- Bake: group uses by `(asset, resolved profile)`; encode ONE artifact per
  distinct pair. Variant artifacts get **minted asset entries in the baked
  asset table** (mirror of the mesh-dedup canonicalization, in reverse) and
  the scene.toml texture refs are rewritten to the variant ids — the player
  needs ZERO new concepts, it just loads `assets/<variant-id>.ktx2` per ref.
  sRGB-per-use already works at bind time (format variant); the CODEC is what
  needs per-use artifacts.
- Inspector: also surface the per-texture profile override (Auto/ETC1S/UASTC)
  — MCP has the modes, the inspector currently only offers "KTX2 (auto)".
- Two-channel normals (F3) rides this same per-use machinery (normal uses →
  two-channel encode).

## F3. Two-channel normals (locked in — quality win, ~zero cost)

Progress:
- [x] Implemented end-to-end; MUST browser-verify in F4 (shader change; no
      offline WGSL validation exists for the built-in templates).
  - Encode: CPU-side X→RGB/Y→A swizzle in the bake (vendored encoder JS has
    no swizzle API); only BUILT-IN normal-slot uses pack (custom-WGSL
    materials sample with user code that can't Z-reconstruct; anisotropy
    direction maps re-kinded to MetallicRoughness so they never pack). The
    packed bit joins the F2 grouping key → its own variant artifact.
  - Runtime: `AssetEntry.texture_two_channel_normal` (bake-set, only when the
    packed KTX2 encode actually shipped — fallbacks/passthrough stay false) →
    TextureCache seeds it → `select_normal_transcode_target[_checked]`
    (BC5 / EAC-RG11 / regular ladder fallback, host-tested). PREFETCH already
    picks the right target (per-artifact flag makes the planned bind-time
    deferral unnecessary — the artifact IS the use, post-F2).
  - Shader: `PbrMaterial.normal_packing` u32 (bits 0-1 main, 2-3 clearcoat;
    per pair 0=RGB, 1=.rg two-plane, 2=.r/.a packed RGBA), core header word
    40, PBR_CORE_WORDS 40→41; shared unpack helper in BOTH color-calc paths
    (opaque compute + transparent forward, main + clearcoat) reconstructs
    z = sqrt(1-x²-y²). No vertex-interface change (flag rides the material
    storage buffer).

Encode side: normal-use textures pack X→RGB, Y→A (CPU-side swizzle before the
worker encode if the vendored encoder JS lacks setSwizzle), UASTC, linear.
Ladder: new `select_normal_transcode_target` — BC5 (bc caps) / EAC-RG11
(etc2) / fall back to today's full-RGB path (astc/rgba32). Runtime: normal-slot
transcode happens at BIND time (slot semantics known there; prefetch keeps raw
ktx2 bytes for normal-flagged uses) with a per-material two-channel flag into
the shader; WGSL Z-reconstruct (`z = sqrt(1 - x² - y²)`, exact for unit
normals). SHADER-INTERFACE CHANGE → full browser verification required.

## F4. Combined on-device verification (one mcp-dev run at the end)

Export the police scene with default options → verify: rig size (was 27.9MB,
strip+compress landed but unmeasured — expect ~3-4MB), LOD glbs now compressed,
options matrix spot-checks (quantization Off ⇒ F32 accessors; compression Off ⇒
no meshopt views; texture Off ⇒ webp), per-use texture variants in the bundle,
two-channel normals rendering correctly on both robots (screenshots).

- [x] DONE (2026-07-14, task mcp-dev on :9085/:9186). Results:
  - **Caught + fixed a real bug**: rigs declaring `KHR_texture_basisu` in
    extensionsRequired failed glb-export's strict parse → strip+compress
    silently fell back, shipping the 29.3MB original since the rig-strip
    commit. Fix: `parse_glb_lenient` in glb-export (tolerated-required
    retain + basisu source lift + re-validate, declarations preserved);
    regression test injects the extension and asserts the pipeline shrinks.
  - Rig sizes after fix: police 29.3MB → **3.84MB**, astrabot 27.6MB →
    **3.48MB**; whole two-robot bundle 120.7MB → 71.1MB.
  - LOD glbs compressed: all 51 declare meshopt+quantization; police lod1
    142KB (shipped 504KB uncompressed pre-F1).
  - Options matrix (per-call overrides): quant-off ⇒ F32 POSITIONs +
    EXT_meshopt only (rig 9.98MB); meshopt-off ⇒ I16 plain views +
    KHR_mesh_quantization only (15.3MB — plain path proven on a real rig);
    tex-off ⇒ raster sources → webp, KTX2 sources passthrough (can't
    re-encode a lossy container losslessly — shipped verbatim, logged), and
    a per-use override still forces its KTX2 variant.
  - F2 on-device: mixed-use asset → packed-normal artifact kept the original
    id + `texture_two_channel_normal = true`; base-color use with the MCP
    per-use uastc override → deterministic minted variant, ref rewritten.
  - F3 on-device: console shows `binding compressed texture as Bc5RgUnorm`;
    sphere A/B (editor classic RGB vs player BC5 two-channel), 1:1 crops:
    mean px diff 1.31/255, 96% within 8/255 — Z-reconstruct correct, and
    NOT pixel-identical (different codecs should differ). Both robots
    screenshot correct through the player path (their ktx2-SOURCE normal
    maps are passthrough by design ⇒ flag=0 classic regime — two-channel
    engages for raster-sourced normal maps, verified via the test sphere).
  - Noted (pre-existing, not fixed here): (a) loading BOTH robots + full LOD
    chains through load_player_bundle trips the dev-only 512MB
    `debug_assert` in create_buffer (mesh pool doubles to 1GiB) — release
    proceeds; consider raising the dev threshold or warn-only. (b)
    `set_node_texture`/`set_builtin_param` silently no-op ("ok") when the
    node has a material palette but NO SELECTED variant —
    `set_texture_use_profile`'s loud reject exposed it; the older tools
    should probably reject loudly too.

FOLLOW-UP QUEUE COMPLETE.

## F5. gltfpack parity (added 2026-07-14, David)

Goal: importing a RAW export (`fixtures/local/astrabot-large.glb`, 188MB) and
bundling from the editor should get CLOSE to the gltfpack artifact
(`astrabot-meshopt.glb`, 14.46MB total) — comparing bundle TOTALS (our
textures ship as separate assets).

Diagnosis (per-stream, astrabot): our POSITION/TEXCOORD/NORMAL/INDICES bytes
are ALREADY byte-equal to gltfpack's. The former 27% rig-size gap was:
- WEIGHTS shipped f32 (1.015MB vs gltfpack's 0.258MB u8-normalized) — 70%.
- TANGENT +0.26MB — legitimate new data (MikkTSpace at import; source has none).
- JOINTS u16 vs u8, IBM 0.133 vs 0.081 (likely per-skin IBM duplication) — minor.

Pieces:
- [x] Quantize skin WEIGHTS → u8-normalized (per-vertex renormalized to sum
      255) + JOINTS → u8 when max joint < 256. CORE glTF component types (no
      new extension). Gated on `mesh_quantization != Off`. Player skin path
      already accepts u8/u16/u32 joints + u8/u16-normalized/f32 weights.
- [x] Pre-encode meshopt optimization passes (`reorder_primitives` in
      compress.rs): optimizeVertexCache → optimizeOverdraw (1.05) →
      optimizeVertexFetch remap applied to EVERY per-vertex stream of the
      primitive (attributes + morph targets) + rewritten indices. No-op on
      already-optimized (gltfpack-sourced) input; the win is raw exports.
      Skips: fetch-compaction that would drop unused vertices, shared-accessor
      primitives, non-triangle modes, non-f32 positions. Roundtrip tests
      rewritten order-insensitively (pair-by-position + triangle multisets).
- [x] Native fixture-gated parity test (`raw_export_compresses_close_to_gltfpack`,
      gated on astrabot-large + astrabot-meshopt): 188MB raw export →
      reexport_clean_scene → strip+compress = **2.547MB vs gltfpack's 2.409MB
      geometry — ratio 1.057** (asserted ≤ 1.25). GOAL MET. Editor adds
      MikkTSpace tangents on top (+~0.26MB, data gltfpack doesn't ship).
- [ ] (deferred candidates) per-skin IBM dedup (0.133 vs 0.081MB — most of
      the remaining 5.7%); quantized-passthrough for unedited imported meshes
      (zero-generation geometry — would also carry source tangent-free
      encoding verbatim).

## Closed / not queued

- **GPU-quantized vertex formats: CLOSED, not approved.** Dual-layout is dead
  (per-pixel mesh resolution in deferred material passes ⇒ per-mesh format
  flags are genuinely divergent per-invocation; fixed-function variant would
  double pipeline permutations). The only surviving variant — ONE canonical
  layout with oct16 normals/tangents (56→40B, max angular error ~0.002°,
  measurably nonzero) — was presented and NOT approved under the strict
  zero-loss bar. Revive only with David's explicit sign-off on that error.
- Wire quantization stays (bundle-time only; project saves are the lossless
  f32 master; one grid application per bake, never accumulating). The `Smart`
  mode IS the agreed guard for large-extent meshes.
