# Media Server Guidelines

This renderer fetches glTF assets (`.gltf` JSON + external `.bin` buffers +
external image textures + optional `.ktx2` compressed textures) over HTTP
from whatever origin you point it at. The defaults in the
`media_base_url_*` env vars assume a plain static-file server.

The rendering hot path is bandwidth- and latency-sensitive on cold loads,
but the renderer does no special HTTP gymnastics — no range requests, no
custom headers. Any compliant HTTP/1.1+ static server will work. What
follows are the settings that materially affect first-load wall-clock time.

## What the renderer fetches

For one glTF scene the loader issues, roughly in parallel:

- One `.gltf` (JSON) request.
- One request per `.bin` buffer referenced by the `.gltf`.
- One request per image referenced by the materials (PNG / JPG / KTX2).

Image requests are fired concurrently via `try_join_all`, so HTTP/2 or
HTTP/3 multiplexing pays off — you'll see all image fetches go out
together rather than serialising on a connection limit.

## Compression

**Do not gzip image binaries.** PNG, JPG, WebP, and KTX2 already contain
compressed payloads. Gzipping them again typically saves <1 % of bytes
while costing real CPU on the server and (for cold connections) blocks
parallel fetches behind whatever single-threaded compressor the dev
server is using. We removed `--gzip` from our local dev `Taskfile` for
exactly this reason — first-load wall-clock dropped noticeably.

**Do gzip text-like assets.** The `.gltf` JSON file and any uncompressed
`.bin` vertex/index payloads are highly compressible. Enable gzip for:

- `application/json` (or `model/gltf+json`)
- `application/octet-stream` **only if** you know your `.bin` payloads
  are not already pre-compressed (e.g. EXT_meshopt_compression). If
  meshopt is involved, leave `.bin` uncompressed at the HTTP layer.

The cheapest correct policy: gzip text MIME types only, leave everything
else alone.

Brotli is similar to gzip — apply the same logic (text yes, images no).

## CORS

The model-tests and scene-editor frontends serve from a different origin
than the media. The fetch will fail without:

```
Access-Control-Allow-Origin: *
```

(Or whatever specific origins you serve from in production — `*` is fine
for static read-only media.) For preflight, also allow:

```
Access-Control-Allow-Methods: GET, HEAD
Access-Control-Allow-Headers: Origin, Content-Length, Content-Type
```

Our dev server (`http-server --cors`) sets these automatically.

## Cache headers

The browser's HTTP cache is the single biggest factor in repeat-load
speed; users who reload after an asset is cached skip the network
entirely. A simple long-lived policy:

```
Cache-Control: public, max-age=31536000, immutable
```

This is correct **only** for content-addressed asset paths (i.e. URLs
that change when the bytes change — e.g. a hash in the filename). For
mutable paths like `Models/DamagedHelmet/glTF/DamagedHelmet.gltf`,
prefer something shorter so users see asset updates:

```
Cache-Control: public, max-age=300, must-revalidate
ETag: "<strong-etag>"
```

`ETag` + `Last-Modified` are essential — a 304 response is nearly free
and avoids resending megabytes of texture data on every load.

## HTTP version

HTTP/2 or HTTP/3 is strongly preferred. The renderer regularly fans out
10–50 parallel image requests for a single scene; HTTP/1.1's per-host
connection limit (typically 6) will serialise them. Any modern CDN
fronts HTTP/2 by default.

## Range requests

Not used. Don't bother enabling/optimising for `Range:` requests for
glTF/image fetches — the renderer reads each asset whole.

## Don't transform images on the fly

Some hosts (Vercel, Netlify, certain Cloudflare configurations) auto-
optimise images — converting JPG to WebP, downsampling, stripping
metadata. For glTF assets this is harmful: the textures are part of the
material's authored appearance and re-encoding them changes color
fidelity. For KTX2 it's also catastrophic, because the file is a
GPU-block-compressed container; any transform will corrupt it.

Disable any "image optimisation" / "polish" feature on the bucket or
origin that hosts the model textures.

---

## Cloudflare R2 + Cache

