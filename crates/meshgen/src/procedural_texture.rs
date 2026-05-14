//! Procedural texture helpers returning raw RGBA buffers.

#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Color([f32; 4]);

impl Color {
    pub fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self([r, g, b, a])
    }

    pub fn to_u8_rgba(self) -> [u8; 4] {
        let c = self.0;
        [
            (c[0].clamp(0.0, 1.0) * 255.0) as u8,
            (c[1].clamp(0.0, 1.0) * 255.0) as u8,
            (c[2].clamp(0.0, 1.0) * 255.0) as u8,
            (c[3].clamp(0.0, 1.0) * 255.0) as u8,
        ]
    }
}

/// Checkerboard pattern at `cells` per axis.
pub fn checker_rgba(
    width: u32,
    height: u32,
    cells_x: u32,
    cells_y: u32,
    a: [f32; 4],
    b: [f32; 4],
) -> Vec<u8> {
    let mut out = Vec::with_capacity((width * height) as usize * 4);
    let col_a = Color::new(a[0], a[1], a[2], a[3]).to_u8_rgba();
    let col_b = Color::new(b[0], b[1], b[2], b[3]).to_u8_rgba();
    for y in 0..height {
        for x in 0..width {
            let cx = (x * cells_x) / width;
            let cy = (y * cells_y) / height;
            let pick = (cx + cy) & 1 == 0;
            let c = if pick { col_a } else { col_b };
            out.extend_from_slice(&c);
        }
    }
    out
}

/// Linear gradient from color `a` (left) to color `b` (right).
pub fn gradient_rgba(
    width: u32,
    height: u32,
    a: [f32; 4],
    b: [f32; 4],
    horizontal: bool,
) -> Vec<u8> {
    let mut out = Vec::with_capacity((width * height) as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let t = if horizontal {
                x as f32 / (width - 1).max(1) as f32
            } else {
                y as f32 / (height - 1).max(1) as f32
            };
            let c = [
                a[0] + (b[0] - a[0]) * t,
                a[1] + (b[1] - a[1]) * t,
                a[2] + (b[2] - a[2]) * t,
                a[3] + (b[3] - a[3]) * t,
            ];
            let col = Color::new(c[0], c[1], c[2], c[3]).to_u8_rgba();
            out.extend_from_slice(&col);
        }
    }
    out
}

/// Value noise (deterministic, seedable). Cheap and good enough for backdrops.
pub fn noise_rgba(width: u32, height: u32, seed: u32, scale: f32) -> Vec<u8> {
    let mut out = Vec::with_capacity((width * height) as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let fx = x as f32 * scale;
            let fy = y as f32 * scale;
            let n = value_noise2(fx, fy, seed);
            let v = ((n * 0.5 + 0.5).clamp(0.0, 1.0) * 255.0) as u8;
            out.extend_from_slice(&[v, v, v, 255]);
        }
    }
    out
}

fn hash2(x: i32, y: i32, seed: u32) -> f32 {
    let mut h = seed.wrapping_mul(0x9E3779B1);
    h ^= (x as u32).wrapping_mul(0x85EBCA77);
    h ^= (y as u32).wrapping_mul(0xC2B2AE3D);
    h = h.wrapping_mul(0x27D4EB2F).wrapping_add(0x165667B1);
    h ^= h >> 16;
    ((h as f32) / (u32::MAX as f32)) * 2.0 - 1.0
}

fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

fn value_noise2(x: f32, y: f32, seed: u32) -> f32 {
    let xi = x.floor() as i32;
    let yi = y.floor() as i32;
    let xf = x - xi as f32;
    let yf = y - yi as f32;
    let a = hash2(xi, yi, seed);
    let b = hash2(xi + 1, yi, seed);
    let c = hash2(xi, yi + 1, seed);
    let d = hash2(xi + 1, yi + 1, seed);
    let u = smoothstep(xf);
    let v = smoothstep(yf);
    let ab = a + (b - a) * u;
    let cd = c + (d - c) * u;
    ab + (cd - ab) * v
}
