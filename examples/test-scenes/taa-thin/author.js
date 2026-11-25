// test-scene: taa-thin (docs/plans/taa-final-round/verification.md §1)
// Everything that flickers or shimmers: a thin-wire fence, alpha-masked leaf
// cards, a very fine checker floor running to a grazing angle (moiré), and a
// clearcoat box with sharp highlights. The TAA harness (scenario `taa-thin`)
// keeps it static for 2 s (convergence + the 05 §4.3 flicker test), then
// spins the node `spinner` at 180°/s for 1.5 s, then pans the camera at
// 0.5 px/frame for 1.5 s (texture-LOD / moiré under motion, G5, 03 §7).
// `spinner` is a 1 m box (analytic coverage mask).
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
  const mat = ID(0x900), matMask = ID(0x901), matCoat = ID(0x902), tex = ID(0x9f0), leaf = ID(0x9f1);
  await d({ cmd: 'add_builtin_material', id: mat, shading: 'pbr' });
  await d({ cmd: 'add_builtin_material', id: matMask, shading: 'pbr' });
  await d({ cmd: 'set_builtin_alpha_mode', material: matMask, mode: { mask: { cutoff: 0.5 } } });
  await d({ cmd: 'add_builtin_material', id: matCoat, shading: 'pbr' });
  const noExt = { emissive_strength: null, ior: null, specular: null, transmission: null, diffuse_transmission: null, volume: null, clearcoat: null, sheen: null, dispersion: null, anisotropy: null, iridescence: null };
  await d({ cmd: 'update_builtin_material', id: matCoat, def: {
    label: 'clearcoat', shading: 'pbr', alpha_mode: 'opaque', double_sided: false,
    base_color: [0.55, 0.04, 0.04, 1], base_color_texture: null,
    metallic: 0.0, roughness: 0.5, metallic_roughness_texture: null,
    normal_scale: 1, normal_texture: null, occlusion_strength: 1, occlusion_texture: null,
    emissive: [0, 0, 0], emissive_texture: null, vertex_colors_enabled: false,
    extensions: Object.assign({}, noExt, { clearcoat: { factor: 1.0, roughness_factor: 0.03, normal_scale: 1, tex: null, roughness_tex: null, normal_tex: null } }),
  } });
  await d({ cmd: 'add_texture_asset', id: tex, proc: 'checker' });
  await d({ cmd: 'import_texture_from_url', id: leaf, url: 'http://localhost:9082/glTF-Sample-Assets/Models/DiffuseTransmissionPlant/glTF/img0.png' });
  for (let tries = 0; ; tries++) {
    const cs = await q({ query: 'save_census' });
    if ((cs.texture_with_bytes ?? 0) >= 1) break; // the procedural checker carries no bytes
    if (tries > 120) throw new Error(`leaf texture import never landed: ${JSON.stringify(cs)}`);
    await new Promise(r => setTimeout(r, 250));
  }
  // Fine checker floor, 30 m deep: cells of ~6 cm, grazing in the distance.
  const floor = await mk(0x01, { primitive: { plane: { width: 30, depth: 30, segments_x: 1, segments_z: 1 } } }, 'floor', mat, [0, 0, -10]);
  await paint(floor, [1, 1, 1, 1], 0.9, tex, [60, 60]);
  // Fence: 25 posts 1.5 cm thick, 3 wires 1 cm thick, dark against the sky.
  let n = 0x02;
  for (let i = 0; i < 25; i++) {
    const p = await mk(n++, { primitive: { box: { dims: [0.015, 1.2, 0.015] } } }, `post-${i}`, mat, [-1.8 + i * 0.12, 0.6, 0.6]);
    await paint(p, [0.06, 0.06, 0.07, 1], 0.4);
  }
  for (const y of [0.3, 0.75, 1.15]) {
    const w = await mk(n++, { primitive: { box: { dims: [2.9, 0.01, 0.01] } } }, `wire-${y}`, mat, [-0.36, y, 0.6]);
    await paint(w, [0.06, 0.06, 0.07, 1], 0.4);
  }
  // Leaf cards: thin masked boxes, front face carries the leaf texture.
  const cards = [[-1.9, 0.6, -0.8, 0.3], [-0.9, 0.5, -1.1, -0.4], [0.1, 0.55, -0.9, 0.6], [1.1, 0.5, -1.2, -0.2]];
  for (const [x, y, z, yaw] of cards) {
    const c = await mk(n++, { primitive: { box: { dims: [1.0, 1.0, 0.002] } } }, `card-${x}`, matMask, [x, y, z], [0, Math.sin(yaw / 2), 0, Math.cos(yaw / 2)]);
    await paint(c, [1, 1, 1, 1], 0.7, leaf);
  }
  // The clearcoat spinner (still until the harness turns it).
  const sp = ID(0x400);
  await d({ cmd: 'insert', id: sp, spec: { primitive: { box: { dims: [1, 1, 1] } } }, parent: null });
  await d({ cmd: 'rename', id: sp, name: 'spinner' });
  await d({ cmd: 'set_transform', id: sp, transform: { translation: [2.3, 0.5, -0.6], rotation: [0, 0.2588, 0, 0.9659], scale: [1, 1, 1] } });
  await d({ cmd: 'add_material_variant', node: sp, material: matCoat, id: ID(0x401), name: "main" });
  await d({ cmd: 'select_material_variant', node: sp, variant: ID(0x401) });
  await addCamera(0x500, 'camera', 0.15, 0.18, 6.5, [0.2, 0.5, -1]);
  return await finish('taa-thin', [0.2, 0.5, -1], 0.15, 0.18, 6.5);
}
