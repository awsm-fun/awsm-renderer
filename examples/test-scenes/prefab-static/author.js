// test-scene: prefab-static
// Prefab duplication of a static textured mesh: one checker-textured source
// box marked prefab (PF badge) duplicated 4x and spread over a floor.
// Correct = five identical textured boxes, one shared mesh asset in the
// project (all five nodes reference the source's MeshDef), and — the axis-4
// acceptance — geometry uploaded ONCE (clone shares buffers; per-instance
// divergence is transforms only).
//
// AXIS-4 BASELINE (2026-07-10, pre-optimization): memory_stats meshes went
// 1 -> 6 across the 4 duplicates + source (each duplicate creates its own
// renderer mesh ENTRY — expected; the axis-4 question is whether the entries
// share geometry BUFFERS). pool_textures 4 -> 5 (one checker upload total —
// texture sharing OK). Re-measure after the axis-4 fix.
//
// duplicate {id} mints its own new node id (read the snapshot to find
// clones); set_prefab {id, prefab: bool}.
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
  const matFloor = ID(2), matBox = ID(3), tex = ID(0xf1);
  await d({ cmd: 'add_builtin_material', id: matFloor, shading: 'pbr' });
  await d({ cmd: 'add_builtin_material', id: matBox, shading: 'pbr' });
  await d({ cmd: 'add_texture_asset', id: tex, proc: 'checker' });
  await d({ cmd: 'insert', id: ID(1), spec: { primitive: { plane: { width: 14, depth: 14, segments_x: 1, segments_z: 1 } } }, parent: null });
  await d({ cmd: 'add_material_variant', node: ID(1), material: matFloor, id: ID(0x40), name: 'floor' });
  await d({ cmd: 'select_material_variant', node: ID(1), variant: ID(0x40) });
  await d({ cmd: 'set_builtin_param', node: ID(1), param: 'base_color', value: [0.4, 0.42, 0.45, 1] });
  await d({ cmd: 'insert', id: ID(0x10), spec: { primitive: { box: { dims: [1.4, 1.4, 1.4] } } }, parent: null });
  await d({ cmd: 'rename', id: ID(0x10), name: 'source-box' });
  await d({ cmd: 'add_material_variant', node: ID(0x10), material: matBox, id: ID(0x41), name: 'box-mat' });
  await d({ cmd: 'select_material_variant', node: ID(0x10), variant: ID(0x41) });
  await d({ cmd: 'set_builtin_texture', node: ID(0x10), slot: 'base_color', texture: tex });
  await d({ cmd: 'set_transform', id: ID(0x10), transform: { translation: [-3, 0.7, -3], rotation: [0, 0, 0, 1], scale: [1, 1, 1] } });
  await d({ cmd: 'set_prefab', id: ID(0x10), prefab: true });
  for (let i = 0; i < 4; i++) await d({ cmd: 'duplicate', id: ID(0x10) });
  await q({ query: 'wait_render_settled' });
  const snap = await q({ query: 'snapshot' });
  const clones = [];
  const walk = (ns) => { for (const n of ns) { if (n.name === 'source-box' && n.id !== ID(0x10)) clones.push(n.id); if (n.children) walk(n.children); } };
  walk(snap.scene_tree || []);
  const pos = [[0, 0.7, -3], [3, 0.7, -3], [-1.5, 0.7, 0.5], [1.5, 0.7, 0.5]];
  clones.slice(0, 4).forEach(async (b, i) => { });
  for (let i = 0; i < Math.min(4, clones.length); i++) {
    await d({ cmd: 'set_transform', id: clones[i], transform: { translation: pos[i], rotation: [0, 0, 0, 1], scale: [1, 1, 1] } });
  }
  await d({ cmd: 'set_selection', ids: [] });
  await d({ cmd: 'set_camera_orbit', yaw: 0.45, pitch: 0.55, radius: 15, look_at: [0, 0.4, -1.5] });
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false });
  await q({ query: 'wait_render_settled' });
  return 'prefab-static authored';
}
