# Debugging the renderer via the preview browser

This is a **load-bearing operational document**. If you are about to
debug a rendering issue, read this completely before you take a single
screenshot. The methodology below is the consolidated lessons from
multiple multi-hour MSAA / shader / light-culling debugging sessions.

---

## Tooling — use real Chrome via the extension, NOT the in-app preview

**Best practice (do this):** drive a **real Chrome window** through the
Claude-in-Chrome extension (`mcp__Claude_in_Chrome__*`) and run the dev
server as a plain background process (`trunk serve …` via the shell), not
via `mcp__Claude_Preview__preview_start`.

**Do NOT use `mcp__Claude_Preview__*` for this renderer.** Those tools
render the WebGPU canvas inside the **Claude app's embedded webview**, and
a heavy WebGPU scene there will **take the Claude app down** (it crashed
repeatedly across a debugging session — first misdiagnosed as a GPU/driver
wedge, then as machine instability; the real cause was the in-app webview
plus oversized tool outputs). Real Chrome is a separate process, so the
renderer load never touches the Claude app.

### Setup

1. Start the server in the shell (background), e.g.:
   `cd crates/frontend/scene-editor && … trunk serve --port 9090 …`
   Wait for `applying new distribution` in the log before loading.
2. `mcp__Claude_in_Chrome__list_connected_browsers` → `select_browser`.
3. `mcp__Claude_in_Chrome__tabs_context_mcp { createIfEmpty: true }` →
   `navigate` your tab to `http://localhost:9090/`. Use **one** tab and
   reuse it; don't churn tabs (it creates new windows and loses focus).
4. Drive it with `javascript_tool` (eval), `read_console_messages`
   (always pass a `pattern`), and the pixel-capture recipes below.

### ⚠️ The tab must be VISIBLE — `requestAnimationFrame` pauses when hidden

This is the single biggest gotcha with real Chrome and it will waste your
time if you don't know it:

> Chrome **pauses `requestAnimationFrame`** for any tab that isn't the
> **foreground, active tab of a non-minimized window**. The renderer's
> draw loop is rAF-driven, so a hidden/backgrounded tab **stops rendering
> entirely** — the canvas goes to its default 300×150 unrendered state,
> and any capture wrapped in `requestAnimationFrame` **never resolves**
> (you get a 45 s CDP `Runtime.evaluate` timeout that *looks* like a hang
> or a wedged GPU but is neither).

Concretely, the tab reports `hidden` when **either**: it's not the active
tab in its window, **or** its Chrome window is minimized / fully occluded
behind another app (e.g. the Claude desktop app in front of it). Driving
the tab over CDP does **not** bring it to the foreground.

**Always confirm visibility before capturing:**

```js
new Promise(r => requestAnimationFrame(() => {})) // never resolves if hidden
// Instead, probe without depending on rAF:
new Promise(res => { let f=false; requestAnimationFrame(()=>{f=true;});
  setTimeout(()=>res({vis: document.visibilityState, rafFired: f,
    canvas: document.querySelector('canvas')?.width + 'x' +
            document.querySelector('canvas')?.height}), 250); })
// Want: vis:"visible", rafFired:true, canvas at the real size (e.g. 1308x793).
```

If it's `hidden` / `rafFired:false` / canvas `300x150`, **ask the user to
bring that Chrome window to the foreground and click the tab** so it's the
active tab. There is no reliable way to force window focus from the
extension. (A `setTimeout`-based read still runs while hidden, but the
canvas is unrendered, so it's useless for pixel verification — you need a
genuinely visible tab.)

**Symptom cheat-sheet:** an `Runtime.evaluate` timeout + a trivial eval
(`Date.now()`) that *does* return instantly = the tab is hidden (rAF
paused), **not** a renderer hang. Don't go chasing a phantom GPU bug.

### Keep tool outputs small

`read_console_messages` without a `pattern` can dump thousands of
per-frame log lines; always filter. Avoid returning whole-canvas pixel
arrays from `javascript_tool` (compute stats in-page, return a short
summary). Minimise screenshots. (Oversized outputs were a second cause of
app instability.)

---

## Core rule — don't trust the screenshot

> **The image `preview_screenshot` returns is not pixel-faithful.** It
> is compressed/resampled before it reaches you (JPEG-ish, possibly
> downscaled). You **cannot** use your visual reading of it to verify
> rendering correctness. Subtle gradients, dim colours, fine
> pixel-level patterns, sub-pixel artifacts, and stair-step aliasing
> will be crushed/smoothed into "looks fine" *even when the user is
> staring at the same canvas and the artifact is glaringly obvious*.

