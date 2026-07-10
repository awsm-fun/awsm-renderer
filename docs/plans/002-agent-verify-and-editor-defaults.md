# 002 — Agent verification workflow: clean screenshots, view toggles, hidden-tab capture

**Order:** second, deliberately early — every later plan (003–008) is verified unattended
by an agent taking screenshots. This plan makes those screenshots (a) clean (no grid,
gizmos, or overlay contaminating the pixels) and (b) possible while the tab is hidden.

## Part A — Editor view defaults + MCP view toggles

### Problem
Screenshots taken during agent sessions routinely include the grid, node/light gizmos,
the MCP info overlay, and camera motion from "follow the agent" — all of which confuse
visual verification (and humans reviewing the captures). There is currently no MCP way
to turn any of them off: `gizmo` / `light_gizmos` are editor Mutables defaulting `true`
(`editor/src/controller/state.rs:192-194, 238-239`), the grid toggle lives in the
viewport UI only, follow-agent is hardwired in `editor/src/remote.rs::follow_agent_mode`,
and the MCP info overlay has no switch.

### Changes
1. **Defaults OFF for agent-facing chrome:**
   - "Follow the agent" default **OFF** (`remote.rs` — make `follow_agent_mode` a no-op
     unless an explicit opt-in Mutable is set; keep the UI toggle so a human can opt in).
   - MCP info overlay default **OFF** (same treatment: Mutable, default false, UI toggle).
   - Grid / gizmos / light-gizmos keep their current human-friendly defaults (grid on,
     gizmos on) — they become MCP-toggleable instead (below).
2. **One partial-update view-options command**, mirroring `SetPostProcess` semantics:
   `EditorCommand::SetViewOptions { grid: Option<bool>, gizmos: Option<bool>,
   light_gizmos: Option<bool>, follow_agent: Option<bool>, mcp_overlay: Option<bool> }`
   (all `#[serde(default)]`), applied to the corresponding Mutables, undoable, and NOT
   persisted to the project (view state, not scene state — same class as selection).
3. **MCP tool `set_view_options`** exposing the command, plus the current values in the
   scene snapshot / a readback (agents must be able to query, not just set — see the
   `get_post_process` lesson in 001).
4. **Guidance where agents will see it:** the `screenshot_scene` tool description (and
   `awsm://docs/*` screenshot workflow doc) must say explicitly: *"for clean feature
   verification, `set_view_options {grid:false, gizmos:false, light_gizmos:false}`
   first; restore after"*. This is the discoverability half of the fix — the toggles
   are useless if agents don't know when to hit them.

### Acceptance — ✅ DONE (browser-verified)
- [x] Fresh editor: activity overlay, follow-agent, and MCP notifications all default OFF
      (`activity_feed.rs` ENABLED/FOLLOW statics, `remote.rs` SHOW_NOTIFICATIONS). The old
      single "Follow agent activity" toggle split into "Agent activity overlay" +
      "Follow agent workspace" (Settings + MCP tab).
- [x] `EditorCommand::SetViewOptions` partial-update (grid/gizmos/light_gizmos/skeleton_viz/
      follow_agent/activity_overlay/mcp_notifications) — transient view state (same class as
      camera; deviation from the sketch's "undoable": selection/camera aren't undoable either).
      Read half: `EditorQuery::ViewOptions`. MCP `set_view_options` + `get_view_options` tools.
- [x] Verified defaults query: {overlay,follow,notifications}=false, {grid,gizmos,light_gizmos,
      skeleton_viz}=true; toggle-off → screenshot contains GEOMETRY ONLY (no grid, no gizmo,
      no selection rect) even with a node selected; round-trip query confirms.
- [x] Fixed two pre-existing gating bugs this exposed: (1) `transform_controller::set_hidden`
      only flipped flags — the force-hidden path returned before the per-frame redraw, so the
      last-drawn gizmo lines stayed on screen forever; all-hidden now clears drawn lines
      immediately (web-shared, benefits every consumer). (2) The screen-space selection box
      (DOM overlay, `selection_box.rs`) was ungated; it now rides the same `gizmo` setting.
- [x] `screenshot_scene` + `set_view_options` tool descriptions carry the when-to-toggle
      guidance (the discoverability half).

## Part B — Hidden-tab / offscreen capture (from the retired offscreen-editor-screenshots plan)

### Problem
Every frame-bound MCP operation (`screenshot_scene`, `wait_render_settled`, thumbnails)
dies when the editor tab is hidden or the display sleeps, because the render loop rides
`requestAnimationFrame` (`editor/src/engine/render_loop.rs`) and browsers pause rAF for
hidden tabs. Real incident: an unattended overnight agent session stalled ~8h with the
lid closed. Current mitigation only fails fast (`HIDDEN_REQUEST_TIMEOUT` = 15s in
`packages/mcp/src/link.rs:39` + the `visibility` EditorEvent) — agents *know* they're
blind but stay blind.

### Approach — on-demand hidden-frame rendering (Option B, decided)
Keep the main-thread rAF loop for the visible case. When the tab is hidden and a
frame-bound request arrives over the WebSocket, render **synchronously inside the
socket message task** (which is not rAF/timer-throttled): a `render_once_for_capture()`
entry point that runs one `tick()` into an offscreen target + readback for the PNG,
never presenting to the invisible swapchain. Animation time is driven from the wall
clock at capture, so scrub-then-screenshot flows are exact.

- Routing: `engine/query.rs::scene_png` + the settle barrier branch on `document.hidden`.
- Continuous background playback is explicitly NOT a goal — agents scrub and capture.
- The full Worker+OffscreenCanvas migration (Option A) stays future work; Option B's
  capture entry point remains useful under it.
- Once green, relax the `HIDDEN_REQUEST_TIMEOUT` fail-fast for capture-capable requests.

### Acceptance — ✅ DONE (browser-verified via the CI recipe)
- [x] `render_once_for_capture()` (render_loop.rs): one on-demand frame — wall-clock
      animation dt via the shared `LAST_TS` clock (tab-show resumes seamlessly), sync
      render (`try_lock`, skips on contention), stats recorded. `capture_scene_rgba`
      branches on `document.hidden`: renders on demand at enqueue + on each unfulfilled
      poll iteration.
- [x] Verified with the plan's CI recipe (stub `document.hidden=true` + `visibilitychange`):
      two scene_png captures succeeded while hidden, pixels changed across camera moves
      between them (scrub-then-capture flow exact), ~4.3s total for both (hidden-clamped
      timers; readback progresses via mapAsync independent of rAF).
- [x] `wait_render_settled` already resolves while hidden — it polls compile counters on
      timers (clamped but alive), not rAF; no change needed.
- [x] No foreground regression: the visible-path capture test ran first (hidden=false,
      950ms/212ms captures, pixels changed) and the rAF loop is untouched; hidden rendering
      is strictly on-demand (one frame per capture request, no free-running loop).
- [x] `HIDDEN_REQUEST_TIMEOUT` (15s fail-fast) left in place — on-demand capture completes
      well inside it, so no relaxation needed; hidden requests now SUCCEED instead of
      timing out.
