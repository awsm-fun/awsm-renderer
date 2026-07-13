# examples/player-tests — the player-runtime test harness (plan 007)

Loads the baked test-scene **player bundles** from
[`examples/test-scenes/`](../test-scenes/README.md) through the **player**
consumption path — `awsm_renderer_scene_loader::load_scene_for_player` over an
`HttpAssets` source, exactly the shape a shipped web game uses — and runs
scripted runtime checks with machine-readable console output, so an agent can
assert on them headlessly (renderer `tracing::info!` also lands in the browser
console).

Unlike `examples/multithreaded` (the threaded reference app) this harness
embeds the renderer **single-threaded on the main thread** — the editor /
model-tests shape, built on the stable default toolchain. Deliberate: no
COOP/COEP isolation is needed, so the cross-origin bundle fetches from the
test-scenes server work with plain CORS.

## Running

```sh
task test-scenes    # terminal 1 — serves the bundles on :9084 (CORS on)
task player-tests   # terminal 2 — trunk-serves this harness on :9091
```

Open `http://localhost:9091` and read the console. One line per check:

```
PLAYER-TEST <name>: PASS — <detail>
PLAYER-TEST <name>: FAIL — <detail>
…
PLAYER-TESTS COMPLETE: <pass>/<total>
```

URL params: `?bundles=<origin>` (default `http://localhost:9084`),
`?scenes=a,b,c` (filter the per-scene checks), `?stream` / `?streambudget=N`
(cluster-streaming flags for `lod-nanite`, mirroring the editor's — they feed
`RendererFeatures::cluster_streaming_budget`, the loader's budget hook).

## The checks

Scene list + expectations are parametrized in `src/checks.rs` (`SCENES`).
Every scene gets a **fresh renderer + device** (cold, isolated loads).

| Check | What it asserts |
|---|---|
| `startup-census` | Fresh renderer, empty scene, BEFORE any load: render/compute-pipeline + shader floor under ceilings; the lazy families (decal/cluster/picking) are feature-gated off and bloom/SSR default off, so the recorded floor is the no-lazy-family number (006 axis 1). |
| `load-transaction:<scene>` | kitchen-sink, anim-skinned, lights-many, lod-classic, lod-nanite, instancing-stress, prefab-skinned-morph each cold-load through the loader's single begin→declare-all→commit transaction without error, then render 3 sanity frames. Load ms in the detail. |
| `counts:<scene>` | Materialized node count >0, ≥ the per-scene expectation, ≤ the authored count parsed from the bundle's `scene.toml`; renderer mesh count >0. |
| `instancing` | instancing-stress: steady-state rAF frame time < 20ms over 60 frames while the renderer mesh count stays <10 (3000 instances ride ONE instanced mesh row). |
| `nanite-streaming` | lod-nanite loads + renders under cluster-streaming residency, camera orbiting in/out so the cut re-selects; budget from `?streambudget=N` or the loader default via `RendererFeatures::cluster_streaming_budget`. |
| `prefab-churn` | prefab-static: 20 × `PrefabTemplate::instantiate` → render → `PrefabInstance::teardown`; geometry resources/bytes stay FLAT while instances live (clones share GPU buffers, 006 axis 4) and every count returns to baseline after. Falls back to whole-bundle load/unload ×5 (counts return to empty baseline) if a bundle has no prefab template. |
| `lod-tri-drop` | lod-classic: camera frames the LOD chain's base mesh (from `renderer.lod` + its world AABB — the scene bounds are floor-dominated, and selection is distance-to-object) at 2× its radius, then steps out (8/20/60/200×) until the selection reroutes to a coarser level; asserts the level index rises AND `visible_triangle_count` (the counter the editor status bar reads) drops. |

A failed load fails its dependent checks with a "skipped" detail, so the
total line count is deterministic for a headless driver.

## Numbers

Record census/load-time/frame-time numbers observed on a verified run here,
next to the 006 baselines in `examples/test-scenes/README.md`.
