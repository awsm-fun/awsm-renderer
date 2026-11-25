//! Ground-truth reference frames (05 §3): accumulate N jittered renders of one
//! frozen instant with the resolve's reconstruction kernel.
//!
//! Pure Rust, natively tested. The jitter / kernel functions are local mirrors
//! of `awsm_renderer::taa::{jitter_offset, jitter_phase_count,
//! reconstruction_weight}`; with the `taa-hooks` feature the harness checks at
//! runtime that both agree (TAA-TEST `hooks-agree`), so the reference can never
//! silently drift from what the resolve does.
//!
//! # Sample positions (03 §1.2, §4.2)
//!
//! With jitter `j` (render pixels), render pixel `p` holds the scene sampled at
//! the unjittered position `p + 0.5 − j`. Output pixel `q` (centre at render
//! position `r = (q + 0.5) · R / O`) accumulates, over the 3×3 render pixels
//! around `floor(r)` of every frame, `w = exp(−2.29 · |p + 0.5 − j − r|²)`.
//! The reference is `Σ w·c / Σ w` over all frames and taps: one global
//! normalisation, i.e. the image the resolve converges to with infinite history
//! on a still scene.
//!
//! # Accumulation space
//!
//! Inputs are the swap-chain bytes (tonemapped, sRGB-encoded). The default,
//! [`AccumSpace::DisplayLinear`], decodes sRGB to linear light, averages, and
//! re-encodes: a box-filter of the light the display emits, i.e. what a
//! supersampled image of the *displayed* scene looks like. [`AccumSpace::Karis`]
//! additionally weights in `t = c / (1 + max(c))` over that linear value, the
//! resolve's working space (03 §4.4) applied to display-linear values, which
//! matches the resolve's weighting exactly where the display tonemap is close
//! to identity (LDR content) and approximately elsewhere. The harness reports
//! the difference between the two on every reference it builds so the
//! systematic bias of the choice is a measured number, not a guess.
//!
//! Averaging HDR pre-tonemap radiance instead would need an HDR readback of
//! `composite` plus a CPU re-implementation of exposure + tonemap + every
//! post effect, and it is not what the resolve converges to either (the
//! resolve blends in the Karis space, not in linear HDR: 01 G11).

use glam::Vec2;

use crate::metrics::Image;

/// Halton sequence value of `index` in `base` (index 0 → 0).
pub fn halton(mut index: u32, base: u32) -> f32 {
    let mut f = 1.0f32;
    let mut r = 0.0f32;
    while index > 0 {
        f /= base as f32;
        r += f * (index % base) as f32;
        index /= base;
    }
    r
}

/// Mirror of `awsm_renderer::taa::jitter_phase_count`: `ceil(8 · (O.w/R.w)²)`.
pub fn jitter_phase_count(render_width: u32, post_width: u32) -> u32 {
    let ratio = post_width.max(1) as f32 / render_width.max(1) as f32;
    ((8.0 * ratio * ratio) - 1e-3).ceil().max(1.0) as u32
}

/// Mirror of `awsm_renderer::taa::jitter_offset`: Halton(2,3) − 0.5 at index
/// `(frame % N) + 1`, in render pixels.
pub fn jitter_offset(frame_index: u32, phase_count: u32) -> Vec2 {
    let i = (frame_index % phase_count.max(1)) + 1;
    Vec2::new(halton(i, 2) - 0.5, halton(i, 3) - 0.5)
}

/// Mirror of `awsm_renderer::taa::RECONSTRUCTION_SHARPNESS`.
pub const RECONSTRUCTION_SHARPNESS: f32 = 2.29;

/// Mirror of `awsm_renderer::taa::reconstruction_weight`.
pub fn reconstruction_weight(d2: f32) -> f32 {
    (-RECONSTRUCTION_SHARPNESS * d2).exp()
}

/// The default number of jitter phases a reference accumulates.
pub const REFERENCE_PHASES: u32 = 64;

/// sRGB-encoded byte → linear light.
pub fn srgb_to_linear(v: u8) -> f32 {
    let c = v as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear light → sRGB-encoded byte (rounded).
pub fn linear_to_srgb(l: f32) -> u8 {
    let l = l.clamp(0.0, 1.0);
    let c = if l <= 0.003_130_8 {
        l * 12.92
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    };
    (c * 255.0).round().clamp(0.0, 255.0) as u8
}

/// Where the reference averages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccumSpace {
    /// Linear light of the display-encoded image.
    DisplayLinear,
    /// Karis (`c / (1 + max c)`) over display-linear values.
    Karis,
}

