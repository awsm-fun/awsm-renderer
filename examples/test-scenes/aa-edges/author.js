// test-scene: aa-edges
// A single dark box rotated 45° about Y, framed tight against the light sky so
// its diagonal silhouette EDGES are the whole subject — the high-contrast-edge
// target for the MSAA + SMAA view-toggle recipes. This scene is Layer-A ONLY:
// MSAA (viewport 4x, STRUCTURAL) and SMAA (post-process) are editor
// SetViewOptions view toggles, not player-bundle state — so there is no
// bundle/ and no player-tests coverage; the whole point is to flip the toggle
// and confirm the SAME framed edge changes (jaggy -> smooth). Flat-shaded on
// purpose so nothing but the silhouette AA moves (see memory
// aa-verify-in-model-viewer: shading detail would distract from the edge).
//
// Run inside the editor page (http://localhost:9085 attached to the dev MCP):
// evaluate this file's function, then screenshot with msaa/smaa off vs on.
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
  await d({ cmd: 'add_builtin_material', id: ID(2), shading: 'pbr' });
  await d({ cmd: 'insert', id: ID(1), spec: { primitive: { box: { dims: [2, 2, 2] } } }, parent: null });
  // 45° yaw quaternion (0, sin(22.5°), 0, cos(22.5°)) -> a crisp diagonal silhouette.
  await d({ cmd: 'set_transform', id: ID(1), transform: { translation: [0, 1, 0], rotation: [0, 0.383, 0, 0.924], scale: [1, 1, 1] } });
  await d({ cmd: 'add_material_variant', node: ID(1), material: ID(2), id: ID(0x40), name: 'box' });
  await d({ cmd: 'select_material_variant', node: ID(1), variant: ID(0x40) });
  await d({ cmd: 'set_builtin_param', node: ID(1), param: 'base_color', value: [0.12, 0.12, 0.14, 1] });
  await d({ cmd: 'set_camera_orbit', yaw: 0.5, pitch: 0.35, radius: 6.0, look_at: [0, 1.0, 0] });
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false, msaa: false, smaa: false });
  await q({ query: 'wait_render_settled' });
  return 'aa-edges authored';
}
