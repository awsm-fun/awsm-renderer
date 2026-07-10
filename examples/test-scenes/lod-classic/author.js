// test-scene: lod-classic
// Discrete LOD chains: three high-poly spheres (96x64 segments, ~12k tris
// each) — two with LOD enabled (the default), one explicitly OPTED OUT via
// set_mesh_lod {node, enabled:false} (the per-mesh opt-out lock).
// LOD levels are generated AT EXPORT BAKE (the bake-at-export design), and
// switching happens in the PLAYER at runtime — the editor always renders
// full resolution. What this scene locks:
//   - the per-mesh `lod.enabled` flag round-trips project -> bundle
//     (bundle scene.toml carries [nodes.kind.mesh.lod] enabled=true/false);
//   - the baked bundle is the fixture for plan 007's player test, which
//     loads it at near/far radii and asserts the rendered triangle count
//     DROPS at distance for the lod-on spheres and does NOT for the
//     opted-out one.
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
  const ID = (n) => `00000000-0000-4000-8000-0000000000${n.toString(16).padStart(2, '0')}`;
  await d({ cmd: 'new_project' });
  await d({ cmd: 'add_builtin_material', id: ID(2), shading: 'pbr' });
  await d({ cmd: 'insert', id: ID(1), spec: { primitive: { plane: { width: 14, depth: 14, segments_x: 1, segments_z: 1 } } }, parent: null });
  await d({ cmd: 'add_material_variant', node: ID(1), material: ID(2), id: ID(0x40), name: 'floor' });
  await d({ cmd: 'select_material_variant', node: ID(1), variant: ID(0x40) });
  await d({ cmd: 'set_builtin_param', node: ID(1), param: 'base_color', value: [0.42, 0.44, 0.47, 1] });
  const cfgs = [['lod-on-a', -2.2, [0.75, 0.35, 0.25, 1]], ['lod-on-b', 0, [0.3, 0.55, 0.75, 1]], ['lod-opt-out', 2.2, [0.4, 0.7, 0.4, 1]]];
  for (let i = 0; i < 3; i++) {
    const [name, x, base] = cfgs[i];
    const n = ID(0x10 + i), v = ID(0x20 + i);
    await d({ cmd: 'insert', id: n, spec: { primitive: { sphere: { radius: 0.9, segments_long: 96, segments_lat: 64 } } }, parent: null });
    await d({ cmd: 'set_transform', id: n, transform: { translation: [x, 1.0, 0], rotation: [0, 0, 0, 1], scale: [1, 1, 1] } });
    await d({ cmd: 'rename', id: n, name });
    await d({ cmd: 'add_material_variant', node: n, material: ID(2), id: v, name });
    await d({ cmd: 'select_material_variant', node: n, variant: v });
    await d({ cmd: 'set_builtin_param', node: n, param: 'base_color', value: base });
  }
  // per-mesh opt-out: MCP tool set_mesh_lod {node, enabled:false} on the third
  // (a SetKind under the hood — run via the MCP client, or patch the kind here)
  await d({ cmd: 'set_camera_orbit', yaw: 0.4, pitch: 0.35, radius: 10, look_at: [0, 0.9, 0] });
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false });
  await q({ query: 'wait_render_settled' });
  return 'lod-classic authored (opt-out via MCP set_mesh_lod after)';
}
