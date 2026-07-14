// test-scene: dynamic-materials
// Custom-WGSL (dynamic) material exercising BOTH per-node override kinds on ONE
// shared material assigned to two spheres:
//   • per-node UNIFORM override — `tint: vec3<f32>`; the left sphere reads the
//     SHARED default (cool, set_material_uniform), the right carries a PER-INSTANCE
//     override (warm, set_node_material_uniform).
//   • per-node TEXTURE override — a `tex` slot sampled in the fragment; the left
//     sphere binds the Cesium logo, the right binds the Duck albedo, each via
//     set_material_texture {node, slot, texture} (which writes the instance's
//     texture_overrides map — inherently per-node).
// Correct = both spheres shaded by the same custom lambert fragment yet visibly
// DIVERGENT in both texture AND tint; live uniform edits re-shade without
// recompiling.
//
// Probed shapes worth keeping:
// - set_custom_material_layout {id, uniforms:[{name, ty}], textures:[{name, ty,
//   color_kind}], buffers:[]} — `ty` is a WGSL TYPE STRING ("vec3<f32>",
//   "texture_2d<f32>", ...); a friendly name is accepted silently and falls
//   back to f32 (register then fails with confusing naga compose errors).
// - register_material returns ok immediately; compile status arrives via the
//   `material_diagnostics` query {material} — ALWAYS check {ok:true,
//   registered:true} after registering.
// - set_material_uniform {material, name, value} accepts the TEXT form
//   ("0.2, 0.4, 0.9"); set_node_material_uniform {node, name, value} requires
//   the tagged form {kind:"vec3", value:[..]}.
// - set_material_texture {node, slot, texture} is PER-NODE (writes the
//   instance's texture_overrides); the `textures` include brings `material_uv`
//   + the generated `material_sample_tex` sampler.
// - Fragment body is wrapped as
//   fn custom_shade_dynamic(input: OpaqueShadingInput) -> OpaqueShadingOutput
//   and must end with `return OpaqueShadingOutput(vec3 color, 1.0);`.
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
  const cmat = ID(5), matFloor = ID(2), texA = ID(0xf0), texB = ID(0xf1);
  await d({ cmd: 'add_builtin_material', id: matFloor, shading: 'pbr' });
  // Two distinct textures for the per-node texture override.
  await d({ cmd: 'import_texture_from_url', id: texA, url: 'http://localhost:9082/glTF-Sample-Assets/Models/BoxTextured/glTF/CesiumLogoFlat.png' });
  await d({ cmd: 'import_texture_from_url', id: texB, url: 'http://localhost:9082/glTF-Sample-Assets/Models/Duck/glTF/DuckCM.png' });
  for (let t = 0; ; t++) { const cs = await q({ query: 'save_census' }); if ((cs.texture_assets ?? 0) >= 2) break; if (t > 120) throw new Error('texture imports never landed'); await new Promise(r => setTimeout(r, 250)); }
  await d({ cmd: 'add_custom_material', id: cmat });
  await d({ cmd: 'set_custom_material_layout', id: cmat, uniforms: [{ name: 'tint', ty: 'vec3<f32>' }], textures: [{ name: 'tex', ty: 'texture_2d<f32>', color_kind: 'albedo' }], buffers: [] });
  await d({ cmd: 'set_custom_material_shader_includes', id: cmat, includes: ['textures'] });
  await d({ cmd: 'set_custom_material_fragment_inputs', id: cmat, inputs: ['normals', 'uv'] });
  const wgsl = `let uv = material_uv(input, 0u);
let base = material_sample_tex(input.material, uv).rgb;
let n = normalize(input.world_normal);
let l = normalize(vec3<f32>(0.4, 0.8, 0.3));
let diff = max(dot(n, l), 0.0) * 0.9 + 0.2;
return OpaqueShadingOutput(base * input.material.tint * diff, 1.0);`;
  await d({ cmd: 'set_custom_material_wgsl', id: cmat, wgsl });
  await d({ cmd: 'register_material', id: cmat });
  await new Promise(r => setTimeout(r, 1600));
  const diag = await q({ query: 'material_diagnostics', material: cmat });
  if (!diag.ok || !diag.registered) throw new Error('custom material failed to register: ' + JSON.stringify(diag.errors));
  await d({ cmd: 'insert', id: ID(1), spec: { primitive: { plane: { width: 10, depth: 10, segments_x: 1, segments_z: 1 } } }, parent: null });
  await d({ cmd: 'add_material_variant', node: ID(1), material: matFloor, id: ID(0x40), name: 'floor' });
  await d({ cmd: 'select_material_variant', node: ID(1), variant: ID(0x40) });
  await d({ cmd: 'set_builtin_param', node: ID(1), param: 'base_color', value: [0.4, 0.42, 0.45, 1] });
  for (let i = 0; i < 2; i++) {
    const n = ID(0x10 + i), v = ID(0x20 + i);
    await d({ cmd: 'insert', id: n, spec: { primitive: { sphere: { radius: 1.0, segments_long: 32, segments_lat: 24 } } }, parent: null });
    await d({ cmd: 'set_transform', id: n, transform: { translation: [i === 0 ? -1.4 : 1.4, 1.1, 0], rotation: [0, 0, 0, 1], scale: [1, 1, 1] } });
    await d({ cmd: 'rename', id: n, name: i === 0 ? 'shared-tex' : 'override-tex' });
    await d({ cmd: 'add_material_variant', node: n, material: cmat, id: v, name: 'custom' });
    await d({ cmd: 'select_material_variant', node: n, variant: v });
  }
  // Shared uniform (cool, light so the texture reads) + a per-node override
  // (warm) on the right sphere.
  await d({ cmd: 'set_material_uniform', material: cmat, name: 'tint', value: '0.55, 0.7, 1.0' });
  await d({ cmd: 'set_node_material_uniform', node: ID(0x11), name: 'tint', value: { kind: 'vec3', value: [1.0, 0.75, 0.5] } });
  // Per-node TEXTURE override: left = Cesium logo, right = Duck albedo.
  await d({ cmd: 'set_material_texture', node: ID(0x10), slot: 'tex', texture: texA });
  await d({ cmd: 'set_material_texture', node: ID(0x11), slot: 'tex', texture: texB });
  await d({ cmd: 'set_selection', ids: [] });
  await d({ cmd: 'set_camera_orbit', yaw: 0.22, pitch: 0.3, radius: 10.5, look_at: [0, 1.0, 0] });
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false });
  await new Promise(r => setTimeout(r, 500));
  await q({ query: 'wait_render_settled' });
  return 'dynamic-materials authored';
}
