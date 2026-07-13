// test-scene: ssr
// Screen-space reflections on the standing probe: a BLACK glossy dielectric
// floor (base 0.02, roughness 0.05, metallic 0 — white saturates, black
// shows the reflection signal) under three emissive RGB columns at staggered
// depths, plus a rough gold-metal sphere for spread blur. Correct = each
// column reflects CONTINUOUSLY into the floor (no horizontal banding — the
// LinearDda lock from plan 004), the rough sphere's reflection is blurred
// (spread), and toggling ssr_enabled visibly adds/removes the reflections
// (the graduated 004 verification scene).
//
// NOTE the dispatch shape: set_post_process takes FLAT ssr_* fields
// (ssr_enabled, ssr_intensity, ...) — a nested `ssr: {...}` object is
// silently ignored (the post_process QUERY returns the nested form; the
// command does not accept it).
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
  const matFloor = ID(2), matCol = ID(3), matRough = ID(4);
  for (const m of [matFloor, matCol, matRough]) await d({ cmd: 'add_builtin_material', id: m, shading: 'pbr' });
  const mk = async (idStr, spec, pos, mat, name, vId) => {
    await d({ cmd: 'insert', id: idStr, spec, parent: null });
    await d({ cmd: 'set_transform', id: idStr, transform: { translation: pos, rotation: [0, 0, 0, 1], scale: [1, 1, 1] } });
    await d({ cmd: 'rename', id: idStr, name });
    await d({ cmd: 'add_material_variant', node: idStr, material: mat, id: vId, name });
    await d({ cmd: 'select_material_variant', node: idStr, variant: vId });
  };
  await mk(ID(1), { primitive: { plane: { width: 20, depth: 20, segments_x: 1, segments_z: 1 } } }, [0, 0, 0], matFloor, 'glossy-floor', ID(0x40));
  await d({ cmd: 'set_builtin_param', node: ID(1), param: 'base_color', value: [0.02, 0.02, 0.02, 1] });
  await d({ cmd: 'set_builtin_param', node: ID(1), param: 'roughness', value: [0.05] });
  await d({ cmd: 'set_builtin_param', node: ID(1), param: 'metallic', value: [0.0] });
  const cols = [[6, 0.3, 0.3], [0.3, 6, 0.3], [0.3, 0.4, 6]];
  for (let i = 0; i < 3; i++) {
    const n = ID(10 + i), v = ID(0x20 + i);
    await mk(n, { primitive: { box: { dims: [0.7, 3.2, 0.7] } } }, [(i - 1) * 2.4, 1.6, -2 - i * 1.5], matCol, `column-${['r', 'g', 'b'][i]}`, v);
    await d({ cmd: 'set_builtin_param', node: n, param: 'base_color', value: [0.02, 0.02, 0.02, 1] });
    await d({ cmd: 'set_builtin_param', node: n, param: 'emissive', value: cols[i] });
  }
  await mk(ID(13), { primitive: { sphere: { radius: 0.8, segments_long: 32, segments_lat: 24 } } }, [2.8, 0.9, 0.5], matRough, 'rough-metal', ID(0x23));
  await d({ cmd: 'set_builtin_param', node: ID(13), param: 'base_color', value: [0.9, 0.75, 0.4, 1] });
  await d({ cmd: 'set_builtin_param', node: ID(13), param: 'metallic', value: [1.0] });
  await d({ cmd: 'set_builtin_param', node: ID(13), param: 'roughness', value: [0.35] });
  await d({ cmd: 'set_post_process', ssr_enabled: true, ssr_intensity: 1.0, ssr_max_distance: 100.0, ssr_thickness: 1.0, ssr_max_steps: 96, ssr_spread_cutoff: 0.6, ssr_edge_fade: 0.04, ssr_resolution_scale: 0.5, ssr_temporal: false });
  await d({ cmd: 'set_camera_orbit', yaw: 0.15, pitch: 0.35, radius: 14, look_at: [0.5, 0.9, -2] });
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false });
  await q({ query: 'wait_render_settled' });
  return 'ssr authored';
}
