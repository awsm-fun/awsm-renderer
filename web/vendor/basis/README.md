# Vendored Basis Universal codec modules

Prebuilt Emscripten builds of the Basis Universal **transcoder** (KTX2/Basis →
GPU block formats, ships in editor **and** player) and **encoder** (RGBA →
Basis-supercompressed KTX2, **editor-only** bake path). Provenance, versions,
and SHA-256 hashes: `BUILD-METADATA.json`. Licenses vendored alongside.

- Non-pthread builds → no COOP/COEP requirement anywhere.
- The **transcoder** is an eval-free build (no `eval` / `new Function`): a
  player can serve it under a CSP with `'wasm-unsafe-eval'` and without
  `'unsafe-eval'`. Its wasm memory is capped at 256 MiB. It is built from
  pinned upstream sources by `rebuild-transcoder.sh`, which reproduces both
  files byte-for-byte and checks their hashes. The **encoder** is still
  upstream's prebuilt and does use `new Function`, so the editor cannot drop
  `'unsafe-eval'` (it serves no CSP today).
- The transcoder is built with `BASISD_SUPPORT_ETC2_EAC_RG11=1` (upstream's
  web build has it off). That is what lets an ETC1S texture transcode to EAC
  RG11, the target the renderer picks for two-channel-packed normal maps on
  ETC2-only GPUs. Keep it on when rebuilding.
- The transcoder is also built **without the formats the renderer never
  uses**: UASTC HDR, XUASTC LDR (with the ASTC LDR / XUBC7 sources that come
  with it) and PVRTC1 are compiled out, which halves the `.wasm` (1,061,651 →
  552,629 bytes; 360,650 → 209,607 with brotli). What is left is everything
  the worker can ask for: ETC1S and UASTC LDR sources (BasisLZ, Zstandard or
  no supercompression) to its nine LDR targets, byte-identical to the
  untrimmed build. Upstream's switches do not compile when turned off at the
  pinned commit, so `basisu-trim.patch` (`#if` guards only) is applied first;
  `rebuild-transcoder.sh` checks its hash and applies it. To get one of those
  formats back, drop the matching `-DBASISD_SUPPORT_…=0` in the script and
  widen `sniff_basis_ktx2` in `codec-basis`, which refuses any
  supercompression scheme other than those three.
- Ship the transcoder together with `web/workers/basis-worker.js` from the
  same revision: the worker keeps ETC1S → BC7 output identical to the previous
  transcoder and recovers from a wasm abort (see the notes in
  `BUILD-METADATA.json`).
- Both `*.js` files are `MODULARIZE` UMD builds with the **same** global
  factory name `BASIS`. They are hosted in Web Workers (isolated scopes) by
  `web/workers/basis-worker.js`; if you ever load them in one page (see
  `smoke-test.html`) capture and delete the global between loads.
- Apps pick these up via `data-trunk rel="copy-file"` links in each app's
  `index.html`, landing at `vendor/basis/` under each dist. The encoder pair is
  copied by the **editor only** — keep it that way (editor-only costs stay
  editor-only).
- Production cache policy: these files are immutable per content-hash in
  `BUILD-METADATA.json`; long-lived `Cache-Control: immutable` headers should be
  set at the Cloudflare Pages layer when this ships.

`smoke-test.html` (Phase-0 exit check) instantiates both modules standalone:
serve this directory (`npx http-server -p <port> web/vendor/basis`) and open it.
