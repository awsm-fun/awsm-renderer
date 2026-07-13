// test-scene: bloom-post
// Bloom + tonemapping: three emissive spheres at increasing strength
// (2 / 5 / 10, red / green / blue) in a depth line over a dark floor, plus
// a non-emissive gray reference sphere. Post: aces tonemapper, bloom on
// (threshold 1.0, intensity 1.2, scatter 1.0). Correct = halo size/strength
// scales with emissive power (red subtle -> blue blown), the gray reference
// sphere has NO halo, and switching tonemapper (aces vs khronos_neutral_pbr)
// visibly re-grades the frame.
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
  const matFloor = ID(2), matEm = ID(3), matRef = ID(4);
  for (const m of [matFloor, matEm, matRef]) await d({ cmd: 'add_builtin_material', id: m, shading: 'pbr' });
  const mk = async (idStr, spec, pos, mat, name, vId) => {
    await d({ cmd: 'insert', id: idStr, spec, parent: null });
    await d({ cmd: 'set_transform', id: idStr, transform: { translation: pos, rotation: [0, 0, 0, 1], scale: [1, 1, 1] } });
    await d({ cmd: 'rename', id: idStr, name });
    await d({ cmd: 'add_material_variant', node: idStr, material: mat, id: vId, name });
    await d({ cmd: 'select_material_variant', node: idStr, variant: vId });
  };
  await mk(ID(1), { primitive: { plane: { width: 16, depth: 16, segments_x: 1, segments_z: 1 } } }, [0, 0, 0], matFloor, 'floor', ID(0x40));
  await d({ cmd: 'set_builtin_param', node: ID(1), param: 'base_color', value: [0.08, 0.08, 0.1, 1] });
  await d({ cmd: 'set_builtin_param', node: ID(1), param: 'roughness', value: [0.8] });
  const strengths = [2, 5, 10];
  const colors = [[4, 0.4, 0.4], [0.4, 4, 0.8], [0.5, 0.7, 5]];
  for (let i = 0; i < 3; i++) {
    const n = ID(10 + i), v = ID(0x20 + i);
    await mk(n, { primitive: { sphere: { radius: 0.7, segments_long: 32, segments_lat: 24 } } }, [(i - 1) * 1.2, 0.9, (i - 1) * 2.6], matEm, `emissive-${strengths[i]}`, v);
    await d({ cmd: 'set_builtin_param', node: n, param: 'base_color', value: [0.02, 0.02, 0.02, 1] });
    await d({ cmd: 'set_builtin_param', node: n, param: 'emissive', value: colors[i].map(x => x * strengths[i] / 4) });
  }
  await mk(ID(13), { primitive: { sphere: { radius: 0.7, segments_long: 32, segments_lat: 24 } } }, [2.6, 0.9, 2.6], matRef, 'gray-reference', ID(0x23));
  await d({ cmd: 'set_builtin_param', node: ID(13), param: 'base_color', value: [0.6, 0.6, 0.6, 1] });
  await d({ cmd: 'set_post_process', tonemapping: 'aces', bloom: true, bloom_threshold: 1.0, bloom_intensity: 1.2, bloom_scatter: 1.0, exposure: 0.0 });
  await d({ cmd: 'set_camera_orbit', yaw: 1.1, pitch: 0.5, radius: 12, look_at: [0, 0.8, 0] });
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false });
  await q({ query: 'wait_render_settled' });
  return 'bloom-post authored';
}
