# Plan: Offscreen editor rendering (screenshots while the tab is hidden)

## Problem

Every frame-bound MCP operation — `screenshot_scene`, `wait_render_settled`,
thumbnail refreshes, anything that awaits a *presented* frame — dies when the
editor tab is not visible, because the render loop rides
`requestAnimationFrame` (`packages/frontend/editor/src/engine/render_loop.rs`)
and browsers pause/throttle rAF for hidden tabs (and for any tab when no
display is on).

This is the single biggest obstacle to **unattended agent sessions**. Real
case (2026-07-05, the DANCE-OFF overnight build): an agent-driven material
pass stalled for ~8 hours because the laptop lid was closed — the editor tab
stayed connected (WebSocket alive, JS-only queries kept answering), but every
screenshot/verify step hung until a human made the tab visible again. An
agent cannot verify what it cannot see.

### Current mitigation (shipped, not a fix)

The editor pushes `visibilitychange` state to the MCP server
(`EditorEvent { kind: "visibility", hidden }`, see
`packages/frontend/editor/src/remote.rs::hook_visibility`); the server tracks
it per connection and:

- `ping` / `pairing_status` report `tab=HIDDEN` / `tab_hidden: true`;
- hidden-tab requests fail fast (15s, `HIDDEN_REQUEST_TIMEOUT` in
  `packages/mcp/src/link.rs`) with an actionable error instead of burning the
  120s timeout.

Agents now *know* they are blind, quickly — but they are still blind.

## Goal

An attached editor can serve `screenshot_scene` / `wait_render_settled` (and
keep animation/preview state advancing predictably) while its tab is hidden,
backgrounded, or the display is off — without a human present.

Acceptance:

1. With the editor tab fully hidden (background tab AND display asleep),
   `screenshot_scene` returns a current, correct render within normal latency.
2. `wait_render_settled` resolves (frames actually advance) while hidden.
3. No regression to foreground behavior: vsync pacing, input latency,
   thumbnails, the activity pulse.
4. Power sanity: hidden-tab rendering is on-demand or heavily throttled — we
   must not spin a full-rate render loop in every backgrounded tab forever.

## Design directions (decide during implementation)

### A. Worker + OffscreenCanvas (the "real" fix)

Move rendering to a Web Worker driving an `OffscreenCanvas`
(`transferControlToOffscreen`). Workers are not rAF-frozen with the tab
(timers are throttled but run), so the loop can keep ticking on
`setTimeout`/`requestAnimationFrame`-in-worker with a fallback cadence.

- WebGPU in workers + OffscreenCanvas is supported in Chromium (our target).
- Largest touch surface: the renderer currently lives on the main thread next
  to the controller/UI (`engine/context.rs`, `with_renderer_mut` callers
  everywhere). Everything crossing to the worker becomes message-passing or
  shared memory — this is effectively the existing "multithreaded" experiment
  (`taskfiles/examples/multithreaded.yml`) promoted to the editor proper.
- Biggest risk: the scene→GPU bridge (`engine/bridge/*`) assumes same-thread
  `Mutable` observers.

### B. On-demand hidden-frame rendering (cheap, screenshot-focused)

Keep the main-thread rAF loop for the visible case. When the tab is hidden
AND a frame-bound request arrives, render **synchronously on demand** off the
request path: run one `tick()` + readback per request (or a short timer-driven
burst while requests are in flight), never presenting to the (invisible)
swapchain — render into an offscreen target and read that back for the PNG.

- Much smaller change: a `render_once_for_capture()` entry point plus routing
  in `engine/query.rs::scene_png` / the settle barrier when
  `document.hidden`.
- Caveats: `setTimeout` in a hidden tab is clamped (≥1s, sometimes more under
  Chrome's Intensive Throttling) — but MCP requests arrive over the WebSocket,
  whose `onmessage` still fires promptly, and rendering *inside that message
  handler's task* avoids timer clamping entirely.
- Animation playback time can be driven from the wall clock at capture time,
  so scrub-then-screenshot flows (the agent's main verify loop) are exact.
- Does NOT make continuous background playback smooth — acceptable: agents
  scrub + capture; they don't watch.

### C. Headless/CLI editor (long-term alternative)

A native (non-browser) headless build serving the same MCP surface (wgpu
instead of web-sys WebGPU). Solves unattended runs completely, but forks the
renderer's "WebGPU via web-sys, browser-only by design" premise (see README);
out of scope unless A/B prove untenable.

## Recommendation

Start with **B** — it directly unblocks the agent verify loop (screenshot /
settle on demand via the socket's message task, no rAF dependency), is
incremental, and doesn't disturb the foreground path. Keep **A** as the
follow-on once the multithreaded experiment matures; B's capture entry point
(`render_once_for_capture`) remains useful under A as well.

## Verification plan

Drive it exactly like the incident: attach an editor, hide the tab (and/or
`Object.defineProperty(document, 'hidden', …)` + dispatch `visibilitychange`
for CI), then over MCP: `set_playhead` → `screenshot_scene` → assert pixels
change across playheads; `wait_render_settled` resolves; `ping` may still
report `tab=HIDDEN` (truthful) while captures succeed. Remove/relax the
`HIDDEN_REQUEST_TIMEOUT` fail-fast for capture-capable requests once green.

## Context / provenance

Extracted from the DANCE-OFF session handoff (UPSTREAM-FIXES.md, item F3 —
the only item not implemented in the 2026-07-05 fix sessions; everything else
from that doc is in the repo as of those sessions' working tree). The
fail-fast mitigation and visibility plumbing referenced above landed there.
