#!/usr/bin/env node
// Editor memory-leak soak harness (docs/plans/crashes.md, Phase 2).
//
// Launches a dedicated Chrome instance pointed at the editor, loads a
// representative project and leaves it IDLE (the crash pattern — an idle editor
// still runs its render loop), and samples memory every N seconds until the tab
// crashes or a duration cap elapses. Writes a CSV (curated stable columns, easy
// to eyeball/plot) plus a lossless JSONL (the full `memory_stats` census per
// sample) so no counter is ever dropped.
//
// Ground-truth virtual-address-space metric = `ps -o vsz` + `vmmap` TOTAL on the
// renderer PID (the direct analogue of the crash dumps'
// `page-allocator-mapped-size`). In-page census (create_buffer_*, ring_*, wasm
// heap, JS heap, object counts) localizes WHICH subsystem grows.
//
// ZERO npm deps: Node >=21 global WebSocket + stdlib only. Nothing here uses the
// MCP/CDP *tools* — it is a standalone process so an 8h soak costs no tokens.
//
// Usage:
//   node tools/soak/soak.mjs [--out DIR] [--sample N] [--memlog N]
//                            [--minutes M] [--url URL] [--load BASE] [--interactive]
// Env overrides mirror the flags (SOAK_OUT, SOAK_SAMPLE, ...). See DEFAULTS.

import { spawn, spawnSync } from "node:child_process";
import { mkdirSync, writeFileSync, appendFileSync, createWriteStream } from "node:fs";
import { join } from "node:path";
import http from "node:http";
import os from "node:os";

// ── config ───────────────────────────────────────────────────────────────────
const DEFAULTS = {
  url: "http://localhost:9085",
  load: "http://localhost:9084/ssr-arena/project", // test-scenes server; degrade-ok
  sample: 30, // seconds between samples
  memlog: 30, // ?memlog=N console-trail interval (backup trail)
  minutes: 600, // hard cap (10h) — a crash ends it sooner
  rssCapMb: 16000, // machine-safety cutoff: end the run if renderer RSS exceeds
  // this (half of a 32GB box). Protects an unattended overnight run from a fast
  // runaway; still leaves ample room for the leak to reproduce / the VA trap to fire.
  cdpPort: 9333,
  gcEvery: 0, // seconds; >0 → every N s force a V8 GC (HeapProfiler.collectGarbage
  // ×3) and log Tag-253/255 region-count + resident BEFORE→AFTER. The decisive
  // discriminator: if forced GC drops the leaked regions, the leak is COLLECTABLE
  // transient JS/wasm handles (fix = drop them promptly); if GC does nothing, it's
  // a true native Dawn/Metal VM-unmapping leak (a different fix).
  vmmapEvery: 0, // seconds; >0 → dump full `vmmap --summary` region tables for
  // EVERY chrome process of the instance to OUT/vmmap/ on that cadence. The
  // Phase-3 VA-region diagnostic: `vmmap --summary` TOTAL rounds to ~0.1T so it
  // hides a 70GB change inside Chrome's ~1.4TB constant reservation — but the
  // leaking region shows as its OWN row climbing MB→GB. Diff early-vs-late dumps.
  interactive: false,
  chrome: "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
};

function parseArgs() {
  const a = { ...DEFAULTS };
  const argv = process.argv.slice(2);
  for (let i = 0; i < argv.length; i++) {
    const k = argv[i];
    const next = () => argv[++i];
    if (k === "--out") a.out = next();
    else if (k === "--sample") a.sample = +next();
    else if (k === "--memlog") a.memlog = +next();
    else if (k === "--minutes") a.minutes = +next();
    else if (k === "--rss-cap-mb") a.rssCapMb = +next();
    else if (k === "--vmmap-every") a.vmmapEvery = +next();
    else if (k === "--gc-every") a.gcEvery = +next();
    else if (k === "--url-extra") a.urlExtra = next(); // extra query flags, e.g. "noring" (ablation)
    else if (k === "--url") a.url = next();
    else if (k === "--load") a.load = next();
    else if (k === "--no-load") a.load = "";
    else if (k === "--cdp-port") a.cdpPort = +next();
    else if (k === "--interactive") a.interactive = true;
    else if (k === "--chrome") a.chrome = next();
    else throw new Error(`unknown arg: ${k}`);
  }
  // env overrides (flags win)
  a.out ||= process.env.SOAK_OUT;
  a.sample = +process.env.SOAK_SAMPLE || a.sample;
  a.minutes = +process.env.SOAK_MINUTES || a.minutes;
  return a;
}

