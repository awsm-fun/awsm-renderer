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

- [ ] Mesh encode (editor bake, pure-Rust): quantize attributes (positions→short,
      normals/tangents→octahedral, UVs→short; dequant transform into node TRS /
      IBMs / tex-transform) → meshopt-encode. `glb-export` writes
      `KHR_mesh_quantization` accessors + `EXT_meshopt_compression` bufferViews
      (+ `fallback:true` buffer), both in `extensionsRequired`. Player/editor
      decode via the Phase-4 path — round-trip must be lossless within the chosen
      quantization tolerance.
- [ ] Textures — authoring: `TextureExport::Ktx2 { profile }` + KTX2
      source-passthrough (`editor-protocol/src/assets.rs`); inspector option +
      `dispatch_texture_export` (`scene_mode/inspector.rs`); MCP
      `set_texture_export`. Bake arm (`editor/src/controller/export.rs` ~243):
      encode via Basis encoder worker — **ETC1S color / UASTC normal** by
      material-slot + color-space; record `TextureEncoding::Ktx2`; passthrough
      ships original KTX2 verbatim; non-4-multiple → WebP-lossless + `log()`. Make
      `Ktx2` the default when `texture_export` is `None` (document re-bake).
- **Exit:** export a scene (meshopt+quant + KTX2 defaults) → load in player →
  matches editor; imported-KTX2 passthrough round-trips byte-identical;
  round-trip/golden tests green.

## Phase 6 — Hardening, tests, perf, GPU-quantized formats

- [ ] **True-first-class GPU optimization:** keep quantized vertex formats through
      the visibility/geometry packing (WebGPU `unorm/snorm 8/16`) instead of
      expanding to f32 — measure the VRAM win; browser-verify the vertex-format
      change.
- [ ] meshopt decode: bounds/limit checks in our FFI wrapper before handing
      untrusted buffers to the C lib (max vertex/index/byte counts; validate
      mode/filter/stride). Basis worker: limits, watchdog, cancellation, restart,
      leak test (thousands of transcode/encode cycles).
- [ ] Golden fixtures: meshopt+quant round-trip (cube / normals+uv / skinned);
      Basis transcode goldens; the two robots as **local** (gitignored) fixtures
      in model-tests / player-tests.
- [ ] Optional: two-channel normals (BC5/EAC-RG) with in-shader Z reconstruction.
- [ ] Verify 4–8× texture-memory reduction + bundle geometry shrink; transcode +
      meshopt-decode never on the render hot path; no per-frame allocations added.
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
