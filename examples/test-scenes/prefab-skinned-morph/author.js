// test-scene: prefab-skinned-morph
// Prefab duplication of a SKINNED model: CesiumMan duplicated 2x (3 figures)
// with the walk clip frozen mid-stride. TARGET (post axis-4): all three
// figures walk independently, geometry uploaded once (clones share vertex/
// index/morph buffers; per-instance data = skin matrices + weights only).
//
// ⚠ KNOWN-BROKEN BASELINE (2026-07-10, pre-axis-4) — the golden captures the
// CURRENT defect on purpose:
//   1. Clones do NOT animate — the imported clip's tracks target the
//      ORIGINAL armature's node ids; duplicated joints get fresh ids and no
//      re-targeted clip. Independent per-instance playback needs axis-4's
//      redesign (or clip re-targeting on duplicate).
//   2. Clones render a MANGLED flat bind pose (not even a clean T-pose) —
//      duplicate_skinned_with_new_skin loses/garbles joint local transforms.
//   3. memory_stats meshes 2 -> 4 across two duplicates (fresh mesh entry
//      per clone; the axis-4 offender re-slices + re-uploads geometry).
// Regenerate this golden after axis 4; the scene then locks the fixed
// behavior (three walking figures).
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
  await d({ cmd: 'import_model_from_url', url: 'http://localhost:9082/glTF-Sample-Assets/Models/CesiumMan/glTF/CesiumMan.gltf' });
  let clip = null, root = null;
  for (let i = 0; i < 30; i++) {
    const snap = await q({ query: 'snapshot' });
    if ((snap.animation.clips || []).length) {
      clip = snap.animation.clips[0].id;
      const hit = (snap.scene_tree || []).find(n => n.name === 'Z_UP');
      if (hit) { root = hit.id; break; }
    }
    await new Promise(r => setTimeout(r, 500));
  }
  if (!root) throw new Error('import did not settle');
  await d({ cmd: 'duplicate', id: root });
  await d({ cmd: 'duplicate', id: root });
  await q({ query: 'wait_render_settled' });
  const snap = await q({ query: 'snapshot' });
  const roots = (snap.scene_tree || []).filter(n => n.name === 'Z_UP').map(n => n.id);
  const pos = [[-1.4, 0, 0], [0, 0, 0], [1.4, 0, 0]];
  for (let i = 0; i < roots.length; i++) {
    await d({ cmd: 'set_transform', id: roots[i], transform: { translation: pos[i], rotation: [0, 0, 0, 1], scale: [1, 1, 1] } });
  }
  await d({ cmd: 'set_current_clip', id: clip });
  await d({ cmd: 'set_frame_time', seconds: 0.9 });
  await d({ cmd: 'set_selection', ids: [] });
  await d({ cmd: 'set_camera_orbit', yaw: 0.1, pitch: 0.2, radius: 5.5, look_at: [0, 0.9, 0] });
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false, skeleton_viz: false });
  await q({ query: 'wait_render_settled' });
  return 'prefab-skinned-morph authored (known-broken baseline pre-axis-4)';
}
