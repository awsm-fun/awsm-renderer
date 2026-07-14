// test-scene: builtin-overrides
// One shared built-in PBR material asset; four spheres each carry their own
// variant overriding base_color / metallic / roughness / emissive, over a
// gray floor sharing the SAME material asset. Correct = four visibly
// different tunings of one material (textured logo, gold metal, cream clay,
// glowing teal) in a 2x2 grid, soft shadows, no grid/gizmos.
//
// The top-left sphere adds a per-node TEXTURE override: `set_builtin_texture
// {node, slot:'base_color', texture}` binds the Cesium logo to that node's
// INLINE material only — a per-node texture override on top of its scalar
// uniform overrides, while the other three spheres (sharing the same material
// asset) stay flat-tuned. This covers "builtin overrides · textures".
//
// Run inside the editor page (http://localhost:9085 attached to the dev MCP):
// evaluate this file's function, then `save_project`, `export_player_bundle`
// and `screenshot_scene` (MCP tools) write project/, bundle/ and golden.png.
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
  // Deterministic ids so re-runs produce an identical project.toml.
  const ID = (n) => `00000000-0000-4000-8000-0000000000${n.toString(16).padStart(2, '0')}`;
  await d({ cmd: 'new_project' });
  const floor = ID(1), mat = ID(2), floorVar = ID(30), tex = ID(0xf0);
  // Texture for the per-node base_color override on the first sphere.
  await d({ cmd: 'import_texture_from_url', id: tex, url: 'http://localhost:9082/glTF-Sample-Assets/Models/BoxTextured/glTF/CesiumLogoFlat.png' });
  for (let t = 0; ; t++) { const cs = await q({ query: 'save_census' }); if ((cs.texture_assets ?? 0) >= 1) break; if (t > 120) throw new Error('texture import never landed'); await new Promise(r => setTimeout(r, 250)); }
  await d({ cmd: 'insert', id: floor, spec: { primitive: { plane: { width: 12, depth: 12, segments_x: 1, segments_z: 1 } } }, parent: null });
  await d({ cmd: 'add_builtin_material', id: mat, shading: 'pbr' });
  await d({ cmd: 'add_material_variant', node: floor, material: mat, id: floorVar, name: 'floor-gray' });
  await d({ cmd: 'select_material_variant', node: floor, variant: floorVar });
  await d({ cmd: 'set_builtin_param', node: floor, param: 'base_color', value: [0.45, 0.45, 0.48, 1] });
  await d({ cmd: 'set_builtin_param', node: floor, param: 'roughness', value: [0.9] });
  const tunings = [
    { name: 'textured-logo', pos: [-1.7, 1.0, -1.7], base: [1, 1, 1, 1],            metallic: 0.0, roughness: 0.5,  emissive: [0, 0, 0], texture: true },
    { name: 'metal-gold',    pos: [1.7, 1.0, -1.7],  base: [1.0, 0.77, 0.34, 1],    metallic: 1.0, roughness: 0.15, emissive: [0, 0, 0] },
    { name: 'rough-clay',    pos: [-1.7, 1.0, 1.7],  base: [0.55, 0.4, 0.3, 1],     metallic: 0.0, roughness: 0.95, emissive: [0, 0, 0] },
    { name: 'emissive-teal', pos: [1.7, 1.0, 1.7],   base: [0.05, 0.05, 0.05, 1],   metallic: 0.0, roughness: 0.5,  emissive: [0, 2.5, 2.2] },
  ];
  for (let i = 0; i < tunings.length; i++) {
    const t = tunings[i];
    const node = ID(10 + i), variant = ID(20 + i);
    await d({ cmd: 'insert', id: node, spec: { primitive: { sphere: { radius: 0.9, segments_long: 32, segments_lat: 24 } } }, parent: null });
    await d({ cmd: 'set_transform', id: node, transform: { translation: t.pos, rotation: [0, 0, 0, 1], scale: [1, 1, 1] } });
    await d({ cmd: 'rename', id: node, name: t.name });
    await d({ cmd: 'add_material_variant', node, material: mat, id: variant, name: t.name });
    await d({ cmd: 'select_material_variant', node, variant });
    await d({ cmd: 'set_builtin_param', node, param: 'base_color', value: t.base });
    await d({ cmd: 'set_builtin_param', node, param: 'metallic', value: [t.metallic] });
    await d({ cmd: 'set_builtin_param', node, param: 'roughness', value: [t.roughness] });
    await d({ cmd: 'set_builtin_param', node, param: 'emissive', value: t.emissive });
    // Per-node TEXTURE override on the first sphere's inline material.
    if (t.texture) await d({ cmd: 'set_builtin_texture', node, slot: 'base_color', texture: tex });
  }
  // Pinned framing for the golden; grid/gizmos/light-gizmos off = clean capture.
  await d({ cmd: 'set_camera_orbit', yaw: 0.0, pitch: 0.55, radius: 11.5, look_at: [0, 0.5, 0] });
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false });
  await q({ query: 'wait_render_settled' });
  return 'builtin-overrides authored';
}
