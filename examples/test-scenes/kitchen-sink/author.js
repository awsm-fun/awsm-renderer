// test-scene: kitchen-sink
// Everything at once — the smoke test and THE startup-census scene:
// PBR variant spheres (plastic/metal/emissive), a procedural-checker box,
// 6 froxel-culled colored point lights, a skinned CesiumMan frozen
// mid-stride, a blended particle fountain, and SSR on a dark glossy floor.
// Census on authoring (2026-07-10 baseline, axis 1 re-measures):
//   render_pipelines 69, compute_pipelines 32, shaders 51, meshes 10,
//   pool_textures 4, render_cpu ~1.8 ms EMA.
async () => {
  const d = async (o) => { const r = await window.wasmBindings.editor_dispatch_json(JSON.stringify(o)); let v = r; try { v = JSON.parse(r); if (typeof v === 'string' && v !== 'ok') { try { v = JSON.parse(v); } catch {} } } catch {}; if (v !== 'ok') throw new Error(`${o.cmd}: ${JSON.stringify(v)}`); return v; };
  const q = async (o) => { const r = await window.wasmBindings.editor_query_json(JSON.stringify(o)); const p = JSON.parse(r); return typeof p === 'string' ? JSON.parse(p) : p; };
  const ID = (n) => `00000000-0000-4000-8000-0000000000${n.toString(16).padStart(2, '0')}`;
  await d({ cmd: 'new_project' });
  const mat = ID(2), tex = ID(0xf1);
  await d({ cmd: 'add_builtin_material', id: mat, shading: 'pbr' });
  await d({ cmd: 'add_texture_asset', id: tex, proc: 'checker' });
  const mk = async (n, spec, pos, name, base, metal, rough, emissive) => {
    const node = ID(n), v = ID(n + 0x40);
    await d({ cmd: 'insert', id: node, spec, parent: null });
    await d({ cmd: 'set_transform', id: node, transform: { translation: pos, rotation: [0, 0, 0, 1], scale: [1, 1, 1] } });
    await d({ cmd: 'rename', id: node, name });
    await d({ cmd: 'add_material_variant', node, material: mat, id: v, name });
    await d({ cmd: 'select_material_variant', node, variant: v });
    await d({ cmd: 'set_builtin_param', node, param: 'base_color', value: base });
    await d({ cmd: 'set_builtin_param', node, param: 'metallic', value: [metal] });
    await d({ cmd: 'set_builtin_param', node, param: 'roughness', value: [rough] });
    if (emissive) await d({ cmd: 'set_builtin_param', node, param: 'emissive', value: emissive });
    return node;
  };
  await mk(1, { primitive: { plane: { width: 20, depth: 20, segments_x: 1, segments_z: 1 } } }, [0, 0, 0], 'floor', [0.12, 0.12, 0.14, 1], 0, 0.15);
  await mk(0x10, { primitive: { sphere: { radius: 0.8, segments_long: 32, segments_lat: 24 } } }, [-3, 0.9, -1], 'red-plastic', [0.8, 0.1, 0.1, 1], 0, 0.35);
  await mk(0x11, { primitive: { sphere: { radius: 0.8, segments_long: 32, segments_lat: 24 } } }, [-1, 0.9, -2.5], 'gold-metal', [1.0, 0.77, 0.34, 1], 1, 0.15);
  await mk(0x12, { primitive: { sphere: { radius: 0.8, segments_long: 32, segments_lat: 24 } } }, [3, 0.9, -1.5], 'emissive', [0.05, 0.05, 0.05, 1], 0, 0.5, [0.5, 2.2, 2.0]);
  const box = await mk(0x13, { primitive: { box: { dims: [1.4, 1.4, 1.4] } } }, [1.2, 0.7, -3], 'checker-box', [1, 1, 1, 1], 0, 0.6);
  await d({ cmd: 'set_builtin_texture', node: box, slot: 'base_color', texture: tex });
  const cols = [[1, 0.2, 0.2], [0.2, 1, 0.2], [0.2, 0.4, 1], [1, 0.9, 0.2], [1, 0.3, 1], [0.2, 1, 1]];
  for (let i = 0; i < 6; i++) {
    const n = `30000000-0000-4000-8000-0000000000${i.toString(16).padStart(2, '0')}`;
    await d({ cmd: 'insert', id: n, spec: { light: 'point' }, parent: null });
    await d({ cmd: 'set_transform', id: n, transform: { translation: [-5 + i * 2, 1.5, 2.5], rotation: [0, 0, 0, 1], scale: [1, 1, 1] } });
    await d({ cmd: 'set_light_param', node: n, param: 'color', value: cols[i] });
    await d({ cmd: 'set_light_param', node: n, param: 'intensity', value: [350] });
    await d({ cmd: 'set_light_param', node: n, param: 'range', value: [2.6] });
  }
  await d({ cmd: 'insert', id: ID(0x50), spec: { particle: {} }, parent: null });
  await d({ cmd: 'set_transform', id: ID(0x50), transform: { translation: [4, 0, 2], rotation: [0, 0, 0, 1], scale: [1, 1, 1] } });
  await d({ cmd: 'set_particle_emitter', node: ID(0x50), spawn_rate: 100, max_alive: 400, lifetime: [0.8, 1.5], initial_speed: [2.5, 4], size: [0.06, 0.14], color_over_life: { linear: { start: [1, 0.7, 0.2, 1], end: [1, 0.2, 0.1, 0] } }, size_over_life: { linear: { start: 1, end: 0.4 } }, forces: [{ gravity: { acceleration: [0, -3.5, 0] } }], blend: true });
  await d({ cmd: 'import_model_from_url', url: 'http://localhost:9082/glTF-Sample-Assets/Models/CesiumMan/glTF/CesiumMan.gltf' });
  let clip = null;
  for (let i = 0; i < 40; i++) {
    const snap = await q({ query: 'snapshot' });
    if ((snap.animation.clips || []).length) { clip = snap.animation.clips[0].id; break; }
    await new Promise(r => setTimeout(r, 500));
  }
  if (clip) { await d({ cmd: 'set_current_clip', id: clip }); await d({ cmd: 'set_frame_time', seconds: 0.9 }); }
  const snap2 = await q({ query: 'snapshot' });
  const zup = (snap2.scene_tree || []).find(n => n.name === 'Z_UP');
  if (zup) await d({ cmd: 'set_transform', id: zup.id, transform: { translation: [0.5, 0, 1.5], rotation: [0, 0, 0, 1], scale: [1, 1, 1] } });
  await d({ cmd: 'set_post_process', ssr_enabled: true, ssr_intensity: 1.0, ssr_resolution_scale: 0.5 });
  await d({ cmd: 'set_selection', ids: [] });
  await d({ cmd: 'set_camera_orbit', yaw: 0.25, pitch: 0.45, radius: 13, look_at: [0, 0.8, 0] });
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false, skeleton_viz: false });
  await new Promise(r => setTimeout(r, 3000));
  await q({ query: 'wait_render_settled' });
  const ms = (await q({ query: 'memory_stats' })).entries;
  return JSON.stringify({pipelines: ms.render_pipelines, compute: ms.compute_pipelines, shaders: ms.shaders, meshes: ms.meshes, tex: ms.pool_textures, cpu: ms.render_cpu_ms});
}
