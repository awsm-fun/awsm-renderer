// test-scene: instancing-stress
// The axis-5 explicit instancer NodeKind: ONE instancer node owning 3000
// box instances (city-height grid, per-instance colors) over a floor —
// 3000 instances never become 3000 scene nodes. Correct = the full grid
// renders at interactive rate with ONE geometry upload.
// MEASURED (2026-07-10, authoring): 4 scene nodes / 2 meshes /
// 3 mesh_resources (the box geometry SHARED by all 3000) / frame 16.6 ms
// (vsync) / render_cpu 1.7 ms.
// Recipe: insert source box -> read its mesh ASSET id via
// node_kind_details -> hide source -> insert {spec:"instancer"} ->
// patch_kind {instancer:{mesh:<asset>}} -> ONE bulk
// set_instancer_transforms {node, transforms:[...3000], per_instance_colors}.
async () => {
  for (let i = 0; i < 40 && !window.wasmBindings?.editor_dispatch_json; i++) await new Promise(r => setTimeout(r, 500));
  const d = async (o) => { const r = await window.wasmBindings.editor_dispatch_json(JSON.stringify(o)); let v = r; try { v = JSON.parse(r); if (typeof v === 'string' && v !== 'ok') { try { v = JSON.parse(v); } catch {} } } catch {}; if (v !== 'ok') throw new Error(`${o.cmd}: ${JSON.stringify(v)}`); return v; };
  const q = async (o) => { const r = await window.wasmBindings.editor_query_json(JSON.stringify(o)); const p = JSON.parse(r); return typeof p === 'string' ? JSON.parse(p) : p; };
  const ID = (n) => `00000000-0000-4000-8000-0000000000${n.toString(16).padStart(2, '0')}`;
  await d({ cmd: 'new_project' });
  await d({ cmd: 'add_builtin_material', id: ID(2), shading: 'pbr' });
  await d({ cmd: 'insert', id: ID(1), spec: { primitive: { plane: { width: 80, depth: 80, segments_x: 1, segments_z: 1 } } }, parent: null });
  await d({ cmd: 'add_material_variant', node: ID(1), material: ID(2), id: ID(0x40), name: 'floor' });
  await d({ cmd: 'select_material_variant', node: ID(1), variant: ID(0x40) });
  await d({ cmd: 'set_builtin_param', node: ID(1), param: 'base_color', value: [0.3, 0.32, 0.35, 1] });
  // source box (mesh asset id derives from node id per insert impl)
  await d({ cmd: 'insert', id: ID(0x10), spec: { primitive: { box: { dims: [0.6, 0.6, 0.6] } } }, parent: null });
  const det = await q({ query: 'node_kind_details', nodes: [ID(0x10)] });
  const meshAsset = det.entries[ID(0x10)].mesh.mesh;
  await d({ cmd: 'set_visible', id: ID(0x10), visible: false });
  // instancer with 3000 instances in a 55x55-ish grid with color bands
  await d({ cmd: 'insert', id: ID(0x20), spec: 'instancer', parent: null });
  await d({ cmd: 'patch_kind', id: ID(0x20), patch: { instancer: { mesh: meshAsset } } });
  const transforms = [], colors = [];
  const N = 3000, COLS = 55;
  for (let i = 0; i < N; i++) {
    const cx = (i % COLS) - COLS / 2, cz = Math.floor(i / COLS) - N / COLS / 2;
    const h = 0.3 + ((i * 2654435761) % 100) / 100 * 2.2;
    transforms.push({ translation: [cx * 1.2, h / 2, cz * 1.2], rotation: [0, 0, 0, 1], scale: [1, h / 0.6, 1] });
    colors.push([0.3 + (i % 7) / 10, 0.4 + (i % 5) / 10, 0.5 + (i % 3) / 10, 1]);
  }
  await d({ cmd: 'set_instancer_transforms', node: ID(0x20), transforms, per_instance_colors: colors });
  await d({ cmd: 'rename', id: ID(0x20), name: 'city-3000' });
  await d({ cmd: 'set_selection', ids: [] });
  await d({ cmd: 'set_camera_orbit', yaw: 0.6, pitch: 0.5, radius: 55, look_at: [0, 0, 0] });
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false });
  await new Promise(r => setTimeout(r, 2000));
  await q({ query: 'wait_render_settled' });
  const ms = (await q({ query: 'memory_stats' })).entries;
  return JSON.stringify({meshes: ms.meshes, resources: ms.mesh_resources, geo_mb: Math.round(ms.mesh_geometry_bytes/1024), cpu_ms: ms.render_cpu_ms, dt: ms.frame_dt_ms, tris: ms.visible_triangles});
}
