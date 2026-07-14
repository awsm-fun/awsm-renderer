// test-scene: contact-shadows
// SSCS + PCSS — neither is exercised by shadows-all (Soft PCF only). A resting
// sphere + a standing pole under a PCSS spot light (2D atlas; PCSS is 2D-atlas
// only, so a SPOT, not the directional cascades), with SSCS enabled
// renderer-wide. Correct = the sphere's contact shadow is contact-HARDENING
// (tight/dark right at the ground-contact point, softening outward — the PCSS
// blocker-search penumbra), plus SSCS short-range contact darkening where
// geometry meets the floor. Compare hardness pcss↔soft and sscs on↔off to see
// each effect.
//
// Shapes: PCSS is per-light via patch_kind on the spot's shadow.hardness
// ('pcss', snake_case) — SetLightParam does NOT cover hardness. SSCS is
// renderer-wide via set_shadows {patch:{sscs_enabled, sscs_step_count, ...}}.
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
  // Remove the seeded directional so the PCSS spot is the sole caster (PCSS is
  // 2D-atlas only — directional cascades don't do contact-hardening).
  const snap = await q({ query: 'snapshot' });
  const walk = (ns, out = []) => { for (const n of ns) { out.push(n); if (n.children) walk(n.children, out); } return out; };
  const dir = walk(snap.scene_tree || []).find(n => (n.name || '').toLowerCase().includes('directional'));
  if (dir) await d({ cmd: 'delete', id: dir.id });
  const matFloor = ID(2), matObj = ID(3);
  await d({ cmd: 'add_builtin_material', id: matFloor, shading: 'pbr' });
  await d({ cmd: 'add_builtin_material', id: matObj, shading: 'pbr' });
  await d({ cmd: 'insert', id: ID(1), spec: { primitive: { plane: { width: 16, depth: 16, segments_x: 1, segments_z: 1 } } }, parent: null });
  await d({ cmd: 'add_material_variant', node: ID(1), material: matFloor, id: ID(0x40), name: 'floor' });
  await d({ cmd: 'select_material_variant', node: ID(1), variant: ID(0x40) });
  await d({ cmd: 'set_builtin_param', node: ID(1), param: 'base_color', value: [0.6, 0.6, 0.62, 1] });
  // Standing pole (base-contact shadow).
  await d({ cmd: 'insert', id: ID(0x10), spec: { primitive: { box: { dims: [0.35, 4.5, 0.35] } } }, parent: null });
  await d({ cmd: 'set_transform', id: ID(0x10), transform: { translation: [0.6, 2.25, 0], rotation: [0, 0, 0, 1], scale: [1, 1, 1] } });
  await d({ cmd: 'rename', id: ID(0x10), name: 'pole' });
  await d({ cmd: 'add_material_variant', node: ID(0x10), material: matObj, id: ID(0x41), name: 'm' });
  await d({ cmd: 'select_material_variant', node: ID(0x10), variant: ID(0x41) });
  await d({ cmd: 'set_builtin_param', node: ID(0x10), param: 'base_color', value: [0.8, 0.3, 0.25, 1] });
  // Resting sphere (PCSS contact-hardening: sharp at the contact point, soft outward).
  await d({ cmd: 'insert', id: ID(0x11), spec: { primitive: { sphere: { radius: 0.7, segments_long: 32, segments_lat: 24 } } }, parent: null });
  await d({ cmd: 'set_transform', id: ID(0x11), transform: { translation: [-1.6, 0.7, 1.0], rotation: [0, 0, 0, 1], scale: [1, 1, 1] } });
  await d({ cmd: 'rename', id: ID(0x11), name: 'resting-sphere' });
  await d({ cmd: 'add_material_variant', node: ID(0x11), material: matObj, id: ID(0x42), name: 'm2' });
  await d({ cmd: 'select_material_variant', node: ID(0x11), variant: ID(0x42) });
  // Overhead PCSS spot (straight down).
  const spot = ID(0x50);
  await d({ cmd: 'insert', id: spot, spec: { light: 'spot' }, parent: null });
  await d({ cmd: 'set_transform', id: spot, transform: { translation: [0, 10, 0], rotation: [-0.707, 0, 0, 0.707], scale: [1, 1, 1] } });
  await d({ cmd: 'set_light_param', node: spot, param: 'intensity', value: [9000] });
  await d({ cmd: 'set_light_param', node: spot, param: 'range', value: [40] });
  await d({ cmd: 'set_light_param', node: spot, param: 'outer_angle', value: [0.9] });
  await d({ cmd: 'set_light_param', node: spot, param: 'inner_angle', value: [0.5] });
  // PCSS + a wider penumbra so the contact-hardening reads.
  await d({ cmd: 'patch_kind', id: spot, patch: { light: { spot: { shadow: { hardness: 'pcss', pcss_penumbra_scale: 3.0 } } } } });
  // SSCS (renderer-wide).
  await d({ cmd: 'set_shadows', patch: { sscs_enabled: true, sscs_step_count: 24, sscs_punctual_darkening: 0.9 } });
  await d({ cmd: 'set_camera_orbit', yaw: 0.6, pitch: 0.42, radius: 13, look_at: [-0.3, 0.8, 0.3] });
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false });
  await new Promise(r => setTimeout(r, 700));
  await q({ query: 'wait_render_settled' });
  return 'contact-shadows authored';
}
