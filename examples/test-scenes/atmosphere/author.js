// test-scene: atmosphere
// Atmospheric haze (post_process.atmosphere): a receding row of SIX IDENTICAL
// white pillars over a dark floor, plus one tall mast spanning the haze layer,
// under a sky-gradient environment. Haze on: color [0.35,0.42,0.55], density
// 0.06, base_height 2.0, height_falloff 0.25.
//
// Everything in the row is the same box with the same material, so any
// difference between the near and far pillar IS the haze — that's the point of
// the arrangement. Correct = aerial perspective down the row (near crisp, far
// nearly dissolved into the haze colour), a vertical gradient up the mast
// (hazed at the base where the medium is full, clearing toward the top where
// height_falloff has thinned it), and a sky that blends toward the haze colour
// at the horizon while staying open overhead.
//
// Bloom is deliberately OFF: this scene should fail for haze reasons only.
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

  const matFloor = ID(2), matPillar = ID(3);
  for (const m of [matFloor, matPillar]) await d({ cmd: 'add_builtin_material', id: m, shading: 'pbr' });

  const mk = async (idStr, spec, pos, mat, name, vId) => {
    await d({ cmd: 'insert', id: idStr, spec, parent: null });
    await d({ cmd: 'set_transform', id: idStr, transform: { translation: pos, rotation: [0, 0, 0, 1], scale: [1, 1, 1] } });
    await d({ cmd: 'rename', id: idStr, name });
    await d({ cmd: 'add_material_variant', node: idStr, material: mat, id: vId, name });
    await d({ cmd: 'select_material_variant', node: idStr, variant: vId });
  };

  // Floor — big enough that it runs past the far pillar, so the ground itself
  // shows the depth ramp too.
  await mk(ID(1), { primitive: { plane: { width: 120, depth: 120, segments_x: 1, segments_z: 1 } } }, [0, 0, -40], matFloor, 'floor', ID(0x40));
  await d({ cmd: 'set_builtin_param', node: ID(1), param: 'base_color', value: [0.10, 0.11, 0.13, 1] });
  await d({ cmd: 'set_builtin_param', node: ID(1), param: 'roughness', value: [0.9] });

  // Six identical pillars receding down -Z. Identical geometry + material is
  // the whole control: the only variable is distance.
  for (let i = 0; i < 6; i++) {
    const n = ID(0x10 + i), v = ID(0x20 + i);
    await mk(
      n,
      { primitive: { box: { dims: [1.6, 3.2, 1.6] } } },
      [3.0, 1.6, -6 - i * 9],
      matPillar,
      `pillar-${i}`,
      v,
    );
  }
  await d({ cmd: 'set_builtin_param', node: ID(0x10), param: 'base_color', value: [0.85, 0.85, 0.86, 1] });
  await d({ cmd: 'set_builtin_param', node: ID(0x10), param: 'roughness', value: [0.55] });

  // The mast — one tall box crossing base_height (2.0). Its base sits in the
  // full-density layer and its top well above it, so a correct height integral
  // reads as a vertical gradient on a single untextured object.
  await mk(ID(0x30), { primitive: { box: { dims: [0.9, 18, 0.9] } } }, [-6, 9, -20], matPillar, 'mast', ID(0x41));

  // Lighting is `new_project`'s seeded directional key — deliberately left
  // alone. The pillars are identical and identically lit; distance is the only
  // variable, so any difference down the row is the haze.

  // A dusk gradient, so the horizon has something for the haze to blend into.
  await d({ cmd: 'set_environment', zenith: [0.06, 0.09, 0.16], nadir: [0.20, 0.19, 0.18] });

  await d({
    cmd: 'set_post_process',
    tonemapping: 'khronos_neutral_pbr',
    bloom: false,
    exposure: 0.0,
    atmosphere_mode: 'fog',
    atmosphere_color: [0.35, 0.42, 0.55],
    atmosphere_density: 0.06,
    atmosphere_base_height: 2.0,
    atmosphere_height_falloff: 0.25,
  });

  // Low and slightly off-axis: the row recedes across the frame rather than
  // stacking into a single silhouette, and the camera sits inside the haze
  // layer (y ≈ 3, base_height 2) so the near ground is already tinted.
  await d({ cmd: 'set_camera_orbit', yaw: 0.35, pitch: 0.12, radius: 22, look_at: [0, 3, -22] });
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false });
  await q({ query: 'wait_render_settled' });
  return 'atmosphere authored';
}
