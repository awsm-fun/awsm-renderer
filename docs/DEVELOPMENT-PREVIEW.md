# Renderer development with the preview browser — practical workflow

Companion to [`DEBUGGING-PREVIEW.md`](DEBUGGING-PREVIEW.md). That doc covers
the **don't trust your eyes on the screenshot** rule and the
`getImageData`-based ground-truth pattern. This doc captures the
**workflow infrastructure** lessons from longer debugging sessions —
pipeline-rebuild loops, branch-vs-branch comparison, multi-step
diagnostic shaders, scratch-file hygiene. Read both before sitting
down to a multi-hour renderer debug.

## Quantitative branch-vs-branch comparison

The single highest-value methodology when an artefact is "visibly worse
than main but I can't tell exactly how": **capture the same frame on
both branches, diff the RGBA buffers, look at where the diffs cluster.**

### Required infrastructure

The MCP `preview_screenshot` returns a compressed JPEG — not pixel-
faithful. Two MCP-bounded ways to get the raw canvas pixels:

1. **`canvas.getImageData` via `preview_eval`** for a handful of
   coordinates. Returns RGBA as a JS array; works fine inline.
   Limit: the eval-result size cap (~200 KB) clips before a full
   1080p buffer fits.

2. **`canvas.toBlob` → `fetch` POST to a local listener** for the
   whole canvas. Spin up a tiny `http.server`-based receiver and have
   the eval snippet POST the PNG. This is the only way to get the
   full backing buffer out of the preview without truncation.

   Sample receiver — runs on port 9999, writes blob to
   `OUT_DIR/<X-Filename header>`:

   ```python
   #!/usr/bin/env python3
   import os
   from http.server import BaseHTTPRequestHandler, HTTPServer

   OUT = "/tmp/scratch/snapshots"
   os.makedirs(OUT, exist_ok=True)

   class H(BaseHTTPRequestHandler):
       def do_POST(self):
           n = int(self.headers.get("Content-Length", "0"))
           body = self.rfile.read(n)
           name = self.headers.get("X-Filename", "blob.bin").replace("/", "_")
           open(os.path.join(OUT, name), "wb").write(body)
           self.send_response(200)
           self.send_header("Access-Control-Allow-Origin", "*")
           self.send_header("Access-Control-Allow-Headers", "X-Filename,Content-Type")
           self.end_headers()
           self.wfile.write(b"ok\n")
       def do_OPTIONS(self):
           self.send_response(204)
           self.send_header("Access-Control-Allow-Origin", "*")
           self.send_header("Access-Control-Allow-Methods", "POST,OPTIONS")
           self.send_header("Access-Control-Allow-Headers", "X-Filename,Content-Type")
           self.end_headers()
       def log_message(self, *args): pass

   HTTPServer(("127.0.0.1", 9999), H).serve_forever()
   ```

   Eval-side snippet to capture + upload:

   ```js
   new Promise(r => requestAnimationFrame(async () => {
     const c = document.querySelector('canvas');
     const off = document.createElement('canvas');
     off.width = c.width; off.height = c.height;
     off.getContext('2d').drawImage(c, 0, 0);
     off.toBlob(async (b) => {
       const resp = await fetch('http://127.0.0.1:9999/', {
         method: 'POST',
         headers: { 'X-Filename': 'branch_zoomed.png' },
         body: b,
       });
       r({ status: resp.status, w: c.width, h: c.height });
     }, 'image/png');
   }))
   ```

### Animation must be frozen for the comparison to be valid

If the scene has live morph / animation, capturing one branch and then
the other will sample different frames. Morph drift produces 200+/255
silhouette deltas that swamp any real rendering difference. **Pin the
animation on BOTH branches before capturing.**

For `model-tests`, patch `fire_raf` in
`crates/frontend/model-tests/src/pages/app/scene.rs`:

```rust
if let Some(last_timestamp) = state.last_request_animation_frame.get() {
    let _time_delta_real = timestamp - last_timestamp;
    let time_delta = 0.0;        // DEBUG-PIN: freeze morph state
    if let Err(err) = state.update_all(time_delta).await { /* ... */ }
    ...
}
```

