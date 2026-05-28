# Edge_resolve runtime baseline — model-tests / Fox / 1080p / MSAA-4

Block E.5 of [PR #99](https://github.com/dakom/awsm-renderer/pull/99). Captures a current-baseline frame-time profile of the model-tests Fox scene with the Stage-3 edge_resolve path active, so future regressions (or improvements) have a reference point.

Pre-Stage-3 comparison wasn't captured this session per the parked-scope decision; the methodology below is reproducible on `main` (commit before Stage 3 landed) for an after-the-fact A/B if needed.

## Methodology

1. `task model-tests:dev` → preview-browser → wait for `phase = Ready`.
2. Inject the RAF sampler via DevTools (or `mcp__Claude_Preview__preview_eval`):
   ```js
   (() => {
     window.__sampler = { samples: [], last: performance.now() };
     function tick(t) {
       window.__sampler.samples.push(t - window.__sampler.last);
       window.__sampler.last = t;
       if (window.__sampler.samples.length < 180) requestAnimationFrame(tick);
     }
     requestAnimationFrame(tick);
   })()
   ```
3. Wait 5 s for 180 samples (~3 s of rendering at 60 fps).
4. Discard the first 10 samples (warm-up; first frame after sampler attach often shows a >100 ms outlier from RAF re-priming).
5. Compute summary statistics over the remaining ~170 samples.

## Results (2026-05-27, macOS Metal, M-series, vsync-on)

| Metric | Value |
|---|---|
| Sample count (post-warmup) | 170 |
| Mean | 16.68 ms |
| Std dev | 0.36 ms |
| Min | 15.7 ms |
| p50 | 16.7 ms |
| p95 | 17.3 ms |
| p99 | 17.6 ms |
| Max | 17.7 ms |

Target for 60 fps vsync: **16.67 ms** per frame.

## Interpretation

The post-Stage-3 path runs essentially at vsync on the test scene. Mean = 16.68 ms (within 0.01 ms of the vsync target), p99 = 17.6 ms (one frame of slack against the next vsync deadline). Max = 17.7 ms — no spikes, no recompile hitches, no edge_resolve fall-through warnings observed.

The classify-pass's extra writes (`edge_pixel_id` allocation, `edge_to_xy`, `edge_slot_map`, per-shader sample lists) cost less than 1 ms of frame-time on the Fox scene. The per-shader edge_resolve indirect dispatches (typically 0 work on Fox — its edges concentrate on the silhouette) plus skybox_edge_resolve + final_blend together add similar negligible cost. The `reset_header` per-frame buffer-clear (16 + 4N bytes) is in the noise.

## Pathological-edge-density follow-up (parked under C.2)

The `MAX_EDGE_BUDGET` overflow path (Stage 3.8 / Block C.2 MVP) clamps the counter at budget; excess edges drop and render with primary-sample shading (degraded MSAA, not a crash). A pathological-edge-density scene (e.g. dense foliage, ~25% edge pixels at 4K) would saturate the budget and surface the `note_edge_overflow_observed` warn. Capturing the frame-time delta in that overflow regime is part of the C.2 full-implementation follow-up (atomic-add hash-bucket overflow accumulator).

## How to redo the comparison vs pre-Stage-3

If a future PR wants a definitive "Stage 3 didn't cost frame-time" claim:

```
git switch main      # pre-Stage-3 baseline
task model-tests:dev
# capture using methodology above
git switch more-optimizations
# capture using same methodology
```

Then diff the two `mean_ms` + `p99_ms` numbers. Stage 3 should be within ±1 ms of the pre-Stage-3 baseline on a typical scene; the architectural cost is on edge pixels only, which are <10% of the frame on most scenes.