impl AccumSpace {
    pub fn name(self) -> &'static str {
        match self {
            AccumSpace::DisplayLinear => "display-linear",
            AccumSpace::Karis => "karis",
        }
    }
}

/// Accumulates jittered frames into one output-size reference.
pub struct ReferenceAccumulator {
    pub out_width: u32,
    pub out_height: u32,
    pub render_width: u32,
    pub render_height: u32,
    pub space: AccumSpace,
    sum: Vec<[f32; 3]>,
    wsum: Vec<f32>,
    pub frames: u32,
    lut: [f32; 256],
    /// Resolve model: each frame's reconstruction is normalised by its own
    /// weight sum and the frames are averaged equally (the resolve
    /// normalises per frame); the reference normalises once over all taps.
    per_frame_norm: bool,
    /// Resolve model: out-of-image taps read the clamped edge texel with
    /// their weight; the reference drops them.
    clamp_borders: bool,
}

impl ReferenceAccumulator {
    pub fn new(
        (render_width, render_height): (u32, u32),
        (out_width, out_height): (u32, u32),
        space: AccumSpace,
    ) -> Self {
        let mut lut = [0.0f32; 256];
        for (i, v) in lut.iter_mut().enumerate() {
            *v = srgb_to_linear(i as u8);
        }
        let n = (out_width * out_height) as usize;
        Self {
            out_width,
            out_height,
            render_width,
            render_height,
            space,
            sum: vec![[0.0; 3]; n],
            wsum: vec![0.0; n],
            frames: 0,
            lut,
            per_frame_norm: false,
            clamp_borders: false,
        }
    }

    /// The resolve's own model of convergence on a static scene (05 review
    /// item 5): per-frame normalisation and clamped borders. Feed it the
    /// resolve's jitter cycle (e.g. the 8 phases at native) to get the floor
    /// a perfectly converged TAA sits at relative to the 64-phase reference.
    pub fn resolve_model(render: (u32, u32), out: (u32, u32), space: AccumSpace) -> Self {
        Self {
            per_frame_norm: true,
            clamp_borders: true,
            ..Self::new(render, out, space)
        }
    }

    /// Add one render-size frame rendered with jitter `j` (render pixels).
    pub fn add_frame(&mut self, frame: &Image, j: Vec2) {
        assert_eq!(
            (frame.width, frame.height),
            (self.render_width, self.render_height),
            "frame is not render-size"
        );
        let scale = Vec2::new(
            self.render_width as f32 / self.out_width as f32,
            self.render_height as f32 / self.out_height as f32,
        );
        let (rw, rh) = (self.render_width as i64, self.render_height as i64);
        // The kernel is separable: exp(−k(dx² + dy²)) = exp(−k dx²)·exp(−k dy²),
        // and along each axis the tap distances depend only on the output
        // column (row) and the jitter. Precompute them once per frame.
        let clamp = self.clamp_borders;
        let axis = |n_out: u32, s: f32, jj: f32, n_render: i64| -> Vec<([i64; 3], [f32; 3])> {
            (0..n_out)
                .map(|q| {
                    let r = (q as f32 + 0.5) * s;
                    let p0 = r.floor() as i64;
                    let mut w = [0.0f32; 3];
                    let mut idx = [0i64; 3];
                    for (t, wt) in w.iter_mut().enumerate() {
                        let p = p0 + t as i64 - 1;
                        let d = p as f32 + 0.5 - jj - r;
                        if p >= 0 && p < n_render {
                            *wt = reconstruction_weight(d * d);
                            idx[t] = p;
                        } else if clamp {
                            *wt = reconstruction_weight(d * d);
                            idx[t] = p.clamp(0, n_render - 1);
                        }
                    }
                    (idx, w)
                })
                .collect()
        };
        let xs = axis(self.out_width, scale.x, j.x, rw);
        let ys = axis(self.out_height, scale.y, j.y, rh);
        // The frame in the accumulation space, once.
        let lin: Vec<[f32; 3]> = frame
            .rgba
            .chunks_exact(4)
            .map(|px| {
                let c = [
                    self.lut[px[0] as usize],
                    self.lut[px[1] as usize],
                    self.lut[px[2] as usize],
                ];
                if self.space == AccumSpace::Karis {
                    let k = 1.0 / (1.0 + c[0].max(c[1]).max(c[2]));
                    [c[0] * k, c[1] * k, c[2] * k]
                } else {
                    c
                }
            })
            .collect();
        // Horizontal pass: every render row, at every output column.
        let ow = self.out_width as usize;
        let mut tmp = vec![[0.0f32; 3]; rh as usize * ow];
        for ry in 0..rh as usize {
            let row = &lin[ry * rw as usize..(ry + 1) * rw as usize];
            let out = &mut tmp[ry * ow..(ry + 1) * ow];
            for (qx, (ix, wx)) in xs.iter().enumerate() {
                let mut acc = [0.0f32; 3];
                for (tx, w) in wx.iter().enumerate() {
                    if *w != 0.0 {
                        let c = row[ix[tx] as usize];
                        acc[0] += w * c[0];
                        acc[1] += w * c[1];
                        acc[2] += w * c[2];
                    }
                }
                out[qx] = acc;
            }
        }
        // Vertical pass into the accumulators. Dropped (out-of-image) taps
        // have weight 0, so this frame's total weight is (Σ wx)(Σ wy).
        let wsx: Vec<f32> = xs.iter().map(|(_, w)| w.iter().sum()).collect();
        for (qy, (iy, wy)) in ys.iter().enumerate() {
            let wsy: f32 = wy.iter().sum();
            for qx in 0..ow {
                let oi = qy * ow + qx;
                let mut acc = [0.0f32; 3];
                for (ty, w) in wy.iter().enumerate() {
                    if *w != 0.0 {
                        let c = tmp[iy[ty] as usize * ow + qx];
                        acc[0] += w * c[0];
                        acc[1] += w * c[1];
                        acc[2] += w * c[2];
                    }
                }
                let wf = wsx[qx] * wsy;
                if self.per_frame_norm {
                    let inv = 1.0 / wf.max(1e-12);
                    for (s, a) in self.sum[oi].iter_mut().zip(acc) {
                        *s += a * inv;
                    }
                    self.wsum[oi] += 1.0;
                } else {
                    for (s, a) in self.sum[oi].iter_mut().zip(acc) {
                        *s += a;
                    }
                    self.wsum[oi] += wf;
                }
            }
        }
        self.frames += 1;
    }

