# Android device verification checklist

**Hardware-gated.** Cannot be done from a developer machine. Pick up
this checklist when you have an Android phone plugged in with
`chrome://flags#enable-unsafe-webgpu` enabled.

The PR #99 work (pipeline-readiness state machine + lazy-pass
migrations + the Stage-3 MSAA edge-resolve replacement) was the whole
reason this branch exists — the goal was to make Android Chrome boot
the renderer without a `VK_ERROR_INITIALIZATION_FAILED` driver
rejection.

---

## Pre-flight

- [ ] Phone plugged in via USB, `adb devices` shows the device.
- [ ] Chrome's `chrome://flags#enable-unsafe-webgpu` is enabled.
- [ ] `task debug-mobile:chrome-check` runs successfully from the
      project root and routes Chrome to the phone's screen.

## Boot

- [ ] Init reaches `phase = Ready` with no `VK_ERROR_INITIALIZATION_FAILED`.
- [ ] Boot-timing log lines for the eager batch show <500 ms total compile.

## Render

- [ ] Load a test scene with a PBR mesh.
- [ ] Skybox + camera UI visible within ~500 ms of `phase = Ready`.
- [ ] PBR mesh appears within ~3 s (the primary pipeline compile
      time on the test Android device).
- [ ] No watchdog kills (`External Instance reference no longer exists`
      absent from logs).
- [ ] Cross-material MSAA edges render correctly (close-up of a
      two-material boundary looks right).

## Toggles

- [ ] MSAA off → on → off. Modal appears, scene recompiles, no
      driver rejection on the recompile.
- [ ] Bloom on. Bloom pipeline submits and resolves; effect appears
      post-recompile.
- [ ] Add a shadow-casting directional light. EVSM + ShadowGen
      submit and resolve; shadows render.

## Dynamic-materials cross-device

- [ ] On desktop: register a dynamic material via material-editor,
      save to project.
- [ ] On Android: load that project in scene-editor.
- [ ] The dynamic material's pipelines compile on Android; the
      material renders.

## Performance sanity

- [ ] At 1080p with a moderate scene (~100 k triangles, mixed
      materials), confirm 60 fps target is held.
- [ ] If not, capture a profile and note the bottleneck — most
      likely the edge-resolve auto-grow loop hasn't converged yet
      (give it a few seconds; should self-stabilize after 1–2
      mapAsync round trips).

---

## What to do if something fails

- **`VK_ERROR_INITIALIZATION_FAILED`**: regression of the whole PR #99
  goal. Check the boot-timing log for a non-eager pipeline being
  compiled at boot.
- **Mesh invisible**: search for `pipeline_scheduler::warn_pipeline_not_compiled`
  in `tracing` output. Indicates a missing trigger somewhere; check
  every mesh-insertion call site for the right lazy-compile hook.
- **MSAA edges look black on Android, correct on desktop**: the
  texture-pool-template-range bug fix from May 27 (`textures.rs::finalize_gpu_textures`)
  applied per-shader-id edge-resolve recompile alongside the primary
  opaque recompile. If this regresses, that's the place to look.
