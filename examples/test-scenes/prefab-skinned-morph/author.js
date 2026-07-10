// test-scene: prefab-skinned-morph
// Prefab duplication of a SKINNED model: CesiumMan duplicated 2x (3 figures)
// with the walk clip frozen mid-stride. Locks the axis-4 behavior:
//   1. Duplicating retargets the clip — tracks targeting nodes inside the
//      duplicated subtree are extended onto the cloned bones — so ALL THREE
//      figures pose mid-stride at the frozen playhead (identical stances,
//      spread on x).
//   2. Clones bind at the source's exact rest pose (same IBMs via the shared
//      skin data; cloned bones carry the authored locals).
//   3. Geometry uploads ONCE: memory_stats `meshes` grows per duplicate
//      (instance records) but `mesh_resources` / `mesh_geometry_bytes` stay
//      FLAT — clones refcount the source's resource; per-instance GPU data is
//      the skin joint-matrix palette (+ morph weights for morphed rigs).
// Golden: three identical mid-stride walkers side by side. (Pre-axis-4 the
// clones rendered a frozen bind pose and each duplicate re-uploaded the full
// geometry — meshes 2 -> 4 with resources growing in lockstep.)
// The scene ALSO carries a 4th duplicate marked prefab=true + visible=false:
// invisible in both editor golden and player render, it exists so the player
// bundle exposes a SKINNED PrefabTemplate for player-tests'
// prefab-churn-skinned check (per-instance cloned-skeleton lifecycle).
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
  // A FOURTH duplicate marked prefab=true: the player loader materializes it as
  // a HIDDEN PrefabTemplate (not rendered — golden stays 3 walkers). It exists
  // for player-tests' prefab-churn-skinned check, which instantiates/tears it
  // down xN and asserts the per-instance cloned skeleton joints don't leak.
  await d({ cmd: 'duplicate', id: root });
  const snap2 = await q({ query: 'snapshot' });
  const templ = (snap2.scene_tree || []).map(n => n.id).find(id => !roots.includes(id) && (snap2.scene_tree || []).find(n => n.id === id).name === 'Z_UP');
  if (!templ) throw new Error('4th duplicate not found');
  await d({ cmd: 'set_prefab', id: templ, prefab: true });
  // Hidden in the EDITOR golden too (the editor renders prefab sources; the
  // player loader captures the template before visibility gating).
  await d({ cmd: 'set_visible', id: templ, visible: false });
  const pos = [[-1.4, 0, 0], [0, 0, 0], [1.4, 0, 0]];
  // Spread on x while PRESERVING each root's authored rotation/scale (Z_UP
  // carries the Z-up -> Y-up rotation; clobbering it lays the figures flat).
  const trs = await q({ query: 'node_transforms', nodes: roots });
  for (let i = 0; i < roots.length; i++) {
    const t = (trs.entries || {})[roots[i]] || { rotation: [0, 0, 0, 1], scale: [1, 1, 1] };
    await d({ cmd: 'set_transform', id: roots[i], transform: { translation: pos[i], rotation: t.rotation, scale: t.scale } });
  }
  await d({ cmd: 'set_current_clip', id: clip });
  await d({ cmd: 'set_frame_time', seconds: 0.9 });
  await d({ cmd: 'set_selection', ids: [] });
  await d({ cmd: 'set_camera_orbit', yaw: 0.1, pitch: 0.2, radius: 5.5, look_at: [0, 0.9, 0] });
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false, skeleton_viz: false });
  await q({ query: 'wait_render_settled' });
  return 'prefab-skinned-morph authored (axis-4: retargeted clip + shared geometry)';
}
