// test-scene: anim-morph
// Multi-track morph blending (the 005 §3 lock, on-device): AnimatedMorphCube
// (2 morph targets: thin / angle) with TWO editor tracks on ONE mesh, each
// driving a DIFFERENT morph index — index 0 ramps 0->1 while index 1 ramps
// 1->0 over 2s. At playhead t=1.0 `morph_data` must read weights [0.5, 0.5]:
// each track only writes ITS index (pre-005§3, the whole-vector blend meant
// the last track's padding zeros stomped the other index). Correct visual =
// the cube deformed by BOTH morphs at half strength (a wedge), and the
// weights query reading [0.5, 0.5].
//
// Track target shape: {target:"morph", node, index}; keyframes
// {clip, track:<usize>, t, value:{kind:"scalar", value}}; playhead via
// set_playhead {t} (set_frame_time {seconds} freezes global time instead).
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
  await d({ cmd: 'import_model_from_url', url: 'http://localhost:9082/glTF-Sample-Assets/Models/AnimatedMorphCube/glTF/AnimatedMorphCube.gltf' });
  let node = null;
  for (let i = 0; i < 30; i++) {
    const snap = await q({ query: 'snapshot' });
    const walk = (ns, out = []) => { for (const n of ns) { out.push(n); if (n.children) walk(n.children, out); } return out; };
    const hit = walk(snap.scene_tree || []).find(n => n.kind === 'skinned_mesh');
    if (hit) { node = hit.id; break; }
    await new Promise(r => setTimeout(r, 500));
  }
  if (!node) throw new Error('import did not settle');
  const clip = ID(0xa0);
  await d({ cmd: 'add_clip', id: clip, name: 'two-morphs' });
  await d({ cmd: 'add_track', clip, target: { target: 'morph', node, index: 0 } });
  await d({ cmd: 'add_track', clip, target: { target: 'morph', node, index: 1 } });
  await d({ cmd: 'add_keyframe', clip, track: 0, t: 0, value: { kind: 'scalar', value: 0 } });
  await d({ cmd: 'add_keyframe', clip, track: 0, t: 2, value: { kind: 'scalar', value: 1 } });
  await d({ cmd: 'add_keyframe', clip, track: 1, t: 0, value: { kind: 'scalar', value: 1 } });
  await d({ cmd: 'add_keyframe', clip, track: 1, t: 2, value: { kind: 'scalar', value: 0 } });
  await d({ cmd: 'set_current_clip', id: clip });
  await d({ cmd: 'set_playhead', t: 1.0 });
  await d({ cmd: 'set_camera_orbit', yaw: 0.7, pitch: 0.4, radius: 0.12, look_at: [0, 0, 0] });
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false, skeleton_viz: false });
  await q({ query: 'wait_render_settled' });
  const w = (await q({ query: 'morph_data', nodes: [node] })).entries[node].weights;
  if (Math.abs(w[0] - 0.5) > 0.01 || Math.abs(w[1] - 0.5) > 0.01) {
    throw new Error(`per-index morph compose broken: weights ${JSON.stringify(w)} (expected [0.5, 0.5])`);
  }
  return 'anim-morph authored; weights ' + JSON.stringify(w);
}
