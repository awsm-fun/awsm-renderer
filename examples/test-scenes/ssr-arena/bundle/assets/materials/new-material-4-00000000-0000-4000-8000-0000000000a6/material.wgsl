let n = normalize(input.world_normal);
let v = input.surface_to_camera;
let facing = clamp(dot(n, v), 0.0, 1.0);
let a = pow(facing, 2.5);
return TransparentShadingOutput(vec4<f32>(input.material.glow * (a * input.material.strength), a * 0.55));