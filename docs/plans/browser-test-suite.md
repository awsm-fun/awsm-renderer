# Browser test suite — comprehensive on-device verification

**Status:** planned, not started. This is the SSOT for building it. It is written to be executed **autonomously from a brand-new session** (assume you remember nothing and the machine may have just rebooted — nothing is running).

## Goal

A repeatable, agent-driven suite that verifies renderer features **in a real browser on a real GPU** — the tier CI structurally cannot cover — so that saying *"run all our awsm-renderer browser tests"* exercises everything and surfaces regressions. Driven by the **`awsm-renderer-browser-tests`** skill.

## Architecture — three layers

- **Layer A · Visual (agent-as-oracle).** For each feature: an authored **scene** (`author.js`, reproducible) + a **`verify.md`** recipe (states to drive + what "correct" looks like in words). The runner opens the scene in a browser via Chrome DevTools MCP, drives the states, screenshots, and **judges visually**. No pixel-diff/SSIM/golden-compare harness — the agent's eyes are the oracle (the user chose this explicitly over offscreen diffing).
- **Layer B · Structural (`examples/player-tests`).** Headless numeric/state asserts with machine-readable `PLAYER-TEST … PASS/FAIL` lines. Locks load-path/counts/texture-binding — including the compression + sprite/decal work.
- **Layer C · Editor/MCP audits (native `cargo test`, runs in CI).** Not browser tests: "all mutations route through EditorCommand" + "MCP tools/docs stay in sync".

The skill orchestrates all three and **prompts for scope** (`all / visual / structural / audits / pick features`).

---

## Session bootstrap — READ AND RUN FIRST (fresh session / post-reboot)

Nothing is running after a reboot. Before any test work:

1. **Repo + branch.** `cd /Users/dakom/Documents/AWSMFUN/AWSM-REPOS/renderer`. Work on branch **`skills`** (`git checkout skills && git pull --ff-only 2>/dev/null`) — it carries the committed skill + this plan, and everything for this effort stays on it. Commit incrementally so progress survives session boundaries.

2. **Ensure the skill is discoverable.** The committed source is `.agents/skills/awsm-renderer-browser-tests/SKILL.md`. The discovery symlink in `.claude/skills/` is gitignored + harness-volatile; if missing, recreate: `mkdir -p .claude/skills && ln -sfn ../../.agents/skills/awsm-renderer-browser-tests .claude/skills/awsm-renderer-browser-tests`.

3. **Start dev servers** (each `run_in_background: true`; skip any already serving — probe with `curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:<port>/`):
   - `task mcp-dev` — editor on **:9085** + MCP on **:9186** (the authoring + screenshot path). Flaky group; if it fails, start `task editor-dev` and the MCP separately.
   - `task test-scenes` — serves `examples/test-scenes/*/bundle` on **:9084** (player-tests input).
   - `task player-tests` — trunk-serves the harness on **:9091** (does NOT start test-scenes for you).
   - `task media-local` (**:9082**, glTF-Sample-Assets) + `task media-additional-assets` (**:9083**) — needed for texture/model imports during scene authoring. `:9082` root = `<repo>/media`.
   - **Wait for trunk builds to settle** before opening pages. `trunk serve`'s watcher is **mtime-blind** — it rebuilds on content change, not `touch`; if a rebuild seems stuck, append a real one-line comment to a watched crate to force it, then strip it. (memory: `stale-wasm-and-livereload-harness-law`)

