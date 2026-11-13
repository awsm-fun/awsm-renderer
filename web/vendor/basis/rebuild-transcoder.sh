#!/usr/bin/env bash
# Rebuild the eval-free, format-trimmed Basis Universal transcoder (basis_transcoder.{js,wasm}) from pinned sources.
#
#   ./rebuild-transcoder.sh <work-dir>
#
# Everything (git clones, emsdk + its toolchain download, build tree) lives under <work-dir>; nothing is
# installed system-wide and no shell profile is touched. Needs: git, cmake >= 3.5, make, python3, curl/xz
# (emsdk's own requirements). basisu-trim.patch must sit beside this script. Verified on macOS 26 arm64
# (Apple M1 Max) on 2026-11-13: a run in a fresh directory reproduced the artifacts byte-for-byte.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PATCH="$HERE/basisu-trim.patch"
WORK="${1:?usage: rebuild-transcoder.sh <work-dir>}"
BASISU_REPO=https://github.com/BinomialLLC/basis_universal.git
BASISU_COMMIT=073eb6a0a34a23d77e75e54f41dcd9653a52da83   # master, 2026-10-30 (no tag contains NO_DYNAMIC_EXECUTION yet)
EMSDK_REPO=https://github.com/emscripten-core/emsdk.git
EMSDK_COMMIT=708dcbcc9ee23bc7d2dad44fc90db377e38b771c     # emsdk tip on 2026-11-12
EMSCRIPTEN_VERSION=6.0.10                                 # the emcc upstream used for its own artifacts at this commit

EXPECT_PATCH=b463f81ecfaf66f9c52d258df000099caccd1231ac8c1f710907aa5129f043ed
EXPECT_JS=5e9822cd29f98488db9ef26bc77e6d1dd551ed98d240de8c04cf7139be737b59
EXPECT_WASM=2d7805eb0f95e6efaffbe082487fdb278ae02944f69c35e5d48a4673a2bf758f

got_patch=$(shasum -a 256 "$PATCH" | cut -d' ' -f1)
[ "$got_patch" = "$EXPECT_PATCH" ] || { echo "basisu-trim.patch sha256 $got_patch != $EXPECT_PATCH" >&2; exit 1; }

mkdir -p "$WORK"
cd "$WORK"

[ -d basis_universal/.git ] || git clone --quiet "$BASISU_REPO" basis_universal
git -C basis_universal fetch --quiet origin "$BASISU_COMMIT" 2>/dev/null || true
git -C basis_universal checkout --quiet --detach "$BASISU_COMMIT"
# This clone belongs to the script: drop any earlier copy of the patch, then apply it.
git -C basis_universal checkout --quiet -- transcoder/basisu_transcoder.cpp
git -C basis_universal apply "$PATCH"

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
# Memory flags go in via CMAKE_EXE_LINKER_FLAGS.
#
# Feature defines, all passed in CMAKE_CXX_FLAGS, which come AFTER the CMakeLists' own -D flags on the
# compile line (-U first where the CMakeLists defines the macro, so there is no redefinition warning):
#
# BASISD_SUPPORT_ETC2_EAC_RG11=1: upstream's web CMakeLists hard-codes it to 0, which makes every
#   ETC1S -> EAC RG11 transcode fail. The renderer picks EAC RG11 for two-channel-packed normal maps on
#   ETC2-only GPUs (most phones).
#
# Format trim (halves the .wasm). The renderer only ever hands the worker KTX2 files with vkFormat 0
# (codec-basis selection.rs sniff_basis_ktx2) and only asks for the 9 LDR targets in basis-worker.js
# TARGET_TO_ENUM; the editor only encodes ETC1S or UASTC LDR 4x4 (setUASTC). So:
#   BASISD_SUPPORT_UASTC_HDR=0  UASTC HDR 4x4 / ASTC HDR 6x6 / UASTC HDR 6x6 sources and the HDR targets
#                               (BC6H, ASTC HDR, RGB(A)_HALF, RGB_9E5). An HDR source can never produce any
#                               of the 9 LDR targets, in this build or the untrimmed one.
#   BASISD_SUPPORT_XUASTC=0     XUASTC LDR, ASTC LDR 4x4-12x12 and XUBC7 sources (v2 formats). ASTC LDR
#                               KTX2s carry a concrete vkFormat and never reach the worker; XUASTC LDR
#                               (vkFormat 0, supercompression 5, DFD model 169) is outside KHR_texture_basisu
#                               and nothing in the pipeline writes it. Such a file now fails cleanly
#                               (transcodeImage returns false -> worker 'transcode-failed').
#   BASISD_SUPPORT_PVRTC1=0     PVRTC1 targets, never requested.
# Upstream's switches do not compile when turned off at this commit; basisu-trim.patch adds the missing
# #if guards (18 lines, transcoder/basisu_transcoder.cpp only). With upstream's default defines the
# patched source builds a .wasm byte-identical to the untrimmed build, so the patch changes nothing
# but what the switches remove. It also makes UASTC_HDR=0 with XUASTC=1 build (not used here).
# Every source x target the renderer can request is byte-identical to the untrimmed build, and the
# embind surface (every export, class method and enum value) is unchanged.
rm -rf build
emcmake cmake -S basis_universal/webgl/transcoder -B build -G "Unix Makefiles" \
  -DNO_DYNAMIC_EXECUTION=ON \
  "-DCMAKE_EXE_LINKER_FLAGS=-sINITIAL_MEMORY=16MB -sMAXIMUM_MEMORY=256MB" \
  "-DCMAKE_CXX_FLAGS=-UBASISD_SUPPORT_ETC2_EAC_RG11 -DBASISD_SUPPORT_ETC2_EAC_RG11=1 -UBASISD_SUPPORT_UASTC_HDR -DBASISD_SUPPORT_UASTC_HDR=0 -DBASISD_SUPPORT_XUASTC=0 -DBASISD_SUPPORT_PVRTC1=0"
cmake --build build -j 8 -- VERBOSE=1 | grep -E -- '-o basis_transcoder\.js' || true

got_js=$(shasum -a 256 build/basis_transcoder.js | cut -d' ' -f1)
got_wasm=$(shasum -a 256 build/basis_transcoder.wasm | cut -d' ' -f1)
echo "basis_transcoder.js   $got_js"
echo "basis_transcoder.wasm $got_wasm"
if [ "$got_js" = "$EXPECT_JS" ] && [ "$got_wasm" = "$EXPECT_WASM" ]; then
  echo "OK: byte-identical to the vendored artifacts"
else
  echo "MISMATCH (expected js $EXPECT_JS, wasm $EXPECT_WASM)" >&2
  exit 1
fi
