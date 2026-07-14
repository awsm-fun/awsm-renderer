// test-scene: decals
// Projection decal over floor + box: the AlphaBlendModeTest label sheet
// (raster, alpha) projected down through a rotated decal volume that
// straddles a box edge. Correct = the texture wraps floor AND box inside
// the (green wireframe) volume, alpha cutouts respected, nothing projected
// on the skybox or outside the volume, and moving the decal node moves the
// projection live.
//
// THIS SCENE'S ORIGIN — editor decals had NEVER projected; authoring it
// found and fixed three stacked bugs (2026-07-10):
//   1. decal-classify HZB gate not migrated to reverse-Z (dropped every
//      decal from every tile under the default convention) + a broken
//      firstLeadingBit mip selection that read the coarsest texel;
//   2. the editor bridge hardcoded texture_index 0 and never resolved the
//      decal's texture asset to the pool's flat index;
//   3. decals were a one-shot world-matrix snapshot — moving the node moved
//      only the wireframe (and the wireframe was drawn at HALF the real
//      projection volume).
// KNOWN GAP: a PROCEDURAL texture in the decal slot still projects white
// (raster works); tracked in docs/plans/006 Phase 0 notes.
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
  const matFloor = ID(2), matBox = ID(3), tex = ID(0xf2);
  await d({ cmd: 'add_builtin_material', id: matFloor, shading: 'pbr' });
  await d({ cmd: 'add_builtin_material', id: matBox, shading: 'pbr' });
  await d({ cmd: 'import_texture_from_url', id: tex, url: 'http://localhost:9082/glTF-Sample-Assets/Models/AlphaBlendModeTest/glTF/AlphaBlendLabels.png' });
  // Wait for the import to land before binding it as a decal — a fresh
  // author.js replay races the import otherwise (the decal projects an empty
  // slot → blank). The baked project/ already has the texture settled.
  for (let tries = 0; ; tries++) {
    const cs = await q({ query: 'save_census' });
    if ((cs.texture_assets ?? 0) >= 1) break;
    if (tries > 120) throw new Error(`texture import never landed: ${JSON.stringify(cs)}`);
    await new Promise(r => setTimeout(r, 250));
  }
  const mk = async (idStr, spec, pos, mat, name, vId, base, rough) => {
    await d({ cmd: 'insert', id: idStr, spec, parent: null });
    await d({ cmd: 'set_transform', id: idStr, transform: { translation: pos, rotation: [0, 0, 0, 1], scale: [1, 1, 1] } });
    await d({ cmd: 'rename', id: idStr, name });
    await d({ cmd: 'add_material_variant', node: idStr, material: mat, id: vId, name });
    await d({ cmd: 'select_material_variant', node: idStr, variant: vId });
    await d({ cmd: 'set_builtin_param', node: idStr, param: 'base_color', value: base });
    await d({ cmd: 'set_builtin_param', node: idStr, param: 'roughness', value: [rough] });
  };
  await mk(ID(1), { primitive: { plane: { width: 12, depth: 12, segments_x: 1, segments_z: 1 } } }, [0, 0, 0], matFloor, 'floor', ID(0x40), [0.5, 0.45, 0.4, 1], 0.8);
  await mk(ID(10), { primitive: { box: { dims: [1.8, 1.2, 1.8] } } }, [0.8, 0.6, 0], matBox, 'box', ID(0x41), [0.35, 0.5, 0.65, 1], 0.6);
  await d({ cmd: 'insert', id: ID(0x60), spec: { decal: {} }, parent: null });
  await d({ cmd: 'patch_kind', id: ID(0x60), patch: { decal: { texture: tex } } });
  const hx = Math.sin(-Math.PI / 4), cx = Math.cos(-Math.PI / 4);
  await d({ cmd: 'set_transform', id: ID(0x60), transform: { translation: [0, 1.2, 0.6], rotation: [hx, 0, 0, cx], scale: [1.5, 1.5, 1.5] } });
  await d({ cmd: 'rename', id: ID(0x60), name: 'decal-checker' });
  await d({ cmd: 'set_selection', ids: [] });
  await d({ cmd: 'set_camera_orbit', yaw: 0.85, pitch: 0.5, radius: 8, look_at: [0.2, 0.4, 0.2] });
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false });
  await q({ query: 'wait_render_settled' });
  return 'decals authored';
}
