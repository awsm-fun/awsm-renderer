// test-scene: taa-motion (docs/plans/taa-final-round/verification.md §1)
// A checker-textured orange cube over a high-contrast checker floor, seen by
// a STILL authored camera. The motion is NOT authored here: the TAA harness
// (examples/taa-tests, scenario `taa-motion`) drives the node named `cube`
// with a fixed timestep — still 0.5 s, then +x at 1.8 m/s with 90°/s yaw
// for 2 s (~3.3 px/frame at 960x720), stopped 1 s, then −x at 4.8 m/s for
// 1 s (~8.7 px/frame). Catches object vectors, dilation, trails (G1, G3).
// The cube is a 1 m box: the harness projects its 8 corners for the exact
// coverage mask of the trail band, so keep it a box of that size.
//
// Run inside the editor page attached to the MCP, then save_project /
// export_player_bundle into examples/test-scenes/taa-motion/{project,bundle}.
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

  await d({ cmd: 'new_project' });
  const matFloor = ID(0x60), matCube = ID(0x61), tex = ID(0xf0);
  await d({ cmd: 'add_builtin_material', id: matFloor, shading: 'pbr' });
  await d({ cmd: 'add_builtin_material', id: matCube, shading: 'pbr' });
  await d({ cmd: 'add_texture_asset', id: tex, proc: 'checker' });
  const mk = async (n, spec, name, mat, pos) => {
    const node = ID(n), v = ID(n + 0x1000);
    await d({ cmd: 'insert', id: node, spec, parent: null });
    await d({ cmd: 'rename', id: node, name });
    await d({ cmd: 'set_transform', id: node, transform: { translation: pos, rotation: [0, 0, 0, 1], scale: [1, 1, 1] } });
    await d({ cmd: 'add_material_variant', node, material: mat, id: v, name: 'main' });
    await d({ cmd: 'select_material_variant', node, variant: v });
    return node;
  };
  const floor = await mk(0x01, { primitive: { plane: { width: 16, depth: 16, segments_x: 1, segments_z: 1 } } }, 'floor', matFloor, [0, 0, 0]);
  await d({ cmd: 'set_builtin_param', node: floor, param: 'base_color', value: [1, 1, 1, 1] });
  await d({ cmd: 'set_builtin_param', node: floor, param: 'roughness', value: [0.8] });
  await d({ cmd: 'set_builtin_texture', node: floor, slot: 'base_color', texture: tex });
  await d({ cmd: 'set_node_texture_transform', node: floor, slot: 'base_color', scale: [4, 4], wrap_u: 'repeat', wrap_v: 'repeat' });
  const cube = await mk(0x02, { primitive: { box: { dims: [1, 1, 1] } } }, 'cube', matCube, [0, 0.5, 0]);
  await d({ cmd: 'set_builtin_param', node: cube, param: 'base_color', value: [1.0, 0.45, 0.1, 1] });
  await d({ cmd: 'set_builtin_param', node: cube, param: 'roughness', value: [0.5] });
  await d({ cmd: 'set_builtin_texture', node: cube, slot: 'base_color', texture: tex });

  // The authored camera the harness renders through: straight on (+Z side),
  // so the cube's x motion is screen-horizontal.
  const cam = ID(0x10);
  await d({ cmd: 'insert', id: cam, spec: 'camera', parent: null });
  await d({ cmd: 'rename', id: cam, name: 'camera' });
  const look = [-0.6, 0.5, 0];
  await d({ cmd: 'set_transform', id: cam, transform: { translation: camPos(0, 0.35, 8, look), rotation: camQuat(0, 0.35), scale: [1, 1, 1] } });
  await d({ cmd: 'patch_kind', id: cam, patch: { camera: { projection: { perspective: { fov_y_rad: 0.7853981633974483 }, orthographic: null }, near: 0.1, far: 100.0 } } });

  await d({ cmd: 'set_post_process', bloom: false });
  await d({ cmd: 'set_camera_orbit', yaw: 0, pitch: 0.35, radius: 8, look_at: look });
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false });
  await d({ cmd: 'set_selection', ids: [] });
  await q({ query: 'wait_render_settled' });
  return 'taa-motion authored';
}
