// test-scene: taa-cluster (docs/plans/taa-final-round/verification.md §4.1)
// Cluster-LOD ("virtual geometry") coverage for the motion-vector gate: the
// committed lod-nanite bundle carries no .clusters.bin, so the cluster path
// never ran in the player. This scene is DamagedHelmet with its LOD kind set
// to `cluster`; export_player_bundle bakes assets/<node>.clusters.bin, and
// the player (RendererFeatures virtual_geometry + cluster_streaming) draws it
// through the GPU cluster cut. The TAA harness (scenario `lod-*`/`taa-cluster`:
// dolly 1.2× → 12× radius) finds the frames where the cut changes and gates
// vectors across them.
//
// Authored over MCP (the bake is an export-pipeline step):
//   new_project
//   import_model_from_url http://localhost:9082/glTF-Sample-Assets/Models/DamagedHelmet/glTF-Binary/DamagedHelmet.glb
//   rename_node <imported root> "helmet"; set_mesh_lod {node: <helmet>, kind: "cluster"}
//   insert_camera; rename_node "camera"; node_set_transform camera t=[0,0.2,3.5] r=[0,0,0,1]
//   save_project out_dir=<this>/project; export_player_bundle out_dir=<this>/bundle
// The same steps as an editor script:
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
  const before = new Set(((await q({ query: 'snapshot' })).scene_tree || []).map(n => n.id));
  await d({ cmd: 'import_model_from_url', url: 'http://localhost:9082/glTF-Sample-Assets/Models/DamagedHelmet/glTF-Binary/DamagedHelmet.glb' });
  let helmet = null;
  for (let i = 0; i < 60 && !helmet; i++) {
    helmet = ((await q({ query: 'snapshot' })).scene_tree || []).map(n => n.id).find(id => !before.has(id)) || null;
    if (!helmet) await new Promise(r => setTimeout(r, 500));
  }
  if (!helmet) throw new Error('import did not settle');
  await d({ cmd: 'rename', id: helmet, name: 'helmet' });
  return `imported helmet ${helmet}: now set_mesh_lod {kind: cluster} + camera via MCP (see header)`;
}
