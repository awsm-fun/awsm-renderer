#!/usr/bin/env bash
# Regenerate src/bindings.rs from a MuJoCo release's headers.
#
#   cargo install bindgen-cli
#   MUJOCO_DIR=~/.local/share/mujoco/3.11.0 packages/crates/mujoco-sys/regen-bindings.sh
#
# Afterwards bump EXPECTED_VERSION in src/lib.rs to match the release.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
: "${MUJOCO_DIR:?set MUJOCO_DIR to an unpacked MuJoCo release (see README.md)}"

# MuJoCo's headers include each other as <mujoco/mjfoo.h>, so the include root
# must be a directory *containing* a `mujoco` dir. The macOS framework layout
# puts them in `Headers/`, so stage a symlink; other layouts already have
# `include/mujoco`.
staging="$(mktemp -d)"
trap 'rm -rf "$staging"' EXIT
if [ -d "$MUJOCO_DIR/mujoco.framework/Headers" ]; then
  ln -s "$MUJOCO_DIR/mujoco.framework/Headers" "$staging/mujoco"
elif [ -d "$MUJOCO_DIR/include/mujoco" ]; then
  ln -s "$MUJOCO_DIR/include/mujoco" "$staging/mujoco"
else
  echo "no MuJoCo headers under $MUJOCO_DIR" >&2
  exit 1
fi

# --dynamic-loading is the point: the output is a struct of symbols resolved off
# a libloading::Library, so the crate needs no build.rs and no link flags.
bindgen "$staging/mujoco/mujoco.h" -o "$here/src/bindings.rs" \
  --allowlist-type '^mj.*' \
  --allowlist-var '^mj.*' \
  --allowlist-function '^mj_(loadXML|loadModel|loadModelBuffer|deleteModel|version|versionString|id2name|name2id|makeData|deleteData|step|forward|resetData|resetDataKeyframe)$' \
  --dynamic-loading Mujoco --dynamic-link-require-all \
  --no-layout-tests --no-doc-comments --default-enum-style rust_non_exhaustive \
  -- -I "$staging"

echo "wrote $here/src/bindings.rs"
