// test-scene: mujoco-capture
// The MuJoCo pipeline end to end, with NO simulator in the loop: a compiled
// model (sidecar, from `awsm-renderer-mujoco-export`) is imported as a sim
// instance, and a RECORDED capture (from `awsm-renderer-mujoco-record`) is baked
// into an ordinary animation clip driving its geoms.
//
// Both fixtures are checked in under `fixtures/`, so this scene is deterministic
// and CI never runs physics — replaying the same JSON must produce the same
// pose. The playhead is frozen at 1.2s, partway through the DeepMind humanoid's
// ragdoll collapse.
//
// Correct = a humanoid mid-fall: limbs at plausible angles, NOT the standing
// initial pose (which would mean the clip is not driving) and NOT a pile of
// primitives at the origin (which would mean the geoms lost their world poses).
// The capsule limbs and sphere head/hands are MuJoCo primitive geoms rendered by
// our importer; the flat ground is the model's own `plane` geom.
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
  const FIX = 'http://localhost:9084/mujoco-capture/fixtures';

  await d({ cmd: 'new_project' });
  await d({ cmd: 'import_mujoco_from_url', sidecar_url: `${FIX}/Humanoid.mujoco.json` });

  // The instance root is the node carrying the model name; the default project
  // also has a Directional Light, so find it rather than taking roots[0].
  let root = null;
  for (let i = 0; i < 30; i++) {
    const snap = await q({ query: 'snapshot' });
    root = (snap.scene_tree || []).find(n => n.name === 'Humanoid');
    if (root && root.children.length >= 20) break;
    await new Promise(r => setTimeout(r, 300));
  }
  if (!root) throw new Error('mujoco import did not settle');

  await d({
    cmd: 'import_mujoco_capture',
    capture_url: `${FIX}/Humanoid.capture.json`,
    instance: root.id,
    name: 'ragdoll fall',
  });

  // Frozen mid-collapse: late enough that the pose is unmistakably NOT the
  // standing import pose, early enough that the limbs are still spread and a
  // wrong rotation would be obvious.
  await d({ cmd: 'set_playhead', t: 1.2 });
  await d({ cmd: 'set_camera_orbit', yaw: 1.2, pitch: 0.22, radius: 3.4, look_at: [0, 0.45, 0] });
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false, skeleton_viz: false });
  await q({ query: 'wait_render_settled' });
  return 'mujoco-capture authored';
}
