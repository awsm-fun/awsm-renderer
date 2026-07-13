// jetpack-knockout arena — awsm-renderer recreation of the Roblox arena
// (ROBLOX-GAMES/JETPACK-KNOCKOUT ArenaBuilder.server.luau + screenshots).
//
// Layout (meters): hex-panel polished floor disc r~41 with red danger band;
// 8 rainbow neon wall rings (blue bottom -> violet top, the Roblox WALL_RINGS
// palette) as ONE torus duplicated, per-duplicate tint via a variant of the
// ONE shared neon material; a blazing white top rim (same torus mesh, scaled
// duplicate); 12 vertical neon ribs (one box duplicated, PROP_NEON palette);
// 6 platforms = one dark deck box duplicated + one glowing trim box duplicated
// with per-duplicate tints; 5 launch pads (one cylinder + one small torus ring
// each, duplicated, first-5 PROP_NEON tints) + a cyan center emblem ring.
// Environment: generated starfield KTX2 on skybox+specular, tiny irradiance
// gradient KTX2 (see src/gen-assets.py). Post: khronos_neutral_pbr, bloom
// tuned "intense but professional" (high threshold so only neon cores bloom),
// SSR on for the polished floor. Neon material carries
// KHR_materials_emissive_strength (strength 3) per the brief.
//
// Prereqs: src/ served on :9095 (npx http-server -p 9095 -c-1). Run via
// /tmp/drive.mjs eval against the editor (:9085), then save/export/screenshot.
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
  const knownIds = async () => new Set(((await q({ query: 'snapshot' })).scene_tree || []).map(n => n.id));
  // duplicate mints its own id — diff the snapshot to find it
  const dup = async (src) => {
    const before = await knownIds();
    await d({ cmd: 'duplicate', id: src });
    const after = await knownIds();
    for (const id of after) if (!before.has(id)) return id;
    throw new Error('duplicate minted no node');
  };
  const yaw = (deg) => {
    const h = (deg * Math.PI / 180) / 2;
    return [0, Math.sin(h), 0, Math.cos(h)];
  };
  const place = (id, pos, rot = [0, 0, 0, 1], scale = [1, 1, 1]) =>
    d({ cmd: 'set_transform', id, transform: { translation: pos, rotation: rot, scale } });
  const tint = async (node, mat, variantId, name, color, opts = {}) => {
    // Neon props are light EMITTERS: shadow receive would paint serrated
    // shadow-map bands across their glowing bodies (the recurring "teeth"
    // artifact), and casts from thin tubes just add acne on neighbours —
    // same call the Roblox ArenaBuilder makes (CastShadow=false everywhere).
    await d({ cmd: 'patch_kind', id: node, patch: { mesh: { shadow: { cast: false, receive: false } } } });
    await d({ cmd: 'add_material_variant', node, material: mat, id: variantId, name });
    await d({ cmd: 'select_material_variant', node, variant: variantId });
    const base = opts.base ?? color.map(c => c * 0.30);
    const glow = opts.glow ?? 1.1;
    await d({ cmd: 'set_builtin_param', node, param: 'base_color', value: [...base.slice(0, 3), 1] });
    await d({ cmd: 'set_builtin_param', node, param: 'emissive', value: color.map(c => c * glow) });
    // 0.66 keeps neon ABOVE the SSR spread cutoff (0.62): emissive props skip
    // the trace entirely (their own reflections are invisible under the glow,
    // and tracing them just added jitter speckle on the tube undersides).
    await d({ cmd: 'set_builtin_param', node, param: 'roughness', value: [opts.roughness ?? 0.66] });
    await d({ cmd: 'set_builtin_param', node, param: 'metallic', value: [opts.metallic ?? 0.0] });
  };

  await d({ cmd: 'new_project' });

  // ---------- asset imports (env cubemaps + floor textures) ----------
  // ALL imports land before anything references them: imports dispatched via
  // the fire-and-forget seam resolve ASYNC (the dispatch returns ok before
  // the fetch+decode lands). Referencing an unlanded asset loses the race
  // silently: patch_environment falls back to the default sky, a material
  // binds an untextured white emissive. Import everything, POLL the save
  // census until the asset table carries it all, THEN bind.
  const envStars = ID(0xe0), envIrr = ID(0xe1), envInt = ID(0xe2);
  const texHex = ID(0x10), texEmit = ID(0x11), texMR = ID(0x13);
  await d({ cmd: 'import_ktx_env_from_url', id: envStars, url: 'http://localhost:9095/starfield.ktx2' });
  await d({ cmd: 'import_ktx_env_from_url', id: envIrr, url: 'http://localhost:9095/irradiance.ktx2' });
  await d({ cmd: 'import_ktx_env_from_url', id: envInt, url: 'http://localhost:9095/interior.ktx2' });
  await d({ cmd: 'import_texture_from_url', id: texHex, url: 'http://localhost:9095/hex-floor.png' });
  await d({ cmd: 'import_texture_from_url', id: texEmit, url: 'http://localhost:9095/hex-floor-emit.png' });
  // tileable 4 m panel DETAIL pair — repeated 21x via the sampler (see the
  // floor texture transforms below); normal breaks the perfect-mirror
  // ambiguity (reflection vs real tube), MR carries per-surface gloss.
  await d({ cmd: 'import_texture_from_url', id: texMR, url: 'http://localhost:9095/floor-tile-mr.png' });
  // texture_assets counts real asset-table entries; commands process in
  // order, so the textures (dispatched LAST) landing implies the earlier ktx
  // imports landed too. (env_ktx_assets counts post-patch environment SLOTS,
  // not assets — polling it pre-patch deadlocks.)
  for (let tries = 0; ; tries++) {
    const cs = await q({ query: 'save_census' });
    if ((cs.texture_assets ?? 0) >= 3) break;
    if (tries > 120) throw new Error(`imports never landed: ${JSON.stringify(cs)}`);
    await new Promise(r => setTimeout(r, 250));
  }
  // SPECULAR slot = interior.ktx2, a hand-authored "global probe" of the
  // arena interior (neon ring bands at their true elevations): SSR misses
  // fall back to the specular env, and with the starfield there the
  // periphery's off-screen wall reflections dissolved to near-black —
  // "missing graphics" while orbiting. The skybox keeps the starfield.
  // PROBE: box-project the specular env against the arena's interior bounds
  // (parallax correction) — fallback reflections (SSR misses: platform-
  // occluded wall pixels, the periphery) anchor to the arena geometry
  // instead of sliding like an infinitely-distant sky. Center = arena
  // center at mid-height (where interior.ktx2 is authored from); half
  // extents = the floor disc + ring stack as a box.
  await d({
    cmd: 'patch_environment',
    skybox: { ktx: { asset_id: envStars } }, specular: { ktx: { asset_id: envInt } }, irradiance: { ktx: { asset_id: envIrr } },
    probe: { enabled: true, center: [0, 13, 0], half_extents: [42, 14, 42] },
  });
  // VERIFY the binding took (labels resolve only if the assets exist).
  {
    const snap = await q({ query: 'snapshot' });
    const sky = snap.project.environment.skybox;
    if (!(sky && sky.kind === 'ktx' && sky.label)) throw new Error(`skybox did not bind: ${JSON.stringify(sky)}`);
  }

  // ---------- materials ----------

  const mFloor = ID(0x20), mNeon = ID(0x21), mDeck = ID(0x22);
  await d({ cmd: 'add_builtin_material', id: mFloor, shading: 'pbr' });
  await d({
    cmd: 'update_builtin_material', id: mFloor, def: {
      // roughness 0.18 (was 0.12): near-mirror sharpness exposed the
      // view-dependent quantization of contact reflections under the neon
      // rings — a pattern that CRAWLS as the camera moves. 0.18 (above the
      // 0.15 near-mirror gate) gives the clean-blur look at every angle.
      // (The pad "beams" once blamed on this smear were actually the pad
      // POINT LIGHTS' specular streaks — lights removed, see the pads
      // section; the wide soft ring reflections are the wet-floor look.)
      // metallic/roughness are FACTORS over the tiled MR map (glTF):
      // panels ride the map (rough ~0.18 face / 0.55 groove, metal ~0.45).
      label: 'arena-floor', base_color: [1, 1, 1, 1], metallic: 1.0, roughness: 1.0,
      emissive: [1, 1, 1], normal_scale: 0.8, occlusion_strength: 1, double_sided: false,
      vertex_colors_enabled: false, alpha_mode: 'opaque', shading: 'pbr',
      base_color_texture: { asset: texHex }, emissive_texture: { asset: texEmit },
      // NO normal map (rev d2): every bevel normal was a glint generator —
      // hex-vertex Y-arrows via IBL (survived SSR-off), eyelash ticks via
      // SSR — at every detail level tried (10mm/3mm, glossy/satin). A
      // polished floor is FLAT; panel definition comes from the albedo
      // grooves + the MR map's rough seams. The MR tile stays.
      metallic_roughness_texture: { asset: texMR },
      extensions: { emissive_strength: { strength: 2.4 } },
    },
  });
  await d({ cmd: 'set_builtin_alpha_mode', material: mFloor, mode: { mask: { cutoff: 0.5 } } });
  await d({ cmd: 'add_builtin_material', id: mNeon, shading: 'pbr' });
  await d({
    cmd: 'update_builtin_material', id: mNeon, def: {
      label: 'arena-neon', base_color: [0.1, 0.1, 0.12, 1], metallic: 0, roughness: 0.55,
      emissive: [1, 1, 1], normal_scale: 1, occlusion_strength: 1, double_sided: false,
      vertex_colors_enabled: false, alpha_mode: 'opaque', shading: 'pbr',
      extensions: { emissive_strength: { strength: 2.4 } },
    },
  });
  await d({ cmd: 'add_builtin_material', id: mDeck, shading: 'pbr' });
  await d({
    cmd: 'update_builtin_material', id: mDeck, def: {
      // roughness 0.65 (was 0.38): under the 0.62 ssr_spread_cutoff the
      // matte decks traced stochastic reflections of the neon trims'
      // bloom-bright pixels — sparse firefly stipple across every deck top
      // at grazing angles. Above the cutoff they take IBL sheen only,
      // matching the Roblox reference's matte plastic decks.
      label: 'arena-deck', base_color: [0.16, 0.17, 0.21, 1], metallic: 0.8, roughness: 0.65,
      emissive: [0, 0, 0], normal_scale: 1, occlusion_strength: 1, double_sided: false,
      vertex_colors_enabled: false, alpha_mode: 'opaque', shading: 'pbr', extensions: {},
    },
  });

  // ---------- floor ----------
  const floor = ID(0x01), floorVar = ID(0x30);
  await d({ cmd: 'insert', id: floor, spec: { primitive: { plane: { width: 84, depth: 84, segments_x: 1, segments_z: 1 } } }, parent: null });
  await d({ cmd: 'rename', id: floor, name: 'Arena_Floor' });
  await d({ cmd: 'add_material_variant', node: floor, material: mFloor, id: floorVar, name: 'floor' });
  await d({ cmd: 'select_material_variant', node: floor, variant: floorVar });
  // detail pair tiles once per 4 m panel: 84 m plane / 4 m = scale 21. The
  // full-floor albedo/emissive keep their 0-1 mapping (disc mask + band).
  await d({ cmd: 'set_node_texture_transform', node: floor, slot: 'metallic_roughness', scale: [21, 21], wrap_u: 'repeat', wrap_v: 'repeat' });
  // Artistic SSR damping (ssr_mask multiplies the fresnel-weighted
  // reflectivity handed to SSR): at 1.0 the floor reads "glass-flooded" —
  // the reflected red band competes with the real one and the hex material
  // drowns. 0.7 keeps the signature wet look while letting the hex panels
  // read and the pads pop. Verified A/B at David-angle + grazing.
  await d({ cmd: 'set_builtin_param', node: floor, param: 'ssr_mask', value: [0.7] });

  // ---------- wall rings: ONE torus, duplicated, per-dup tint ----------
  // Roblox WALL_RINGS palette, bottom -> top.
  const RINGS = [
    [50, 100, 255], [0, 220, 235], [50, 235, 90], [255, 220, 30],
    [255, 125, 0], [255, 65, 70], [255, 55, 200], [165, 75, 255],
  ].map(c => c.map(v => v / 255));
  const ring0 = ID(0x40);
  await d({ cmd: 'insert', id: ring0, spec: { primitive: { torus: { radius: 40, thickness: 0.3, segments_major: 128, segments_minor: 32 } } }, parent: null });
  const ringIds = [ring0];
  for (let i = 1; i < RINGS.length; i++) ringIds.push(await dup(ring0));
  for (let i = 0; i < RINGS.length; i++) {
    await d({ cmd: 'rename', id: ringIds[i], name: `WallRing_${i + 1}` });
    await place(ringIds[i], [0, 2.6 + i * 2.9, 0]);
    await tint(ringIds[i], mNeon, ID(0x50 + i), `ring-${i + 1}`, RINGS[i]);
  }
  // Blazing white top rim: same torus mesh, scaled duplicate.
  const topRim = await dup(ring0);
  await d({ cmd: 'rename', id: topRim, name: 'TopRim' });
  await place(topRim, [0, 2.6 + RINGS.length * 2.9 + 0.9, 0], [0, 0, 0, 1], [1.02, 1.7, 1.02]);
  await tint(topRim, mNeon, ID(0x5e), 'top-rim', [1, 1, 1], { glow: 1.3, base: [0.9, 0.92, 1.0] });

  // ---------- neon halo shells (the "AAA glowing tube" pass) ----------
  // Each wall ring gets a fat concentric shell in a custom view-facing
  // gradient material: alpha = pow(dot(N,V), 2.5) is 1 head-on (right over
  // the tube) and 0 at the shell's silhouette, so the glow hugs the core
  // and dissolves — a volumetric-looking halo the bloom alone can't give
  // (bloom is screen-space; this one sits IN the scene, occludes correctly,
  // and thickens the tube's apparent body). The silhouette-alpha reaching 0
  // is also what makes the MSAA-less transparent pass safe here: there is
  // no hard edge to alias (the earlier glass-shell attempt failed exactly
  // there — ssr-followups #8).
  const mHalo = ID(0xa6);
  await d({ cmd: 'add_custom_material', id: mHalo });
  await d({
    cmd: 'set_custom_material_layout', id: mHalo,
    uniforms: [
      { name: 'glow', ty: 'vec3<f32>', val: '1.0,1.0,1.0' },
      { name: 'strength', ty: 'f32', val: '1.4' },
    ], textures: [], buffers: [],
  });
  await d({ cmd: 'set_custom_material_fragment_inputs', id: mHalo, inputs: ['normals', 'view_dir'] });
  await d({ cmd: 'set_custom_material_alpha_mode', id: mHalo, mode: { blend: null } });
  await d({
    cmd: 'set_custom_material_wgsl', id: mHalo, wgsl: `let n = normalize(input.world_normal);
let v = input.surface_to_camera;
let facing = clamp(dot(n, v), 0.0, 1.0);
let a = pow(facing, 2.5);
return TransparentShadingOutput(vec4<f32>(input.material.glow * (a * input.material.strength), a * 0.55));`,
  });
  // Auto-register is debounced — wait, then hard-verify the compile.
  await new Promise(r => setTimeout(r, 2500));
  {
    const diag = await q({ query: 'material_diagnostics', material: mHalo });
    if (!(diag && diag.ok)) throw new Error(`halo material failed to register: ${JSON.stringify(diag)}`);
  }
  const shell0 = ID(0xa7);
  await d({ cmd: 'insert', id: shell0, spec: { primitive: { torus: { radius: 40, thickness: 0.85, segments_major: 128, segments_minor: 24 } } }, parent: null });
  const shellIds = [shell0];
  for (let i = 1; i < RINGS.length + 1; i++) shellIds.push(await dup(shell0));
  for (let i = 0; i < RINGS.length + 1; i++) {
    const isTop = i === RINGS.length;
    await d({ cmd: 'rename', id: shellIds[i], name: isTop ? 'HaloShell_TopRim' : `HaloShell_${i + 1}` });
    if (isTop) {
      // Match TopRim's scaled torus (xz 1.02, fat white crown).
      await place(shellIds[i], [0, 2.6 + RINGS.length * 2.9 + 0.9, 0], [0, 0, 0, 1], [1.02, 1.9, 1.02]);
    } else {
      await place(shellIds[i], [0, 2.6 + i * 2.9, 0]);
    }
    await d({ cmd: 'patch_kind', id: shellIds[i], patch: { mesh: { shadow: { cast: false, receive: false } } } });
    const sv = ID(0x96 + i);
    await d({ cmd: 'add_material_variant', node: shellIds[i], material: mHalo, id: sv, name: isTop ? 'halo-top' : `halo-${i + 1}` });
    await d({ cmd: 'select_material_variant', node: shellIds[i], variant: sv });
    const glow = isTop ? [1, 1, 1] : RINGS[i];
    await d({ cmd: 'set_node_material_uniform', node: shellIds[i], name: 'glow', value: { kind: 'vec3', value: glow } });
  }

  // ---------- vertical ribs: ONE box, duplicated every 30 deg ----------
  const PROP = [
    [255, 45, 45], [20, 235, 95], [255, 40, 200], [255, 80, 0],
    [110, 50, 255], [255, 225, 0], [150, 255, 35],
  ].map(c => c.map(v => v / 255));
  const ribH = 2.6 + RINGS.length * 2.9 - 1.2;
  const rib0 = ID(0x60);
  await d({ cmd: 'insert', id: rib0, spec: { primitive: { box: { dims: [0.34, ribH, 0.34] } } }, parent: null });
  const ribIds = [rib0];
  for (let i = 1; i < 12; i++) ribIds.push(await dup(rib0));
  for (let i = 0; i < 12; i++) {
    const a = i * 30, r = 40.6;
    await d({ cmd: 'rename', id: ribIds[i], name: `Rib_${i + 1}` });
    await place(ribIds[i], [r * Math.sin(a * Math.PI / 180), ribH / 2 + 1.2, r * Math.cos(a * Math.PI / 180)], yaw(a));
    await tint(ribIds[i], mNeon, ID(0x70 + i), `rib-${i + 1}`, PROP[i % PROP.length], { glow: 0.9 });
  }

  // ---------- platforms: ONE deck box + ONE trim box, duplicated ----------
  const PLATS = [
    { r: 24, a: 0, y: 7 }, { r: 27, a: 60, y: 11 }, { r: 22, a: 120, y: 15 },
    { r: 28, a: 180, y: 9 }, { r: 24, a: 240, y: 18 }, { r: 26, a: 300, y: 13 },
  ];
  const TRIM_TINT = [PROP[5], PROP[2], PROP[1], PROP[3], PROP[0], PROP[4]]; // spatially varied
  const deck0 = ID(0x80), trim0 = ID(0x81);
  await d({ cmd: 'insert', id: deck0, spec: { primitive: { box: { dims: [7, 0.9, 7] } } }, parent: null });
  await d({ cmd: 'insert', id: trim0, spec: { primitive: { box: { dims: [7.7, 0.24, 7.7] } } }, parent: null });
  const deckIds = [deck0], trimIds = [trim0];
  for (let i = 1; i < PLATS.length; i++) { deckIds.push(await dup(deck0)); trimIds.push(await dup(trim0)); }
  for (let i = 0; i < PLATS.length; i++) {
    const p = PLATS[i];
    const x = p.r * Math.sin(p.a * Math.PI / 180), z = p.r * Math.cos(p.a * Math.PI / 180);
    await d({ cmd: 'rename', id: deckIds[i], name: `Platform_${i + 1}` });
    await d({ cmd: 'rename', id: trimIds[i], name: `Platform_${i + 1}_Trim` });
    await place(deckIds[i], [x, p.y, z], yaw(p.a));
    await place(trimIds[i], [x, p.y + 0.15, z], yaw(p.a));
    const dv = ID(0x90 + i), tv = ID(0xa0 + i);
    await d({ cmd: 'add_material_variant', node: deckIds[i], material: mDeck, id: dv, name: `deck-${i + 1}` });
    await d({ cmd: 'select_material_variant', node: deckIds[i], variant: dv });
    await tint(trimIds[i], mNeon, tv, `trim-${i + 1}`, TRIM_TINT[i], { glow: 1.2 });
  }

  // ---------- launch pads: ONE cylinder + ONE small torus, duplicated ----------
  // GROUNDED, irregularly-scattered launch pads (gameplay: players walk
  // onto the rings). Hand-scattered deterministic positions — a regular
  // pentagon read as artificial; min ~8.5 m apart, clear of the center
  // emblem and the rim.
  const PAD_POS = [
    [3.5, 9.5], [-10.2, 5.4], [12.8, -3.0], [-5.6, -12.3], [10.5, 14.5],
  ];
  const padBase0 = ID(0xb0), padRing0 = ID(0xb1);
  // Z-SAFETY: no pad face may be coplanar with the floor (y=0) or tangent
  // to it — the grounded rev.1 put the base's bottom cap AT the floor plane
  // and the ring tube tangent to it: starburst z-fighting on every pad.
  // Everything floats >= 2 cm with distinct heights.
  await d({ cmd: 'insert', id: padBase0, spec: { primitive: { cylinder: { radius: 2.25, height: 0.1, radial_segments: 64 } } }, parent: null });
  await d({ cmd: 'insert', id: padRing0, spec: { primitive: { torus: { radius: 2.45, thickness: 0.16, segments_major: 96, segments_minor: 24 } } }, parent: null });
  // Sci-fi pad composition (per the branding thumbnail's pads): outer neon
  // ring + thin inner ring + hot near-white CORE over a dark metal base.
  const padInner0 = ID(0xb3), padCore0 = ID(0xb4);
  await d({ cmd: 'insert', id: padInner0, spec: { primitive: { torus: { radius: 1.45, thickness: 0.08, segments_major: 72, segments_minor: 16 } } }, parent: null });
  await d({ cmd: 'insert', id: padCore0, spec: { primitive: { cylinder: { radius: 0.62, height: 0.08, radial_segments: 48 } } }, parent: null });
  const padBaseIds = [padBase0], padRingIds = [padRing0], padInnerIds = [padInner0], padCoreIds = [padCore0];
  for (let i = 1; i < 5; i++) {
    padBaseIds.push(await dup(padBase0));
    padRingIds.push(await dup(padRing0));
    padInnerIds.push(await dup(padInner0));
    padCoreIds.push(await dup(padCore0));
  }
  for (let i = 0; i < 5; i++) {
    const [x, z] = PAD_POS[i];
    await d({ cmd: 'rename', id: padBaseIds[i], name: `Pad_${i + 1}` });
    await d({ cmd: 'rename', id: padRingIds[i], name: `Pad_${i + 1}_Ring` });
    // Grounded-but-not-coplanar: base 0.02..0.12, outer ring bottom 0.02,
    // inner ring bottom 0.05, core 0.08..0.16 — distinct heights, no
    // floor-plane contact, no underside gap to mirror.
    await place(padBaseIds[i], [x, 0.07, z]);
    await place(padRingIds[i], [x, 0.18, z]);
    await place(padInnerIds[i], [x, 0.13, z]);
    await place(padCoreIds[i], [x, 0.12, z]);
    const bv = ID(0xc0 + i), rv = ID(0xd0 + i);
    await d({ cmd: 'patch_kind', id: padBaseIds[i], patch: { mesh: { shadow: { cast: false, receive: false } } } });
    await d({ cmd: 'add_material_variant', node: padBaseIds[i], material: mNeon, id: bv, name: `pad-${i + 1}` });
    await d({ cmd: 'select_material_variant', node: padBaseIds[i], variant: bv });
    // Dark metal base — the rings + core read against it (thumbnail-style).
    await d({ cmd: 'set_builtin_param', node: padBaseIds[i], param: 'base_color', value: [0.06, 0.065, 0.08, 1] });
    await d({ cmd: 'set_builtin_param', node: padBaseIds[i], param: 'emissive', value: PROP[i].map(c => c * 0.06) });
    await d({ cmd: 'set_builtin_param', node: padBaseIds[i], param: 'roughness', value: [0.66] });
    await tint(padRingIds[i], mNeon, rv, `pad-ring-${i + 1}`, PROP[i], { glow: 1.1 });
    const iv = ID(0xd8 + i), cv2 = ID(0xe8 + i);
    await d({ cmd: 'rename', id: padInnerIds[i], name: `Pad_${i + 1}_Inner` });
    await d({ cmd: 'rename', id: padCoreIds[i], name: `Pad_${i + 1}_Core` });
    await d({ cmd: 'patch_kind', id: padInnerIds[i], patch: { mesh: { shadow: { cast: false, receive: false } } } });
    await d({ cmd: 'patch_kind', id: padCoreIds[i], patch: { mesh: { shadow: { cast: false, receive: false } } } });
    await tint(padInnerIds[i], mNeon, iv, `pad-inner-${i + 1}`, PROP[i], { glow: 0.55 });
    await d({ cmd: 'add_material_variant', node: padCoreIds[i], material: mNeon, id: cv2, name: `pad-core-${i + 1}` });
    await d({ cmd: 'select_material_variant', node: padCoreIds[i], variant: cv2 });
    await d({ cmd: 'set_builtin_param', node: padCoreIds[i], param: 'base_color', value: [0.04, 0.04, 0.05, 1] });
    await d({ cmd: 'set_builtin_param', node: padCoreIds[i], param: 'emissive', value: PROP[i].map(c => 1.4 + c * 1.4) });
  }
  // RED BASE WALL (2026-07-13c, replaced the guard-rail kit + the floor's
  // red danger band per David: "a flat glowing red wall from the floor to
  // the first tori"). An OPEN cylinder shell via a Lathe recipe — the
  // cylinder primitive carries caps (a 40 m red ceiling disc at y2.45, no),
  // a lathe profile is capless by construction. r 40.6 sits 0.6 m behind
  // the r40 ring circle so the blue ring floats just in front of it; the
  // top ends INSIDE the lowest ring's tube shadow-line (2.45 vs tube bottom
  // 2.3 — overlap, never tangent), the bottom is sunk 0.1 m under the
  // floor. Double-sided: the shell must read from the arena AND the
  // exterior establishing shot.
  const mWall = ID(0x25), baseWall = ID(0xba);
  await d({ cmd: 'add_builtin_material', id: mWall, shading: 'pbr' });
  await d({
    cmd: 'update_builtin_material', id: mWall, def: {
      label: 'arena-basewall', base_color: [0.16, 0.02, 0.02, 1], metallic: 0.0,
      // roughness 0.66: above the SSR cutoff (emissive glow, not a mirror)
      roughness: 0.66,
      emissive: [0.62, 0.055, 0.045], normal_scale: 1, occlusion_strength: 1,
      double_sided: true, vertex_colors_enabled: false, alpha_mode: 'opaque',
      shading: 'pbr', extensions: { emissive_strength: { strength: 2.4 } },
    },
  });
  await d({ cmd: 'insert', id: baseWall, spec: { primitive: { cylinder: { radius: 40.6, height: 2.0, radial_segments: 192 } } }, parent: null });
  await d({ cmd: 'rename', id: baseWall, name: 'BaseWall' });
  // swap the recipe to a capless lathe shell (profile = [height, radius])
  {
    const kd = await q({ query: 'node_kind_details', nodes: [baseWall] });
    // entries[<node>] = { mesh: { mesh: <asset-id>, ... } } — kind wrapper
    const meshId = kd.entries[baseWall].mesh && kd.entries[baseWall].mesh.mesh;
    if (!meshId) throw new Error(`BaseWall mesh id missing: ${JSON.stringify(kd)}`);
    await d({
      cmd: 'set_mesh_modifiers', mesh: meshId, stack: {
        base: { lathe: { profile: [[-0.1, 40.6], [2.45, 40.6]], segments: 192, angle: 6.283185307179586 } },
        modifiers: [],
      },
    });
  }
  await place(baseWall, [0, 0, 0]);
  await d({ cmd: 'patch_kind', id: baseWall, patch: { mesh: { shadow: { cast: false, receive: false } } } });
  const wallV = ID(0xcf);
  await d({ cmd: 'add_material_variant', node: baseWall, material: mWall, id: wallV, name: 'base-wall' });
  await d({ cmd: 'select_material_variant', node: baseWall, variant: wallV });

  // (CenterEmblem removed 2026-07-13 — David: the big cyan center ring read
  // as a sixth launchpad without being one; the floor center stays clear.)

  // ---------- lights ----------
  // Retune the default directional: dim cool key so neon + emissive carry.
  const snap = await q({ query: 'snapshot' });
  const dirLight = (snap.scene_tree || []).find(n => n.name === 'Directional Light');
  if (dirLight) {
    await d({ cmd: 'patch_kind', id: dirLight.id, patch: { light: { directional: { color: [0.72, 0.78, 1.0], intensity: 0.8 } } } });
    await d({ cmd: 'set_transform', id: dirLight.id, transform: { translation: [0, 0, 0], rotation: [-0.42, 0.16, 0.08, 0.89], scale: [1, 1, 1] } });
  }
  // (Pad point lights REMOVED 2026-07-13: a point light 1.6 m over the
  // glossy floor paints a meter-long vertical GGX specular streak — the
  // "beam under every launchpad" David flagged. It was never SSR/bloom/
  // emissive (isolated by deleting one light: its beam vanished, SSR-off
  // and bloom-off left it). The pads' own emissive + bloom carry the glow.)

  // ---------- post-processing ----------
  await d({
    cmd: 'set_post_process',
    tonemapping: 'khronos_neutral_pbr', exposure: 0.05,
    bloom: true, bloom_threshold: 1.15, bloom_knee: 0.55, bloom_intensity: 0.5, bloom_scatter: 0.62,
    // ssr_thickness 0.02 (was 0.3): a fat acceptance window lets grazing
    // rays accept 30cm-deep strays across the pads' float gap, shredding
    // their floor reflections into vertical column banding at the contact.
    // 2cm stays under the 6cm pad float and the tube radii; the trace's
    // adaptive step-advance acceptance covers continuous surfaces.
    ssr_enabled: true, ssr_intensity: 0.9, ssr_max_distance: 45, ssr_thickness: 0.02,
    ssr_temporal: true, ssr_temporal_weight: 0.85,
    ssr_max_steps: 96, ssr_resolution_scale: 1.0, ssr_edge_fade: 0.025, ssr_spread_cutoff: 0.62,
    // Software-BVH reflections ON (2026-07-13e): at low cameras most of the
    // wall stack is off-screen, so without real off-screen geometry nearly
    // the whole floor falls back to probe wash. Real ring/wall hits instead.
    ssr_bvh_reflections: true,
  });

  // ---------- framing ----------
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false, skeleton_viz: false });
  await d({ cmd: 'set_camera_orbit', yaw: 0.55, pitch: 0.42, radius: 88, look_at: [0, 10, 0] });
  await d({ cmd: 'set_selection', ids: [] });
  await q({ query: 'wait_render_settled' });
  // ---------- golden camera pin (test-scene copy only) ----------
  // The gameplay verification angle used throughout the SSR branch work
  // ("David-angle"): platform occluder column + probe band + pad glows +
  // masked-floor reflections all in one frame.
  await d({ cmd: 'set_camera_orbit', yaw: 2.9, pitch: 0.28, radius: 32, look_at: [0, 2, 0] });
  await d({ cmd: 'set_view_options', grid: false, gizmos: false, light_gizmos: false });
  await q({ query: 'wait_render_settled' });
  return 'jetpack-knockout arena authored';
}
