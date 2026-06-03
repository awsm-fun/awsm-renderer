#!/usr/bin/env bash
set -euo pipefail

URL="${CHECK_URL:-http://localhost:${PORT}/?check=android}"

adb reverse "tcp:${PORT}" "tcp:${PORT}" >/dev/null

adb shell am force-stop com.android.chrome
sleep 0.5

adb shell am start \
  -n com.android.chrome/com.google.android.apps.chrome.Main \
  -a android.intent.action.VIEW \
  -d "$URL" >/dev/null

sleep 1

adb forward --remove tcp:9222 >/dev/null 2>&1 || true
adb forward tcp:9222 localabstract:chrome_devtools_remote >/dev/null

cargo run --bin android_chrome_check