    /// The accumulated value per output pixel, in the accumulation space
    /// (display-linear, or Karis-weighted), not encoded.
    pub fn mean_linear(&self) -> Vec<[f32; 3]> {
        self.sum
            .iter()
            .zip(&self.wsum)
            .map(|(s, w)| {
                let w = w.max(1e-12);
                [s[0] / w, s[1] / w, s[2] / w]
            })
            .collect()
    }

    /// The accumulated reference, display-encoded.
    pub fn finish(&self) -> Image {
        let mut out = Image::filled(self.out_width, self.out_height, [0, 0, 0, 255]);
        for (i, px) in out.rgba.chunks_exact_mut(4).enumerate() {
            let w = self.wsum[i].max(1e-12);
            let mut c = [self.sum[i][0] / w, self.sum[i][1] / w, self.sum[i][2] / w];
            if self.space == AccumSpace::Karis {
                let k = 1.0 / (1.0 - c[0].max(c[1]).max(c[2])).max(1e-6);
                c = [c[0] * k, c[1] * k, c[2] * k];
            }
            px[0] = linear_to_srgb(c[0]);
            px[1] = linear_to_srgb(c[1]);
            px[2] = linear_to_srgb(c[2]);
            px[3] = 255;
        }
        out
    }
}

/// One frame's resolve-model reconstruction (per-frame normalised, clamped
/// borders), display-linear, at render = output size.
pub fn resolve_frame_linear(frame: &Image, j: Vec2) -> Vec<[f32; 3]> {
    let size = (frame.width, frame.height);
    let mut acc = ReferenceAccumulator::resolve_model(size, size, AccumSpace::DisplayLinear);
    acc.add_frame(frame, j);
    acc.mean_linear()
}

/// One frame's resolve-model reconstruction in the given accumulation space
/// (Karis-weighted values when `karis`; average them, then `encode_space`).
pub fn resolve_frame(frame: &Image, j: Vec2, karis: bool) -> Vec<[f32; 3]> {
    let size = (frame.width, frame.height);
    let space = if karis {
        AccumSpace::Karis
    } else {
        AccumSpace::DisplayLinear
    };
    let mut acc = ReferenceAccumulator::resolve_model(size, size, space);
    acc.add_frame(frame, j);
    acc.mean_linear()
}