This passes `0.0` for the time-delta every frame, so morph weights
don't advance from their initial state. **Apply on BOTH the comparison
branch AND `main` before capturing.** Drop the patch before any
commit lands — it's debug-only.

### Canvas backing size must match

`canvas.width / canvas.height` is the buffer size; `getBoundingClientRect`
gives the CSS size; their ratio is `devicePixelRatio`. The MCP preview
sometimes opens at different sizes between sessions (CSS 344×517,
DPR 2 → buffer 689×1034; or CSS 1138×1360, DPR 1 → buffer 1138×1360).
Pixel-coordinate-keyed diffs are **only valid between captures from
the same browser instance** (same canvas backing dimensions).

Sanity check inside the eval before captures:

```js
(() => ({ w: document.querySelector('canvas').width,
          h: document.querySelector('canvas').height,
          dpr: devicePixelRatio }))()
```

### Cherry-picking debug helpers onto `main`

Diagnostic-only commits that landed on the feature branch (e.g. the
model-tests `?cam=…` URL helper in commit `6988b7c`) often need to
also be applied to `main` so the comparison capture uses the exact
same camera. Pattern:

```sh
git checkout main
git cherry-pick <helper-commit>
# capture main_zoomed.png
git reset --hard origin/main   # drop the cherry-pick
```

Don't push the temporary `main` commit — `git reset` after capturing.

## Multi-step diagnostic shader colours

When you can't tell where in a multi-pass pipeline a value goes wrong,
inject **binary high-contrast colours** at each stage and read pixels
back via `getImageData`. Each step narrows the bug location by one
function or one dispatch.

The pattern that worked for the Stage 3 MSAA debug:

| DIAG | Edit                                                                            | What it tells you                                                |
|------|---------------------------------------------------------------------------------|------------------------------------------------------------------|
| A    | `final_blend.wgsl`: `textureStore(opaque_tex, coords, vec4(0,1,1,1)); return;`  | Does classify gate fire at the broken pixel? (cyan = yes)        |
| B    | `shade_sample` top: `return vec4(1,0,0,1);`                                     | Does dispatch chain reach the output? (red = yes)                |
| C    | Each early-return uses a distinct colour (green/blue/yellow/red)                | Which path is shade_sample taking? (single dominant colour wins) |
| D    | Just before final write: `return vec4(mat_color.base.rgb, 1.0);`                | Is bug in lighting or upstream? (dark = upstream)                |
| E    | Constant red in PBR shade; constant green in skybox shade                       | What's the per-shader-bucket / skybox sample ratio?              |
| G    | `return vec4(material.base_color_factor.rgb, 1.0);`                             | Is bug in mat-data lookup or texture sample? (white = texture)   |
| I    | Sentinel magenta `vec4(1,0,1,1)` in `texture_pool_sample_grad`'s default branch | Did the texture sample fall into out-of-range default?           |

For each DIAG: edit one shader, wait for trunk rebuild
(`applying new distribution` in the dev.log), reload page, sample 3-5
known-broken pixel coordinates via `getImageData`. Each step:

* If pixel reads the diagnostic colour → that path executes / fires.
* If pixel reads something else → the broken value comes from earlier.

**Always revert each DIAG before adding the next** so the test isolates
one variable.

### sRGB / tonemap quirks to expect

The opaque target is HDR (`rgba16float`), and the final framebuffer
goes through tonemapping + gamma. A linear `(1.0, 0.0, 0.0)` written
into `opaque_tex` does NOT read back as `(255, 0, 0)` via `getImageData`
— typically `(225, 8, 0)` once the tone curve and any compositing
attenuation applies. Plan for ±20/255 wiggle on each channel when
identifying "is this pixel ~the diagnostic colour"; only the
**dominant channel pattern** is reliable.

## Trunk rebuild flow

`trunk serve --watch` re-emits an `applying new distribution` line in
the dev log whenever it ships a new wasm bundle. Use it as the rebuild
signal, not a wall-clock guess:

