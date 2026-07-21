// test-scene: player-cameras
// PLAYER visual regression through AUTHORED scene cameras (the bundle-player
// tier): an asymmetric arrangement of primary-colored primitives on a floor,
// with TWO exported Camera nodes — `cam-perspective` (45° fov, elevated 3/4
// view from the front-right) and `cam-ortho` (orthographic half_height 3.2,
// elevated view from the front-LEFT). The arrangement is deliberately
// asymmetric (red box left, green sphere front-center, tall blue box right,
// small yellow box behind) so a wrong camera, wrong projection, or mirrored
// axis is unmistakable in the goldens.
//
// The goldens are NOT editor captures: `task bundle-player` (:9092) loads this
// scene's exported bundle/ through the real player path and renders through
// each camera on a fixed 800x600 canvas — `golden-cam-perspective.png` and
// `golden-cam-ortho.png` are screenshots of THAT page. See verify.md.
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

  // Materials: one per object so each color is a real assignment.
  const mats = { floor: ID(0x60), red: ID(0x61), green: ID(0x62), blue: ID(0x63), yellow: ID(0x64) };
  const colors = { floor: [0.42, 0.44, 0.48, 1], red: [0.75, 0.12, 0.10, 1], green: [0.10, 0.62, 0.22, 1], blue: [0.10, 0.22, 0.72, 1], yellow: [0.85, 0.72, 0.10, 1] };
  for (const k of Object.keys(mats)) {
    await d({ cmd: 'add_builtin_material', id: mats[k], shading: 'pbr' });
  }

  const place = async (node, spec, name, mat, color, pos) => {
    await d({ cmd: 'insert', id: node, spec, parent: null });
    await d({ cmd: 'rename', id: node, name });
    await d({ cmd: 'set_transform', id: node, transform: { translation: pos, rotation: [0, 0, 0, 1], scale: [1, 1, 1] } });
    const variant = node.replace(/..$/, 'f0');
    await d({ cmd: 'add_material_variant', node, material: mat, id: variant, name: 'main' });
    await d({ cmd: 'select_material_variant', node, variant });
    await d({ cmd: 'set_builtin_param', node, param: 'base_color', value: color });
    await d({ cmd: 'set_builtin_param', node, param: 'roughness', value: [0.65] });
  };

  await place(ID(0x01), { primitive: { plane: { width: 20, depth: 20, segments_x: 1, segments_z: 1 } } }, 'floor', mats.floor, colors.floor, [0, 0, 0]);
  await place(ID(0x02), { primitive: { box: { dims: [1.2, 1.2, 1.2] } } }, 'red-box', mats.red, colors.red, [-2, 0.6, 0]);
  await place(ID(0x03), { primitive: { sphere: { radius: 0.8, segments_long: 32, segments_lat: 20 } } }, 'green-sphere', mats.green, colors.green, [0.5, 0.8, 1.5]);
  await place(ID(0x04), { primitive: { box: { dims: [0.8, 2.4, 0.8] } } }, 'blue-pillar', mats.blue, colors.blue, [2.2, 1.2, -1.2]);
  await place(ID(0x05), { primitive: { box: { dims: [0.6, 0.6, 0.6] } } }, 'yellow-box', mats.yellow, colors.yellow, [0, 0.3, -2.2]);

  // ── The two exported cameras. Orbit-pose math (yaw about Y ∘ -pitch about X,
  // camera looks down local -Z). ──
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
  const addCamera = async (node, name, yaw, pitch, radius, look, projection) => {
    await d({ cmd: 'insert', id: node, spec: 'camera', parent: null });
    await d({ cmd: 'rename', id: node, name });
    await d({ cmd: 'set_transform', id: node, transform: { translation: camPos(yaw, pitch, radius, look), rotation: camQuat(yaw, pitch), scale: [1, 1, 1] } });
    await d({ cmd: 'patch_kind', id: node, patch: { camera: { projection, near: 0.1, far: 100.0 } } });
  };
  // 3/4 view from the front-right, 45° fov. (RFC 7386 merge: null the other
  // projection key so the kind stays a single-variant enum.)
  await addCamera(ID(0x10), 'cam-perspective', 0.6, 0.35, 9.0, [0, 0.8, 0],
    { perspective: { fov_y_rad: 0.7853981633974483 }, orthographic: null });
  // Elevated view from the front-LEFT, parallel projection.
  await addCamera(ID(0x11), 'cam-ortho', -0.8, 0.45, 10.0, [0, 0.8, 0],
    { orthographic: { half_height: 3.2 }, perspective: null });

  await d({ cmd: 'set_selection', ids: [] });
  await q({ query: 'wait_render_settled' });
  return 'player-cameras authored';
}
