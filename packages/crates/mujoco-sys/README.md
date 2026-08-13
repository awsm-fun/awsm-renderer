# awsm-renderer-mujoco-sys

Raw FFI to the official [MuJoCo](https://github.com/google-deepmind/mujoco) release
library, for **native tools only** (the exporter CLI). No MuJoCo code — runtime or
otherwise — ever ships in the renderer or in wasm; see `docs/mujoco.md`.

## Why dlopen instead of linking

The bindings are generated with bindgen's `--dynamic-loading`, so the crate has no
`build.rs`, no link flags, and no MuJoCo dependency at *build* time. That matters
because this is a workspace member: `cargo check`/`cargo test` across the workspace
must keep working on a machine (or CI runner) with no MuJoCo installed. Only
actually *loading* a model needs the library, and that failure is a clean
`Error::Library` at runtime.

## Installing MuJoCo

Grab the official release for your platform from
<https://github.com/google-deepmind/mujoco/releases> and unpack it, e.g. for macOS:

```sh
mkdir -p ~/.local/share/mujoco/3.11.0
curl -L -o /tmp/mujoco.dmg \
  https://github.com/google-deepmind/mujoco/releases/download/3.11.0/mujoco-3.11.0-macos-universal2.dmg
hdiutil attach -nobrowse -readonly /tmp/mujoco.dmg
cp -R /Volumes/MuJoCo/mujoco.framework /Volumes/MuJoCo/model ~/.local/share/mujoco/3.11.0/
hdiutil detach /Volumes/MuJoCo
```

Then point the tools at it:

```sh
export MUJOCO_DIR=~/.local/share/mujoco/3.11.0
```

`Library::load()` searches, in order: `$MUJOCO_LIB` (a full path to the shared
library), `$MUJOCO_DIR` (the unpacked release root — macOS framework, Linux
`lib/`, Windows `bin/`), then the plain platform soname on the system loader
path.

## Version lock

`mjModel`/`mjData` are plain C structs whose layout changes between MuJoCo
releases, so these bindings are locked to the release they were generated from
(`EXPECTED_VERSION`, currently **3.11.0**). `Library::load()` calls `mj_version()` and refuses a
mismatch loudly — a silently-misread struct would produce garbage geometry that
looks almost right.

## Regenerating

After bumping MuJoCo, re-run (needs `cargo install bindgen-cli`):

```sh
MUJOCO_DIR=~/.local/share/mujoco/<new-version> packages/crates/mujoco-sys/regen-bindings.sh
```

That's all: `EXPECTED_VERSION` reads the regenerated `mjVERSION_HEADER`, so there
is no second place to bump. Re-run the tests with `MUJOCO_DIR` set — with it set
they refuse to skip, so a bad load fails instead of quietly passing.
