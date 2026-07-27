# Project load replayed a STALE `project.toml` from the browser cache

Status: **RETRACTED as an environment-persistence bug; fixed as an HTTP-cache
bug** (2026-07-26).

## What I claimed

That loading a project whose `specular`/`irradiance` slots are `EnvSlot::Ktx`
and saving it straight back silently reverted both to `sky_gradient` and dropped
the `.ktx2` side files — i.e. that env persistence was broken.

## What was actually happening

The editor was loading a **stale cached `project.toml`**. `http-server` (and
`python3 -m http.server`, and most static dev servers) send a long
`cache-control: max-age`; after re-saving the project over the same URL, the
browser kept serving the pre-edit copy. Everything downstream was correct — it
was faithfully persisting an environment that the file it *read* genuinely
didn't have.

Proof: instrumenting the parse showed
`specular=SkyGradient { zenith: [0.015, 0.02, 0.05], … }` — the **skybox's**
gradient values, i.e. the project as it looked *before* the IBL was added.
Copying the identical bytes to a fresh, never-fetched URL and loading that gave
`env_ktx_assets = 2` immediately. Independently corroborated: the
JETPACK-JOUST scene persists its environment and skybox fine.

## The real fix (applied)

`load_project_from_url` now fetches `project.toml` **and every side file** with
`web_sys::RequestCache::NoStore`, matching the existing precedent in
`renderer-gltf`'s loader (cache MODE rather than a `?cb=<ts>` cachebuster, so the
URL / cache key stays clean).

An authoring tool must never silently open a stale project. This failure mode is
maximally confusing: the scene loads, looks *plausible*, and quietly lacks
whatever you last saved — there is no error to notice, and the natural conclusion
is that saving is broken. It cost a long debugging session here and produced two
wrong bug reports before the cache was suspected.

## Kept from the false alarm

Two regression tests, both passing, both cheap, pinning the KTX environment shape
so a *future* real persistence break is caught immediately:

- `awsm-renderer-scene`: `environment::tests::ktx_specular_and_irradiance_parse_from_project_toml_shape`
- `awsm-renderer-editor-protocol`: `project::env_roundtrip_tests::project_toml_preserves_ktx_environment_slots`

## Lesson

Before concluding a persistence/serialization bug from a dev-server workflow,
confirm the bytes the app actually *received* — not the bytes on disk. A fresh
URL is a two-second test and would have skipped straight past this.
