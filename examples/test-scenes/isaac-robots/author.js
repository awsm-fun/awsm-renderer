// test-scene: isaac-robots
// Isaac Sim / Isaac Lab robots (USD) through the MuJoCo seam: two assets from
// NVIDIA's public Isaac 5.0 bucket, exported offline by
// `awsm-renderer-isaac-export` into the same sidecar + GLB the MuJoCo exporter
// writes, then imported with the ordinary `import_mujoco_from_url`. Both
// fixtures are checked in under `fixtures/`; no USD, no simulator in the loop.
//
// - `panda` — the Franka arm (Performance variant): MDL OmniPBR materials split
//   per GeomSubset, an emissive status LED, 11 rigid-body links.
// - `anymal` — ANYmal-D: 38 visual meshes under instanceable prims, textured
//   OmniPBR averaged to base colours, and 26 guide-purpose collider shapes that
//   must NOT render.
//
// Two instances in one world, each placed at its root like any sim instance.
// ANYmal's authored origin is its base, so its root is lifted until the feet
// touch the floor (lowest foot vertex is 0.6815 m below the base).
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
  const FIX = 'http://localhost:9084/isaac-robots/fixtures';
  // The importer's convention rotation (MuJoCo/Isaac Z-up → our Y-up), xyzw:
  // -90° about X. Re-stated because set_transform replaces the whole TRS.
  const S = Math.SQRT1_2;
  const ZUP = [-S, 0, 0, S];

  await d({ cmd: 'new_project' });

  const importRobot = async (file, name, visible) => {
    await d({ cmd: 'import_mujoco_from_url', sidecar_url: `${FIX}/${file}` });
    for (let i = 0; i < 40; i++) {
      const snap = await q({ query: 'snapshot' });
      const root = (snap.scene_tree || []).find(n => n.name === name);
      if (root && root.children.length >= visible) return root;
      await new Promise(r => setTimeout(r, 300));
    }
    throw new Error(`${name} import did not settle`);
  };
  // Visible geom counts: hidden-group geoms (colliders) get no node.
  const panda = await importRobot('panda.mujoco.json', 'panda', 44);
  const anymal = await importRobot('anymal.mujoco.json', 'anymal', 38);

  await d({ cmd: 'set_transform', id: panda.id, transform: { translation: [-0.8, 0, 0], rotation: ZUP, scale: [1, 1, 1] } });
  await d({ cmd: 'set_transform', id: anymal.id, transform: { translation: [0.55, 0.6815, 0], rotation: ZUP, scale: [1, 1, 1] } });

  // A floor, so "standing on the ground" is checkable with the grid off.
  // Deterministic ids so re-runs produce an identical project.toml (the robot
  // subtrees are import-minted, like every mujoco-* scene).
  const ID = (n) => `00000000-0000-4000-8000-0000000000${n.toString(16).padStart(2, '0')}`;
  const floor = ID(1), mat = ID(2), floorVar = ID(3);
  await d({ cmd: 'insert', id: floor, spec: { primitive: { plane: { width: 3.6, depth: 2.4, segments_x: 1, segments_z: 1 } } }, parent: null });
  await d({ cmd: 'rename', id: floor, name: 'floor' });
  await d({ cmd: 'add_builtin_material', id: mat, shading: 'pbr' });
  await d({ cmd: 'add_material_variant', node: floor, material: mat, id: floorVar, name: 'floor-gray' });
  await d({ cmd: 'select_material_variant', node: floor, variant: floorVar });
  await d({ cmd: 'set_builtin_param', node: floor, param: 'base_color', value: [0.42, 0.42, 0.45, 1] });
  await d({ cmd: 'set_builtin_param', node: floor, param: 'roughness', value: [0.9] });

  await d({ cmd: 'set_camera_orbit', yaw: 0.45, pitch: 0.22, radius: 3.6, look_at: [-0.1, 0.4, 0] });
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false, skeleton_viz: false });
  await q({ query: 'wait_render_settled' });
  return 'isaac-robots authored';
}
