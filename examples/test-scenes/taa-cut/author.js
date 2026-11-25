// test-scene: taa-cut (docs/plans/taa-final-round/verification.md §1)
// Three authored cameras (`cam-a`, `cam-b`, `cam-c`) on one textured scene;
// the TAA harness (scenario `taa-cut`) CUTS between them every 2 s (calling
// `reset_temporal_history` on the cut frame) and teleports the node
// `teleporter` (a 0.8 m magenta box) between three spots every second.
// Catches history surviving a cut or a teleport (G9, G10).
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
  const mat = ID(0x900), tex = ID(0x9f0);
  await d({ cmd: 'add_builtin_material', id: mat, shading: 'pbr' });
  await d({ cmd: 'add_texture_asset', id: tex, proc: 'checker' });
  const floor = await mk(0x01, { primitive: { plane: { width: 20, depth: 20, segments_x: 1, segments_z: 1 } } }, 'floor', mat, [0, 0, 0]);
  await paint(floor, [0.9, 0.9, 0.9, 1], 0.85, tex, [5, 5]);
  const shapes = [[{ box: { dims: [1.2, 1.2, 1.2] } }, [-2.5, 0.6, -2], [0.85, 0.2, 0.15, 1]], [{ sphere: { radius: 0.7, segments_long: 48, segments_lat: 32 } }, [2.5, 0.7, -1.5], [0.2, 0.6, 0.9, 1]], [{ box: { dims: [0.6, 2.4, 0.6] } }, [0.5, 1.2, -3], [0.95, 0.8, 0.2, 1]], [{ box: { dims: [2.0, 0.5, 1.0] } }, [-0.5, 0.25, 2.5], [0.3, 0.8, 0.35, 1]]];
  for (let i = 0; i < shapes.length; i++) {
    const [spec, pos, color] = shapes[i];
    const n = await mk(0x02 + i, { primitive: spec }, `shape-${i}`, mat, pos);
    await paint(n, color, 0.5, tex, [2, 2]);
  }
  // The teleporter sits at the origin; the harness offsets it per second.
  const tp = await mk(0x10, { primitive: { box: { dims: [0.8, 0.8, 0.8] } } }, 'teleporter', mat, [0, 0.4, 0]);
  await paint(tp, [0.9, 0.1, 0.8, 1], 0.4);
  await addCamera(0x500, 'cam-a', 0.4, 0.35, 9, [0, 0.6, 0]);
  await addCamera(0x501, 'cam-b', 2.4, 0.5, 8, [0, 0.6, 0]);
  await addCamera(0x502, 'cam-c', -1.9, 0.25, 10, [0, 0.6, 0]);
  return await finish('taa-cut', [0, 0.6, 0], 0.4, 0.35, 9);
}
