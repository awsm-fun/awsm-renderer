# Debugging via the preview browser — reliable methodology

This is a **load-bearing operational document**. If you are an agent in a fresh session and you are about to debug a rendering issue using `mcp__Claude_Preview__*` (or `mcp__Claude_in_Chrome__*`), read this completely before you take a single screenshot.

## Core rule

> **The screenshot you receive from `preview_screenshot` is not pixel-faithful.** It is compressed/resampled before it reaches you (JPEG-ish, possibly downscaled). You **cannot** use your visual reading of it to verify rendering correctness. Subtle gradients, dim colors, fine pixel-level patterns, sub-pixel artifacts, and stair-step aliasing will be crushed/smoothed into "looks fine" *even when the user is staring at the same canvas and the artifact is glaringly obvious*.

This is the single biggest failure mode for a renderer-debugging session. An agent that doesn't internalise this rule will repeatedly:

1. Make a change
2. Look at the screenshot
3. Declare "fixed" or "looks good"
4. Get told no, it's still broken
5. Iterate on a false premise

Don't do that. Use the methodology in this doc.

## What you can and cannot use a screenshot for

| Use | Verdict | Why |
|---|---|---|
| Confirming the scene loaded (some geometry visible) | ✅ | A blank canvas is obvious even compressed |
| Confirming GROSS rendering broken (entire mesh black/missing/wrong colour) | ✅ | Big regressions survive downscaling |
| Confirming a **named high-contrast colour shows up at all** (e.g. "is there *any* hot-pink in the image") | ✅ | If your shader writes `vec4<f32>(1.0, 0.0, 1.0, 1.0)` over a wide region you'll spot it |
| Confirming a **specific 1-2 pixel artifact** (stair-step, dark outline, dim band, sub-pixel shimmer) | ❌ | Downscaled away. **Trust the user, not your screenshot.** |
| Comparing two screenshots for **subtle quality differences** (smoother vs less smooth silhouette) | ❌ | Both will look the same to you. Both could look very different to the user. |
| Reading the **exact colour value** of a specific pixel | ❌ | JPEG compression alters values. Use `getImageData` instead — see below. |
| Verifying **gradient smoothness** or **MSAA effectiveness** at silhouettes | ❌ | The compression IS effectively a low-pass filter. Everything will look smooth to you. |

## Trustable channels (in order of preference)

### 1. User's eyes

Best. The user is on a real display, looking at the live canvas, with their visual system intact. They see what is actually there.

The pattern to use:
1. Set up a **specific** diagnostic (one variable, binary on/off colours)
2. Ask the user a **specific** yes/no question — "Do you see a bright red band along the bottom-front edge of the platform?"
3. Trust their answer absolutely
4. Iterate

Do **not** ask vague questions like "does it look better now". The user can't usefully answer that and will get frustrated; you can't usefully interpret the answer; the methodology degenerates into a guessing game.

### 2. `getImageData` pixel reads

You can read exact canvas pixels via `preview_eval`. Two-step process:

**Step 1: create an offscreen 2D canvas of identical dimensions, drawImage the WebGPU canvas onto it, then read pixels.**

```js
(() => {
  const c = document.querySelector('canvas');
  const w = c.width, h = c.height;
  const off = document.createElement('canvas');
  off.width = w; off.height = h;
  const ctx = off.getContext('2d');
  ctx.drawImage(c, 0, 0);
  // Read a single pixel at (x, y)
  const px = (x, y) => {
    const d = ctx.getImageData(x, y, 1, 1).data;
    return [d[0], d[1], d[2], d[3]];
  };
  return JSON.stringify({
    center: px(w/2 | 0, h/2 | 0),
    corner: px(0, 0),
    silhouette_guess: px(420, 380),
    // ...add the specific (x,y) coords you want
  });
})()
```

**Step 2: read the returned JSON and verify the exact RGBA values.**

This bypasses screenshot compression entirely. You're reading the canvas itself, not a JPEG of it. Use this whenever you need to verify that "the shader actually wrote the value I expected at coords (x, y)".

Caveats:
- The WebGPU canvas must be configured with `preserveDrawingBuffer: true` OR the read must happen on the same frame as the draw. In practice this means: do the draw, then **inside the same RAF tick** issue the `drawImage` and `getImageData`. From `preview_eval` you can do this by `requestAnimationFrame`'ing the read. If you get all-zero pixels, the drawing-buffer was cleared after the present — wrap the read in `requestAnimationFrame` and try again.
- For multi-frame scenes (animations), the pixel you read corresponds to whichever frame you happened to catch. For deterministic reads, freeze the animation if possible (pause / step-frame button if the frontend has one).

### 3. Tracing logs

`tracing::info!` macros in Rust code emit through `tracing-wasm` to the browser console. Read them via `preview_console_logs`.

Use this for:
- Pipeline compile order / counts (confirm the right shaders are being compiled)
- One-shot diagnostics ("did this branch fire at least once this frame")
- Frame-level state dumps (counters, sizes)

Caveats:
- The `lines` parameter is capped (default 50, max 200) — long-running scenes can drown your diagnostic line in render-frame chatter
- The log capture is the LAST N lines — boot-time logs scroll off after a minute or two of running
- Multiline messages get truncated awkwardly in the wrapping the tool does

For shader-side diagnostics, you can't `printf` — instead, write debug values to a small CPU-readable buffer via `bufferMapAsync` readback (more work, but exact and reliable). Common pattern: dedicate a single `<storage, read_write> debug_buf: array<u32, 64>;` binding, atomicStore into it from a one-shot `if (coords == vec2<i32>(SAMPLE_X, SAMPLE_Y)) { debug_buf[0] = some_value; }` guard, then read back from CPU. Only do this when `preview_eval` pixel reads can't tell you what you need.

