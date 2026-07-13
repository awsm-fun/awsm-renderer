let n = normalize(input.world_normal);
let l = normalize(vec3<f32>(0.4, 0.8, 0.3));
let diff = max(dot(n, l), 0.0) * 0.9 + 0.15;
return OpaqueShadingOutput(input.material.tint * diff, 1.0);