// test-scene: taa-skin (docs/plans/taa-upscaling/05-verification.md §1)
// A skinned walk cycle (Fox, glTF sample, its Walk clip) and a morph-target
// cube (AnimatedMorphCube) side by side under a still authored camera. The
// Fox walks by its own clip, advanced by the TAA harness with the fixed 1/60 s
// step (scenario `taa-skin`). The cube's morph weights are set by the harness
// script each frame (2 s period): the player path does not apply morph clips
// (pre-existing: weights stay 0 even in anim-morph), so the `morph-pingpong`
// clip authored here is kept for when it does, but the script is what moves
// it. Catches missing previous joint matrices / morph weights (G1).
// Fox, not CesiumMan: CesiumMan renders lying flat in the player path (a
// pre-existing 0.45 issue, see the TAA report), and this scene must show an
// upright, clearly moving character in the PLAYER.
// Placement: the player loads a skinned mesh's whole rig glb UNDER the
// skinned-mesh scene node (scene-loader `SkinnedMesh` arm) and ignores the
// skeleton root group's own transform, so both models are placed through
// their skinned-mesh node (`fox`, `morph-cube`), never through `fox-rig`.
// (The editor draws a skinned mesh by its skeleton instead, so the editor
// preview of this scene is NOT what the player renders; verify in the
// harness.)
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
  const ID = (n) => `00000000-0000-4000-8000-${n.toString(16).padStart(12, "0")}`;
  // Orbit pose → camera node transform (camera looks down local −Z).
  const camQuat = (yaw, pitch) => {
    const qy = [0, Math.sin(yaw / 2), 0, Math.cos(yaw / 2)];
    const qx = [Math.sin(-pitch / 2), 0, 0, Math.cos(-pitch / 2)];
    return [
      qy[3] * qx[0] + qy[0] * qx[3] + qy[1] * qx[2] - qy[2] * qx[1],
      qy[3] * qx[1] - qy[0] * qx[2] + qy[1] * qx[3] + qy[2] * qx[0],
      qy[3] * qx[2] + qy[0] * qx[1] - qy[1] * qx[0] + qy[2] * qx[3],
      qy[3] * qx[3] - qy[0] * qx[0] - qy[1] * qx[1] - qy[2] * qx[2],
    ];
  };
  const camPos = (yaw, pitch, radius, look) => [
    look[0] + radius * Math.cos(pitch) * Math.sin(yaw),
    look[1] + radius * Math.sin(pitch),
    look[2] + radius * Math.cos(pitch) * Math.cos(yaw),
  ];


  const addCamera = async (n, name, yaw, pitch, radius, look) => {
    const cam = ID(n);
    await d({ cmd: 'insert', id: cam, spec: 'camera', parent: null });
    await d({ cmd: 'rename', id: cam, name });
    await d({ cmd: 'set_transform', id: cam, transform: { translation: camPos(yaw, pitch, radius, look), rotation: camQuat(yaw, pitch), scale: [1, 1, 1] } });
    await d({ cmd: 'patch_kind', id: cam, patch: { camera: { projection: { perspective: { fov_y_rad: 0.7853981633974483 }, orthographic: null }, near: 0.1, far: 100.0 } } });
  };
  const mk = async (n, spec, name, mat, pos, rot) => {
    const node = ID(n), v = ID(n + 0x1000);
    await d({ cmd: 'insert', id: node, spec, parent: null });
    await d({ cmd: 'rename', id: node, name });
    await d({ cmd: 'set_transform', id: node, transform: { translation: pos, rotation: rot || [0, 0, 0, 1], scale: [1, 1, 1] } });
    await d({ cmd: 'add_material_variant', node, material: mat, id: v, name: 'main' });
    await d({ cmd: 'select_material_variant', node, variant: v });
    return node;
  };
  const paint = async (node, color, rough, tex, scale) => {
    await d({ cmd: 'set_builtin_param', node, param: 'base_color', value: color });
    await d({ cmd: 'set_builtin_param', node, param: 'roughness', value: [rough] });
    if (tex) {
      await d({ cmd: 'set_builtin_texture', node, slot: 'base_color', texture: tex });
      if (scale) await d({ cmd: 'set_node_texture_transform', node, slot: 'base_color', scale, wrap_u: 'repeat', wrap_v: 'repeat' });
    }
  };
  const finish = async (name, look, yaw, pitch, radius) => {
    await d({ cmd: 'set_post_process', bloom: false });
    await d({ cmd: 'set_camera_orbit', yaw, pitch, radius, look_at: look });
    await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false });
    await d({ cmd: 'set_selection', ids: [] });
    await q({ query: 'wait_render_settled' });
    return `${name} authored`;
  };

  await d({ cmd: 'new_project' });
  const mat = ID(0x900);
  await d({ cmd: 'add_builtin_material', id: mat, shading: 'pbr' });
  const floor = await mk(0x01, { primitive: { plane: { width: 12, depth: 12, segments_x: 1, segments_z: 1 } } }, 'floor', mat, [0, 0, 0]);
  await paint(floor, [0.42, 0.44, 0.48, 1], 0.85);
  const topLevel = async () => ((await q({ query: 'snapshot' })).scene_tree || []).map(n => n.id);
  const importRoot = async (url) => {
    const before = new Set(await topLevel());
    await d({ cmd: 'import_model_from_url', url });
    for (let i = 0; i < 60; i++) {
      const added = (await topLevel()).filter(id => !before.has(id));
      if (added.length) { await new Promise(r => setTimeout(r, 500)); return (await topLevel()).filter(id => !before.has(id)); }
      await new Promise(r => setTimeout(r, 500));
    }
    throw new Error('import did not settle: ' + url);
  };
  // A skinned import adds two top-level nodes: the skinned mesh (whose own
  // transform glTF skinning ignores) and the skeleton root group, which is
  // what places the character.
  const kinds = async () => Object.fromEntries(((await q({ query: 'snapshot' })).scene_tree || []).map(n => [n.id, n.kind]));
  const foxNodes = await importRoot('http://localhost:9082/glTF-Sample-Assets/Models/Fox/glTF/Fox.gltf');
  const k1 = await kinds();
  const foxMesh = foxNodes.find(id => k1[id] === 'skinned_mesh');
  const foxRig = foxNodes.find(id => k1[id] === 'group');
  if (!foxMesh || !foxRig) throw new Error('fox import shape: ' + JSON.stringify(foxNodes.map(id => k1[id])));
  await d({ cmd: 'rename', id: foxMesh, name: 'fox' });
  await d({ cmd: 'rename', id: foxRig, name: 'fox-rig' });
  // Fox is authored in centimetre-ish units (~155 long, walking along +z):
  // 0.018 → ~2.8 m, turned to walk across the view (+x).
  await d({ cmd: 'set_transform', id: foxMesh, transform: { translation: [-1.3, 0, 0], rotation: [0, 0.7071068, 0, 0.7071068], scale: [0.018, 0.018, 0.018] } });
  // Keep only the Walk clip (Survey / Run would play on top of it).
  for (let i = 0; i < 60; i++) {
    const clips = (await q({ query: 'snapshot' })).animation.clips || [];
    if (clips.length >= 3) {
      for (const c of clips) if (c.name !== 'Walk') await d({ cmd: 'delete_clip', id: c.id });
      break;
    }
    await new Promise(r => setTimeout(r, 500));
  }
  const cube = (await importRoot('http://localhost:9082/glTF-Sample-Assets/Models/AnimatedMorphCube/glTF/AnimatedMorphCube.gltf'))[0];
  await d({ cmd: 'rename', id: cube, name: 'morph-cube' });
  // The rig self-places as a 2-unit cube (0.02 under its own ×100 node):
  // ×0.6 → 1.2 m, beside the fox.
  await d({ cmd: 'set_transform', id: cube, transform: { translation: [1.6, 0.8, 0], rotation: [0, 0, 0, 1], scale: [0.6, 0.6, 0.6] } });
  // The cube's imported clip does not play in the player path (its tracks
  // target import-minted ids; same as anim-morph's note), so drop it and author
  // our own: both morph targets, ping-pong over 2 s (anim-morph's recipe).
  for (let i = 0; ; i++) {
    const clips = (await q({ query: 'snapshot' })).animation.clips || [];
    if (clips.length >= 2) {
      for (const c of clips) if (c.name !== 'Walk') await d({ cmd: 'delete_clip', id: c.id });
      break;
    }
    if (i > 60) throw new Error('clips never registered: ' + JSON.stringify(clips));
    await new Promise(r => setTimeout(r, 500));
  }
  const morphClip = ID(0xa00);
  await d({ cmd: 'add_clip', id: morphClip, name: 'morph-pingpong' });
  await d({ cmd: 'add_track', clip: morphClip, target: { target: 'morph', node: cube, index: 0 } });
  await d({ cmd: 'add_track', clip: morphClip, target: { target: 'morph', node: cube, index: 1 } });
  await d({ cmd: 'add_keyframe', clip: morphClip, track: 0, t: 0, value: { kind: 'scalar', value: 0 } });
  await d({ cmd: 'add_keyframe', clip: morphClip, track: 0, t: 1, value: { kind: 'scalar', value: 1 } });
  await d({ cmd: 'add_keyframe', clip: morphClip, track: 1, t: 0, value: { kind: 'scalar', value: 1 } });
  await d({ cmd: 'add_keyframe', clip: morphClip, track: 1, t: 1, value: { kind: 'scalar', value: 0 } });
  await d({ cmd: 'set_clip_loop', id: morphClip, loop_style: 'ping_pong' });
  // From the front (+z): the fox walks across the view in place.
  await addCamera(0x500, 'camera', 0, 0.2, 7, [0, 0.8, 0]);
  return await finish('taa-skin', [0, 0.8, 0], 0, 0.2, 7);
}
