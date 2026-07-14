let g = fract(input.uv * 5.0) - vec2<f32>(0.5);
return select(1.0, 0.0, dot(g, g) < 0.12);