// test-scene: taa-hud (docs/plans/taa-upscaling/05-verification.md §1)
// HUD meshes over a moving world. The world: textured shapes on a checker
// floor. The HUD: `hud-panel` (a dark translucent-looking slab) and
// `hud-marker` (a bright small box), placed in front of the authored camera.
// HUD is a runtime flag, not scene state: the TAA harness (scenario `taa-hud`)
// calls `set_mesh_hud` on those two nodes after load, keeps them rigidly
// attached to the camera, and orbits the camera 30°/s for 2 s. The HUD pixels
// must match a TAA-off render (phase 3, 03 §5.4).
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
  const floor = await mk(0x01, { primitive: { plane: { width: 24, depth: 24, segments_x: 1, segments_z: 1 } } }, 'floor', mat, [0, 0, 0]);
  await paint(floor, [0.9, 0.9, 0.9, 1], 0.85, tex, [6, 6]);
  const shapes = [[{ box: { dims: [1, 1, 1] } }, [-1.8, 0.5, 0], [0.85, 0.2, 0.15, 1]], [{ sphere: { radius: 0.6, segments_long: 48, segments_lat: 32 } }, [0, 0.6, -1.2], [0.2, 0.6, 0.9, 1]], [{ box: { dims: [0.5, 2.0, 0.5] } }, [1.8, 1.0, 0.3], [0.95, 0.8, 0.2, 1]]];
  for (let i = 0; i < shapes.length; i++) {
    const [spec, pos, color] = shapes[i];
    const n = await mk(0x02 + i, { primitive: spec }, `shape-${i}`, mat, pos);
    await paint(n, color, 0.5, tex, [2, 2]);
  }
  // Camera first, so the HUD can be placed in its frame.
  const yaw = 0, pitch = 0.3, radius = 8, look = [0, 0.6, 0];
  await addCamera(0x500, 'camera', yaw, pitch, radius, look);
  // HUD: 1.5 m in front of the camera, lower-left panel + upper-right marker.
  const eye = camPos(yaw, pitch, radius, look), rot = camQuat(yaw, pitch);
  const fwd = [-Math.cos(pitch) * Math.sin(yaw), -Math.sin(pitch), -Math.cos(pitch) * Math.cos(yaw)];
  const right = [Math.cos(yaw), 0, -Math.sin(yaw)];
  const up = [right[1] * fwd[2] - right[2] * fwd[1], right[2] * fwd[0] - right[0] * fwd[2], right[0] * fwd[1] - right[1] * fwd[0]];
  const at = (f, r, u) => [0, 1, 2].map(i => eye[i] + fwd[i] * f + right[i] * r + up[i] * u);
  const panel = await mk(0x10, { primitive: { box: { dims: [0.5, 0.18, 0.01] } } }, 'hud-panel', mat, at(1.5, -0.38, -0.3), rot);
  await paint(panel, [0.05, 0.06, 0.09, 1], 0.9);
  const marker = await mk(0x11, { primitive: { box: { dims: [0.08, 0.08, 0.01] } } }, 'hud-marker', mat, at(1.5, 0.45, 0.32), rot);
  await paint(marker, [1.0, 0.9, 0.1, 1], 0.4);
  await d({ cmd: 'set_builtin_param', node: marker, param: 'emissive', value: [1.0, 0.9, 0.1] });
  return await finish('taa-hud', look, yaw, pitch, radius);
}
