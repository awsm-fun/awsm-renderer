// test-scene: pbr-extensions
// One sphere per KHR material extension vs a plain-PBR reference, each on
// its OWN builtin material asset configured via update_builtin_material
// {id, def: <full MaterialDef>} (extensions live in def.extensions; field
// shapes are the scene crate's ext_struct definitions — factor/tex per
// extension). Correct = every sphere visually distinct from the reference:
// transmission glassy, volume tinted absorption, clearcoat lacquer double
// highlight, sheen rim fuzz, iridescence thin-film rainbow, anisotropy
// stretched highlight, specular tinted F0, dispersion prismatic edges,
// diffuse-transmission warm bleed, emissive_strength glow, high-IOR glass.
// Each extension composes its own shader bucket (status bar: 13 buckets =
// specialize-only pipeline per feature-set).
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
  const noExt = { emissive_strength: null, ior: null, specular: null, transmission: null, diffuse_transmission: null, volume: null, clearcoat: null, sheen: null, dispersion: null, anisotropy: null, iridescence: null };
  const defBase = (over) => Object.assign({
    label: '', shading: 'pbr', alpha_mode: 'opaque', double_sided: false,
    base_color: [0.6, 0.62, 0.65, 1], base_color_texture: null,
    metallic: 0.0, roughness: 0.25, metallic_roughness_texture: null,
    normal_scale: 1, normal_texture: null, occlusion_strength: 1, occlusion_texture: null,
    emissive: [0, 0, 0], emissive_texture: null, vertex_colors_enabled: false,
    extensions: noExt,
  }, over);
  const ext = (o) => Object.assign({}, noExt, o);
  const exts = [
    ['reference',    defBase({})],
    ['transmission', defBase({ base_color: [0.9, 0.9, 0.95, 1], roughness: 0.05, extensions: ext({ transmission: { factor: 1.0, tex: null }, ior: { ior: 1.5 } }) })],
    ['volume',       defBase({ base_color: [0.4, 0.8, 0.5, 1], roughness: 0.05, extensions: ext({ transmission: { factor: 1.0, tex: null }, ior: { ior: 1.5 }, volume: { thickness_factor: 1.0, attenuation_distance: 0.6, attenuation_color: [0.2, 0.9, 0.3], thickness_tex: null } }) })],
    ['clearcoat',    defBase({ base_color: [0.55, 0.05, 0.05, 1], roughness: 0.6, extensions: ext({ clearcoat: { factor: 1.0, roughness_factor: 0.05, normal_scale: 1, tex: null, roughness_tex: null, normal_tex: null } }) })],
    ['sheen',        defBase({ base_color: [0.25, 0.1, 0.4, 1], roughness: 0.9, extensions: ext({ sheen: { roughness_factor: 0.35, color_factor: [0.9, 0.7, 1.0], color_tex: null, roughness_tex: null } }) })],
    ['iridescence',  defBase({ base_color: [0.2, 0.2, 0.25, 1], metallic: 1.0, roughness: 0.15, extensions: ext({ iridescence: { factor: 1.0, ior: 1.3, thickness_min: 100, thickness_max: 400, tex: null, thickness_tex: null } }) })],
    ['anisotropy',   defBase({ base_color: [0.8, 0.75, 0.6, 1], metallic: 1.0, roughness: 0.35, extensions: ext({ anisotropy: { strength: 1.0, rotation: 0, tex: null } }) })],
    ['specular',     defBase({ base_color: [0.1, 0.1, 0.5, 1], roughness: 0.15, extensions: ext({ specular: { factor: 1.0, color_factor: [1.0, 0.3, 0.1], tex: null, color_tex: null } }) })],
    ['dispersion',   defBase({ base_color: [0.95, 0.95, 0.98, 1], roughness: 0.02, extensions: ext({ transmission: { factor: 1.0, tex: null }, ior: { ior: 1.7 }, dispersion: { dispersion: 0.3 } }) })],
    ['diffuse-trans',defBase({ base_color: [0.9, 0.8, 0.6, 1], roughness: 0.8, extensions: ext({ diffuse_transmission: { factor: 1.0, color_factor: [1.0, 0.8, 0.5], tex: null, color_tex: null } }) })],
    ['emissive-str', defBase({ base_color: [0.1, 0.1, 0.1, 1], emissive: [0.8, 0.3, 0.1], extensions: ext({ emissive_strength: { strength: 5.0 } }) })],
    ['ior-only',     defBase({ base_color: [0.9, 0.9, 0.95, 1], roughness: 0.05, extensions: ext({ transmission: { factor: 1.0, tex: null }, ior: { ior: 2.4 } }) })],
  ];
  await d({ cmd: 'add_builtin_material', id: ID(2), shading: 'pbr' });
  await d({ cmd: 'insert', id: ID(1), spec: { primitive: { plane: { width: 16, depth: 16, segments_x: 1, segments_z: 1 } } }, parent: null });
  await d({ cmd: 'add_material_variant', node: ID(1), material: ID(2), id: ID(0x40), name: 'floor' });
  await d({ cmd: 'select_material_variant', node: ID(1), variant: ID(0x40) });
  await d({ cmd: 'set_builtin_param', node: ID(1), param: 'base_color', value: [0.45, 0.47, 0.5, 1] });
  for (let i = 0; i < exts.length; i++) {
    const [name, def] = exts[i];
    const mat = ID(0x50 + i), node = ID(0x10 + i), variant = ID(0x70 + i);
    await d({ cmd: 'add_builtin_material', id: mat, shading: 'pbr' });
    await d({ cmd: 'update_builtin_material', id: mat, def });
    const col = i % 3, row = Math.floor(i / 3);
    await d({ cmd: 'insert', id: node, spec: { primitive: { sphere: { radius: 0.8, segments_long: 32, segments_lat: 24 } } }, parent: null });
    await d({ cmd: 'set_transform', id: node, transform: { translation: [(col - 1) * 2.3, 0.9, (row - 1.5) * 2.3], rotation: [0, 0, 0, 1], scale: [1, 1, 1] } });
    await d({ cmd: 'rename', id: node, name });
    await d({ cmd: 'add_material_variant', node, material: mat, id: variant, name });
    await d({ cmd: 'select_material_variant', node, variant });
  }
  await d({ cmd: 'set_camera_orbit', yaw: 0.0, pitch: 0.8, radius: 14.5, look_at: [0, 0.2, 0] });
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false });
  await new Promise(r => setTimeout(r, 4000)); // extension buckets compile
  await q({ query: 'wait_render_settled' });
  return 'pbr-extensions authored';
}
