let uv = material_uv(input, 0u);
let base = material_sample_tex(input.material, uv).rgb;
let n = normalize(input.world_normal);
let l = normalize(vec3<f32>(0.4, 0.8, 0.3));
let diff = max(dot(n, l), 0.0) * 0.9 + 0.2;
return OpaqueShadingOutput(base * input.material.tint * diff, 1.0);