R2 is a good fit: cheap egress (free to Cloudflare network), automatic
CDN caching, no per-request markup over standard S3-style bucket access.
Recommended setup:

### Bucket settings

- **Object naming**: name immutable objects with a content hash in the
  path so you can apply a far-future `Cache-Control: immutable`.
  Mutable objects (the canonical sample assets, for example) keep
  shorter cache lifetimes.
- **CORS rules**: in the R2 dashboard → `Settings` → `CORS Policy`,
  add a single permissive read rule:

  ```json
  [
    {
      "AllowedOrigins": ["*"],
      "AllowedMethods": ["GET", "HEAD"],
      "AllowedHeaders": ["*"],
      "MaxAgeSeconds": 86400
    }
  ]
  ```

  Narrow `AllowedOrigins` if the bucket is private to your sites.

### Custom domain + Cache Rules

R2's `*.r2.dev` URL is **rate-limited and uncached** — never serve
production traffic from it. Bind a custom domain (e.g.
`media.example.com`) via Cloudflare's R2 → Settings → "Connect Domain";
this routes through the full Cloudflare cache.

In the dashboard zone for that domain, under **Caching → Cache Rules**,
add a rule that targets your media path and forces caching:

- **When**: `(http.request.uri.path matches "^/.*\.(png|jpg|jpeg|webp|ktx2|bin|gltf|glb)$")`
- **Then**:
  - **Cache Eligibility**: *Eligible for cache*
  - **Edge TTL**: *Override origin*, value 1 year (`31536000` s)
  - **Browser TTL**: *Override origin*, value matching your asset
    mutability (long for content-hashed paths, short — e.g. 5 min — for
    mutable paths)

R2 doesn't set strong `Cache-Control` headers on its own, so the
override above is what actually puts assets in the edge cache. Without
it you'll see hits go to origin (R2 storage) repeatedly.

### Compression rules

Cloudflare's automatic Brotli compression is **on by default** for a
configured set of content types (HTML, CSS, JS, JSON, SVG, etc.) and
does NOT touch image binaries. That's exactly the policy this renderer
wants — leave the default in place. If you have set up a custom
Compression Rule that lists `image/*`, remove it.

### Disable image transforms

In the Cloudflare zone settings:

- **Speed → Optimization → Image Resizing**: leave **off** for the
  media zone.
- **Speed → Optimization → Polish**: **off**.
- **Speed → Optimization → Mirage**: **off**.
- **Speed → Optimization → WebP**: **off**.

These are useful for HTML-embedded site images, but they will alter the
bytes the renderer receives and break visual fidelity (and KTX2 files
outright).

### HTTPS / HTTP version

Cloudflare's edge speaks HTTP/2 and HTTP/3 by default. Verify under
**Network** that "HTTP/2" and "HTTP/3 (with QUIC)" are both enabled.
This unlocks the parallel-image-fetch behaviour described above.

### Verifying the setup

After deploying, fetch one of your texture URLs and inspect the headers:

```bash
curl -sI 'https://media.example.com/path/to/texture.png' | grep -iE 'cache-control|cf-cache-status|content-encoding|content-type'
```

You want to see:

- `cf-cache-status: HIT` (after one warm-up request)
- `content-type: image/png` (or whatever's appropriate)
- **No** `content-encoding: gzip` / `br` on image responses
- `cache-control` reflecting the TTL from your Cache Rule

If `cf-cache-status` is `DYNAMIC` or `BYPASS`, your Cache Rule isn't
matching — re-check the URI pattern.

---

## Summary checklist

- [ ] No gzip / brotli on image binaries or KTX2.
- [ ] gzip enabled for `.gltf` (JSON) and uncompressed `.bin`.
- [ ] CORS allows `GET`/`HEAD` from your frontend origin(s).
- [ ] HTTP/2 or HTTP/3 enabled at the edge.
- [ ] `Cache-Control` with `ETag` set; long TTL for hashed paths, short
      TTL + revalidation for mutable paths.
- [ ] Any "auto image optimisation" feature disabled.
- [ ] R2: bound to a custom domain (not `*.r2.dev`) with a Cache Rule
      forcing edge caching for media file extensions.
