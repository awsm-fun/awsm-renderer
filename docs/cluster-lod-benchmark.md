# Cluster-style cluster-LOD — multi-million-triangle benchmark (A6)

On-device benchmark backing the north-star claims in [`docs/cluster-lod.md`](../cluster-lod.md)
§"Streaming residency (how multi-million-triangle assets fit)": **bounded draw** (cut tri
count tracks screen resolution + pixel-error budget, *not* source size) and **bounded VRAM**
(residency holds only the working set). Captured iter 39 (2026-06-26) on `cluster`, browser
healthy (un-frozen — watertight screenshot + live readbacks), `?vg&paging&trace=sub-frame`.

## Asset

A genuine multi-million-triangle asset (subdivided UV sphere, 48×44 base, 4 midpoint
subdivisions) round-tripped through the player cluster path (`load_player_bundle`):

| Quantity | Value |
|---|---|
| Source triangles | **1,081,344** |
| Full cluster DAG (all LOD levels) | **2,393,468 tris / 51,753 clusters** |
| Resident render mesh **M** | **29,850 tris** (capped to the residency budget 30,000) |
| Page pool (bounded VRAM) | **3,862 slots × 384 verts × 56 B = ~83 MB** |

`M` and the pool are **constant** regardless of camera or resolution — VRAM tracks the
**budget**, not the asset (the source/DAG is ~28–80× larger than what is ever resident). The
runtime M (83 MB) is far below the 512 MB allocation guard; the runtime needs no guard change
(see the editor-authoring caveat in NORTHSTAR-GAPS — authoring a >1M editable mesh in the
editor is a separate dense-`?vg` concern).

## Bounded draw — drawn cut vs source

Drawn cut = `draw_args.index_count / 3` (the compacted indirect draw), read from the periodic
GPU readback. The cut is a tiny fraction of the source and is driven by **camera + viewport
height**, never the source size:

| Camera | Render res (w×h px) | Drawn cut (tris) | % of source (1.08M) | % of DAG (2.39M) |
|---|---|---|---|---|
| Far (default) | 1392×746 | **4,908** | 0.45% | 0.21% |
| Near (sphere fills view) | 872×365 | **9,153** | 0.85% | 0.38% |
| Near (sphere fills view) | 1392×746 | **14,835** | 1.37% | 0.62% |
| Near (sphere fills view) | **3312**×746 | **14,835** | 1.37% | 0.62% |

Two independent observations, both confirming "bounded by screen, not source":

- **Tracks vertical resolution.** Same near camera: halving canvas height 746→365 px shrinks
  the cut 14,835→9,153 tris; the cut scales with viewport height (the pixel-error projection
  uses `viewport_h`).
- **Independent of width and source.** Widening the canvas 2.4× (1392→3312 px, same 746 height)
  leaves the cut **unchanged** at 14,835 tris — the cut is set by the vertical pixel budget, not
  raster width, and certainly not by the 1.08M-tri source.

> Note on 4K: this capture display caps the canvas at ~746 px tall, so a literal 2160p row is
> not capturable on this machine. The 365→746 px height sweep + the 2.4× width sweep together
> establish the scaling law; extrapolating the height-driven term to 2160 px (~2.9× the 746 row)
> keeps the cut well under the doc's "~2M triangles for typical resolutions" ceiling and still
> ≪ the source.

## Per-pass timing & frame time (1392×746, near camera)

Per-pass **CPU encode** time from the `?trace=sub-frame` spans (User-Timing `measure` entries,
steady state, ~180 frames). These are command-encode costs on the CPU; the renderer does not
currently wire GPU timestamp queries, so GPU execution time is not separately reported — the
dominant GPU cost is the bounded visibility raster of the **drawn cut** (tri count in the table
above) plus the per-pixel deferred shade (bounded by screen pixels).

| Pass (CPU encode) | avg ms |
|---|---|
| Render (whole frame encode) | **1.96** |
| Geometry (visibility) RenderPass | 0.055 |
| Occlusion Cull RenderPass | 0.045 |
| Material Opaque RenderPass | 0.041 |
| Material Prep RenderPass | 0.034 |
| Material Classify / Display RenderPass | 0.022 |
| Light Culling / HZB / Effects RenderPass | ~0.016 each |

**Frame time: 8.3 ms/frame (≈120 FPS), vsync-capped** at 1392×746 — i.e. the renderer has
headroom on a 1.08M-tri-source / 2.39M-tri-DAG asset; it is display-refresh-bound, not
GPU-bound, at this resolution.

## Conclusion

A 1.08M-triangle source (2.39M-triangle DAG) renders through the runtime cluster-LOD path with
**~83 MB of resident geometry** and a **drawn cut of 4.9k–14.8k triangles** (0.2–0.6% of the
DAG), the cut scaling with viewport height + camera and independent of source size — exactly the
north-star's two bounds (bounded VRAM, bounded draw). A2 + A6 verified.
