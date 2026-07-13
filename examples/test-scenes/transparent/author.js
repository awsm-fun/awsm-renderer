// test-scene: transparent
// Transparent pass ordering over opaque: an orange opaque box behind three
// blend-mode glass panes (red/green/blue, base_color alpha 0.35) staggered in
// depth. Correct = through-glass tints compose in depth order (box reads
// yellow-ish through green glass, panes tint each other where they overlap),
// no popping, opaque box and floor unaffected outside the panes.
// Alpha mode lives on the MATERIAL asset: set_builtin_alpha_mode
// {material, mode: "opaque"|"blend"|{mask:{cutoff}}}.
//
// Run inside the editor page attached to the dev MCP; then save_project /
// export_player_bundle / screenshot_scene write project/, bundle/, golden.png.
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
  const matOpaque = ID(2), matGlassR = ID(3), matGlassG = ID(4), matGlassB = ID(5);
  await d({ cmd: 'add_builtin_material', id: matOpaque, shading: 'pbr' });
  for (const m of [matGlassR, matGlassG, matGlassB]) {
    await d({ cmd: 'add_builtin_material', id: m, shading: 'pbr' });
    await d({ cmd: 'set_builtin_alpha_mode', material: m, mode: 'blend' });
  }
  const mk = async (n, spec, pos, mat, name, base, rough) => {
    const node = ID(n), variant = ID(n + 32);
    await d({ cmd: 'insert', id: node, spec, parent: null });
    await d({ cmd: 'set_transform', id: node, transform: { translation: pos, rotation: [0, 0, 0, 1], scale: [1, 1, 1] } });
    await d({ cmd: 'rename', id: node, name });
    await d({ cmd: 'add_material_variant', node, material: mat, id: variant, name });
    await d({ cmd: 'select_material_variant', node, variant });
    await d({ cmd: 'set_builtin_param', node, param: 'base_color', value: base });
    await d({ cmd: 'set_builtin_param', node, param: 'roughness', value: [rough] });
  };
  await mk(1, { primitive: { plane: { width: 14, depth: 14, segments_x: 1, segments_z: 1 } } }, [0, 0, 0], matOpaque, 'floor', [0.5, 0.5, 0.52, 1], 0.9);
  await mk(10, { primitive: { box: { dims: [1.6, 1.6, 1.6] } } }, [0, 0.8, -2.5], matOpaque, 'opaque-box', [0.85, 0.55, 0.1, 1], 0.4);
  await mk(11, { primitive: { box: { dims: [2.6, 2.2, 0.08] } } }, [-0.6, 1.1, -0.8], matGlassR, 'glass-red', [0.9, 0.1, 0.1, 0.35], 0.05);
  await mk(12, { primitive: { box: { dims: [2.6, 2.2, 0.08] } } }, [0.0, 1.1, 0.6], matGlassG, 'glass-green', [0.1, 0.9, 0.1, 0.35], 0.05);
  await mk(13, { primitive: { box: { dims: [2.6, 2.2, 0.08] } } }, [0.6, 1.1, 2.0], matGlassB, 'glass-blue', [0.1, 0.2, 0.9, 0.35], 0.05);
  await d({ cmd: 'set_camera_orbit', yaw: 0.35, pitch: 0.25, radius: 9, look_at: [0, 1.0, 0] });
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false });
  await q({ query: 'wait_render_settled' });
  return 'transparent authored';
}
