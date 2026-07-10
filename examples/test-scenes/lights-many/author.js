// test-scene: lights-many
// Froxel (clustered) light culling under many punctual lights: a 24x24
// floor, 3x3 pillar grid, and 36 colored point lights in a 6x6 grid
// (spacing 3, y=1.4, intensity 350, range 2.6 — deliberately tighter than
// the spacing so each light reads as a DISTINCT colored pool). Correct =
// per-light colored pools in row-order (red/green/blue/yellow/magenta/cyan
// cycling), pillars lit on their light-facing sides, interactive frame rate.
// The seeded key directional light is DELETED so punctual lights are the
// only dynamic illumination (IBL ambient remains).
//
// THIS SCENE'S ORIGIN: authoring it found the froxel reverse-Z regression
// (tile unproject anchored at NDC z=0 -> NaN side planes -> ALL punctual
// lights culled). It is the permanent lock against that class of bug.
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
  const snap = await q({ query: 'snapshot' });
  const seeded = (snap.scene_tree || []).find(n => n.kind === 'light');
  if (seeded) await d({ cmd: 'delete', id: seeded.id });
  const matFloor = ID(2), matPillar = ID(3);
  await d({ cmd: 'add_builtin_material', id: matFloor, shading: 'pbr' });
  await d({ cmd: 'add_builtin_material', id: matPillar, shading: 'pbr' });
  const mk = async (idStr, spec, pos, mat, name, base, rough, vId) => {
    await d({ cmd: 'insert', id: idStr, spec, parent: null });
    await d({ cmd: 'set_transform', id: idStr, transform: { translation: pos, rotation: [0, 0, 0, 1], scale: [1, 1, 1] } });
    await d({ cmd: 'rename', id: idStr, name });
    await d({ cmd: 'add_material_variant', node: idStr, material: mat, id: vId, name });
    await d({ cmd: 'select_material_variant', node: idStr, variant: vId });
    await d({ cmd: 'set_builtin_param', node: idStr, param: 'base_color', value: base });
    await d({ cmd: 'set_builtin_param', node: idStr, param: 'roughness', value: [rough] });
  };
  await mk(ID(1), { primitive: { plane: { width: 24, depth: 24, segments_x: 1, segments_z: 1 } } }, [0, 0, 0], matFloor, 'floor', [0.25, 0.25, 0.28, 1], 0.5, ID(0x40));
  for (let x = -1; x <= 1; x++) for (let z = -1; z <= 1; z++) {
    const i = (x + 1) * 3 + (z + 1);
    await mk(`10000000-0000-4000-8000-0000000000${i.toString(16).padStart(2, '0')}`,
      { primitive: { box: { dims: [0.7, 2.4, 0.7] } } }, [x * 4, 1.2, z * 4], matPillar, `pillar-${i}`,
      [0.7, 0.7, 0.72, 1], 0.6, `20000000-0000-4000-8000-0000000000${i.toString(16).padStart(2, '0')}`);
  }
  const cols = [[1, 0.2, 0.2], [0.2, 1, 0.2], [0.2, 0.4, 1], [1, 0.9, 0.2], [1, 0.3, 1], [0.2, 1, 1]];
  for (let i = 0; i < 6; i++) for (let j = 0; j < 6; j++) {
    const n = `30000000-0000-4000-8000-0000000000${(i * 6 + j).toString(16).padStart(2, '0')}`;
    await d({ cmd: 'insert', id: n, spec: { light: 'point' }, parent: null });
    await d({ cmd: 'set_transform', id: n, transform: { translation: [-7.5 + i * 3, 1.4, -7.5 + j * 3], rotation: [0, 0, 0, 1], scale: [1, 1, 1] } });
    await d({ cmd: 'set_light_param', node: n, param: 'color', value: cols[(i * 6 + j) % 6] });
    await d({ cmd: 'set_light_param', node: n, param: 'intensity', value: [350] });
    await d({ cmd: 'set_light_param', node: n, param: 'range', value: [2.6] });
  }
  await d({ cmd: 'set_camera_orbit', yaw: 0.6, pitch: 0.7, radius: 20, look_at: [0, 0, 0] });
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false });
  await q({ query: 'wait_render_settled' });
  return 'lights-many authored';
}
