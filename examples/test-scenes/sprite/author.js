// test-scene: sprite
// Billboard SPRITE TEXTURE on the particle emitter — the sprite half of the
// sprite/decal/particle texture-binding lock. A slow, large-particle fountain
// over a dark floor where each particle samples a bound sprite texture
// (set_particle_emitter {texture}); blend on so the sprite's alpha fades the
// billboard edges. Correct = a fountain of TEXTURED billboards (the logo image
// reads on each quad), not flat color squares — and on-device the sprite
// texture must materialize in the GPU pool (player-tests expected_min_textures).
//
// The imported texture id is deterministic (ImportTextureFromUrl takes the id),
// so set_particle_emitter can reference it. texture field shape:
// texture: <AssetId> (Some(Some(id)) — a bound sprite; Some(None) would clear).
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
  await d({ cmd: 'insert', id: ID(1), spec: { primitive: { plane: { width: 10, depth: 10, segments_x: 1, segments_z: 1 } } }, parent: null });
  await d({ cmd: 'add_material_variant', node: ID(1), material: matFloor, id: ID(0x40), name: 'floor' });
  await d({ cmd: 'select_material_variant', node: ID(1), variant: ID(0x40) });
  await d({ cmd: 'set_builtin_param', node: ID(1), param: 'base_color', value: [0.12, 0.12, 0.15, 1] });
  // The sprite texture (deterministic id so the emitter can bind it). The
  // CesiumLogo has clear alpha — good for a soft-edged billboard.
  const spriteTex = ID(0xf0);
  await d({ cmd: 'import_texture_from_url', id: spriteTex, url: 'http://localhost:9082/glTF-Sample-Assets/Models/BoxTextured/glTF/CesiumLogoFlat.png' });
  await d({ cmd: 'insert', id: ID(0x50), spec: { particle: {} }, parent: null });
  await d({ cmd: 'rename', id: ID(0x50), name: 'sprite-fountain' });
  // Slow, LARGE particles so the sprite texture reads on each billboard.
  await d({ cmd: 'set_particle_emitter', node: ID(0x50), spawn_rate: 40, max_alive: 256, lifetime: [1.2, 2.0], initial_speed: [2.0, 3.0], size: [0.35, 0.6], color_over_life: { linear: { start: [1, 1, 1, 1], end: [1, 1, 1, 0] } }, size_over_life: { linear: { start: 1, end: 0.8 } }, forces: [{ gravity: { acceleration: [0, -3, 0] } }], blend: true, texture: spriteTex });
  await d({ cmd: 'set_camera_orbit', yaw: 0.5, pitch: 0.3, radius: 9, look_at: [0, 1.6, 0] });
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false });
  await new Promise(r => setTimeout(r, 2000)); // let the fountain reach steady state
  await q({ query: 'wait_render_settled' });
  return 'sprite authored';
}