4. **Driving the editor headlessly** (Layer A authoring + capture): use Chrome DevTools MCP `evaluate_script` to call `window.wasmBindings.editor_dispatch_json(json)` / `editor_query_json(json)` — the exact path every `examples/test-scenes/*/author.js` uses. `editor_dispatch_json` is fire-and-forget (returns `"ok"` before apply); settle with a query (`wait_render_settled`) before capture. Also available: `editor_tick_animation` (advance the clock deterministically — essential for "shadow moves with animation"), `editor_query_texture_png`.
   - **Screenshots:** MCP `screenshot_scene` (captures the live editor canvas) or Chrome DevTools `take_screenshot`. Returns base64 inline — decode to a file; **never** route bulk bytes through control channels (memory: `no-bulk-bytes-through-mcp-results`).
   - **MCP tools** (`export_player_bundle`, `save_project`, `screenshot_scene`, `set_*`): call over the MCP link. If the harness registers no awsm-scene MCP tools, drive HTTP via the `/tmp/mcp.py` fallback (memory: `mcp-direct-http-client`; note `/tmp` is wiped by reboot — recreate the helper if gone). `export_player_bundle` requires a `name` param.
   - Open a **fresh** editor tab at `http://localhost:9085/?mcp=http://127.0.0.1:9186`; never reuse another session's tab.
   - Renderer `tracing::info!/warn!` surface in the **browser console**, not the editor's log buffer (memory: `renderer-tracing-in-browser-console`).