```sh
prev=$(grep -c "applying new distribution" /tmp/scratch/dev.log)
while true; do
  cur=$(grep -c "applying new distribution" /tmp/scratch/dev.log)
  if [ "$cur" -gt "$prev" ]; then echo "rebuild-done"; break; fi
  sleep 2
done
```

After the rebuild lands, the page must be **hard-reloaded**
(`window.location.reload(true)`); a soft reload re-uses the previous
wasm. After hard-reload, the per-shader-id edge_resolve pipelines take
~60 s to compile on the test M-series machine. Look for the
`pipeline_scheduler::subcompile-complete` log lines (or the per-pipeline
`compute:Material Opaque:PipelineLayoutKey(N) cum=Xms ok` Lessons-A
lines) before sampling pixels — sampling before compile completes
gives stale-frame artefacts.

## When the bug spans two pipelines

If a value is written by pipeline A and read by pipeline B, both have
to agree on the bind-group layout, the texture format, and any
shader-template substitutions used to specialize their compiled SPIR-V.
The Stage 3 silhouette bug was exactly this: `material_opaque`'s primary
pipeline templated `texture_pool_arrays_len` correctly at
`finalize_gpu_textures`, but `edge_pipelines` kept its boot-time value
(usually 0) and never re-templated. Per-sample texture lookups in
edge_resolve dropped into the `default → vec4(0)` branch.

When chasing "A and B should produce the same output but B is wrong",
check:

* Both bind-group layouts agree on every binding's shape (texture vs
  storage, sampleable vs storage-write, multisampled flag, format).
* Both shaders are compiled with the same template substitutions for
  any value derived from runtime state (`texture_pool_arrays_len`,
  `texture_pool_samplers_len`, `msaa_sample_count`, `bucket_entries`).
* Any recompile path (texture-pool grow, MSAA flip, dynamic material
  register) recompiles BOTH A and B in lock-step, not just A.

## Scratch-file hygiene

Multi-hour debug sessions accumulate:

* `/tmp/scratch/` PNG snapshots (gigabytes if not pruned).
* Diff scripts (`diff.py`, `probe.js`, …) that should NOT land in
  the repo.
* A localhost upload server that needs killing at shutdown.

Convention: confine all scratch artefacts to `/tmp/scratch/` (or a
named sibling like `/tmp/msaa-work/`). Never write inside the repo
unless you intend the file to be committed. At session end:

```sh
# Stop local servers
pkill -f /tmp/scratch/upload.py
# Wipe scratch
rm -rf /tmp/scratch

# Confirm preview MCP is not still holding ports
mcp__Claude_Preview__preview_list
mcp__Claude_Preview__preview_stop <id>
```

The dev.log can usually be archived (move into the scratch dir before
wiping) if a future session might want to grep for compile timing.

## Capturing a baseline that survives a fix

Before changing anything, capture a "before" state — at minimum:

* `branch_zoomed.png` of the current view via `canvas.toBlob` → upload.
* `git rev-parse HEAD` so you can later diff against the same commit.
* A note in your scratch dir of which specific pixels you're targeting
  (`x, y, current value, target value`).

After landing the fix, capture again and compute per-pixel max
RGBA delta. Cluster the still-differing pixels by location to
identify any residual issues:

```python
from PIL import Image
import numpy as np
b = np.asarray(Image.open('branch_post_fix.png').convert('RGB'), dtype=np.int16)
m = np.asarray(Image.open('main.png').convert('RGB'), dtype=np.int16)
d = np.max(np.abs(b - m), axis=2)
print(f'mean={d.mean():.3f}, max={d.max()}, p99={np.percentile(d,99):.0f}')
print(f'pixels >5: {(d>=5).sum()}/{d.size}')
# Cluster remaining diffs
big = np.argwhere(d >= 20)
ys, xs = big[:,0], big[:,1]
if len(ys):
    print(f'bbox: y[{ys.min()}..{ys.max()}] x[{xs.min()}..{xs.max()}]')
```

For the Stage 3 fix this revealed two separate problems: the first
fix dropped 510 → 55 differing pixels; the residual 55 clustered in a
single diagonal stripe (y=636-640) that turned out to be a second
distinct bug (missing normal-discontinuity edge detection).
