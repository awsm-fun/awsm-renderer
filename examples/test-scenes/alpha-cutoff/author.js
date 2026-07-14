// test-scene: alpha-cutoff
// Masked materials at two cutoff values plus a blend reference, all sharing
// the glTF AlphaBlendModeTest label sheet (a real alpha texture) on thin
// boxes over a gray floor. Correct = hard-edged stripe cutouts whose coverage
// DIFFERS between cutoff 0.25 and 0.75 (more survives at 0.25), the blend
// pane shows smooth translucency instead of hard edges, and the cutouts also
// show in the cast shadows.
// KNOWN COSMETIC: the label text renders V-flipped (glTF-authored UV atlas on
// an editor primitive quad) — deterministic, irrelevant to the cutoff test.
//
// Requires the local media server (task mcp-dev serves repo media/ on :9082).
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
  const tex = ID(0xf0);
  await d({ cmd: 'import_texture_from_url', id: tex, url: 'http://localhost:9082/glTF-Sample-Assets/Models/AlphaBlendModeTest/glTF/AlphaBlendLabels.png' });
  // Wait for the import to land in the asset table before binding it — a fresh
  // author.js replay races the import otherwise (the mask panels bind an
  // empty slot → blank/untextured). The baked project/ is unaffected (the
  // texture already settled at bake), so verify.md's load_project drive is fine.
  for (let tries = 0; ; tries++) {
    const cs = await q({ query: 'save_census' });
    if ((cs.texture_assets ?? 0) >= 1) break;
    if (tries > 120) throw new Error(`texture import never landed: ${JSON.stringify(cs)}`);
    await new Promise(r => setTimeout(r, 250));
  }
  const matLo = ID(2), matHi = ID(3), matBlend = ID(4), matFloor = ID(5);
  await d({ cmd: 'add_builtin_material', id: matFloor, shading: 'pbr' });
  for (const [m, mode] of [[matLo, { mask: { cutoff: 0.25 } }], [matHi, { mask: { cutoff: 0.75 } }], [matBlend, 'blend']]) {
    await d({ cmd: 'add_builtin_material', id: m, shading: 'pbr' });
    await d({ cmd: 'set_builtin_alpha_mode', material: m, mode });
  }
  const mk = async (n, spec, pos, mat, name, tex2) => {
    const node = ID(n), variant = ID(n + 32);
    await d({ cmd: 'insert', id: node, spec, parent: null });
    await d({ cmd: 'set_transform', id: node, transform: { translation: pos, rotation: [0, 0, 0, 1], scale: [1, 1, 1] } });
    await d({ cmd: 'rename', id: node, name });
    await d({ cmd: 'add_material_variant', node, material: mat, id: variant, name });
    await d({ cmd: 'select_material_variant', node, variant });
    if (tex2) await d({ cmd: 'set_builtin_texture', node, slot: 'base_color', texture: tex2 });
  };
  await mk(1, { primitive: { plane: { width: 12, depth: 12, segments_x: 1, segments_z: 1 } } }, [0, 0, 0], matFloor, 'floor', null);
  await d({ cmd: 'set_builtin_param', node: ID(1), param: 'base_color', value: [0.5, 0.5, 0.52, 1] });
  await mk(10, { primitive: { box: { dims: [3.2, 2.4, 0.05] } } }, [0, 1.3, 2.2], matLo, 'mask-025', tex);
  await mk(11, { primitive: { box: { dims: [3.2, 2.4, 0.05] } } }, [0, 1.3, 0], matHi, 'mask-075', tex);
  await mk(12, { primitive: { box: { dims: [3.2, 2.4, 0.05] } } }, [0, 1.3, -2.2], matBlend, 'blend-ref', tex);
  await d({ cmd: 'set_camera_orbit', yaw: 2.6, pitch: 0.35, radius: 11, look_at: [0, 1.1, 0] });
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false });
  await q({ query: 'wait_render_settled' });
  return 'alpha-cutoff authored';
}