5. **Recall these memories** before starting: `stale-wasm-and-livereload-harness-law`, `mcp-improvements-loop-mechanics`, `mcp-direct-http-client`, `renderer-tracing-in-browser-console`, `headless-editor-verify-no-evict`, `aa-verify-in-model-viewer`, `cluster-gpu-cut-draws-zero-p0` (frozen-tab ⇒ restart browser, don't blame renderer), `no-bulk-bytes-through-mcp-results`.

---

## The `verify.md` recipe convention (Layer A)

One `examples/test-scenes/<scene>/verify.md` per scene, committed next to `author.js`. Human- and agent-readable. Format:

```
# verify: <scene>
drive:  <ordered steps to pose the scene: load bundle (or load_project),
        tick animation to t=…, toggle SetViewOptions{…}, orbit camera, etc.,
        screenshotting the named states>
expect: <what CORRECT looks like, concretely, per state>
fail:   <the specific wrong-looking outcomes to reject>
```

The runner reads `verify.md`, executes `drive`, captures each state, and judges against `expect`/`fail`, reporting PASS/FAIL **with the screenshot(s) and its reasoning** so a human can spot-check. A scene with no `verify.md` is visual-untested.

---

## The skill upgrade (`.agents/skills/awsm-renderer-browser-tests/SKILL.md`)

Rewrite the skill to orchestrate all three layers:

1. **Enumerate** available checks: Layer A = every `examples/test-scenes/*/verify.md`; Layer B = the player-tests scene list; Layer C = the native audit tests.
2. **Prompt for scope** with AskUserQuestion: `Everything` / `Visual only` / `Structural only` / `Audits only` / `Pick features…` (multiselect of scene/feature groups). Default recommend Everything.
3. **Run Layer C first** (fast, native: `cargo test -p awsm-renderer-mcp` + the no-bypass lint) — cheap fail-fast.
4. **Run Layer B**: bootstrap servers, drive `:9091`, wait for `PLAYER-TESTS COMPLETE`, grep console for `PLAYER-TEST` lines (don't dump — it's huge), report `<pass>/<total>` + quote FAILs.
5. **Run Layer A**: for each selected `verify.md`, drive the editor, capture, judge, report with screenshot + reasoning.
6. **Summarize**: one table across all layers; leave servers running (offer to stop). Keep the existing gotchas section (mtime-blind watcher, frozen-tab, WebGPU requirement, ports).

---

## The work — checklist (execute in phase order; tick + annotate as you go)

Authoring-command references are `packages/mcp/editor-protocol/src/command.rs:<line>` unless noted. The universal per-scene skeleton: `new_project` → assets/materials → nodes → `SetViewOptions{grid:false,gizmos:false,light_gizmos:false}` → pinned `SetCameraOrbit` → `wait_render_settled` → `save_project` + `export_player_bundle` + `screenshot_scene` (golden). Copy an existing sibling `author.js` as the starting template.

### Phase 1 — skill + recipes over existing scenes + structural locks (fast, high value)

- [x] **Skill upgrade** to the 3-layer orchestration + scope prompt (spec above). — rewrote `SKILL.md`: enumerate + AskUserQuestion scope prompt (Everything/Visual/Structural/Audits/Pick), Layer C→B→A ordering, per-layer procedures, gotchas.
- [x] **`verify.md` for existing scenes** (no authoring; just author the recipe + confirm each renders correctly once):
  - [x] `pbr-extensions` — each ext sphere visibly distinct from plain PBR (transmission/clearcoat/sheen/iridescence/dispersion/anisotropy/specular/ior/emissive_strength/diffuse_transmission). — verify.md written; HEAD re-render confirmed all 12 variants distinct (13 buckets/13 materials).
  - [x] `shadows-all` — directional cascade + spot + point/cube all cast contact-tight shadows; no Peter-Pan gap, no donut/hole under lowered meshes. — verify.md written; HEAD confirmed all 3 light types cast, lowered-box contact-tight (no donut).
  - [x] Punctual lights — covered by `shadows-all` (spot+point) + `lights-many` + the seeded directional; recipe confirms each type illuminates + falls off correctly. — `lights-many/verify.md` written; HEAD confirmed all 36 point lights contribute distinct row-cycling pools (froxel reverse-Z lock holds), pillars directionally lit.
  - [x] `ssr`, `mirror`, `ssr-arena` — orbit to grazing angles; reflections continuous, track emitters, clean silhouettes; mirror pixel-shaped; arena floor mirrors rings, occluder stays soft maroon (not black). — 3 verify.md written; HEAD confirmed: ssr columns continuous + toggle removes reflections; mirror pixel-shaped incl thin torus; arena floor mirrors neon rings, occluder soft (not black).
  - [x] `anim-skinned` — mid-stride at t=0.5, no T-pose, no candy-wrapper collapse. — verify.md written (2 paths: load-project persisted pose + author.js replay for live scrubbing); HEAD confirmed mid-stride walk, textured, shadow, no candy-wrapper. Noted: set_frame_time does NOT re-scrub a loaded project (clip tracks target stale post-load node ids).
  - [x] `anim-morph` — two morph indices driven independently. — verify.md written (author.js replay path; load_project can't reproduce due to stale track targets); HEAD confirmed weights [0.5,0.5] + wedge (per-index compose lock holds). Settle-timing gotcha documented.
  - [x] `anim-blend` — blended pose distinct from either source. — verify.md written (author.js replay; mixer NLA); HEAD confirmed gait blend (2 mixer layers, playhead 0.35) + walk-only stance visibly differs, proving the Run layer blends in.
  - [x] `builtin-overrides` — 4 spheres, one shared asset, visibly different tunings (uniform overrides). — verify.md written; HEAD confirmed red/gold/clay/teal from 1 material (1 bucket).
  - [x] `dynamic-materials` — per-instance uniform override visibly diverges from shared default. — verify.md written; HEAD confirmed blue shared-tint vs orange per-instance override, custom lambert.
  - [x] **Adds:** `transparent` (through-glass ordering, no popping), `alpha-cutoff` (hard cutouts + double-sided back faces + cutouts in cast shadow), `env-ibl` (3 slots independently swapped; reflections track specular slot), `bloom-post` (halo scales with intensity; tonemapper switch re-grades), `decals` (lands on geometry only, no skybox bleed — re-verified this session), `instancing-stress` (thousands of instances, interactive), `kitchen-sink` (smoke). — all 7 verify.md written; each HEAD-confirmed (transparent depth-tint compose; cutout coverage+striped shadows; env studio reflection/irradiance/glossy; bloom scales 2/5/10; decal wraps floor+box no skybox bleed; 3000 instances / 4 nodes; kitchen-sink all-compose).
  - [x] **MSAA** recipe (any high-contrast-edge scene): `SetViewOptions{msaa:false}` → screenshot edge → `{msaa:true}` (structural recompile; `wait_render_settled`) → screenshot → confirm edges visibly change. `SetViewOptions` = command.rs:692; transient/view-only. — new `aa-edges` scene (author.js+verify.md, Layer-A only) + HEAD confirmed jaggy→smooth on the box diagonal silhouette across the msaa toggle (structural recompile settled).
  - [x] **SMAA** recipe: same pattern with `smaa` toggle — **prove pixels change** (turns "can't tell if it's on" into a real assertion). Use a glossy/edge-rich model (Fox is flat — see memory `aa-verify-in-model-viewer`). — covered by `aa-edges/verify.md` (smaa state); HEAD confirmed smaa post-process smooths the same silhouette edges vs no-aa (pixels change).
- [x] **Structural locks** (`examples/player-tests/src/checks.rs`):
  - [x] Add a **texture-binding** assertion: extend `Counts` to read the renderer texture-pool count; add `decals`, `particles`, and a sprite scene to `SCENES` with an `expected_min_textures`; assert bound > 0 and no `slot left unbound`. This locks the sprite/decal/particle silent-drop bug class. — Counts.pool_textures added; decals (min 1, KTX2), particles (0), and a new `sprite` scene (particle emitter binding a sprite KTX2, min 1) in SCENES; on-device player-tests PASS (decals pool=3, sprite pool=2). Note: sprite fountain only renders in the player runtime (headless editor render loop idles), so sprite is Layer-B-primary.
  - [x] Add an **opaque-KTX2** on-device lock: the decals/particles bundles carry KTX2 textures; assert they load + bind on-device (transitively exercises the opaque BC1/ETC2-RGB rung + alpha rungs). Optionally tally transcode targets if a cheap hook exists. — `decals` added to SCENES with expected_min_textures=1; player-tests PASS on-device (pool_textures=3 ≥1) — the KTX2 label sheet transcodes + binds, not silently dropped.

### Phase 2 — new gap scenes (author + `verify.md`)

- [x] **`cutoff-dynamic`** — custom-WGSL material with **Mask** alpha. `AddCustomMaterial`(492) → `SetCustomMaterialLayout`(1132) → `SetCustomMaterialWgsl`(529) → `SetCustomMaterialAlphaMode{Mask{cutoff}}`(1124) → `SetCustomMaterialAlphaWgsl`(538) → `RegisterMaterial`(522); assign via `AddMaterialVariant`(816)/`SelectMaterialVariant`(803). Verify: hard-edged cutout driven by custom WGSL alpha.
- [x] **`cutoff-anim-shadow`** — a masked mesh **animated under a light**, shadow must track the moving cutout. Masked material (built-in `SetBuiltinAlphaMode`(1180) or custom as above) + `AddClip`(1225) + `AddSpinTrack`/`AddTrack`(1266/1258) + `AddKeyframe`(1307); seeded directional light casts. Verify: `editor_tick_animation` t=0 vs t=0.5 — the shadow's holes track the cutout silhouette as it moves; no static shadow, no solid shadow ignoring cutouts.
- [x] **`contact-shadows`** — enable SSCS and PCSS (neither is exercised today; `shadows-all` is Soft PCF only). SSCS: `SetShadows{patch:{sscs_enabled:true, sscs_step_count:…}}`(582; `shadows_patch.rs:20`). PCSS: per-light `hardness:'pcss'` via `PatchKind` on the light node's `LightConfig.shadow.hardness` (`light.rs:142`; note `SetLightParam` does NOT cover hardness). Verify: contact-hardening penumbra that tightens at contact; SSCS short-range contact darkening.
- [x] **`dynamic-material-textures`** — custom WGSL sampling a texture. `SetCustomMaterialLayout{textures:[SlotSpec{name,ty:'texture_2d<f32>',color_kind}]}`(1132; SlotSpec at :123) → `ImportTextureFromUrl`(429) → `SetMaterialTexture{node,slot,texture}`(1198). Verify: the custom shader samples the bound texture (visibly textured, correct color space per `color_kind`). — author.js + bundle + golden + verify.md; fresh-replay verified: DuckCM albedo (yellow body, black/white eye, orange beak) sharp + correct sRGB via `material_sample_tex`/`material_uv` + `textures` include.
- [x] **`dynamic-material-attributes`** — per-instance data into a custom material. Either per-instance buffer (`SetCustomMaterialLayout{buffers:[SlotSpec{ty:'array<vec4<f32>>'}]}` + `SetMaterialBuffer`(1213)) or fragment interpolants (`SetCustomMaterialFragmentInputs`(1143), keys validated against `FRAGMENT_INPUT_KEYS`) fed by instancer `SetInstancerTransforms{per_instance_colors}`(293). Verify: per-instance divergence driven by attribute data, not uniforms. — took the fragment-interpolant path: one instancer + 12 rainbow `per_instance_colors` read via `material_vertex_color(input,0u)` (`vertex_color` include+input); author.js + bundle + golden + verify.md; fresh-replay verified 12 distinct hues from ONE material with ONE (identical) uniform.
- [x] **Extend `dynamic-materials`** — add a per-node **texture override** (`SetMaterialTexture`(1198)) so item "dynamic overrides · textures" is covered; update its `verify.md`. — material now has a `tex` slot sampled by the lambert fragment; left sphere binds the Cesium logo, right binds the Duck albedo (per-node `set_material_texture`), on top of the existing per-node tint override — both diverge per-node. author.js + bundle + golden (re-baked) + verify.md updated; on-device verified.
- [x] **Extend `builtin-overrides`** — add a per-node **texture override** (`SetBuiltinTexture`(1058)) alongside the uniform overrides; update its `verify.md`. — the top-left sphere now binds the Cesium logo to its inline `base_color` slot via `set_builtin_texture` (per-node, on the shared PBR asset) alongside the three scalar-tuned spheres; still 1 material / 1 bucket (builtin texture rides the same PBR pipeline). author.js + bundle + golden + verify.md updated; on-device verified.

### Phase 3 — native editor/MCP audits (Layer C, `cargo test`)

- [ ] **#9 "everything routes through EditorCommand".** Canonical scene state already has no bypass (chokepoint = `controller/state.rs` `apply`). Add a guard so it stays true: a CI/test lint asserting no `.<field>.set(`/`.set_neq(` on `EditorController` fields outside `packages/frontend/editor/src/controller/` (fields are `pub`). Allow-list the intentional view-only exceptions (`active_camera`, drawer/settings toggles). **Fix the real drift:** the camera clip-plane sliders (`app.rs:703,711`) mutate `settings.cam_clip_near/far` directly instead of dispatching `SetCameraClip` (which exists, `state.rs:3469`) — route them through the command.
- [ ] **#10 MCP tools/docs correctness.** `packages/mcp/tests/parity.rs` today only checks enum-tag vocabulary. Add: assert every `EditorCommand` wire tag has a row in `docs/mcp-parity.md` (parse the table) so "command added, no MCP tool/doc" fails a test. Param-shape/description drift is prose-hard — at minimum assert coverage + that each dedicated tool constructs an existing command variant. Document what remains manually reviewed.

---

## Working rules

- Keep `task lint` (rustfmt + clippy `-D warnings`, all features, tests) and `cargo test --all-features` **green** after every commit. `task lint` mirrors CI exactly.
- **Browser-verify** every Layer-A scene and every shader/transcode/AA change — `cargo test` green does NOT mean it renders (memory: `stale-wasm-and-livereload-harness-law`).
- **Never commit** `fixtures/local` bytes or paid acceptance assets; scenes are authored from generated or sample-server sources.
- Goldens are window-dependent visual references, not byte-exact CI locks — regenerate via the `author.js` replay path; explain any golden change in the commit.
- Commit per scene / per check (small, reversible). Tick the checkbox here + annotate the result in the commit.

## Definition of done

Every box above checked; the skill runs all three layers and prompts for scope; `task lint` + `cargo test --all-features` green; each new scene has `author.js` + `bundle/` + `golden.png` + `verify.md`; a full `awsm-renderer-browser-tests` run reports across all layers with no unexplained FAILs. Then this plan is deletable (capture residuals in memory).