This is the single biggest failure mode for a renderer-debugging
session. Internalise it. An agent that doesn't will repeatedly:

1. Make a change
2. Look at the screenshot
3. Declare "fixed" or "looks good"
4. Get told no, it's still broken
5. Iterate on a false premise

Don't do that. Use the methodology in this doc.

### What you can and cannot use a screenshot for

| Use | Verdict | Why |
|---|---|---|
| Confirming the scene loaded (some geometry visible) | ✅ | A blank canvas is obvious even compressed |
| Confirming GROSS rendering broken (entire mesh black/missing/wrong colour) | ✅ | Big regressions survive downscaling |
| Confirming a **named high-contrast colour shows up at all** (e.g. "is there *any* hot-pink in the image") | ✅ | If your shader writes `vec4<f32>(1.0, 0.0, 1.0, 1.0)` over a wide region you'll spot it |
| Confirming a **specific 1-2 pixel artifact** (stair-step, dark outline, dim band, sub-pixel shimmer) | ❌ | Downscaled away. **Trust the user, not your screenshot.** |
| Comparing two screenshots for **subtle quality differences** (smoother vs less smooth silhouette) | ❌ | Both will look the same to you. Both could look very different to the user. |
| Reading the **exact colour value** of a specific pixel | ❌ | JPEG compression alters values. Use `getImageData` instead — see below. |
| Verifying **gradient smoothness** or **MSAA effectiveness** at silhouettes | ❌ | The compression IS effectively a low-pass filter. Everything will look smooth to you. |

---

## Trustable channels (in order of preference)

### 1. User's eyes

Best. The user is on a real display, looking at the live canvas, with
their visual system intact.

The pattern to use:

1. Set up a **specific** diagnostic (one variable, binary on/off
   colours).
2. Ask the user a **specific** yes/no question — "Do you see a bright
   red band along the bottom-front edge of the platform?"
3. Trust their answer absolutely.
4. Iterate.

Do **not** ask vague questions like "does it look better now". The
user can't usefully answer that and will get frustrated; you can't
usefully interpret the answer; the methodology degenerates into a
guessing game.

### 2. `getImageData` pixel reads

Read exact canvas pixels via `preview_eval`. Two-step process:

**Step 1.** Create an offscreen 2D canvas of identical dimensions,
`drawImage` the WebGPU canvas onto it, then `getImageData` from
the offscreen.

```js
new Promise(r => requestAnimationFrame(() => {
  const c = document.querySelector('canvas');
  const w = c.width, h = c.height;
  const off = document.createElement('canvas');
  off.width = w; off.height = h;
  const ctx = off.getContext('2d');
  ctx.drawImage(c, 0, 0);
  const px = (x, y) => {
    const d = ctx.getImageData(x, y, 1, 1).data;
    return [d[0], d[1], d[2], d[3]];
  };
  r(JSON.stringify({
    center: px(w/2 | 0, h/2 | 0),
    corner: px(0, 0),
    silhouette_guess: px(420, 380),
    // ...add the specific (x,y) coords you want
  }));
}))
```

**Step 2.** Read the returned JSON and verify exact RGBA values.

This bypasses screenshot compression entirely — you're reading the
canvas itself, not a JPEG of it. Use it whenever you need to verify
that "the shader actually wrote the value I expected at coords (x, y)".

Caveats:

- The WebGPU canvas must be configured with `preserveDrawingBuffer:
  true`, OR the read must happen on the same frame as the draw. From
  `preview_eval` that means wrapping the read in `requestAnimationFrame`
  (above). If you get all-zero pixels, the drawing-buffer was cleared
  after the present — wrap in `requestAnimationFrame` and try again.
