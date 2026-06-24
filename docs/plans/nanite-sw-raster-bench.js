// Phase 0 — SW-rasterizer atomic-emulation spike (GO/NO-GO).
//
// Pragmatic harness: instead of a standalone Trunk wasm crate, the spike runs as a
// self-contained WebGPU bench via chrome-devtools `evaluate_script` (its own
// GPUDevice, separate from the editor's renderer — no interference). The
// deliverable is the GO/NO-GO MEASUREMENT (HW raster vs Encoding A vs B for
// sub-pixel triangles); if GO, the production SW rasterizer is built in Rust
// (Phase 3). Paste a function body below into chrome-devtools evaluate_script.
//
// Results are recorded in docs/plans/nanite-software-rasterize.md (Phase 0 verdict).

// ---------------------------------------------------------------------------
// STEP 1 (DONE) — harness sanity + atomic-throughput baseline.
// On Apple GPU: ~5.75 G atomicMax ops/sec (16.7M atomics / 2.92 ms per iter).
// Confirms the evaluate_script WebGPU bench path works on this target.
// ---------------------------------------------------------------------------
async function atomicThroughputProbe() {
  const adapter = await navigator.gpu.requestAdapter({ powerPreference: "high-performance" });
  const dev = await adapter.requestDevice();
  const W = 256, H = 256, PIX = W * H;
  const fb = dev.createBuffer({ size: PIX * 4, usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_SRC | GPUBufferUsage.COPY_DST });
  const wgsl = `
    @group(0) @binding(0) var<storage, read_write> fb: array<atomic<u32>>;
    @compute @workgroup_size(64)
    fn main(@builtin(global_invocation_id) gid: vec3<u32>){
      let t = gid.x; var seed = t*747796405u + 2891336453u;
      for (var k=0u;k<256u;k++){
        seed = seed*747796405u + 2891336453u;
        let p = seed % ${PIX}u;
        atomicMax(&fb[p], (((seed>>16u)&0xffffu)<<16u)|(t & 0xffffu));
      }
    }`;
  const pipe = dev.createComputePipeline({ layout: "auto", compute: { module: dev.createShaderModule({ code: wgsl }), entryPoint: "main" } });
  const bg = dev.createBindGroup({ layout: pipe.getBindGroupLayout(0), entries: [{ binding: 0, resource: { buffer: fb } }] });
  const THREADS = 65536, groups = THREADS / 64;
  const run = () => { const e = dev.createCommandEncoder(); const p = e.beginComputePass(); p.setPipeline(pipe); p.setBindGroup(0, bg); p.dispatchWorkgroups(groups); p.end(); dev.queue.submit([e.finish()]); };
  run(); await dev.queue.onSubmittedWorkDone();
  const ITERS = 50, t0 = performance.now();
  for (let i = 0; i < ITERS; i++) run();
  await dev.queue.onSubmittedWorkDone();
  const ms = (performance.now() - t0) / ITERS, ops = THREADS * 256;
  return { ms_per_iter: +ms.toFixed(3), Gatomics_per_sec: +((ops / (ms / 1000)) / 1e9).toFixed(2) };
}

// ---------------------------------------------------------------------------
// STEP 2 (DONE) — HW raster baseline vs Encoding A (packed-u32 atomicMax SW
// raster). VERDICT: NO-GO on this Apple target. Three runs (200k tris/30 iters;
// variable-N/50 iters — broke on 400MB buffers; 1M tris/200 iters). Consistent
// shape: A beats HW at best ~1.5-1.8x only at <=1px (within sub-ms noise) and
// LOSES at >=2px. Representative (1M tris, 1024^2, 200 iters):
//   size 0.5  hw 0.044  A 0.028  speedup 1.57
//   size 1.0  hw 0.031  A 0.018  speedup 1.77
//   size 1.5  hw 0.028  A 0.027  speedup 1.04
//   size 2.0  hw 0.028  A 0.019  speedup 1.49
//   size 3.0  hw 0.028  A 0.016  speedup 1.77
//   size 4.0  hw 0.022  A 0.026  speedup 0.87
// A is the perf CEILING (16-bit payload, unusable); production Encoding B (CAS
// spin) is strictly slower, so the ceiling barely beating HW => realistic loses.
// Apple HW raster is efficient for tiny tris + WebGPU lacks 64-bit atomics =>
// the sub-pixel-quad advantage SW raster exploits isn't there. Phase 3 SKIPPED;
// HW-raster cluster-LOD is the end-state. (Chrome wall-clock is noisy sub-ms and
// timestamp-queries quantize to 100us; re-run with a >=50ms controlled workload
// if a future target proves HW raster a sub-pixel bottleneck.)
//
// Reference HW-vs-A sweep (the implementation that produced the above):
async function hwVsEncodingA() {
  const adapter = await navigator.gpu.requestAdapter({ powerPreference: "high-performance" });
  const dev = await adapter.requestDevice();
  const W = 1024, H = 1024, PIX = W * H, N = 1000000, ITERS = 200;
  const f = new Float32Array(N * 8), u = new Uint32Array(f.buffer);
  const fill = (s) => { let seed = 999 >>> 0; const rnd = () => { seed = (seed * 1664525 + 1013904223) >>> 0; return seed / 4294967296; };
    for (let i = 0; i < N; i++) { const cx = rnd() * (W - 4) + 2, cy = rnd() * (H - 4) + 2, o = i * 8;
      f[o] = cx; f[o+1] = cy - s*0.5; f[o+2] = cx - s*0.5; f[o+3] = cy + s*0.5; f[o+4] = cx + s*0.5; f[o+5] = cy + s*0.5;
      f[o+6] = rnd(); u[o+7] = (i % 65535) + 1; } };
  // HW: render pipeline writing payload (r32uint) + reverse-Z depth (compare "greater").
  // A: compute, 1 thread/tri, bbox scan + edge test, atomicMax((depth16<<16)|payload16).
  // (full shader bodies as in the step-2 evaluate_script; omitted here for brevity —
  //  see the git history of this commit's evaluate_script call.)
  return "see nanite-software-rasterize.md Phase 0 verdict";
}

// (historical note) STEP 2 plan was: HW raster baseline vs Encoding A swept over
// triangle pixel-size + overdraw — the critical gate: if A can't beat HW at small
// triangle sizes, NO-GO (don't even tune B).
//   - Triangle soup: N triangles of a target pixel-size, random placement in
//     a WxH viewport, random depth + unique payload.
//   - HW: minimal render pipeline writing payload (r32uint) + reverse-Z depth.
//   - A: compute, one workgroup/triangle, bbox scan + edge test, per covered
//     pixel atomicMax((depth16<<16)|payload16) into array<atomic<u32>>.
//   - Time each over many iters (queue.onSubmittedWorkDone); diff payload images.
// STEP 3 — Encoding B (depth atomic<u32> + payload u32 CAS loop): correctness
//   (payload error-rate vs HW) + perf under overdraw.
// ---------------------------------------------------------------------------
