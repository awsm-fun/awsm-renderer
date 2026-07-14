// test-scene: dynamic-material-textures
// A custom-WGSL material that SAMPLES A BOUND TEXTURE — the dynamic-material
// texture-slot path (layout `textures:[{name:'tex',...}]` → generated
// `material_sample_tex` sampler + `material_uv` accessor from the `textures`
// include), distinct from dynamic-materials (procedural, no texture) and from
// builtin base_color_texture. A box panel with the Duck albedo (DuckCM.png)
// bound to slot `tex`, sampled at the mesh's own UV0 and shaded with a simple
// wrap-diffuse term. Correct = the duck reads sharp and in correct sRGB color
// (yellow body, black/white eye) across the panel — proving the texture slot
// binds, uploads sRGB (color_kind albedo), and samples at the real UVs.
//
// Layer-A (visual). No animation. The golden is the settled textured panel.
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
  const cmat = ID(5), tex = ID(0xf0);
  // Import the Duck albedo and give it time to land in the asset table.
  await d({ cmd: 'import_texture_from_url', id: tex, url: 'http://localhost:9082/glTF-Sample-Assets/Models/Duck/glTF/DuckCM.png' });
  for (let tries = 0; ; tries++) {
    const cs = await q({ query: 'save_census' });
    if ((cs.texture_assets ?? 0) >= 1) break;
    if (tries > 120) throw new Error(`texture import never landed: ${JSON.stringify(cs)}`);
    await new Promise(r => setTimeout(r, 250));
  }
  // Custom material with ONE texture slot (albedo → sRGB decode) + the
  // `textures` include (brings `material_uv` and the generated
  // `material_sample_tex` sampler).
  await d({ cmd: 'add_custom_material', id: cmat });
  await d({ cmd: 'set_custom_material_layout', id: cmat, uniforms: [], textures: [{ name: 'tex', ty: 'texture_2d<f32>', color_kind: 'albedo' }], buffers: [] });
  await d({ cmd: 'set_custom_material_shader_includes', id: cmat, includes: ['textures'] });
  await d({ cmd: 'set_custom_material_fragment_inputs', id: cmat, inputs: ['uv'] });
  const shade = `let uv = material_uv(input, 0u);
let base = material_sample_tex(input.material, uv).rgb;
let n = normalize(input.world_normal);
let l = normalize(vec3<f32>(0.4, 0.8, 0.3));
let diff = max(dot(n, l), 0.0) * 0.75 + 0.4;
return OpaqueShadingOutput(base * diff, 1.0);`;
  await d({ cmd: 'set_custom_material_wgsl', id: cmat, wgsl: shade });
  await d({ cmd: 'set_custom_material_double_sided', id: cmat, double_sided: true });
  await d({ cmd: 'register_material', id: cmat });
  await new Promise(r => setTimeout(r, 1800));
  const diag = await q({ query: 'material_diagnostics', material: cmat });
  if (!diag.ok || !diag.registered) throw new Error('custom material failed to register: ' + JSON.stringify(diag.errors));
  // Upright panel carrying the custom material + the bound texture.
  await d({ cmd: 'insert', id: ID(0x10), spec: { primitive: { box: { dims: [3.0, 3.0, 0.06] } } }, parent: null });
  await d({ cmd: 'set_transform', id: ID(0x10), transform: { translation: [0, 1.9, 0], rotation: [0, 0, 0, 1], scale: [1, 1, 1] } });
  await d({ cmd: 'rename', id: ID(0x10), name: 'textured-panel' });
  await d({ cmd: 'add_material_variant', node: ID(0x10), material: cmat, id: ID(0x30), name: 'custom-tex' });
  await d({ cmd: 'select_material_variant', node: ID(0x10), variant: ID(0x30) });
  // Bind the imported texture to slot `tex` on this instance.
  await d({ cmd: 'set_material_texture', node: ID(0x10), slot: 'tex', texture: tex });
  await d({ cmd: 'set_camera_orbit', yaw: 0.2, pitch: 0.15, radius: 7, look_at: [0, 1.7, 0] });
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false });
  await new Promise(r => setTimeout(r, 400));
  await q({ query: 'wait_render_settled' });
  return 'dynamic-material-textures authored';
}
