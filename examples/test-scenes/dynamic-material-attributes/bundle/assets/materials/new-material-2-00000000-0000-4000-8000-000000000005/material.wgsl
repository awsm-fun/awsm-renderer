let vc = material_vertex_color(input, 0u);
let n = normalize(input.world_normal);
let l = normalize(vec3<f32>(0.3, 0.8, 0.4));
let diff = max(dot(n, l), 0.0) * 0.7 + input.material.ambient;
return OpaqueShadingOutput(vc.rgb * diff, 1.0);