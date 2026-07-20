// test-scene: env-bc6h-spheres
//
// A roughness sweep in front of a REAL HDRI, on BC6H cubemaps.
//
// Purpose — this scene is the regression net for three things that were all
// invisible before it existed:
//
// 1. BC6H environment cubemaps load and render. All three slots bind
//    `VK_FORMAT_BC6H_UFLOAT_BLOCK` cubemaps (1 byte/texel, block-compressed in
//    VRAM, 4x smaller than B10G11R11). Baked by `awsm-renderer-env-bake`
//    (packages/tools/env-bake-cli) — the Khronos `ktx` CLI cannot encode BC6H.
//
// 2. The skybox actually varies by view direction. It used to render a single
//    flat colour at every camera angle: `sample_skybox` unprojected NDC z=0,
//    which under the infinite-far reverse-Z projection is the far plane at
//    INFINITY, giving w==0 → Inf → NaN → a NaN cube fetch. Every pixel got one
//    implementation-defined texel. A flat-ish studio HDRI hid it; a neon
//    cityscape does not.
//
// 3. The roughness ladder in `env.ktx2` is selected correctly. The metals run
//    0.02 → 0.22 → 0.45, which should read as: crisp windows/signs → smeared
//    streaks → broad tonal blocks. That walks mip 0 → ~2 of cmgen's 6
//    `--ibl-ld` bands via `roughness * max_mip`.
//
// Environment source: cybernetic-megalopolis (HDRi_Scifi_011_4K.hdr), measured
// max channel 8.5 with 2.5% of channels above 1.0 — genuinely HDR, unlike a
// tonemapped panorama re-saved as RGBE (which measures max ~1.0 and bakes to
// flat IBL no matter how carefully it is packed).
//
// Layer-A only (like aa-edges): MSAA/skybox behaviour is a view-time concern,
// not player-bundle state, so this scene ships author.js + verify.md with no
// bundle/ and no golden.png. See verify.md for the pass criteria and for the
// MSAA stability + quality numbers this framing is used to measure.
async () => {
  const d = async (o) => {
    const r = await window.wasmBindings.editor_dispatch_json(JSON.stringify(o));
    let v = r;
    try { v = JSON.parse(r); if (typeof v === 'string' && v !== 'ok') { try { v = JSON.parse(v); } catch {} } } catch {}
    if (v !== 'ok') throw new Error(`${o.cmd}: ${JSON.stringify(v)}`);
    return v;
  };
  const q = async (o) => {
    const r = await window.wasmBindings.editor_query_json(JSON.stringify(o));
    const p = JSON.parse(r);
    return typeof p === 'string' ? JSON.parse(p) : p;
  };
  const ID = (n) => `00000000-0000-4000-8000-000000000${n.toString(16).padStart(3, '0')}`;

  await d({ cmd: 'new_project' });
  await d({ cmd: 'add_builtin_material', id: ID(2), shading: 'pbr' });

  // Floor — mildly glossy so it picks up the city glow rather than reading flat.
  await d({ cmd: 'insert', id: ID(1), spec: { primitive: { plane: { width: 20, depth: 20, segments_x: 1, segments_z: 1 } } }, parent: null });
  await d({ cmd: 'add_material_variant', node: ID(1), material: ID(2), id: ID(0x40), name: 'floor' });
  await d({ cmd: 'select_material_variant', node: ID(1), variant: ID(0x40) });
  await d({ cmd: 'set_builtin_param', node: ID(1), param: 'base_color', value: [0.16, 0.17, 0.20, 1] });
  await d({ cmd: 'set_builtin_param', node: ID(1), param: 'metallic', value: [0.0] });
  await d({ cmd: 'set_builtin_param', node: ID(1), param: 'roughness', value: [0.25] });

  // Three metals across the roughness ladder + two dielectrics. The dielectrics
  // separate the specular and irradiance slots: glossy-blue keeps a sharp
  // sheen over base colour, matte-red is almost pure irradiance and should pick
  // up a warm bounce from the orange city lights (NOT flat grey).
  const spheres = [
    ['chrome',      [0.95, 0.95, 0.95, 1], 1.0, 0.02, -4.5],
    ['brushed',     [0.90, 0.85, 0.75, 1], 1.0, 0.22, -2.25],
    ['satin-metal', [0.85, 0.80, 0.85, 1], 1.0, 0.45,  0.0],
    ['glossy-blue', [0.06, 0.10, 0.45, 1], 0.0, 0.10,  2.25],
    ['matte-red',   [0.45, 0.10, 0.08, 1], 0.0, 0.80,  4.5],
  ];
  for (let i = 0; i < spheres.length; i++) {
    const [name, base, metal, rough, x] = spheres[i];
    const n = ID(0x10 + i), v = ID(0x20 + i);
    await d({ cmd: 'insert', id: n, spec: { primitive: { sphere: { radius: 1.0, segments_long: 64, segments_lat: 48 } } }, parent: null });
    await d({ cmd: 'set_transform', id: n, transform: { translation: [x, 1.0, 0], rotation: [0, 0, 0, 1], scale: [1, 1, 1] } });
    await d({ cmd: 'rename', id: n, name });
    await d({ cmd: 'add_material_variant', node: n, material: ID(2), id: v, name });
    await d({ cmd: 'select_material_variant', node: n, variant: v });
    await d({ cmd: 'set_builtin_param', node: n, param: 'base_color', value: base });
    await d({ cmd: 'set_builtin_param', node: n, param: 'metallic', value: [metal] });
    await d({ cmd: 'set_builtin_param', node: n, param: 'roughness', value: [rough] });
  }

  // All three slots on the BC6H set. Served from the test-assets repo
  // (port 9083 in dev). Total 10.5 MB vs 42 MB for the B10G11R11 equivalent.
  const BASE = 'http://localhost:9083/cyber_bc6h';
  await d({ cmd: 'import_ktx_env_from_url', id: ID(0x701), url: `${BASE}/skybox.ktx2` });
  await d({ cmd: 'import_ktx_env_from_url', id: ID(0x702), url: `${BASE}/env.ktx2` });
  await d({ cmd: 'import_ktx_env_from_url', id: ID(0x703), url: `${BASE}/irradiance.ktx2` });
  await new Promise(r => setTimeout(r, 5000));
  await d({ cmd: 'patch_environment',
    skybox:     { ktx: { asset_id: ID(0x701) } },
    specular:   { ktx: { asset_id: ID(0x702) } },
    irradiance: { ktx: { asset_id: ID(0x703) } } });

  // Yaw 2.35 puts the dense skyline behind the spheres, so silhouettes sit on
  // high-frequency neon — the worst case for edge handling, and the reason this
  // framing was chosen over a prettier one.
  await d({ cmd: 'set_camera_orbit', yaw: 2.35, pitch: 0.13, radius: 13.5, look_at: [0, 1.1, 0] });
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false });
  await new Promise(r => setTimeout(r, 2000));
  await q({ query: 'wait_render_settled' });
  return 'env-bc6h-spheres authored';
}
