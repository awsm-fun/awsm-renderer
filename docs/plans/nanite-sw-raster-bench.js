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
// STEP 2 (NEXT) — HW raster baseline vs Encoding A (packed-u32 atomicMax SW
// raster), swept over triangle pixel-size + overdraw. The critical gate: if A
// can't beat HW at small triangle sizes, NO-GO (don't even tune B).
//   - Triangle soup: N triangles of a target pixel-size, random placement in
//     a WxH viewport, random depth + unique payload.
//   - HW: minimal render pipeline writing payload (r32uint) + reverse-Z depth.
//   - A: compute, one workgroup/triangle, bbox scan + edge test, per covered
//     pixel atomicMax((depth16<<16)|payload16) into array<atomic<u32>>.
//   - Time each over many iters (queue.onSubmittedWorkDone); diff payload images.
// STEP 3 — Encoding B (depth atomic<u32> + payload u32 CAS loop): correctness
//   (payload error-rate vs HW) + perf under overdraw.
// ---------------------------------------------------------------------------