const CFG = parseArgs();
const OUT =
  CFG.out ||
  join(
    process.env.SOAK_OUT_ROOT || os.tmpdir(),
    `soak-${new Date().toISOString().replace(/[:.]/g, "-")}`,
  );
mkdirSync(OUT, { recursive: true });

const CSV_PATH = join(OUT, "soak.csv");
const JSONL_PATH = join(OUT, "soak.jsonl");
const LOG_PATH = join(OUT, "soak.log");
const logStream = createWriteStream(LOG_PATH, { flags: "a" });
function log(...m) {
  const line = `[${new Date().toISOString()}] ${m.join(" ")}`;
  process.stdout.write(line + "\n");
  logStream.write(line + "\n");
}

// Curated, stable CSV columns. Everything (incl. anything not listed here) also
// lands in the JSONL, so this list is for convenience, not completeness.
const CSV_COLS = [
  "elapsed_s",
  "wall_iso",
  "rss_kb",
  "vsz_kb",
  "vmmap_virtual_bytes",
  "renderer_pid",
  "renderer_count",
  // ground-truth-adjacent in-page signals
  "create_buffer_count",
  "create_buffer_bytes",
  "create_bind_group_count",
  "create_command_encoder_count",
  // in-page GPU-object census (injected JS prototype wrappers, not Rust) — the
  // readback + query-set per-frame-churn probes. Cumulative; divide the delta by
  // the soak_raf_frames delta for a per-frame rate.
  "map_async_count",
  "map_async_read_count",
  "map_async_write_count",
  "get_mapped_range_count",
  "unmap_count",
  "create_query_set_count",
  "create_texture_count",
  "create_view_count",
  "submit_count",
  "write_buffer_count",
  "soak_raf_frames",
  "dom_nodes",
  "perf_measures",
  "perf_marks",
  "ring_bytes_uploaded",
  "ring_fallback_count",
  "ring_peak_depth",
  "ring_map_async_wait_ms",
  "ring_resize_count",
  "js_heap_used_bytes",
  "js_heap_total_bytes",
  "wasm_heap_bytes",
  // object counts (leak localization)
  "meshes",
  "mesh_resources",
  "mesh_geometry_bytes",
  "transforms",
  "materials",
  "render_pipelines",
  "compute_pipelines",
  "shaders",
  "pool_textures",
  "cubemaps",
  "samplers",
  "dynamic_materials",
  "undo_bytes",
  "redo_bytes",
  "frame_dt_ms",
  "render_cpu_ms",
];
writeFileSync(CSV_PATH, CSV_COLS.join(",") + "\n");

// In-page GPU-object census. `create_buffer_*` / `create_bind_group_*` come from
// Rust-side atomics via memory_stats, but mapAsync (readbacks) and createQuerySet
// (timestamp/occlusion queries) sit behind no central Rust seam — so we count them
// by wrapping the WebGPU prototypes in the page. Cumulative counters + our OWN rAF
// tick (independent of the app's render loop) so per-frame rates are exact:
// `Δmap_async_count / Δsoak_raf_frames`. Idempotent; safe to eval more than once.
const INSTALL_INPAGE_CENSUS = `(() => {
  if (window.__soak) return 'already';
  const c = window.__soak = {
    map_async_count: 0, map_async_read_count: 0, map_async_write_count: 0,
    get_mapped_range_count: 0, unmap_count: 0,
    create_query_set_count: 0, create_texture_count: 0, create_view_count: 0,
    submit_count: 0, write_buffer_count: 0, soak_raf_frames: 0,
  };
  const wrap = (proto, name, key) => {
    if (!proto || typeof proto[name] !== 'function') return;
    const orig = proto[name];
    proto[name] = function (...a) { c[key]++; return orig.apply(this, a); };
  };
  const B = (typeof GPUBuffer !== 'undefined') && GPUBuffer.prototype;
  // mapAsync split by mode (READ=1 = readbacks, WRITE=2 = mapped-staging ring).
  // The ?noring ablation already exonerated the WRITE path, so READ/frame is the
  // real suspect. First arg is the GPUMapMode bitflags.
  if (B && typeof B.mapAsync === 'function') {
    const orig = B.mapAsync;
    B.mapAsync = function (mode, ...rest) {
      c.map_async_count++;
      if (mode & 1) c.map_async_read_count++;
      if (mode & 2) c.map_async_write_count++;
      return orig.call(this, mode, ...rest);
    };
  }
  wrap(B, 'getMappedRange', 'get_mapped_range_count');
  wrap(B, 'unmap', 'unmap_count');
  const D = (typeof GPUDevice !== 'undefined') && GPUDevice.prototype;
  wrap(D, 'createQuerySet', 'create_query_set_count');
  // Per-frame texture/view churn probe — a size-derived texture rebuilt every
  // frame (e.g. an odd-viewport round-trip bug) leaks GPU/VM backing without ever
  // touching create_buffer (flat) or create_bind_group (0). The gap in the census.
  wrap(D, 'createTexture', 'create_texture_count');
  const T = (typeof GPUTexture !== 'undefined') && GPUTexture.prototype;
  wrap(T, 'createView', 'create_view_count');
  const Q = (typeof GPUQueue !== 'undefined') && GPUQueue.prototype;
  wrap(Q, 'submit', 'submit_count');
  wrap(Q, 'writeBuffer', 'write_buffer_count');
  const raf = window.requestAnimationFrame.bind(window);
  const tick = () => { c.soak_raf_frames++; raf(tick); };
  raf(tick);
  return 'installed';
})()`;

