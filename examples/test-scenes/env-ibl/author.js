// test-scene: env-ibl
// The 3-slot environment (skybox / specular / irradiance) on KTX2 assets:
// photo_studio cubemaps imported via import_ktx_env_from_url and bound
// with patch_environment {slot: {ktx: {asset_id}}}. Probe trio: mirror
// metal (reflects the studio interior), rough dielectric (irradiance),
// glossy blue dielectric (sharp env highlights). Correct = all slots read
// back kind:"ktx" in snapshot.project.environment and reflections/ambient
// visibly track the studio environment.
// SLOT INDEPENDENCE (verified during authoring): swapping ONLY the skybox
// changed the background but left reflections/lighting untouched
// (A 234,858 B -> B 235,514 B); swapping specular+irradiance re-lit the
// probes (C 311,458 B). patch_environment is PARTIAL — omitted slots keep
// their bindings. Slot values: "built_in_default" | {ktx:{asset_id}} |
// sky_gradient.
// SIZE NOTE: the versioned scene binds env.ktx2 (8 MB, prefiltered) to
// BOTH skybox and specular — photo_studio's dedicated skybox.ktx2 is an
// uncompressed 128 MB cube (over GitHub's 100 MB file limit). Slot
// independence is still exercised (three slots, two distinct assets).
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
  await d({ cmd: 'add_builtin_material', id: ID(2), shading: 'pbr' });
  await d({ cmd: 'insert', id: ID(1), spec: { primitive: { plane: { width: 12, depth: 12, segments_x: 1, segments_z: 1 } } }, parent: null });
  await d({ cmd: 'add_material_variant', node: ID(1), material: ID(2), id: ID(0x40), name: 'floor' });
  await d({ cmd: 'select_material_variant', node: ID(1), variant: ID(0x40) });
  await d({ cmd: 'set_builtin_param', node: ID(1), param: 'base_color', value: [0.35, 0.36, 0.38, 1] });
  const cfgs = [['mirror', [0.95, 0.95, 0.95, 1], 1.0, 0.03], ['rough', [0.6, 0.3, 0.2, 1], 0.0, 0.85], ['glossy-dielectric', [0.1, 0.15, 0.5, 1], 0.0, 0.12]];
  for (let i = 0; i < 3; i++) {
    const [name, base, metal, rough] = cfgs[i];
    const n = ID(0x10 + i), v = ID(0x20 + i);
    await d({ cmd: 'insert', id: n, spec: { primitive: { sphere: { radius: 0.9, segments_long: 32, segments_lat: 24 } } }, parent: null });
    await d({ cmd: 'set_transform', id: n, transform: { translation: [0, 1.0, (i - 1) * 2.4], rotation: [0, 0, 0, 1], scale: [1, 1, 1] } });
    await d({ cmd: 'rename', id: n, name });
    await d({ cmd: 'add_material_variant', node: n, material: ID(2), id: v, name });
    await d({ cmd: 'select_material_variant', node: n, variant: v });
    await d({ cmd: 'set_builtin_param', node: n, param: 'base_color', value: base });
    await d({ cmd: 'set_builtin_param', node: n, param: 'metallic', value: [metal] });
    await d({ cmd: 'set_builtin_param', node: n, param: 'roughness', value: [rough] });
  }
  await d({ cmd: 'import_ktx_env_from_url', id: ID(0xe1), url: 'http://localhost:9083/photo_studio/env.ktx2' });
  await d({ cmd: 'import_ktx_env_from_url', id: ID(0xe2), url: 'http://localhost:9083/photo_studio/irradiance.ktx2' });
  await new Promise(r => setTimeout(r, 2000));
  await d({ cmd: 'patch_environment', skybox: { ktx: { asset_id: ID(0xe1) } }, specular: { ktx: { asset_id: ID(0xe1) } }, irradiance: { ktx: { asset_id: ID(0xe2) } } });
  await d({ cmd: 'set_camera_orbit', yaw: 0.5, pitch: 0.25, radius: 9, look_at: [0, 1, 0] });
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false });
  await new Promise(r => setTimeout(r, 1500));
  await q({ query: 'wait_render_settled' });
  return 'env-ibl authored';
}
