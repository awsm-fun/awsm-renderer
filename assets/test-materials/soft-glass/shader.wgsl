// soft-glass — view-angle modulated transparent material.
//
// Worked example for the Blend alpha-mode path. Returns a tinted
// color with Schlick-style alpha falloff: pixels viewed near edge-on
// (low dot(normal, surface_to_camera)) are nearly opaque (edge_alpha);
// pixels viewed face-on are nearly transparent (face_alpha). No
// opaque_background sampling — the rendered glass overlays whatever
// the opaque pass produced.
//
// Uniforms (from material.json):
//   tint: Color3  — body color
//   edge_alpha: F32  — alpha at grazing angles (default 0.95)
//   face_alpha: F32  — alpha at facing angles (default 0.25)
let n = normalize(input.world_normal);
let v = normalize(input.surface_to_camera);
let n_dot_v = clamp(dot(n, v), 0.0, 1.0);

// Schlick-style edge factor: 1 at grazing, 0 facing the camera.
// pow(1 - cos, 5) is the classical Schlick weight; we use it as a
// raw alpha modulator here rather than as a Fresnel reflectance term.
let edge = pow(1.0 - n_dot_v, 5.0);
let alpha = mix(input.material.face_alpha, input.material.edge_alpha, edge);

// Body color is the tint, slightly brightened at grazing edges so
// the glass reads as catching light along its silhouette.
let rgb = input.material.tint * (1.0 + 0.3 * edge);

return TransparentShadingOutput(vec4<f32>(rgb, alpha));
