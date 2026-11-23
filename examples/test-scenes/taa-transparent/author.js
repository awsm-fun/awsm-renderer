// test-scene: taa-transparent (docs/plans/taa-upscaling/05-verification.md §1)
// Things with no correct motion vector, in front of a MOVING background:
// a blended particle fountain, a glass (transmission) sphere, a UV-flow
// "water" plane, and fat lines. The TAA harness (scenario `taa-transparent`)
// slides the node `backdrop` (a 4 × 3 × 0.1 m checker box — its exact hull
// is the trail-band mask) 2 m right and back at 1.33 m/s. Catches the reactive
// mask (G8, phase 3).
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
  const mat = ID(0x900), matGlass = ID(0x901), tex = ID(0x9f0), noise = ID(0x9f1);
  await d({ cmd: 'add_builtin_material', id: mat, shading: 'pbr' });
  await d({ cmd: 'add_builtin_material', id: matGlass, shading: 'pbr' });
  const noExt = { emissive_strength: null, ior: null, specular: null, transmission: null, diffuse_transmission: null, volume: null, clearcoat: null, sheen: null, dispersion: null, anisotropy: null, iridescence: null };
  const def = (over) => Object.assign({
    label: '', shading: 'pbr', alpha_mode: 'opaque', double_sided: false,
    base_color: [0.9, 0.9, 0.95, 1], base_color_texture: null,
    metallic: 0.0, roughness: 0.05, metallic_roughness_texture: null,
    normal_scale: 1, normal_texture: null, occlusion_strength: 1, occlusion_texture: null,
    emissive: [0, 0, 0], emissive_texture: null, vertex_colors_enabled: false,
    extensions: noExt,
  }, over);

  await d({ cmd: 'update_builtin_material', id: matGlass, def: def({ label: 'glass', extensions: Object.assign({}, noExt, { transmission: { factor: 1.0, tex: null }, ior: { ior: 1.5 } }) }) });
  await d({ cmd: 'add_texture_asset', id: tex, proc: 'checker' });
  await d({ cmd: 'add_texture_asset', id: noise, proc: 'noise' });
  const floor = await mk(0x01, { primitive: { plane: { width: 14, depth: 14, segments_x: 1, segments_z: 1 } } }, 'floor', mat, [0, 0, 0]);
  await paint(floor, [0.45, 0.45, 0.48, 1], 0.9);
  const back = await mk(0x02, { primitive: { box: { dims: [4, 3, 0.1] } } }, 'backdrop', mat, [-1, 1.5, -2.5]);
  await paint(back, [1, 0.95, 0.85, 1], 0.7, tex, [4, 3]);
  // Water: a UV-flowing noise plane in front (texture moves, geometry does not).
  const water = await mk(0x03, { primitive: { plane: { width: 5, depth: 2.5, segments_x: 1, segments_z: 1 } } }, 'water', mat, [0, 0.02, 1.2]);
  await paint(water, [0.3, 0.55, 0.9, 1], 0.2, noise, [2, 1]);
  await d({ cmd: 'set_node_texture_transform', node: water, slot: 'base_color', flow: [0.25, 0.08] });
  // Glass sphere in front of the backdrop.
  const glass = await mk(0x04, { primitive: { sphere: { radius: 0.6, segments_long: 48, segments_lat: 32 } } }, 'glass', matGlass, [0.9, 0.9, -0.6]);
  // Particle fountain (blended).
  await d({ cmd: 'insert', id: ID(0x05), spec: { particle: {} }, parent: null });
  await d({ cmd: 'rename', id: ID(0x05), name: 'fountain' });
  await d({ cmd: 'set_transform', id: ID(0x05), transform: { translation: [-1.2, 0.2, -0.4], rotation: [0, 0, 0, 1], scale: [1, 1, 1] } });
  await d({ cmd: 'set_particle_emitter', node: ID(0x05), spawn_rate: 120, max_alive: 512, lifetime: [0.8, 1.6], initial_speed: [2.5, 3.5], size: [0.06, 0.12], color_over_life: { linear: { start: [1, 0.6, 0.15, 1], end: [1, 0.1, 0.05, 0] } }, size_over_life: { linear: { start: 1, end: 0.4 } }, forces: [{ gravity: { acceleration: [0, -4, 0] } }], blend: true });
  // Fat lines: a zig-zag in front of the backdrop.
  await d({ cmd: 'insert', id: ID(0x06), spec: 'line', parent: null });
  await d({ cmd: 'rename', id: ID(0x06), name: 'zigzag' });
  const pts = [];
  for (let i = 0; i <= 12; i++) pts.push({ pos: [-2.2 + i * 0.35, 1.9 + (i % 2 ? 0.35 : -0.35), -1.4], color: [0.1, 1.0, 0.4, 1] });
  await d({ cmd: 'patch_kind', id: ID(0x06), patch: { line: { points: pts, width_px: 3.0, depth_test_always: false } } });
  await addCamera(0x500, 'camera', 0, 0.2, 7.5, [0, 1.1, -0.5]);
  return await finish('taa-transparent', [0, 1.1, -0.5], 0, 0.2, 7.5);
}
