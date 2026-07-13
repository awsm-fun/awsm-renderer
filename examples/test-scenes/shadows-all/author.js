// test-scene: shadows-all
// All three shadow-casting light types over one receiver floor:
// - seeded DIRECTIONAL (cascades) — tall-box + sphere shadows
// - SPOT straight down over the thin-bar / lowered-box area (cone pool +
//   bar shadow inside it)
// - POINT (cube shadows) low near the sphere — soft radial shadow
// Plus the PR#169 world-referenced depth-bias lock: `lowered-box` sits with
// its bottom slightly under the nearby floor level — correct = a
// contact-tight shadow with NO donut/hole under it and no Peter-Pan gap on
// any caster.
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
  const mk = async (n, spec, pos, name, base, rough) => {
    const node = ID(n), v = ID(n + 0x30);
    await d({ cmd: 'insert', id: node, spec, parent: null });
    await d({ cmd: 'set_transform', id: node, transform: { translation: pos, rotation: [0, 0, 0, 1], scale: [1, 1, 1] } });
    await d({ cmd: 'rename', id: node, name });
    await d({ cmd: 'add_material_variant', node, material: ID(2), id: v, name });
    await d({ cmd: 'select_material_variant', node, variant: v });
    await d({ cmd: 'set_builtin_param', node, param: 'base_color', value: base });
    await d({ cmd: 'set_builtin_param', node, param: 'roughness', value: [rough] });
  };
  await mk(1, { primitive: { plane: { width: 18, depth: 18, segments_x: 1, segments_z: 1 } } }, [0, 0, 0], 'receiver-floor', [0.55, 0.55, 0.58, 1], 0.85);
  await mk(0x10, { primitive: { box: { dims: [1.2, 2.2, 1.2] } } }, [-2.5, 1.1, -1.5], 'tall-box', [0.7, 0.5, 0.35, 1], 0.6);
  await mk(0x11, { primitive: { sphere: { radius: 0.9, segments_long: 32, segments_lat: 24 } } }, [0.5, 0.9, -2.5], 'sphere', [0.4, 0.55, 0.7, 1], 0.4);
  await mk(0x12, { primitive: { box: { dims: [0.25, 3.0, 0.25] } } }, [2.5, 1.5, 0], 'thin-bar', [0.65, 0.3, 0.3, 1], 0.5);
  await mk(0x13, { primitive: { box: { dims: [0.9, 0.35, 0.9] } } }, [0.2, 0.13, 1.6], 'lowered-box', [0.35, 0.6, 0.4, 1], 0.6);
  await d({ cmd: 'insert', id: ID(0x50), spec: { light: 'spot' }, parent: null });
  const hx = Math.sin(-Math.PI / 4), cx = Math.cos(-Math.PI / 4);
  await d({ cmd: 'set_transform', id: ID(0x50), transform: { translation: [1.8, 6, 0.8], rotation: [hx, 0, 0, cx], scale: [1, 1, 1] } });
  await d({ cmd: 'set_light_param', node: ID(0x50), param: 'color', value: [1, 0.9, 0.7] });
  await d({ cmd: 'set_light_param', node: ID(0x50), param: 'intensity', value: [1800] });
  await d({ cmd: 'set_light_param', node: ID(0x50), param: 'range', value: [25] });
  await d({ cmd: 'set_light_param', node: ID(0x50), param: 'inner_angle', value: [0.35] });
  await d({ cmd: 'set_light_param', node: ID(0x50), param: 'outer_angle', value: [0.5] });
  await d({ cmd: 'insert', id: ID(0x51), spec: { light: 'point' }, parent: null });
  await d({ cmd: 'set_transform', id: ID(0x51), transform: { translation: [-1.2, 1.6, -3.8], rotation: [0, 0, 0, 1], scale: [1, 1, 1] } });
  await d({ cmd: 'set_light_param', node: ID(0x51), param: 'color', value: [0.4, 0.6, 1] });
  await d({ cmd: 'set_light_param', node: ID(0x51), param: 'intensity', value: [1500] });
  await d({ cmd: 'set_light_param', node: ID(0x51), param: 'range', value: [12] });
  await d({ cmd: 'set_camera_orbit', yaw: 0.5, pitch: 0.6, radius: 13, look_at: [0, 0.3, 0] });
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false });
  await q({ query: 'wait_render_settled' });
  return 'shadows-all authored';
}
