// test-scene: anim-blend
// Mixer (NLA) blending: Fox with TWO layers — layer 0 a Walk strip
// (Replace, weight 1), layer 1 a Run strip at weight 0.5 — playhead frozen
// at 0.35s inside both strips. Correct = a gait pose distinct from either
// source clip (a walk/run blend), no T-pose, no popping between frames.
// Mixer shapes: add_layer (no fields) -> add_strip {layer, clip, start,
// len} -> set_layer_weight {layer, weight}; playhead set_playhead {t}.
// The Fox model is ~100 units tall — camera radius is in the hundreds.
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
  await d({ cmd: 'new_project' });
  await d({ cmd: 'import_model_from_url', url: 'http://localhost:9082/glTF-Sample-Assets/Models/Fox/glTF/Fox.gltf' });
  let clips = null;
  for (let i = 0; i < 30; i++) {
    const snap = await q({ query: 'snapshot' });
    if ((snap.animation.clips || []).length >= 3) { clips = snap.animation.clips; break; }
    await new Promise(r => setTimeout(r, 500));
  }
  if (!clips) throw new Error('import did not settle');
  const walk = clips.find(c => c.name === 'Walk').id;
  const run = clips.find(c => c.name === 'Run').id;
  await d({ cmd: 'add_layer' });
  await d({ cmd: 'add_strip', layer: 0, clip: walk, start: 0, len: 2.0 });
  await d({ cmd: 'add_layer' });
  await d({ cmd: 'add_strip', layer: 1, clip: run, start: 0, len: 2.0 });
  await d({ cmd: 'set_layer_weight', layer: 1, weight: 0.5 });
  await d({ cmd: 'set_playhead', t: 0.35 });
  await d({ cmd: 'set_camera_orbit', yaw: 1.2, pitch: 0.3, radius: 380, look_at: [0, 45, 0] });
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false, skeleton_viz: false });
  await q({ query: 'wait_render_settled' });
  return 'anim-blend authored';
}