## Binary high-contrast diagnostic colours

When you do use visual confirmation (asking the user), encode your diagnostic into colours that survive compression:

| OK to use | Avoid |
|---|---|
| `(1, 0, 0)` — pure red | `(0.7, 0.3, 0.3)` — dim red |
| `(0, 1, 0)` — pure green | Any gradient |
| `(0, 0, 1)` — pure blue | `(0.5, 0.5, 0.5)` — grey (loses contrast against the scene) |
| `(1, 1, 0)` — yellow | Mixed channels at fractional intensity |
| `(1, 0, 1)` — magenta | |
| `(0, 1, 1)` — cyan | |

Combine multiple binary signals into **distinct named colours**, not into gradients:

```wgsl
// GOOD — binary categorical
var col = vec3<f32>(0.0, 0.0, 1.0); // baseline: blue
if (cond_a) { col = vec3<f32>(1.0, 0.0, 0.0); } // red
if (cond_b) { col = vec3<f32>(0.0, 1.0, 0.0); } // green
if (cond_a && cond_b) { col = vec3<f32>(1.0, 1.0, 0.0); } // yellow

// BAD — gradient-encoded
var col = vec3<f32>(f32(s0.z) / 65535.0, 0.0, 0.0); // dim red varying with value — unreadable
```

Then ask the user: "do you see yellow anywhere?" "where do you see red vs green?" — concrete, unambiguous.

## Forcing trunk to actually rebuild

Trunk's `--watch` does not always pick up `touch` updates with no content change. After editing `.wgsl` files, sometimes the dev bundle doesn't rebuild. Symptom: you make a shader change, reload the page, and the output is identical to before.

**Reliable rebuild trigger:** add and immediately remove a comment line in `crates/frontend/model-tests/src/main.rs` (or any `.rs` file under a watched path). Trunk picks up real file-content changes reliably.

```rust
#![allow(dead_code)]
#![allow(clippy::arc_with_non_send_sync)]
#![allow(clippy::type_complexity)]
// rebuild marker  // ← add, save; trunk rebuilds; remove later
```

**Verify the rebuild actually deployed:**
1. Watch `/tmp/model-tests-dev.log` (or wherever the dev server is logging) for a new `applying new distribution` line after your edit.
2. After page reload, check `preview_console_logs` for new "pipeline N/M compute:..." compile lines (the per-pipeline timing logs from `Lessons A`). If the labels include shaders you just changed, the new code is live.

## Browser cache / hot-reload pitfalls

- After a shader change, **hard reload**: `window.location.reload()` (which forces module re-init). A soft reload can re-use the previous wasm.
- If you change a bind-group layout, the cached pipeline keys may be stale. Hard reload is mandatory.
- The animation morph cycle continues between reloads — so the "same" camera state will show a different morph frame. Don't compare two screenshots taken seconds apart and treat them as identical scenes.

## `?cam=` URL override (model-tests frontend) — use it on EVERY test

`crates/frontend/model-tests/src/pages/app/scene/camera/view/orbit.rs::setup_from_gltf` accepts a `?cam=yaw,pitch,radius,lx,ly,lz` query string to seed the OrbitCamera. **Always use this on every navigation** for a reproducible camera. Without it the camera lands at the auto-aabb default and you cannot compare against any reference image the user is comparing against.

The MSAA-debugging canonical configuration:

```
/app/model/MorphStressTest?cam=-0.48,0.13,2.2916,0,0.2,0
```

For close-zoom verification (the camera the user has been using to point out aliasing the agent could not see), additionally dispatch 8 wheel-down notches on the canvas after page-load:

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

**Verify every test at both default zoom AND close zoom.** Aliasing artifacts that are invisible at default zoom can become glaring at close zoom — that asymmetry was a major factor in the previous session's misdiagnoses.

## Methodology checklist

When you sit down to debug a rendering issue, follow this in order. No skipping.

1. **Identify what you do NOT know** about the failure. Write it down as a list of empirical questions, each answerable with a binary observation. Example: "Does classify mark capsule silhouette pixels as edges?" → "Does final_blend write to those edge pixels?" → "Does the value final_blend writes match what the user sees?"

2. **For each question**, design a minimal diagnostic: shader change that produces a binary high-contrast colour at the pixels you're testing the hypothesis on.

3. **Build, hard-reload, screenshot, AND** ask the user a specific yes/no question about a specific location. Trust their answer over your own visual reading of the screenshot.

4. **Record the answer** and move to the next question. Don't conflate two unknowns by changing multiple things at once.

5. **Never** declare a bug "fixed" based on your reading of a preview screenshot. Either the user confirms specifically, or you've used `getImageData` to read the exact RGBA at the pixels that should have changed.

6. **Revert every diagnostic** before committing the actual fix. Don't ship `textureStore(opaque_tex, coords, vec4<f32>(1.0, 0.0, 1.0, 1.0))`.

## Related infrastructure

- `mcp__Claude_Preview__preview_start` reads `.claude/launch.json` for available servers; `model-tests` is the canonical canvas for MSAA/rendering experiments.
- `preview_eval` runs JS in the live page; you can drive UI, inspect state, trigger events, and read DOM (and via `getImageData`, canvas pixels).
- `preview_console_logs` returns the recent console output — useful for `tracing::info!` messages.
- For deeper interaction (DOM tree, accessibility, multi-tab), `mcp__Claude_in_Chrome__*` exists. Heavier; only reach for it if `preview_*` can't do what you need.
