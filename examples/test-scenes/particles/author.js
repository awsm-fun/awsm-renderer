// test-scene: particles
// The particle emitter (the existing GPU-instancing path): an upward cone
// fountain over a dark floor — 120/s spawn, gravity pulls arcs down,
// color-over-life orange -> transparent red, size-over-life shrink, blend on.
// Correct = a live fountain of warm-tinted blended sprites arcing outward;
// emission is STOCHASTIC so the golden is a representative frame, not a
// pixel lock (player-tests assert emitter behavior programmatically).
//
// Dispatch shape gotchas: set_particle_emitter takes FLAT config fields
// ({node, spawn_rate, ...} — a nested `emitter:{}` object is silently
// ignored); the gravity force field is `acceleration`, not `accel`.
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
  const matFloor = ID(2);
  await d({ cmd: 'add_builtin_material', id: matFloor, shading: 'pbr' });
  await d({ cmd: 'insert', id: ID(1), spec: { primitive: { plane: { width: 10, depth: 10, segments_x: 1, segments_z: 1 } } }, parent: null });
  await d({ cmd: 'add_material_variant', node: ID(1), material: matFloor, id: ID(0x40), name: 'floor' });
  await d({ cmd: 'select_material_variant', node: ID(1), variant: ID(0x40) });
  await d({ cmd: 'set_builtin_param', node: ID(1), param: 'base_color', value: [0.15, 0.15, 0.18, 1] });
  await d({ cmd: 'insert', id: ID(0x50), spec: { particle: {} }, parent: null });
  await d({ cmd: 'rename', id: ID(0x50), name: 'fountain' });
  await d({ cmd: 'set_particle_emitter', node: ID(0x50), spawn_rate: 120, max_alive: 512, lifetime: [0.8, 1.6], initial_speed: [3, 4.5], size: [0.08, 0.16], color_over_life: { linear: { start: [1, 0.6, 0.15, 1], end: [1, 0.1, 0.05, 0] } }, size_over_life: { linear: { start: 1, end: 0.4 } }, forces: [{ gravity: { acceleration: [0, -4, 0] } }], blend: true });
  await d({ cmd: 'set_camera_orbit', yaw: 0.5, pitch: 0.3, radius: 9, look_at: [0, 1.4, 0] });
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false });
  await new Promise(r => setTimeout(r, 1800)); // let the fountain reach steady state
  await q({ query: 'wait_render_settled' });
  return 'particles authored';
}