// ── CDP over raw WebSocket ─────────────────────────────────────────────────────
function httpGetJson(url) {
  return new Promise((resolve, reject) => {
    http
      .get(url, (res) => {
        let body = "";
        res.on("data", (c) => (body += c));
        res.on("end", () => {
          try {
            resolve(JSON.parse(body));
          } catch (e) {
            reject(e);
          }
        });
      })
      .on("error", reject);
  });
}

function sleep(ms) {
  return new Promise((r) => setTimeout(r, ms));
}

async function waitForHttp(url, timeoutMs, label) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      await new Promise((resolve, reject) => {
        const req = http.get(url, (res) => {
          res.resume();
          resolve();
        });
        req.on("error", reject);
        req.setTimeout(2000, () => req.destroy(new Error("timeout")));
      });
      return true;
    } catch {
      await sleep(1000);
    }
  }
  throw new Error(`timed out waiting for ${label} (${url})`);
}

// Minimal CDP client: one WebSocket, id-keyed request/response, event listeners.
class CDP {
  constructor(wsUrl) {
    this.ws = new WebSocket(wsUrl);
    this.id = 0;
    this.pending = new Map();
    this.listeners = new Map();
    this.closed = false;
    this.ws.addEventListener("message", (ev) => {
      const msg = JSON.parse(ev.data);
      if (msg.id != null && this.pending.has(msg.id)) {
        const { resolve, reject } = this.pending.get(msg.id);
        this.pending.delete(msg.id);
        msg.error ? reject(new Error(JSON.stringify(msg.error))) : resolve(msg.result);
      } else if (msg.method) {
        (this.listeners.get(msg.method) || []).forEach((cb) => cb(msg.params));
      }
    });
    this.ws.addEventListener("close", () => (this.closed = true));
    this.ws.addEventListener("error", () => (this.closed = true));
  }
  open() {
    return new Promise((resolve, reject) => {
      this.ws.addEventListener("open", resolve, { once: true });
      this.ws.addEventListener("error", reject, { once: true });
    });
  }
  on(method, cb) {
    if (!this.listeners.has(method)) this.listeners.set(method, []);
    this.listeners.get(method).push(cb);
  }
  send(method, params = {}) {
    const id = ++this.id;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.ws.send(JSON.stringify({ id, method, params }));
      setTimeout(() => {
        if (this.pending.has(id)) {
          this.pending.delete(id);
          reject(new Error(`CDP ${method} timed out`));
        }
      }, 15000);
    });
  }
}

// ── OS-level VA sampling (the page-allocator-mapped-size analogue) ─────────────
function rendererPids(userDataDir) {
  const r = spawnSync("pgrep", ["-f", userDataDir], { encoding: "utf8" });
  if (r.status !== 0) return [];
  const pids = r.stdout.trim().split(/\s+/).filter(Boolean).map(Number);
  // keep only --type=renderer processes
  return pids.filter((pid) => {
    const c = spawnSync("ps", ["-o", "command=", "-p", String(pid)], { encoding: "utf8" });
    return c.status === 0 && c.stdout.includes("--type=renderer");
  });
}

