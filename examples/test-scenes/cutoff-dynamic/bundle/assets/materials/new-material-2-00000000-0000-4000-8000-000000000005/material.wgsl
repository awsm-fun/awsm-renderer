let n = normalize(input.world_normal);
let l = normalize(vec3<f32>(0.3, 0.7, 0.5));
let diff = max(dot(n, l), 0.0) * 0.85 + 0.2;
return OpaqueShadingOutput(input.material.tint * diff, 1.0);