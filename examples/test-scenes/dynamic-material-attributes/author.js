// test-scene: dynamic-material-attributes
// Per-instance ATTRIBUTE data into a custom material — divergence driven by a
// per-vertex/per-instance channel, NOT by a uniform. ONE instancer owns 12 box
// instances; each instance is given a distinct rainbow color via
// `set_instancer_transforms {per_instance_colors}`. The custom material reads
// that channel with `material_vertex_color(input, 0u)` (the `vertex_color`
// include + the `vertex_color` fragment input) and outputs it — so the 12 boxes
// span a rainbow even though they share ONE material with ONE uniform
// (`ambient`, identical for all). If the color came from the uniform every box
// would be identical; the rainbow proves the attribute path.
//
// This is the ATTRIBUTE counterpart to dynamic-materials (uniform override) and
// dynamic-material-textures (texture slot). Layer-A (visual); golden = the
// settled rainbow row.
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
  // Custom material: reads the per-instance color channel (fed as vertex color
  // 0 by the instancer). One SHARED uniform `ambient` — identical for all
  // instances, so it cannot be the source of the per-box divergence.
  await d({ cmd: 'add_custom_material', id: cmat });
  await d({ cmd: 'set_custom_material_layout', id: cmat, uniforms: [{ name: 'ambient', ty: 'f32', val: '0.35' }], textures: [], buffers: [] });
  await d({ cmd: 'set_custom_material_shader_includes', id: cmat, includes: ['vertex_color'] });
  await d({ cmd: 'set_custom_material_fragment_inputs', id: cmat, inputs: ['normals', 'vertex_color'] });
  const shade = `let vc = material_vertex_color(input, 0u);
let n = normalize(input.world_normal);
let l = normalize(vec3<f32>(0.3, 0.8, 0.4));
let diff = max(dot(n, l), 0.0) * 0.7 + input.material.ambient;
return OpaqueShadingOutput(vc.rgb * diff, 1.0);`;
  await d({ cmd: 'set_custom_material_wgsl', id: cmat, wgsl: shade });
  await d({ cmd: 'register_material', id: cmat });
  await new Promise(r => setTimeout(r, 1800));
  const diag = await q({ query: 'material_diagnostics', material: cmat });
  if (!diag.ok || !diag.registered) throw new Error('custom material failed to register: ' + JSON.stringify(diag.errors));
  // Floor.
  await d({ cmd: 'insert', id: ID(1), spec: { primitive: { plane: { width: 30, depth: 30, segments_x: 1, segments_z: 1 } } }, parent: null });
  await d({ cmd: 'add_material_variant', node: ID(1), material: matFloor, id: ID(0x40), name: 'floor' });
  await d({ cmd: 'select_material_variant', node: ID(1), variant: ID(0x40) });
  await d({ cmd: 'set_builtin_param', node: ID(1), param: 'base_color', value: [0.3, 0.32, 0.35, 1] });
  // Source box → its mesh asset drives the instancer; hide the source node.
  await d({ cmd: 'insert', id: ID(0x10), spec: { primitive: { box: { dims: [1.0, 1.0, 1.0] } } }, parent: null });
  const det = await q({ query: 'node_kind_details', nodes: [ID(0x10)] });
  const meshAsset = det.entries[ID(0x10)].mesh.mesh;
  await d({ cmd: 'set_visible', id: ID(0x10), visible: false });
  // Instancer carrying the custom material.
  await d({ cmd: 'insert', id: ID(0x20), spec: 'instancer', parent: null });
  // An instancer has ONE material (no variant palette — add_material_variant
  // rejects it); `patch_kind {instancer: {material: ..}}` is the setter.
  await d({ cmd: 'patch_kind', id: ID(0x20), patch: { instancer: { mesh: meshAsset, material: { asset: cmat } } } });
  // 12 instances in a 3×4 grid, each with a DISTINCT per-instance color
  // (rainbow by index) — the grid fills the native portrait canvas aspect.
  const transforms = [], colors = [];
  const N = 12, COLS = 3, ROWS = 4, SP = 2.4;
  for (let i = 0; i < N; i++) {
    const cx = (i % COLS) - (COLS - 1) / 2;
    const cz = Math.floor(i / COLS) - (ROWS - 1) / 2;
    const hue = i / N;
    const r = 0.5 + 0.5 * Math.cos(6.283 * (hue + 0.0));
    const g = 0.5 + 0.5 * Math.cos(6.283 * (hue + 0.333));
    const b = 0.5 + 0.5 * Math.cos(6.283 * (hue + 0.666));
    transforms.push({ translation: [cx * SP, 0.6, cz * SP], rotation: [0, 0, 0, 1], scale: [1, 1, 1] });
    colors.push([r, g, b, 1]);
  }
  await d({ cmd: 'set_instancer_transforms', node: ID(0x20), transforms, per_instance_colors: colors });
  await d({ cmd: 'rename', id: ID(0x20), name: 'attr-instancer' });
  await d({ cmd: 'set_selection', ids: [] });
  await d({ cmd: 'set_camera_orbit', yaw: 0.18, pitch: 0.55, radius: 17.5, look_at: [0, 0.4, 0] });
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false });
  await new Promise(r => setTimeout(r, 800));
  await q({ query: 'wait_render_settled' });
  return 'dynamic-material-attributes authored';
}