// Every chrome process of this instance, with its --type (browser/gpu/renderer/…).
function instancePids(userDataDir) {
  const r = spawnSync("pgrep", ["-f", userDataDir], { encoding: "utf8" });
  if (r.status !== 0) return [];
  const pids = r.stdout.trim().split(/\s+/).filter(Boolean).map(Number);
  return pids.map((pid) => {
    const c = spawnSync("ps", ["-o", "command=", "-p", String(pid)], { encoding: "utf8" });
    const cmd = c.status === 0 ? c.stdout : "";
    const m = cmd.match(/--type=([a-z-]+)/);
    return { pid, type: m ? m[1] : "browser" };
  });
}

// Dump full `vmmap --summary` region tables for every instance process. Raw text
// (parsed at analysis time) so nothing fragile lives in the harness.
function dumpVmmap(userDataDir, dir, elapsed_s) {
  for (const { pid, type } of instancePids(userDataDir)) {
    const r = spawnSync("vmmap", ["--summary", String(pid)], {
      encoding: "utf8",
      timeout: 15000,
      maxBuffer: 64 * 1024 * 1024,
    });
    if (r.stdout) {
      writeFileSync(join(dir, `t${String(elapsed_s).padStart(5, "0")}s-${type}-${pid}.txt`), r.stdout);
    }
  }
}

function psRssVsz(pid) {
  const r = spawnSync("ps", ["-o", "rss=,vsz=", "-p", String(pid)], { encoding: "utf8" });
  if (r.status !== 0) return null;
  const m = r.stdout.trim().split(/\s+/).map(Number);
  return { rss_kb: m[0] || 0, vsz_kb: m[1] || 0 };
}

// vmmap --summary TOTAL line, VIRTUAL SIZE column (first number). Best-effort;
// slower than ps and can fail on a sandboxed child, so it degrades to null.
function vmmapVirtualBytes(pid) {
  const r = spawnSync("vmmap", ["--summary", String(pid)], {
    encoding: "utf8",
    timeout: 10000,
  });
  if (r.status !== 0 || !r.stdout) return null;
  // Line like: "TOTAL                            829.7M   118.4M ..."
  const line = r.stdout.split("\n").find((l) => /^TOTAL\b/.test(l.trim()));
  if (!line) return null;
  const m = line.trim().match(/^TOTAL\s+([0-9.]+)([KMGT]?)/);
  if (!m) return null;
  const mult = { "": 1, K: 1024, M: 1024 ** 2, G: 1024 ** 3, T: 1024 ** 4 }[m[2]];
  return Math.round(parseFloat(m[1]) * mult);
}

// Region count + resident bytes for a specific PageTag from `vmmap --summary`.
// Tag 253 = PartitionAlloc, Tag 255 = V8 page allocator (the crash dump's
// page-allocator-mapped-size). The trailing column is REGION COUNT, col 2 is
// RESIDENT. Best-effort; degrades to null.
function vmmapTagStats(pid, tag) {
  const r = spawnSync("vmmap", ["--summary", String(pid)], {
    encoding: "utf8",
    timeout: 15000,
    maxBuffer: 64 * 1024 * 1024,
  });
  if (r.status !== 0 || !r.stdout) return null;
  const re = new RegExp(`Memory Tag ${tag}\\b`);
  const line = r.stdout.split("\n").find((l) => re.test(l));
  if (!line) return null;
  const rest = line.split(re)[1].trim();
  const toks = rest.split(/\s+/);
  if (toks.length < 2) return null;
  const parseSz = (s) => {
    const m = String(s).match(/^([0-9.]+)([KMGT]?)$/);
    if (!m) return null;
    const mult = { "": 1, K: 1024, M: 1024 ** 2, G: 1024 ** 3, T: 1024 ** 4 }[m[2]];
    return Math.round(parseFloat(m[1]) * mult);
  };
  return { regions: parseInt(toks[toks.length - 1], 10), resident: parseSz(toks[1]) };
}

// pick the renderer that is "the tab" = max RSS among renderers
function sampleOs(userDataDir) {
  const pids = rendererPids(userDataDir);
  if (pids.length === 0) return { renderer_count: 0, renderer_pid: 0 };
  let best = null;
  for (const pid of pids) {
    const rv = psRssVsz(pid);
    if (rv && (!best || rv.rss_kb > best.rss_kb)) best = { pid, ...rv };
  }
  if (!best) return { renderer_count: pids.length, renderer_pid: 0 };
  return {
    renderer_count: pids.length,
    renderer_pid: best.pid,
    rss_kb: best.rss_kb,
    vsz_kb: best.vsz_kb,
    vmmap_virtual_bytes: vmmapVirtualBytes(best.pid),
  };
}