/// Display-encode a value of the accumulation space (Karis values are
/// un-weighted first, as `ReferenceAccumulator::finish` does).
pub fn encode_space(c: [f32; 3], karis: bool) -> [u8; 4] {
    if karis {
        let k = 1.0 / (1.0 - c[0].max(c[1]).max(c[2])).max(1e-6);
        encode_linear([c[0] * k, c[1] * k, c[2] * k])
    } else {
        encode_linear(c)
    }
}

/// Display-encode one linear colour.
pub fn encode_linear(c: [f32; 3]) -> [u8; 4] {
    [
        linear_to_srgb(c[0]),
        linear_to_srgb(c[1]),
        linear_to_srgb(c[2]),
        255,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::abs_error;

    #[test]
    fn halton_values() {
        assert_eq!(halton(1, 2), 0.5);
        assert_eq!(halton(2, 2), 0.25);
        assert_eq!(halton(3, 2), 0.75);
        assert!((halton(1, 3) - 1.0 / 3.0).abs() < 1e-6);
        assert!((halton(2, 3) - 2.0 / 3.0).abs() < 1e-6);
        assert!((halton(3, 3) - 1.0 / 9.0).abs() < 1e-6);
    }

    #[test]
    fn jitter_matches_plan() {
        // Index 1 → (0.5 − 0.5, 1/3 − 0.5).
        let j = jitter_offset(0, 8);
        assert!((j.x - 0.0).abs() < 1e-6 && (j.y + 1.0 / 6.0).abs() < 1e-6);
        // Cycles with N; never the (−0.5, −0.5) corner.
        assert_eq!(jitter_offset(8, 8), jitter_offset(0, 8));
        for i in 0..64 {
            let j = jitter_offset(i, 64);
            assert!(j.x > -0.5 && j.x < 0.5 && j.y > -0.5 && j.y < 0.5);
        }
        assert_eq!(jitter_phase_count(1920, 1920), 8);
        assert_eq!(jitter_phase_count(960, 1920), 32);
    }

    #[test]
    fn srgb_roundtrip_is_exact_for_every_byte() {
        for v in 0..=255u8 {
            assert_eq!(linear_to_srgb(srgb_to_linear(v)), v);
        }
    }

    #[test]
    fn flat_image_reference_is_the_flat_image() {
        let img = Image::filled(16, 12, [37, 140, 222, 255]);
        for space in [AccumSpace::DisplayLinear, AccumSpace::Karis] {
            let mut acc = ReferenceAccumulator::new((16, 12), (16, 12), space);
            for i in 0..REFERENCE_PHASES {
                acc.add_frame(&img, jitter_offset(i, REFERENCE_PHASES));
            }
            let s = abs_error(&acc.finish(), &img, None);
            assert_eq!(s.max, 0, "{space:?}: {s}");
        }
    }

    /// Render a vertical half-plane edge at unjittered x = `edge` (white
    /// left, black right), point-sampled at pixel centres with jitter `j`
    /// applied the renderer's way: pixel p samples the scene at p + 0.5 − j.
    fn render_edge(w: u32, h: u32, edge: f32, j: Vec2) -> Image {
        let mut img = Image::filled(w, h, [0, 0, 0, 255]);
        for y in 0..h {
            for x in 0..w {
                let sx = x as f32 + 0.5 - j.x;
                if sx < edge {
                    img.set(x, y, [255, 255, 255, 255]);
                }
            }
        }
        img
    }

    #[test]
    fn edge_reference_is_antialiased_and_centred() {
        // Edge exactly through the centre of pixel 8 (x = 8.5): the reference
        // must be symmetric about it and pixel 8 must be the midpoint in
        // linear light.
        let (w, h) = (16, 4);
        let mut acc = ReferenceAccumulator::new((w, h), (w, h), AccumSpace::DisplayLinear);
        for i in 0..REFERENCE_PHASES {
            let j = jitter_offset(i, REFERENCE_PHASES);
            acc.add_frame(&render_edge(w, h, 8.5, j), j);
        }
        let r = acc.finish();
        let lin = |x: u32| srgb_to_linear(r.get(x, 1)[0]);
        assert!((lin(8) - 0.5).abs() < 0.03, "centre pixel {}", lin(8));
        // Symmetric: lin(8 − k) + lin(8 + k) ≈ 1.
        for k in 1..4 {
            let s = lin(8 - k) + lin(8 + k);
            assert!((s - 1.0).abs() < 0.03, "k={k} sum {s}");
        }
        // Far from the edge: exact.
        assert_eq!(r.get(2, 1)[0], 255);
        assert_eq!(r.get(13, 1)[0], 0);
        // A wrong jitter sign would shift the edge by about a pixel; the
        // midpoint test above is what catches it (see the next test).
    }

    #[test]
    fn wrong_jitter_sign_is_detected() {
        let (w, h) = (16, 4);
        let mut acc = ReferenceAccumulator::new((w, h), (w, h), AccumSpace::DisplayLinear);
        for i in 0..REFERENCE_PHASES {
            let j = jitter_offset(i, REFERENCE_PHASES);
            // Accumulate with the NEGATED jitter: the samples are placed at
            // mirrored positions, which blurs the edge instead of resolving it.
            acc.add_frame(&render_edge(w, h, 8.5, j), -j);
        }
        let r = acc.finish();
        let mut good = ReferenceAccumulator::new((w, h), (w, h), AccumSpace::DisplayLinear);
        for i in 0..REFERENCE_PHASES {
            let j = jitter_offset(i, REFERENCE_PHASES);
            good.add_frame(&render_edge(w, h, 8.5, j), j);
        }
        let g = good.finish();
        // The correctly placed edge is a sharper ramp: its pixel 7 is brighter.
        assert!(
            g.get(7, 1)[0] > r.get(7, 1)[0] + 5,
            "{} vs {}",
            g.get(7, 1)[0],
            r.get(7, 1)[0]
        );
    }

    #[test]
    fn upscaled_reference_matches_native_on_flat_regions() {
        // Render at half size, output at full size: a flat image stays flat.
        let img = Image::filled(8, 6, [90, 90, 90, 255]);
        let mut acc = ReferenceAccumulator::new((8, 6), (16, 12), AccumSpace::DisplayLinear);
        for i in 0..32 {
            acc.add_frame(&img, jitter_offset(i, 32));
        }
        let r = acc.finish();
        assert_eq!((r.width, r.height), (16, 12));
        assert!(r.rgba.chunks_exact(4).all(|p| p[0] == 90));
    }

    #[test]
    fn resolve_model_is_flat_on_flat_and_close_to_reference_on_an_edge() {
        let img = Image::filled(16, 12, [90, 140, 30, 255]);
        let mut m =
            ReferenceAccumulator::resolve_model((16, 12), (16, 12), AccumSpace::DisplayLinear);
        for i in 0..8 {
            m.add_frame(&img, jitter_offset(i, 8));
        }
        assert_eq!(abs_error(&m.finish(), &img, None).max, 0);
        // Clamped borders keep the border pixels exact on a flat image even
        // with a single phase.
        let mut one =
            ReferenceAccumulator::resolve_model((16, 12), (16, 12), AccumSpace::DisplayLinear);
        one.add_frame(&img, jitter_offset(3, 8));
        assert_eq!(abs_error(&one.finish(), &img, None).max, 0);
        // On an edge the 8-phase per-frame-normalised model differs from the
        // 64-phase reference by a little, not by the edge's contrast.
        let (w, h) = (16, 4);
        let mut r64 = ReferenceAccumulator::new((w, h), (w, h), AccumSpace::DisplayLinear);
        for i in 0..REFERENCE_PHASES {
            let j = jitter_offset(i, REFERENCE_PHASES);
            r64.add_frame(&render_edge(w, h, 8.5, j), j);
        }
        let mut m8 = ReferenceAccumulator::resolve_model((w, h), (w, h), AccumSpace::DisplayLinear);
        for i in 0..8 {
            let j = jitter_offset(i, 8);
            m8.add_frame(&render_edge(w, h, 8.5, j), j);
        }
        let e = abs_error(&m8.finish(), &r64.finish(), None);
        assert!(e.max > 0 && e.max < 60, "{e}");
    }

    #[test]
    fn karis_darkens_bright_dark_edge_midpoint() {
        // Karis weighting pulls a 50/50 bright/dark mix below the linear mean.
        let (w, h) = (16, 4);
        let mut lin = ReferenceAccumulator::new((w, h), (w, h), AccumSpace::DisplayLinear);
        let mut kar = ReferenceAccumulator::new((w, h), (w, h), AccumSpace::Karis);
        for i in 0..REFERENCE_PHASES {
            let j = jitter_offset(i, REFERENCE_PHASES);
            let f = render_edge(w, h, 8.5, j);
            lin.add_frame(&f, j);
            kar.add_frame(&f, j);
        }
        let (l, k) = (lin.finish(), kar.finish());
        assert!(k.get(8, 1)[0] < l.get(8, 1)[0]);
        assert_eq!(k.get(2, 1)[0], 255);
    }
}
