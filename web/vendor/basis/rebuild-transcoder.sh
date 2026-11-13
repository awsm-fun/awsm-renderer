#!/usr/bin/env bash
# Rebuild the eval-free Basis Universal transcoder (dist/basis_transcoder.{js,wasm}) from pinned sources.
#
#   ./rebuild.sh <work-dir>
#
# Everything (git clones, emsdk + its toolchain download, build tree) lives under <work-dir>; nothing is
# installed system-wide and no shell profile is touched. Needs: git, cmake >= 3.5, make, python3, curl/xz
# (emsdk's own requirements). Verified on macOS 26 arm64 (Apple M1 Max) on 2026-11-13: running this in a
# fresh directory reproduced dist/ byte-for-byte (see README "Reproducibility").
set -euo pipefail

WORK="${1:?usage: rebuild.sh <work-dir>}"
BASISU_REPO=https://github.com/BinomialLLC/basis_universal.git
BASISU_COMMIT=073eb6a0a34a23d77e75e54f41dcd9653a52da83   # master, 2026-10-30 (no tag contains NO_DYNAMIC_EXECUTION yet)
EMSDK_REPO=https://github.com/emscripten-core/emsdk.git
EMSDK_COMMIT=708dcbcc9ee23bc7d2dad44fc90db377e38b771c     # emsdk tip on 2026-11-12
EMSCRIPTEN_VERSION=6.0.10                                 # the emcc upstream used for its own artifacts at this commit

EXPECT_JS=5e9822cd29f98488db9ef26bc77e6d1dd551ed98d240de8c04cf7139be737b59
EXPECT_WASM=751a007d46cd295f1aed9bffe593aaf8fbca3b3622aeef5892253845f61ef7fc

mkdir -p "$WORK"
cd "$WORK"

[ -d basis_universal/.git ] || git clone --quiet "$BASISU_REPO" basis_universal
git -C basis_universal fetch --quiet origin "$BASISU_COMMIT" 2>/dev/null || true
git -C basis_universal checkout --quiet --detach "$BASISU_COMMIT"

[ -d emsdk/.git ] || git clone --quiet "$EMSDK_REPO" emsdk
git -C emsdk checkout --quiet --detach "$EMSDK_COMMIT"
./emsdk/emsdk install "$EMSCRIPTEN_VERSION"
./emsdk/emsdk activate "$EMSCRIPTEN_VERSION"      # writes emsdk/.emscripten only
# shellcheck disable=SC1091
source ./emsdk/emsdk_env.sh >/dev/null
emcc --version | head -1

# NO_DYNAMIC_EXECUTION=ON is upstream's CMake option (PR #474): it appends
#   -s DYNAMIC_EXECUTION=0 -s EMBIND_AOT=1
# to upstream's link flags. EMBIND_AOT=1 means "AOT embind invokers" on Emscripten 3.1.51 .. 7.0.0;
# on 7.0.1+ it means something else (ES-module exports) -- do NOT bump past 7.0.0 without re-checking.
# Memory flags go in via CMAKE_EXE_LINKER_FLAGS so upstream sources stay unpatched.
#
# BASISD_SUPPORT_ETC2_EAC_RG11=1: upstream's web CMakeLists hard-codes it to 0, which makes every
# ETC1S -> EAC RG11 transcode fail. The renderer picks EAC RG11 for two-channel-packed normal maps on
# ETC2-only GPUs (most phones), so an ETC1S normal map never loaded there. The CMakeLists' own
# -D...=0 comes first on the compile line; -U then -D after it (CMAKE_CXX_FLAGS) turns it on without
# patching upstream and without a macro-redefinition warning. Costs 5.1 KB of wasm (+0.6 KB brotli);
# every other target's output is byte-identical with and without it.
rm -rf build
emcmake cmake -S basis_universal/webgl/transcoder -B build -G "Unix Makefiles" \
  -DNO_DYNAMIC_EXECUTION=ON \
  "-DCMAKE_EXE_LINKER_FLAGS=-sINITIAL_MEMORY=16MB -sMAXIMUM_MEMORY=256MB" \
  "-DCMAKE_CXX_FLAGS=-UBASISD_SUPPORT_ETC2_EAC_RG11 -DBASISD_SUPPORT_ETC2_EAC_RG11=1"
cmake --build build -j 8 -- VERBOSE=1 | grep -E -- '-o basis_transcoder\.js' || true

got_js=$(shasum -a 256 build/basis_transcoder.js | cut -d' ' -f1)
got_wasm=$(shasum -a 256 build/basis_transcoder.wasm | cut -d' ' -f1)
echo "basis_transcoder.js   $got_js"
echo "basis_transcoder.wasm $got_wasm"
if [ "$got_js" = "$EXPECT_JS" ] && [ "$got_wasm" = "$EXPECT_WASM" ]; then
  echo "OK: byte-identical to dist/"
else
  echo "MISMATCH vs dist/ (expected js $EXPECT_JS, wasm $EXPECT_WASM)" >&2
  exit 1
fi
