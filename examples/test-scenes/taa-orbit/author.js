// test-scene: taa-orbit (docs/plans/taa-upscaling/05-verification.md §1)
// A STATIC textured scene; only the camera moves, driven by the TAA harness
// (examples/taa-tests, scenario `taa-orbit`) from the authored `camera`:
// still 0.5 s, orbit 30°/s about `pivot` for 2 s, strafe right at 1 px/frame
// (at the pivot's depth) for 1.5 s, then stop for 1 s. Catches camera
// vectors, jitter removal, and history blur after stopping (G2, G4); the
// strafe is the 05 §4.3 slow-pan blur test.
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
  const ID = (n) => `00000000-0000-4000-8000-${n.toString(16).padStart(12, "0")}`;
  // Orbit pose → camera node transform (camera looks down local −Z).
  const camQuat = (yaw, pitch) => {
    const qy = [0, Math.sin(yaw / 2), 0, Math.cos(yaw / 2)];
    const qx = [Math.sin(-pitch / 2), 0, 0, Math.cos(-pitch / 2)];
    return [
      qy[3] * qx[0] + qy[0] * qx[3] + qy[1] * qx[2] - qy[2] * qx[1],
      qy[3] * qx[1] - qy[0] * qx[2] + qy[1] * qx[3] + qy[2] * qx[0],
      qy[3] * qx[2] + qy[0] * qx[1] - qy[1] * qx[0] + qy[2] * qx[3],
      qy[3] * qx[3] - qy[0] * qx[0] - qy[1] * qx[1] - qy[2] * qx[2],
    ];
  };
  const camPos = (yaw, pitch, radius, look) => [
    look[0] + radius * Math.cos(pitch) * Math.sin(yaw),
    look[1] + radius * Math.sin(pitch),
    look[2] + radius * Math.cos(pitch) * Math.cos(yaw),
  ];


  const addCamera = async (n, name, yaw, pitch, radius, look) => {
    const cam = ID(n);
    await d({ cmd: 'insert', id: cam, spec: 'camera', parent: null });
    await d({ cmd: 'rename', id: cam, name });
    await d({ cmd: 'set_transform', id: cam, transform: { translation: camPos(yaw, pitch, radius, look), rotation: camQuat(yaw, pitch), scale: [1, 1, 1] } });
    await d({ cmd: 'patch_kind', id: cam, patch: { camera: { projection: { perspective: { fov_y_rad: 0.7853981633974483 }, orthographic: null }, near: 0.1, far: 100.0 } } });
  };
  const mk = async (n, spec, name, mat, pos, rot) => {
    const node = ID(n), v = ID(n + 0x1000);
    await d({ cmd: 'insert', id: node, spec, parent: null });
    await d({ cmd: 'rename', id: node, name });
    await d({ cmd: 'set_transform', id: node, transform: { translation: pos, rotation: rot || [0, 0, 0, 1], scale: [1, 1, 1] } });
    await d({ cmd: 'add_material_variant', node, material: mat, id: v, name: 'main' });
    await d({ cmd: 'select_material_variant', node, variant: v });
    return node;
  };
  const paint = async (node, color, rough, tex, scale) => {
    await d({ cmd: 'set_builtin_param', node, param: 'base_color', value: color });
    await d({ cmd: 'set_builtin_param', node, param: 'roughness', value: [rough] });
    if (tex) {
      await d({ cmd: 'set_builtin_texture', node, slot: 'base_color', texture: tex });
      if (scale) await d({ cmd: 'set_node_texture_transform', node, slot: 'base_color', scale, wrap_u: 'repeat', wrap_v: 'repeat' });
    }
  };
  const finish = async (name, look, yaw, pitch, radius) => {
    await d({ cmd: 'set_post_process', bloom: false });
    await d({ cmd: 'set_camera_orbit', yaw, pitch, radius, look_at: look });
    await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false });
    await d({ cmd: 'set_selection', ids: [] });
    await q({ query: 'wait_render_settled' });
    return `${name} authored`;
  };

  await d({ cmd: 'new_project' });
  const mat = ID(0x60), tex = ID(0xf0);
  await d({ cmd: 'add_builtin_material', id: mat, shading: 'pbr' });
  await d({ cmd: 'add_texture_asset', id: tex, proc: 'checker' });
  const floor = await mk(0x01, { primitive: { plane: { width: 24, depth: 24, segments_x: 1, segments_z: 1 } } }, 'floor', mat, [0, 0, 0]);
  await paint(floor, [0.9, 0.9, 0.9, 1], 0.85, tex, [6, 6]);
  // A ring of textured shapes around the pivot (distinct colours, mixed
  // silhouettes and texture frequencies) so every direction has edges.
  const shapes = [
    [{ box: { dims: [1, 1, 1] } }, 0.5, [0.85, 0.2, 0.15, 1], [1, 1]],
    [{ sphere: { radius: 0.6, segments_long: 48, segments_lat: 32 } }, 0.6, [0.2, 0.6, 0.9, 1], [2, 2]],
    [{ box: { dims: [0.5, 2.4, 0.5] } }, 1.2, [0.95, 0.8, 0.2, 1], [1, 4]],
    [{ box: { dims: [1.6, 0.6, 0.8] } }, 0.3, [0.3, 0.8, 0.35, 1], [3, 1]],
    [{ sphere: { radius: 0.4, segments_long: 48, segments_lat: 32 } }, 0.4, [0.8, 0.3, 0.8, 1], [4, 4]],
    [{ box: { dims: [0.3, 1.6, 1.6] } }, 0.8, [0.9, 0.5, 0.1, 1], [1, 2]],
  ];
  for (let i = 0; i < shapes.length; i++) {
    const [spec, y, color, scale] = shapes[i];
    const a = (i / shapes.length) * Math.PI * 2;
    const rot = [0, Math.sin(a / 2), 0, Math.cos(a / 2)];
    const n = await mk(0x02 + i, { primitive: spec }, `shape-${i}`, mat, [2.6 * Math.sin(a), y, 2.6 * Math.cos(a)], rot);
    await paint(n, color, 0.5, tex, scale);
  }
  // The orbit centre the harness reads by name.
  await d({ cmd: 'insert', id: ID(0x0f), spec: 'empty', parent: null });
  await d({ cmd: 'rename', id: ID(0x0f), name: 'pivot' });
  await d({ cmd: 'set_transform', id: ID(0x0f), transform: { translation: [0, 0.5, 0], rotation: [0, 0, 0, 1], scale: [1, 1, 1] } });
  await addCamera(0x10, 'camera', 0.3, 0.4, 9, [0, 0.5, 0]);
  return await finish('taa-orbit', [0, 0.5, 0], 0.3, 0.4, 9);
}
