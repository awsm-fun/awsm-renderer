#!/bin/zsh
# Launch a team Chrome on the built-in Retina display, at its natural size
# (the same display + size round 1 measured on), not maximised.
#   chrome-retina.sh <debug-port> <user-data-dir> [extra chrome flags...] [url]
#   CHROME_SIZE=WxH chrome-retina.sh ...   # optional explicit window size (points)
# The display position is looked up at launch, so re-arranging monitors is fine.
# After launch, it also moves the window over CDP, because a reused profile
# can restore a saved window position that overrides the flags.
set -e
PORT=$1; PROFILE=$2; shift 2
HERE=${0:A:h}
read X Y W H <<<"$(swift "$HERE/screens.swift" 2>/dev/null | awk '/builtin=true/ {
  for (i=1;i<=NF;i++) { split($i,kv,"="); v[kv[1]]=kv[2] } print v["x"], v["y"], v["w"], v["h"] }')"
if [[ -z "$X" ]]; then echo "no built-in display found" >&2; exit 1; fi
mkdir -p "$PROFILE"
SIZE_FLAG=()
[[ -n "$CHROME_SIZE" ]] && SIZE_FLAG=(--window-size=${CHROME_SIZE/x/,})
"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
  --remote-debugging-port=$PORT --user-data-dir="$PROFILE" \
  --no-first-run --no-default-browser-check \
  --disable-background-timer-throttling --disable-renderer-backgrounding \
  --disable-backgrounding-occluded-windows \
  --window-position=$((X+40)),$((Y+40)) $SIZE_FLAG "$@" >/dev/null 2>&1 &
# wait for CDP, then force the window onto the Retina display and maximise it there
for i in {1..50}; do curl -s "http://127.0.0.1:$PORT/json/version" >/dev/null && break; sleep 0.2; done
node - "$PORT" "$X" "$Y" "$W" "$H" "${CHROME_SIZE:-}" <<'JS'
const [port, x, y, w, h] = process.argv.slice(2, 7).map(Number);
const want = (process.argv[7] || '').split('x').map(Number);
(async () => {
  const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
  const page = targets.find(t => t.type === 'page');
  const ver = await (await fetch(`http://127.0.0.1:${port}/json/version`)).json();
  const ws = new WebSocket(ver.webSocketDebuggerUrl);
  let id = 0; const call = (method, params) => new Promise(r => {
    const myId = ++id; ws.addEventListener('message', function on(e) {
      const m = JSON.parse(e.data); if (m.id === myId) { ws.removeEventListener('message', on); r(m.result); } });
    ws.send(JSON.stringify({ id: myId, method, params })); });
  await new Promise(r => ws.addEventListener('open', r));
  const { windowId } = await call('Browser.getWindowForTarget', { targetId: page.id });
  await call('Browser.setWindowBounds', { windowId, bounds: { windowState: 'normal' } });
  const cur = (await call('Browser.getWindowBounds', { windowId })).bounds;
  // keep the natural size (or CHROME_SIZE), clamped to the display; only move it there
  const bw = Math.min(want[0] || cur.width, w), bh = Math.min(want[1] || cur.height, h);
  await call('Browser.setWindowBounds', { windowId, bounds: {
    left: x + Math.max(0, Math.min(40, w - bw)), top: y + Math.max(0, Math.min(40, h - bh)), width: bw, height: bh } });
  const b = await call('Browser.getWindowBounds', { windowId });
  console.log(`chrome on :${port} at ${JSON.stringify(b.bounds)} (Retina display ${x},${y} ${w}x${h})`);
  ws.close();
})().catch(e => { console.error('CDP placement failed:', e.message); process.exit(1); });
JS
