// test-scene: mujoco-flex
// A MuJoCo DEFORMABLE, end to end, with NO simulator in the loop. Same shape as
// `mujoco-capture`, one id space over: a compiled model (sidecar, from
// `awsm-renderer-mujoco-export`) is imported as a sim instance, and a RECORDED
// capture (from `awsm-renderer-mujoco-record`) is baked into an ordinary
// animation clip. But a flex is not driven by geom poses — it imports as a
// SKINNED MESH whose joints are the bodies its cage rides, so the clip drives
// the capture's BODY channel and the surface deforms on the GPU.
//
// Both fixtures are checked in under `fixtures/`, so this scene is deterministic
// and CI never runs physics. The playhead is frozen at 1.1s, partway through the
// flag's flap, so the cloth is unmistakably NOT at its flat bind pose.
//
// This scene exists because both flex bugs found so far produced silently WRONG
// GEOMETRY rather than errors, and nothing would have caught a regression:
//   - `reexport_clean` dropped JOINTS_1/WEIGHTS_1, so weights summed to ~0.5 and
//     the surface collapsed toward its joints;
//   - a skinned mesh's world AABB described its BIND pose, so a deformed flag
//     left that box and was frustum culled — drawn as nothing at all.
// A golden image catches both immediately, which is the whole point.
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
  const FIX = 'http://localhost:9084/mujoco-flex/fixtures';

  await d({ cmd: 'new_project' });
  await d({ cmd: 'import_mujoco_from_url', sidecar_url: `${FIX}/Flag.mujoco.json` });

  // The instance root carries the model name; the default project also has a
  // Directional Light, so find it rather than taking roots[0]. The flag imports
  // as a floor geom plus ONE flex mesh node with 171 skin joints under it, so
  // "settled" is the root existing with children, not a geom count.
  let root = null;
  for (let i = 0; i < 40; i++) {
    const snap = await q({ query: 'snapshot' });
    root = (snap.scene_tree || []).find(n => n.name === 'Flag');
    if (root && root.children.length >= 2) break;
    await new Promise(r => setTimeout(r, 300));
  }
  if (!root) throw new Error('mujoco flex import did not settle');

  await d({
    cmd: 'import_mujoco_capture',
    capture_url: `${FIX}/Flag.capture.json`,
    instance: root.id,
    name: 'flag flap',
  });

  // The flag model declares no materials, so the cloth and the floor would both
  // render in the same default grey — and a white sheet against a white plane
  // hides exactly the failures this scene exists to catch. A saturated,
  // double-sided cloth makes the surface unmistakable: the lit side reads red
  // and the away-facing side reads near-black, so the CURL direction is legible
  // too, not just the silhouette.
  const MAT = '7a1c2e40-0000-4000-8000-0000000f1a61';
  const VAR = '7a1c2e40-0000-4000-8000-0000000f1a62';
  await d({ cmd: 'add_builtin_material', id: MAT, shading: 'pbr' });
  await d({ cmd: 'update_builtin_material', id: MAT, def: {
    label: 'flag cloth',
    base_color: [0.75, 0.12, 0.10, 1],
    base_color_texture: null,
    metallic: 0, roughness: 0.85,
    metallic_roughness_texture: null,
    emissive: [0, 0, 0], emissive_texture: null,
    normal_texture: null, normal_scale: 1,
    occlusion_texture: null, occlusion_strength: 1,
    ssr_mask: 1,
    double_sided: true,
    vertex_colors_enabled: false,
    alpha_mode: 'opaque', shading: 'pbr',
    extensions: { emissive_strength: null, ior: null, specular: null, transmission: null,
      diffuse_transmission: null, volume: null, clearcoat: null, sheen: null,
      dispersion: null, anisotropy: null, iridescence: null, secondary_maps: null },
  }});
  const flag = root.children.find(c => c.name === 'flag');
  if (!flag) throw new Error('no flex mesh node named "flag"');
  await d({ cmd: 'add_material_variant', node: flag.id, material: MAT, id: VAR, name: 'cloth' });
  await d({ cmd: 'select_material_variant', node: flag.id, variant: VAR });

  // Frozen mid-flap: late enough that the cloth is visibly curved and NOT the
  // flat bind pose, early enough that it has not settled into a limp hang. The
  // camera looks DOWN on it — a flag under this wind blows nearly horizontal,
  // so an edge-on view would show a sliver and prove nothing.
  await d({ cmd: 'set_playhead', t: 1.1 });
  await d({ cmd: 'set_camera_orbit', yaw: 2.5, pitch: 0.5, radius: 1.35, look_at: [-0.30, 1.24, 0.62] });
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false, skeleton_viz: false });
  await q({ query: 'wait_render_settled' });
  return 'mujoco-flex authored';
}
