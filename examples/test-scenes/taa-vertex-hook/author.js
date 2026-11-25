// test-scene: taa-vertex-hook (docs/plans/taa-final-round/verification.md §4.1)
// A mesh whose vertices move ONLY through a time-driven custom vertex hook
// (`wgsl_vertex`): `wave`, a 3 × 1.5 m plane, 96 × 24 segments, floating at
// 0.5 m, displaced by a travelling sine wave (amplitude 0.15 m, 2 waves / m,
// 4 rad/s) with the matching analytic normal so the crests shade. The motion
// vectors must reproduce the hook's motion (the hook is evaluated twice, with
// `time` and `prev_time`, 03 §2.3). The harness (scenario `taa-vertex-hook`)
// only advances time; the camera is still. Also the AA-toggle check
// (`?mode=aa-toggle`): none → TAA → MSAA → TAA must leave the mesh displaced.
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
  const mat = ID(0x900), cmat = ID(0x901), tex = ID(0x9f0);
  await d({ cmd: 'add_builtin_material', id: mat, shading: 'pbr' });
  await d({ cmd: 'add_texture_asset', id: tex, proc: 'checker' });
  const floor = await mk(0x01, { primitive: { plane: { width: 12, depth: 12, segments_x: 1, segments_z: 1 } } }, 'floor', mat, [0, 0, 0]);
  await paint(floor, [0.9, 0.9, 0.9, 1], 0.85, tex, [4, 4]);
  // The custom material: lambert shading of an orange tint, plus the vertex hook.
  await d({ cmd: 'add_custom_material', id: cmat });
  await d({ cmd: 'set_custom_material_layout', id: cmat, uniforms: [{ name: 'tint', ty: 'vec3<f32>' }], textures: [], buffers: [] });
  await d({ cmd: 'set_custom_material_fragment_inputs', id: cmat, inputs: ['normals'] });
  await d({ cmd: 'set_custom_material_wgsl', id: cmat, wgsl: `let n = normalize(input.world_normal);
let l = normalize(vec3<f32>(0.3, 0.9, 0.4));
let diff = max(dot(n, l), 0.0) * 0.85 + 0.15;
return OpaqueShadingOutput(input.material.tint * diff, 1.0);` });
  await d({ cmd: 'set_custom_material_vertex_wgsl', id: cmat, wgsl: `let k = 12.566371; // 2 waves per metre
let w = 4.0;        // rad / s
let a = 0.15;       // metres
let phase = input.position.x * k - input.globals.time * w;
let h = a * sin(phase);
let dhdx = a * k * cos(phase);
let n = normalize(vec3<f32>(-dhdx, 1.0, 0.0));
return VertexDisplaceOutput(input.position + vec3<f32>(0.0, h, 0.0), n, input.tangent);` });
  await d({ cmd: 'register_material', id: cmat });
  for (let t = 0; ; t++) {
    await new Promise(r => setTimeout(r, 500));
    const diag = await q({ query: 'material_diagnostics', material: cmat });
    if (diag.ok && diag.registered) break;
    if (t > 30) throw new Error('vertex-hook material failed to register: ' + JSON.stringify(diag));
  }
  await d({ cmd: 'set_material_uniform', material: cmat, name: 'tint', value: '0.95, 0.45, 0.12' });
  const wave = ID(0x02);
  await d({ cmd: 'insert', id: wave, spec: { primitive: { plane: { width: 3, depth: 1.5, segments_x: 96, segments_z: 24 } } }, parent: null });
  await d({ cmd: 'rename', id: wave, name: 'wave' });
  await d({ cmd: 'set_transform', id: wave, transform: { translation: [0, 0.5, 0], rotation: [0, 0, 0, 1], scale: [1, 1, 1] } });
  await d({ cmd: 'add_material_variant', node: wave, material: cmat, id: ID(0x1002), name: 'main' });
  await d({ cmd: 'select_material_variant', node: wave, variant: ID(0x1002) });
  await addCamera(0x500, 'camera', 0.35, 0.45, 4.5, [0, 0.5, 0]);
  return await finish('taa-vertex-hook', [0, 0.5, 0], 0.35, 0.45, 4.5);
}