// ── main ──────────────────────────────────────────────────────────────────────
let chromeProc = null;
let done = false;

function finish(reason, extra = {}) {
  if (done) return;
  done = true;
  const summary = {
    reason,
    ended: new Date().toISOString(),
    elapsed_s: Math.round((Date.now() - START) / 1000),
    out_dir: OUT,
    ...extra,
  };
  writeFileSync(join(OUT, "summary.json"), JSON.stringify(summary, null, 2));
  log(`SOAK END: ${reason} after ${summary.elapsed_s}s — ${OUT}`);
  try {
    chromeProc && chromeProc.kill("SIGTERM");
  } catch {}
  logStream.end();
  setTimeout(() => process.exit(0), 500);
}

const START = Date.now();

async function main() {
  const encLoad = CFG.load ? `&load=${encodeURIComponent(CFG.load)}` : "";
  const extra = CFG.urlExtra ? `&${CFG.urlExtra}` : "";
  const pageUrl = `${CFG.url}/?memlog=${CFG.memlog}${encLoad}${extra}`;
  const userDataDir = join(OUT, "chrome-profile");

  log(`SOAK START out=${OUT}`);
  log(`waiting for editor ${CFG.url} ...`);
  await waitForHttp(CFG.url, 180000, "editor");
  if (CFG.load) {
    const base = CFG.load.replace(/\/$/, "");
    try {
      await waitForHttp(`${base}/project.toml`, 20000, "project");
      log(`project reachable: ${base}/project.toml`);
    } catch (e) {
      log(`WARN project not reachable (${e.message}) — soak will run the idle default scene`);
    }
  }

  // Launch a dedicated Chrome. Non-headless (reliable WebGPU) + anti-throttle so
  // the idle render loop stays at full rate even if the window is occluded.
  const chromeArgs = [
    `--remote-debugging-port=${CFG.cdpPort}`,
    `--user-data-dir=${userDataDir}`,
    "--no-first-run",
    "--no-default-browser-check",
    "--disable-background-timer-throttling",
    "--disable-backgrounding-occluded-windows",
    "--disable-renderer-backgrounding",
    "--disable-features=CalculateNativeWinOcclusion",
    "--new-window",
    pageUrl,
  ];
  log(`launching chrome → ${pageUrl}`);
  chromeProc = spawn(CFG.chrome, chromeArgs, { stdio: "ignore", detached: false });
  chromeProc.on("exit", (code) => {
    if (!done) finish("chrome-exited", { code });
  });

  // Find the page target.
  await waitForHttp(`http://localhost:${CFG.cdpPort}/json/version`, 30000, "cdp");
  let target = null;
  for (let i = 0; i < 30 && !target; i++) {
    const targets = await httpGetJson(`http://localhost:${CFG.cdpPort}/json`);
    target = targets.find((t) => t.type === "page" && t.url.includes(new URL(CFG.url).host));
    if (!target) await sleep(1000);
  }
  if (!target) return finish("no-page-target");
  log(`attached page target ${target.id}`);

  const cdp = new CDP(target.webSocketDebuggerUrl);
  await cdp.open();
  await cdp.send("Runtime.enable");
  await cdp.send("Inspector.enable").catch(() => {});
  await cdp.send("HeapProfiler.enable").catch(() => {}); // for collectGarbage (--gc-every)
  cdp.on("Inspector.targetCrashed", () => finish("target-crashed"));
  cdp.on("Runtime.exceptionThrown", (p) => {
    const txt = p?.exceptionDetails?.exception?.description || p?.exceptionDetails?.text || "";
    if (txt) appendFileSync(join(OUT, "exceptions.log"), `[${new Date().toISOString()}] ${txt}\n`);
  });

  // Give the editor time to boot + auto-load the project before the first sample.
  log("waiting 45s for editor boot + project load ...");
  await sleep(45000);

  async function evalInPage(expr, awaitPromise = false) {
    const r = await cdp.send("Runtime.evaluate", {
      expression: expr,
      awaitPromise,
      returnByValue: true,
    });
    if (r.exceptionDetails) throw new Error(r.exceptionDetails.text);
    return r.result.value;
  }

  // Install the in-page GPU-object census (readback / query-set prototype
  // wrappers). Counters run monotonically from here, so the per-frame RATE is
  // correct regardless of this install offset.
  try {
    const res = await evalInPage(INSTALL_INPAGE_CENSUS);
    log(`in-page GPU census: ${res}`);
  } catch (e) {
    log(`WARN in-page census install failed: ${e.message}`);
  }

  async function queryInPageCensus() {
    try {
      // __soak counters + live DOM stats. dom_nodes = attached element count
      // (Blink C++/Tag253 backing); a per-frame climb ⇒ overlay DOM accumulation.
      // dom_listeners is a rough attached-node total; detached-but-retained nodes
      // won't show here (they need a heap snapshot).
      return (
        (await evalInPage(`(() => {
          const s = window.__soak || {};
          let pm = 0, pk = 0;
          try { pm = performance.getEntriesByType('measure').length; } catch {}
          try { pk = performance.getEntriesByType('mark').length; } catch {}
          return Object.assign({}, s, {
            dom_nodes: document.getElementsByTagName('*').length,
            perf_measures: pm,
            perf_marks: pk,
          });
        })()`)) || {}
      );
    } catch {
      return {};
    }
  }

  // Force a full GC and measure whether the leaked Tag-253/255 regions are
  // reclaimed. Decisive: collectable-JS-handle churn vs true native VM leak.
  async function gcProbe(pid, elapsed_s) {
    const before253 = vmmapTagStats(pid, 253);
    const before255 = vmmapTagStats(pid, 255);
    for (let i = 0; i < 3; i++) {
      await cdp.send("HeapProfiler.collectGarbage").catch(() => {});
      await sleep(500);
    }
    await sleep(1500); // let native finalizers run + regions unmap
    const after253 = vmmapTagStats(pid, 253);
    const after255 = vmmapTagStats(pid, 255);
    const fmt = (b, a) =>
      `regions ${b?.regions}→${a?.regions} (Δ${(a?.regions ?? 0) - (b?.regions ?? 0)}), ` +
      `resident ${Math.round((b?.resident ?? 0) / 1e6)}M→${Math.round((a?.resident ?? 0) / 1e6)}M`;
    log(`GC-PROBE t=${elapsed_s}s  Tag253 ${fmt(before253, after253)}  |  Tag255 ${fmt(before255, after255)}`);
  }

  async function queryCensus() {
    // window.wasmBindings.editor_query_json is async → awaitPromise.
    const raw = await evalInPage(
      `window.wasmBindings.editor_query_json('{"query":"memory_stats"}')`,
      true,
    );
    const parsed = JSON.parse(raw);
    // QueryResult::Map serializes as {"Map":{"kind":"memory_stats","entries":{...}}}
    return parsed?.Map?.entries || parsed?.entries || parsed || {};
  }

  // Optional interactive variant: nudge the camera / undo-redo each minute to
  // cover interactive paths (picker readback, command churn) the idle run misses.
  let interactiveTick = 0;
  async function interactivePoke() {
    if (!CFG.interactive) return;
    interactiveTick++;
    try {
      // orbit the camera a touch via a synthetic drag over the canvas
      await evalInPage(`(() => {
        const c = document.querySelector('canvas'); if (!c) return 'no-canvas';
        const r = c.getBoundingClientRect();
        const x = r.left + r.width/2, y = r.top + r.height/2;
        const opt = (t, dx=0) => new PointerEvent(t, {bubbles:true, clientX:x+dx, clientY:y, button:0, buttons:1});
        c.dispatchEvent(opt('pointerdown'));
        for (let i=1;i<=8;i++) c.dispatchEvent(opt('pointermove', i*6));
        c.dispatchEvent(new PointerEvent('pointerup', {bubbles:true, clientX:x+48, clientY:y}));
        return 'ok';
      })()`);
    } catch (e) {
      log(`interactive poke failed: ${e.message}`);
    }
  }

  let firstCensus = null;
  let sampleN = 0;
  const capMs = CFG.minutes * 60 * 1000;
  const vmmapDir = join(OUT, "vmmap");
  let lastVmmapAt = -Infinity;
  let lastGcAt = -Infinity;
  if (CFG.vmmapEvery > 0) {
    mkdirSync(vmmapDir, { recursive: true });
    log(`vmmap region dumps every ${CFG.vmmapEvery}s → ${vmmapDir}`);
  }
  if (CFG.gcEvery > 0) log(`forced-GC region probe every ${CFG.gcEvery}s`);

  async function tick() {
    if (done) return;
    if (Date.now() - START > capMs) return finish("duration-cap");
    if (cdp.closed) return finish("cdp-disconnected");

    const elapsed_s = Math.round((Date.now() - START) / 1000);
    const wall_iso = new Date().toISOString();
    let census = {};
    try {
      census = await queryCensus();
    } catch (e) {
      // A crashed/unresponsive renderer makes evaluate throw — treat as crash
      // once the OS PID is gone; otherwise log + keep sampling OS metrics.
      log(`census query failed at ${elapsed_s}s: ${e.message}`);
    }
    // In-page GPU-object census (readback/query-set prototype wrappers). Merged
    // into the same row so it lands in both JSONL + CSV.
    const inpage = await queryInPageCensus();
    Object.assign(census, inpage);
    const osm = sampleOs(userDataDir);
    if (osm.renderer_count === 0) return finish("renderer-process-gone", { last: firstCensus });
    // Periodic full-region vmmap dump (Phase-3 VA-region diagnostic).
    if (CFG.vmmapEvery > 0 && (Date.now() - START) / 1000 - lastVmmapAt >= CFG.vmmapEvery) {
      lastVmmapAt = (Date.now() - START) / 1000;
      try {
        dumpVmmap(userDataDir, vmmapDir, elapsed_s);
      } catch (e) {
        log(`vmmap dump failed at ${elapsed_s}s: ${e.message}`);
      }
    }
    // Forced-GC region probe (collectable-churn vs native-leak discriminator).
    if (
      CFG.gcEvery > 0 &&
      osm.renderer_pid &&
      (Date.now() - START) / 1000 - lastGcAt >= CFG.gcEvery
    ) {
      lastGcAt = (Date.now() - START) / 1000;
      try {
        await gcProbe(osm.renderer_pid, elapsed_s);
      } catch (e) {
        log(`gc probe failed at ${elapsed_s}s: ${e.message}`);
      }
    }
    // Machine-safety cutoff: bail before a runaway can thrash the box overnight.
    if (osm.rss_kb && osm.rss_kb / 1024 > CFG.rssCapMb) {
      return finish("rss-safety-cap", {
        rss_kb: osm.rss_kb,
        rss_cap_mb: CFG.rssCapMb,
        note: "renderer RSS exceeded the safety cap — a leak reproduced, cut to protect the machine",
      });
    }

    const row = { elapsed_s, wall_iso, ...osm, ...census };
    if (!firstCensus && Object.keys(census).length) firstCensus = { ...census, ...osm };

    // JSONL (lossless) + CSV (curated)
    appendFileSync(JSONL_PATH, JSON.stringify(row) + "\n");
    const csvRow = CSV_COLS.map((c) => {
      const v = row[c];
      return v == null ? "" : typeof v === "string" ? `"${v}"` : v;
    }).join(",");
    appendFileSync(CSV_PATH, csvRow + "\n");

    sampleN++;
    if (sampleN % 10 === 1 || CFG.sample >= 30) {
      log(
        `t=${elapsed_s}s rss=${osm.rss_kb}k vsz=${osm.vsz_kb}k vmmap=${osm.vmmap_virtual_bytes} ` +
          `cbuf=${census.create_buffer_count} cbg=${census.create_bind_group_count} ` +
          `cce=${census.create_command_encoder_count} ` +
          `map=${census.map_async_count} qset=${census.create_query_set_count} ` +
          `raf=${census.soak_raf_frames} ` +
          `wasm=${census.wasm_heap_bytes} jsheap=${census.js_heap_used_bytes} meshes=${census.meshes}`,
      );
    }

    // interactive poke roughly once a minute
    if (CFG.interactive && elapsed_s > 0 && Math.floor(elapsed_s / 60) > interactiveTick - 1) {
      await interactivePoke();
    }

    setTimeout(tick, CFG.sample * 1000);
  }

  log(`sampling every ${CFG.sample}s (cap ${CFG.minutes}m, interactive=${CFG.interactive})`);
  tick();
}

process.on("SIGINT", () => finish("sigint"));
process.on("SIGTERM", () => finish("sigterm"));
main().catch((e) => finish("harness-error", { error: e.message, stack: e.stack }));
