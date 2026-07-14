// test-scene: cutoff-dynamic
// Custom-WGSL (dynamic) material with MASK alpha driven by a custom alpha
// fragment: an upright panel whose opaque body is punched by a procedural grid
// of circular HOLES computed in WGSL (custom_alpha_dynamic returns an f32
// alpha; Mask{cutoff} alpha-tests it). Correct = hard-edged circular cutouts
// (sky visible through the holes), NOT a smooth fade — the cutout is driven by
// the custom alpha WGSL, not a texture. Double-sided so the back face shows
// through the holes; a directional light casts a HOLE-PUNCHED shadow on the
// floor.
//
// Shapes (see dynamic-materials for the base custom-material flow):
//  - set_custom_material_alpha_wgsl {id, wgsl}: body of
//    `fn custom_alpha_dynamic(input: MaskAlphaInput) -> f32` — return alpha in [0,1].
//    input.uv is available; alpha < cutoff = discarded (hole).
//  - set_custom_material_alpha_mode {id, mode:{mask:{cutoff}}} (snake_case enum).
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
  const cmat = ID(5), matFloor = ID(2);
  await d({ cmd: 'add_builtin_material', id: matFloor, shading: 'pbr' });
  await d({ cmd: 'add_custom_material', id: cmat });
  await d({ cmd: 'set_custom_material_layout', id: cmat, uniforms: [{ name: 'tint', ty: 'vec3<f32>' }], textures: [], buffers: [] });
  const shade = `let n = normalize(input.world_normal);
let l = normalize(vec3<f32>(0.3, 0.7, 0.5));
let diff = max(dot(n, l), 0.0) * 0.85 + 0.2;
return OpaqueShadingOutput(input.material.tint * diff, 1.0);`;
  // Procedural grid of circular holes: opaque (1.0) everywhere except inside a
  // 5×5 grid of discs (0.0). Mask{cutoff 0.5} discards the discs → hard holes.
  const alpha = `let g = fract(input.uv * 5.0) - vec2<f32>(0.5);
return select(1.0, 0.0, dot(g, g) < 0.12);`;
  await d({ cmd: 'set_custom_material_wgsl', id: cmat, wgsl: shade });
  await d({ cmd: 'set_custom_material_alpha_wgsl', id: cmat, wgsl: alpha });
  await d({ cmd: 'set_custom_material_alpha_mode', id: cmat, mode: { mask: { cutoff: 0.5 } } });
  await d({ cmd: 'set_custom_material_double_sided', id: cmat, double_sided: true });
  await d({ cmd: 'register_material', id: cmat });
  await new Promise(r => setTimeout(r, 1800));
  const diag = await q({ query: 'material_diagnostics', material: cmat });
  if (!diag.ok || !diag.registered) throw new Error('custom material failed to register: ' + JSON.stringify(diag.errors));
  await d({ cmd: 'set_material_uniform', material: cmat, name: 'tint', value: '0.9, 0.35, 0.15' });
  // Floor.
  await d({ cmd: 'insert', id: ID(1), spec: { primitive: { plane: { width: 12, depth: 12, segments_x: 1, segments_z: 1 } } }, parent: null });
  await d({ cmd: 'add_material_variant', node: ID(1), material: matFloor, id: ID(0x40), name: 'floor' });
  await d({ cmd: 'select_material_variant', node: ID(1), variant: ID(0x40) });
  await d({ cmd: 'set_builtin_param', node: ID(1), param: 'base_color', value: [0.4, 0.42, 0.45, 1] });
  // Upright panel (thin box, +Z face toward the default camera) with the masked custom material.
  await d({ cmd: 'insert', id: ID(0x10), spec: { primitive: { box: { dims: [3.0, 3.0, 0.06] } } }, parent: null });
  await d({ cmd: 'set_transform', id: ID(0x10), transform: { translation: [0, 1.8, 0], rotation: [0, 0, 0, 1], scale: [1, 1, 1] } });
  await d({ cmd: 'rename', id: ID(0x10), name: 'masked-panel' });
  await d({ cmd: 'add_material_variant', node: ID(0x10), material: cmat, id: ID(0x30), name: 'custom-mask' });
  await d({ cmd: 'select_material_variant', node: ID(0x10), variant: ID(0x30) });
  await d({ cmd: 'set_camera_orbit', yaw: 0.25, pitch: 0.2, radius: 8, look_at: [0, 1.6, 0] });
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false });
  await q({ query: 'wait_render_settled' });
  return 'cutoff-dynamic authored';
}
