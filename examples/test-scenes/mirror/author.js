// test-scene: mirror
// PERFECT-MIRROR SSR: a flat silver metallic floor (metallic 1.0,
// roughness 0.0 -> reflection descriptor spread 0 -> the spatially
// deterministic mirror trace, tight resolve AA kernel, temporal
// supersampling) under emissive probes at varied heights — a white sphere,
// a red box, a THIN torus (the thin-geometry acceptance case) and a
// floor-TOUCHING sphere (the contact case). SSR runs FULL-res with bloom
// OFF so the mirror is judged bare. Camera pinned low + grazing so the
// reflections dominate the floor. Correct = each reflection is
// pixel-identical in SHAPE to its geometry: no serration at the contact
// lines (sphere/box/torus meeting their reflections), no stipple/noise on
// the reflection interiors, no dashed gaps through the thin torus ring —
// nothing beyond normal 1 px rasterization aliasing.
//
// NOTE the dispatch shape: set_post_process takes FLAT ssr_* fields
// (ssr_enabled, ssr_intensity, ...) — a nested `ssr: {...}` object is
// silently ignored (the post_process QUERY returns the nested form; the
// command does not accept it). `bloom` is the plain bloom toggle.
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
  // Deterministic ids so re-runs produce an identical project.toml.
  const ID = (n) => `00000000-0000-4000-8000-0000000000${n.toString(16).padStart(2, '0')}`;
  await d({ cmd: 'new_project' });
  // Dim the seeded key directional light (default intensity 4): the emissive
  // probes should dominate; the directional only keeps the floor plane read
  // as a surface rather than a void.
  const snap = await q({ query: 'snapshot' });
  const seeded = (snap.scene_tree || []).find(n => n.kind === 'light');
  if (seeded) await d({ cmd: 'set_light_param', node: seeded.id, param: 'intensity', value: [1.0] });
  const matMirror = ID(2), matProbe = ID(3);
  for (const m of [matMirror, matProbe]) await d({ cmd: 'add_builtin_material', id: m, shading: 'pbr' });
  const mk = async (idStr, spec, pos, mat, name, vId) => {
    await d({ cmd: 'insert', id: idStr, spec, parent: null });
    await d({ cmd: 'set_transform', id: idStr, transform: { translation: pos, rotation: [0, 0, 0, 1], scale: [1, 1, 1] } });
    await d({ cmd: 'rename', id: idStr, name });
    await d({ cmd: 'add_material_variant', node: idStr, material: mat, id: vId, name });
    await d({ cmd: 'select_material_variant', node: idStr, variant: vId });
  };
  // The PERFECT MIRROR floor: metallic 1.0 + roughness 0.0 writes descriptor
  // spread 0 (roughness -> spread) and F0 = base color. SILVER base — a
  // metal's reflection is tinted by its base color, so a near-black base is
  // (physically correctly!) a black mirror that reflects nothing. A real
  // mirror is metallic + near-white: the floor must show the SKY between
  // object reflections, exactly like glass.
  await mk(ID(1), { primitive: { plane: { width: 400, depth: 400, segments_x: 1, segments_z: 1 } } }, [0, 0, 0], matMirror, 'mirror-floor', ID(0x40));
  await d({ cmd: 'set_builtin_param', node: ID(1), param: 'base_color', value: [0.95, 0.96, 0.97, 1] });
  await d({ cmd: 'set_builtin_param', node: ID(1), param: 'metallic', value: [1.0] });
  await d({ cmd: 'set_builtin_param', node: ID(1), param: 'roughness', value: [0.0] });
  // Three emissive probes at varied heights. Near-black base + strong
  // emissive = pure self-lit shapes whose reflections have crisp silhouettes.
  const probes = [
    // White sphere, floating: contact-free curved silhouette.
    { id: ID(0x10), v: ID(0x20), name: 'sphere-white', spec: { primitive: { sphere: { radius: 0.8, segments_long: 48, segments_lat: 32 } } }, pos: [-2.4, 1.5, -1.2], emissive: [5, 5, 5] },
    // Red box, resting ON the floor: the contact-line case (reflection must
    // meet the geometry with no dark serrated teeth).
    { id: ID(0x11), v: ID(0x21), name: 'box-red', spec: { primitive: { box: { dims: [1.0, 1.8, 1.0] } } }, pos: [0.4, 0.9, -2.2], emissive: [5, 0.15, 0.15] },
    // THIN torus, high: the thin-geometry acceptance case (its reflection
    // must be a continuous ring, not dashes).
    { id: ID(0x12), v: ID(0x22), name: 'torus-thin', spec: { primitive: { torus: { radius: 1.0, thickness: 0.06, segments_major: 64, segments_minor: 16 } } }, pos: [2.6, 2.0, -1.0], emissive: [0.4, 2.2, 5] },
    // TOUCHING sphere: the curved-CONTACT case — reflected rays run nearly
    // parallel to the mirror at the contact, magnifying depth-texel
    // quantization into vertical streaks unless the resolve widens there
    // (the tangency channel). The reflection under the contact must be a
    // smooth continuation, no streaks/teeth.
    { id: ID(0x13), v: ID(0x23), name: 'sphere-touching', spec: { primitive: { sphere: { radius: 0.8, segments_long: 48, segments_lat: 32 } } }, pos: [-5.2, 0.8, -1.2], emissive: [4, 3.2, 1.2] },
  ];
  for (const p of probes) {
    await mk(p.id, p.spec, p.pos, matProbe, p.name, p.v);
    await d({ cmd: 'set_builtin_param', node: p.id, param: 'base_color', value: [0.02, 0.02, 0.02, 1] });
    await d({ cmd: 'set_builtin_param', node: p.id, param: 'emissive', value: p.emissive });
  }
  // FULL-res SSR, temporal ON, bloom OFF. Temporal is part of the mirror's
  // CORRECTNESS, not a mask: the trace cycles the mirror march phase over 8
  // 16 frames (decorrelated lateral + depth-phase sub-texel dither) and the
  // accumulator converges magnified quantization at grazing curved
  // silhouettes (sphere apex/contact rows) into true coverage — the same way
  // TAA antialiases geometry edges. TIGHT thickness (5mm): the 3cm default
  // let grazing rays accept on objects' antialiased RIM texels (source-buffer
  // contamination), hanging dark "eyelash" strokes off every reflected
  // silhouette; the adaptive step-advance acceptance supplies the rest.
  await d({ cmd: 'set_post_process', bloom: false, ssr_enabled: true, ssr_intensity: 1.0, ssr_max_distance: 120.0, ssr_thickness: 0.005, ssr_max_steps: 128, ssr_spread_cutoff: 0.6, ssr_edge_fade: 0.1, ssr_resolution_scale: 1.0, ssr_temporal: true, ssr_temporal_weight: 0.94 });
  // Low + grazing camera: Fresnel at grazing pushes the metallic mirror to
  // full reflectance, so the reflections dominate the frame.
  await d({ cmd: 'set_camera_orbit', yaw: 0.1, pitch: 0.12, radius: 12, look_at: [0, 1.0, -1.2] });
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false });
  await q({ query: 'wait_render_settled' });
  return 'mirror authored';
}
