# Vendored Basis Universal codec modules

Prebuilt Emscripten builds of the Basis Universal **transcoder** (KTX2/Basis →
GPU block formats, ships in editor **and** player) and **encoder** (RGBA →
Basis-supercompressed KTX2, **editor-only** bake path). Provenance, versions,
and SHA-256 hashes: `BUILD-METADATA.json`. Licenses vendored alongside.

- Non-pthread builds → no COOP/COEP requirement anywhere.
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
