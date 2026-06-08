// scanline — opaque worked example from contract-opaque.md.
//
// The wrapper auto-loads MaterialData from material_offset before
// calling this fragment, so the author has typed access to every
// uniform declared in material.json's "uniforms" array.
let coords_f = vec2<f32>(f32(input.coords.x), f32(input.coords.y));
let dims_f = vec2<f32>(f32(input.screen_dims.x), f32(input.screen_dims.y));
let uv = coords_f / dims_f;
let fg = frame_globals_from_raw(frame_globals_raw);
let scan = sin(uv.y * input.material.scan_freq + fg.time * input.material.scan_speed);
let overlay = mix(vec3<f32>(0.0), input.material.tint, scan * input.material.scan_strength);
let color = vec3<f32>(0.5, 0.5, 0.5) + overlay;
return OpaqueShadingOutput(color, 1.0);