- For animated scenes, the pixel you read corresponds to whichever
  frame you happened to catch. For deterministic reads, freeze the
  animation (see [Animation pin](#animation-pin) below).

### 3. Full-buffer capture via `canvas.toBlob` upload

`getImageData` is fine for a handful of coordinates, but the
`preview_eval` result is size-capped (~200 KB) — a full 1080p RGBA
buffer doesn't fit. For full-canvas captures (needed for branch-vs-
branch pixel diffs), POST the PNG to a tiny local HTTP receiver:

Sample receiver (lives in `/tmp/<scratch>/upload.py`, runs on port
9999, writes blob to `OUT_DIR/<X-Filename header>`):

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

Eval-side snippet:

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

### 4. Tracing logs

`tracing::info!` macros in Rust code emit through `tracing-wasm` to the
browser console. Read them via `preview_console_logs`.

Use this for:

- Pipeline compile order / counts (confirm the right shaders are being
  compiled).
- One-shot diagnostics ("did this branch fire at least once this
  frame").
- Frame-level state dumps (counters, sizes).

Caveats:

- The `lines` parameter is capped (default 50, max 200) — long-running
  scenes can drown your diagnostic line in render-frame chatter.
- The log capture is the LAST N lines — boot-time logs scroll off after
  a minute or two of running.
- Multiline messages get truncated awkwardly in the wrapping the tool
  does.

For shader-side diagnostics, you can't `printf`. Instead, write debug
values to a small CPU-readable buffer via `bufferMapAsync` readback
(more work, but exact and reliable). Common pattern: dedicate a single
`<storage, read_write> debug_buf: array<u32, 64>;` binding,
`atomicStore` into it from a one-shot
`if (coords == vec2<i32>(SAMPLE_X, SAMPLE_Y)) { debug_buf[0] = some_value; }`
guard, then read back from CPU. Only do this when `preview_eval` pixel
reads can't tell you what you need.

---

## Binary high-contrast diagnostic colours

When you ask the user (or yourself) to confirm a colour, encode your
diagnostic into colours that survive compression:

| OK to use | Avoid |
|---|---|
| `(1, 0, 0)` — pure red | `(0.7, 0.3, 0.3)` — dim red |
| `(0, 1, 0)` — pure green | Any gradient |
| `(0, 0, 1)` — pure blue | `(0.5, 0.5, 0.5)` — grey (loses contrast against the scene) |
| `(1, 1, 0)` — yellow | Mixed channels at fractional intensity |
| `(1, 0, 1)` — magenta | |
| `(0, 1, 1)` — cyan | |

Combine multiple binary signals into **distinct named colours**, not
gradients:

```wgsl
// GOOD — binary categorical
var col = vec3<f32>(0.0, 0.0, 1.0); // baseline: blue
if (cond_a) { col = vec3<f32>(1.0, 0.0, 0.0); } // red
if (cond_b) { col = vec3<f32>(0.0, 1.0, 0.0); } // green
if (cond_a && cond_b) { col = vec3<f32>(1.0, 1.0, 0.0); } // yellow

// BAD — gradient-encoded
var col = vec3<f32>(f32(s0.z) / 65535.0, 0.0, 0.0); // dim red varying with value — unreadable
```

### sRGB / tonemap quirks to expect

The opaque target is HDR (`rgba16float`), and the final framebuffer
goes through tonemapping + gamma. A linear `(1.0, 0.0, 0.0)` written
into `opaque_tex` does NOT read back as `(255, 0, 0)` via `getImageData`
— typically `(225, 8, 0)` once the tone curve and any compositing
attenuation applies. Plan for ±20/255 wiggle on each channel when
identifying "is this pixel ~the diagnostic colour"; only the
**dominant channel pattern** is reliable.

---

## Multi-step diagnostic colour pattern

When you can't tell where in a multi-pass pipeline a value goes wrong,
inject **binary high-contrast colours** at each stage and read pixels
back via `getImageData`. Each step narrows the bug location by one
function or one dispatch.

The chain that found the Stage 3 MSAA texture-pool bug:

| DIAG | Edit                                                                            | What it tells you                                                |
|------|---------------------------------------------------------------------------------|------------------------------------------------------------------|
| A    | `final_blend.wgsl`: `textureStore(opaque_tex, coords, vec4(0,1,1,1)); return;`  | Does classify gate fire at the broken pixel? (cyan = yes)        |
| B    | `shade_sample` top: `return vec4(1,0,0,1);`                                     | Does dispatch chain reach the output? (red = yes)                |
| C    | Each early-return uses a distinct colour (green/blue/yellow/red)                | Which path is shade_sample taking? (single dominant colour wins) |
| D    | Just before final write: `return vec4(mat_color.base.rgb, 1.0);`                | Is bug in lighting or upstream? (dark = upstream)                |
| E    | Constant red in PBR shade; constant green in skybox shade                       | What's the per-shader-bucket / skybox sample ratio?              |
| G    | `return vec4(material.base_color_factor.rgb, 1.0);`                             | Is bug in mat-data lookup or texture sample? (white = texture)   |
| I    | Sentinel magenta `vec4(1,0,1,1)` in `texture_pool_sample_grad`'s default branch | Did the texture sample fall into out-of-range default?           |

For each DIAG: edit one shader, wait for trunk rebuild (see below),
reload page, sample 3-5 known-broken pixel coordinates via
`getImageData`. Each step:

- Pixel reads the diagnostic colour → that path executes / fires.
- Pixel reads something else → the broken value comes from earlier.

**Always revert each DIAG before adding the next** so the test isolates
one variable. **Never commit a DIAG.**

---

## Quantitative branch-vs-branch comparison

The highest-value methodology when an artefact is "visibly worse than
main but I can't tell exactly how": **capture the same frame on both
branches, diff the RGBA buffers, look at where the diffs cluster.**

### Animation pin

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

This passes `0.0` every frame so morph weights don't advance from the
initial state. **Apply on BOTH the comparison branch AND `main` before
capturing.** Drop the patch before any commit lands — it's debug-only.

### Canvas backing size must match

`canvas.width / canvas.height` is the buffer size; `getBoundingClientRect`
gives the CSS size; their ratio is `devicePixelRatio`. The MCP preview
sometimes opens at different sizes between sessions (CSS 344×517,
DPR 2 → buffer 689×1034; or CSS 1138×1360, DPR 1 → buffer 1138×1360).
**Pixel-coordinate-keyed diffs are only valid between captures from
the same browser instance** (same canvas backing dimensions). Sanity-
check before capture:

```js
(() => ({
  w: document.querySelector('canvas').width,
  h: document.querySelector('canvas').height,
  dpr: devicePixelRatio,
}))()
```

### Cherry-picking debug helpers onto `main`

Diagnostic-only commits that landed on the feature branch (e.g. the
model-tests `?cam=…` URL helper in commit `6988b7c`) often need to also
be applied to `main` so the comparison capture uses the same camera.
Pattern:

```sh
git checkout main
git cherry-pick <helper-commit>
# capture main_zoomed.png
git reset --hard origin/main   # drop the cherry-pick
```

**Don't push the temporary `main` commit** — `git reset` after
capturing.

### Diff script

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

For the Stage 3 silhouette fix this revealed two distinct bugs: the
first fix dropped 510 → 55 differing pixels; the residual 55 clustered
in a single diagonal stripe (y=636-640) that turned out to be a second
distinct bug (missing normal-discontinuity edge detection).

---

## When the bug spans two pipelines

If a value is written by pipeline A and read by pipeline B, both have
to agree on the bind-group layout, the texture format, and any
shader-template substitutions used to specialize their compiled SPIR-V.

The Stage 3 silhouette bug was exactly this: `material_opaque`'s
primary pipeline templated `texture_pool_arrays_len` correctly at
`finalize_gpu_textures`, but `edge_pipelines` kept its boot-time value
(usually 0) and never re-templated. Per-sample texture lookups in
edge_resolve dropped into the `default → vec4(0)` branch.

When chasing "A and B should produce the same output but B is wrong",
check:

- Both bind-group layouts agree on every binding's shape (texture vs
  storage, sampleable vs storage-write, multisampled flag, format).
- Both shaders are compiled with the same template substitutions for
  any value derived from runtime state (`texture_pool_arrays_len`,
  `texture_pool_samplers_len`, `msaa_sample_count`, `bucket_entries`).
- Any recompile path (texture-pool grow, MSAA flip, dynamic material
  register) recompiles BOTH A and B in lock-step, not just A.

---

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
`pipeline_scheduler::subcompile-complete` log lines (or the
per-pipeline `compute:Material Opaque:PipelineLayoutKey(N) cum=Xms ok`
Lessons-A lines) before sampling pixels — sampling before compile
completes gives stale-frame artefacts.

### Forcing trunk to rebuild

Trunk's `--watch` does not always pick up `touch` updates with no
content change. After editing `.wgsl` files, sometimes the dev bundle
doesn't rebuild. Symptom: you make a shader change, reload the page,
and the output is identical to before.

**Reliable rebuild trigger:** add and immediately remove a comment line
in `crates/frontend/model-tests/src/main.rs` (or any `.rs` file under a
watched path). Trunk picks up real file-content changes reliably.

### Verify the rebuild actually deployed

1. Watch the trunk dev log for a new `applying new distribution` line
   after your edit.
2. After page reload, check `preview_console_logs` for new
   `pipeline N/M compute:…` compile lines (the Lessons-A per-pipeline
   timing logs). If the labels include shaders you just changed, the
   new code is live.

### Browser cache / hot-reload pitfalls

- After a shader change, **hard reload** (`window.location.reload(true)`),
  not a soft reload — soft reload re-uses the previous wasm.
- If you change a bind-group layout, the cached pipeline keys may be
  stale. Hard reload is mandatory.
- The animation morph cycle continues between reloads — so the "same"
  camera state will show a different morph frame. Don't compare two
  screenshots taken seconds apart and treat them as identical scenes.

---

## `?cam=` URL override (model-tests frontend) — use it on EVERY test

`crates/frontend/model-tests/src/pages/app/scene/camera/view/orbit.rs::setup_from_gltf`
accepts a `?cam=yaw,pitch,radius,lx,ly,lz` query string to seed the
OrbitCamera. **Always use this on every navigation** for a reproducible
camera. Without it the camera lands at the auto-aabb default and you
cannot compare against any reference image the user is comparing
against.

The MSAA-debugging canonical configuration:

```
/app/model/MorphStressTest?cam=-0.48,0.13,2.2916,0,0.2,0
```

For close-zoom verification, additionally dispatch 8 wheel-down notches
on the canvas after page-load:

```js
const c = document.querySelector('canvas');
const r = c.getBoundingClientRect();
for (let i = 0; i < 8; i++) {
  c.dispatchEvent(new WheelEvent('wheel', {
    deltaY: -100, bubbles: true,
    clientX: r.left + r.width/2, clientY: r.top + r.height/2,
  }));
}
```

**Verify every test at both default zoom AND close zoom.** Aliasing
artefacts that are invisible at default zoom can become glaring at
close zoom — that asymmetry was a major factor in earlier misdiagnoses.

---

## Methodology checklist

When you sit down to debug a rendering issue, follow this in order. No
skipping.

1. **Identify what you do NOT know** about the failure. Write it down
   as a list of empirical questions, each answerable with a binary
   observation. Example: "Does classify mark capsule silhouette pixels
   as edges?" → "Does final_blend write to those edge pixels?" →
   "Does the value final_blend writes match what the user sees?"

2. **For each question**, design a minimal diagnostic: shader change
   that produces a binary high-contrast colour at the pixels you're
   testing the hypothesis on.

3. **Capture a baseline** (full-canvas PNG via `canvas.toBlob` upload)
   and note the specific (x, y) pixel coordinates you're targeting +
   their current values.

4. **Build, hard-reload, sample (`getImageData` for known-broken
   coords) AND/OR ask the user** a specific yes/no question about a
   specific location.

5. **Record the answer** and move to the next question. Don't conflate
   two unknowns by changing multiple things at once.

6. **Never** declare a bug "fixed" based on your reading of a preview
   screenshot. Either the user confirms specifically, or you've used
   `getImageData` to read the exact RGBA at the pixels that should
   have changed.

7. **Revert every diagnostic** before committing the actual fix. Don't
   ship `textureStore(opaque_tex, coords, vec4<f32>(1.0, 0.0, 1.0, 1.0))`.

---

## Scratch-file hygiene

Multi-hour debug sessions accumulate:

- `/tmp/scratch/` PNG snapshots (gigabytes if not pruned).
- Diff scripts (`diff.py`, `probe.js`, …) that should NOT land in the
  repo.
- A localhost upload server that needs killing at shutdown.

**Convention: confine all scratch artefacts to `/tmp/scratch/`** (or a
named sibling like `/tmp/msaa-work/`). Never write inside the repo
unless you intend the file to be committed.

At session end:

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

---

## Related infrastructure

The recipes above were originally written for the in-app
`mcp__Claude_Preview__*` tools. **For this renderer, use real Chrome
instead** (see the "Tooling" section at the top) — the JS bodies are
identical; only the tool names change:

| Recipe says | Real-Chrome equivalent (`mcp__Claude_in_Chrome__*`) |
|---|---|
| `preview_eval(...)` | `javascript_tool { action: "javascript_exec", text: … }` |
| `preview_console_logs` | `read_console_messages` (always pass a `pattern`) |
| `preview_screenshot` | (avoid; prefer `getImageData` stats) |
| `preview_start` | run `trunk serve …` in the shell (background) |

- The dev server is launched from the shell, not `.claude/launch.json`.
  The scene-editor canvas is the canonical surface for light-culling /
  shading experiments; `model-tests` for MSAA.
- `javascript_tool` runs JS in the live page (drive UI, inspect state,
  read DOM, and via `getImageData`, canvas pixels). **Remember the
  visibility gotcha** — rAF-wrapped reads only resolve when the tab is
  the foreground active tab of a non-minimized window.
- `read_console_messages` returns recent console output (useful for
  `tracing::info!`); pass a `pattern` so per-frame spam doesn't bury the
  line you want.
