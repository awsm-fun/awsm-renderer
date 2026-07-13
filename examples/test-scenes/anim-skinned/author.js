// test-scene: anim-skinned
// Skinned playback + rig roundtrip: CesiumMan (glTF sample) over a floor,
// its walk clip frozen mid-stride (set_frame_time 0.9s of the ~1.96s clip).
// Correct = a walking pose (no T-pose = clip evaluates; no candy-wrapper
// collapse = skinning correct), textured, casting a shadow. The saved
// project carries rig.glb + bake side files — the skinned persistence path.
//
// Import is ASYNC: poll the snapshot until the model subtree appears.
// set_frame_time takes `seconds`; imported node/clip ids are import-minted
// (not deterministic) — resolve them from the snapshot.
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
  await d({ cmd: 'insert', id: ID(1), spec: { primitive: { plane: { width: 8, depth: 8, segments_x: 1, segments_z: 1 } } }, parent: null });
  await d({ cmd: 'add_material_variant', node: ID(1), material: matFloor, id: ID(0x40), name: 'floor' });
  await d({ cmd: 'select_material_variant', node: ID(1), variant: ID(0x40) });
  await d({ cmd: 'set_builtin_param', node: ID(1), param: 'base_color', value: [0.4, 0.42, 0.45, 1] });
  await d({ cmd: 'import_model_from_url', url: 'http://localhost:9082/glTF-Sample-Assets/Models/CesiumMan/glTF/CesiumMan.gltf' });
  let clip = null;
  for (let i = 0; i < 30; i++) {
    const snap = await q({ query: 'snapshot' });
    if ((snap.animation.clips || []).length) { clip = snap.animation.clips[0].id; break; }
    await new Promise(r => setTimeout(r, 500));
  }
  if (!clip) throw new Error('import did not settle');
  await d({ cmd: 'set_current_clip', id: clip });
  await d({ cmd: 'set_frame_time', seconds: 0.9 });
  await d({ cmd: 'set_camera_orbit', yaw: 0.5, pitch: 0.25, radius: 4, look_at: [0, 0.9, 0] });
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false, skeleton_viz: false });
  await q({ query: 'wait_render_settled' });
  return 'anim-skinned authored';
}
