// test-scene: volumetrics
// Froxel volumetric scattering: one spot aimed straight down through a slotted
// occluder into a hazy room. Haze mode `volumetric`, density 0.03,
// volumetric_distance 45, scattering_anisotropy 0.3, temporal off.
//
// The occluder is THREE separate bars with gaps, not one slotted slab, because
// the gaps are the test: a correct volume shows lit air ABOVE the bars, a dark
// shaft BELOW each bar, and lit air in the gaps between them — the beam being
// occluded IN THE MEDIUM, not just on the floor. A scene that only shows floor
// stripes proves the shadow map works and says nothing about the volume.
//
// Bloom is off on purpose: a bloomed beam looks convincing whether or not the
// volume is doing anything, which would defeat the point.
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

  const mat = ID(2);
  await d({ cmd: 'add_builtin_material', id: mat, shading: 'pbr' });

  const mk = async (idStr, spec, pos, name, vId) => {
    await d({ cmd: 'insert', id: idStr, spec, parent: null });
    await d({ cmd: 'set_transform', id: idStr, transform: { translation: pos, rotation: [0, 0, 0, 1], scale: [1, 1, 1] } });
    await d({ cmd: 'rename', id: idStr, name });
    await d({ cmd: 'add_material_variant', node: idStr, material: mat, id: vId, name });
    await d({ cmd: 'select_material_variant', node: idStr, variant: vId });
  };

  await mk(ID(1), { primitive: { plane: { width: 40, depth: 40, segments_x: 1, segments_z: 1 } } }, [0, 0, 0], 'floor', ID(0x40));
  await d({ cmd: 'set_builtin_param', node: ID(1), param: 'base_color', value: [0.12, 0.12, 0.14, 1] });

  // Three bars at x = -3 / 0 / +3, each 1 m wide — so the gaps are 2 m of open
  // air. Long in Z so the shafts read as slabs rather than as spots.
  const bars = [-3, 0, 3];
  for (let i = 0; i < bars.length; i++) {
    await mk(ID(0x10 + i), { primitive: { box: { dims: [1, 0.4, 14] } } }, [bars[i], 6, 0], `bar-${i}`, ID(0x50 + i));
  }

  // The spot sits well above the occluder so there is a long stretch of lit air
  // between fixture and bars, and another between bars and floor.
  await d({ cmd: 'insert', id: ID(0x60), spec: { light: 'spot' }, parent: null });
  await d({ cmd: 'rename', id: ID(0x60), name: 'beam' });
  await d({ cmd: 'set_transform', id: ID(0x60), transform: { translation: [0, 14, 0], rotation: [0, 0, 0, 1], scale: [1, 1, 1] } });
  await d({ cmd: 'set_light_param', node: ID(0x60), param: 'color', value: [1, 0.92, 0.75] });
  await d({ cmd: 'set_light_param', node: ID(0x60), param: 'intensity', value: [3000] });
  await d({ cmd: 'set_light_param', node: ID(0x60), param: 'range', value: [40] });
  await d({ cmd: 'set_light_param', node: ID(0x60), param: 'inner_angle', value: [0.3] });
  await d({ cmd: 'set_light_param', node: ID(0x60), param: 'outer_angle', value: [0.5] });
  await d({ cmd: 'look_at', node: ID(0x60), target: [0, 0, 0] });

  await d({
    cmd: 'set_post_process',
    tonemapping: 'khronos_neutral_pbr',
    bloom: false,
    exposure: 0.0,
    atmosphere_mode: 'volumetric',
    atmosphere_color: [0.4, 0.42, 0.5],
    atmosphere_density: 0.03,
    atmosphere_base_height: 30.0,
    atmosphere_height_falloff: 0.0,
    atmosphere_scattering_anisotropy: 0.3,
    atmosphere_volumetric_distance: 45.0,
  });

  // Side-on and slightly above: the beam crosses the frame vertically, so the
  // air between the fixture and the floor is what most of the image is made of.
  await d({ cmd: 'set_camera_orbit', yaw: 1.35, pitch: 0.22, radius: 22, look_at: [0, 4, 0] });
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false });
  await q({ query: 'wait_render_settled' });
  return 'volumetrics authored';
}
