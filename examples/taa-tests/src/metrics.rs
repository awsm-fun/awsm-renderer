//! Image metrics for the TAA gates (docs/plans/taa-upscaling/05-verification.md §4).
//!
//! Everything here is pure Rust over **display-encoded RGBA8** images (what the
//! swap chain holds after tonemapping + sRGB encoding), so it is unit-tested
//! natively with `cargo test -p awsm-renderer-taa-tests-example`.
//!
//! # Error definition
//!
//! The per-pixel error between two images is the **largest absolute channel
//! difference over R, G, B** in 0..=255 units (alpha ignored: the swap chain's
//! alpha is don't-care). Taking the max channel instead of the channel mean is
//! the stricter choice: a pure-red ghost on a grey background counts in full.
//! Image statistics (mean / max / 99th percentile) are over the pixels of a
//! mask (all pixels when no mask is given). Errors are integers, so the 99th
//! percentile is exact (a 256-bin histogram, no interpolation).

use glam::Vec2;

/// A tightly packed RGBA8 image, row-major, `y` down.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Image {
    /// A new image filled with one colour.
    pub fn filled(width: u32, height: u32, rgba: [u8; 4]) -> Self {
        let mut data = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..width * height {
            data.extend_from_slice(&rgba);
        }
        Self {
            width,
            height,
            rgba: data,
        }
    }

    /// Wrap existing bytes. Panics if the length does not match.
    pub fn from_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Self {
        assert_eq!(
            rgba.len(),
            (width * height * 4) as usize,
            "rgba length does not match {width}x{height}"
        );
        Self {
            width,
            height,
            rgba,
        }
    }

    #[inline]
    pub fn idx(&self, x: u32, y: u32) -> usize {
        ((y * self.width + x) * 4) as usize
    }

    #[inline]
    pub fn get(&self, x: u32, y: u32) -> [u8; 4] {
        let i = self.idx(x, y);
        [
            self.rgba[i],
            self.rgba[i + 1],
            self.rgba[i + 2],
            self.rgba[i + 3],
        ]
    }

    #[inline]
    pub fn set(&mut self, x: u32, y: u32, px: [u8; 4]) {
        let i = self.idx(x, y);
        self.rgba[i..i + 4].copy_from_slice(&px);
    }

    /// A copy of the `w`×`h` region at `(x, y)` (clipped to the image).
    pub fn crop(&self, x: u32, y: u32, w: u32, h: u32) -> Image {
        let w = w.min(self.width.saturating_sub(x));
        let h = h.min(self.height.saturating_sub(y));
        let mut out = Image::filled(w, h, [0, 0, 0, 255]);
        for yy in 0..h {
            let src = self.idx(x, y + yy);
            let dst = out.idx(0, yy);
            out.rgba[dst..dst + (w * 4) as usize]
                .copy_from_slice(&self.rgba[src..src + (w * 4) as usize]);
        }
        out
    }

    pub fn same_size(&self, other: &Image) -> bool {
        self.width == other.width && self.height == other.height
    }

    pub fn pixel_count(&self) -> usize {
        (self.width * self.height) as usize
    }
}

/// A per-pixel boolean mask.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mask {
    pub width: u32,
    pub height: u32,
    pub bits: Vec<bool>,
}

impl Mask {
    pub fn new(width: u32, height: u32, value: bool) -> Self {
        Self {
            width,
            height,
            bits: vec![value; (width * height) as usize],
        }
    }

    #[inline]
    pub fn get(&self, x: u32, y: u32) -> bool {
        self.bits[(y * self.width + x) as usize]
    }

    #[inline]
    pub fn set(&mut self, x: u32, y: u32, v: bool) {
        self.bits[(y * self.width + x) as usize] = v;
    }

    pub fn count(&self) -> usize {
        self.bits.iter().filter(|b| **b).count()
    }

    pub fn and(&self, other: &Mask) -> Mask {
        assert_eq!((self.width, self.height), (other.width, other.height));
        Mask {
            width: self.width,
            height: self.height,
            bits: self
                .bits
                .iter()
                .zip(&other.bits)
                .map(|(a, b)| *a && *b)
                .collect(),
        }
    }

    pub fn or(&self, other: &Mask) -> Mask {
        assert_eq!((self.width, self.height), (other.width, other.height));
        Mask {
            width: self.width,
            height: self.height,
            bits: self
                .bits
                .iter()
                .zip(&other.bits)
                .map(|(a, b)| *a || *b)
                .collect(),
        }
    }

    pub fn not(&self) -> Mask {
        Mask {
            width: self.width,
            height: self.height,
            bits: self.bits.iter().map(|b| !*b).collect(),
        }
    }

    /// Grow the mask by `r` pixels (Chebyshev / square structuring element).
    pub fn dilate(&self, r: u32) -> Mask {
        if r == 0 {
            return self.clone();
        }
        let (w, h) = (self.width as i64, self.height as i64);
        let r = r as i64;
        // Separable: rows, then columns.
        let mut tmp = vec![false; self.bits.len()];
        for y in 0..h {
            for x in 0..w {
                let lo = (x - r).max(0);
                let hi = (x + r).min(w - 1);
                tmp[(y * w + x) as usize] = (lo..=hi).any(|xx| self.bits[(y * w + xx) as usize]);
            }
        }
        let mut out = vec![false; self.bits.len()];
        for y in 0..h {
            for x in 0..w {
                let lo = (y - r).max(0);
                let hi = (y + r).min(h - 1);
                out[(y * w + x) as usize] = (lo..=hi).any(|yy| tmp[(yy * w + x) as usize]);
            }
        }
        Mask {
            width: self.width,
            height: self.height,
            bits: out,
        }
    }
}

/// A mergeable histogram of integer per-pixel errors (0..=255).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ErrorHistogram {
    pub bins: [u64; 256],
}

impl Default for ErrorHistogram {
    fn default() -> Self {
        Self { bins: [0; 256] }
    }
}

impl ErrorHistogram {
    #[inline]
    pub fn add(&mut self, e: u8) {
        self.bins[e as usize] += 1;
    }

    pub fn merge(&mut self, other: &ErrorHistogram) {
        for (a, b) in self.bins.iter_mut().zip(other.bins.iter()) {
            *a += *b;
        }
    }

    pub fn count(&self) -> u64 {
        self.bins.iter().sum()
    }

    pub fn stats(&self) -> ErrorStats {
        let count = self.count();
        if count == 0 {
            return ErrorStats::default();
        }
        let sum: u64 = self
            .bins
            .iter()
            .enumerate()
            .map(|(e, n)| e as u64 * *n)
            .sum();
        let max = self.bins.iter().rposition(|n| *n > 0).unwrap_or(0) as u8;
        let nonzero = count - self.bins[0];
        ErrorStats {
            count,
            mean: sum as f64 / count as f64,
            max,
            p99: self.percentile(0.99),
            nonzero,
        }
    }

    /// The smallest error `e` such that at least `q` of the samples are `<= e`
    /// (nearest-rank percentile; exact for integer data).
    pub fn percentile(&self, q: f64) -> u8 {
        let count = self.count();
        if count == 0 {
            return 0;
        }
        let rank = ((q * count as f64).ceil() as u64).clamp(1, count);
        let mut acc = 0u64;
        for (e, n) in self.bins.iter().enumerate() {
            acc += *n;
            if acc >= rank {
                return e as u8;
            }
        }
        255
    }
}

/// Summary of a per-pixel error distribution.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ErrorStats {
    /// Pixels measured.
    pub count: u64,
    pub mean: f64,
    pub max: u8,
    pub p99: u8,
    /// Pixels with a non-zero error.
    pub nonzero: u64,
}

impl std::fmt::Display for ErrorStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "mean {:.4} p99 {} max {} nonzero {}/{}",
            self.mean, self.p99, self.max, self.nonzero, self.count
        )
    }
}

/// Per-pixel error (max |Δ| over RGB).
#[inline]
pub fn pixel_error(a: [u8; 4], b: [u8; 4]) -> u8 {
    let dr = a[0].abs_diff(b[0]);
    let dg = a[1].abs_diff(b[1]);
    let db = a[2].abs_diff(b[2]);
    dr.max(dg).max(db)
}

/// Histogram of per-pixel errors between `a` and `b` over `mask`.
pub fn error_histogram(a: &Image, b: &Image, mask: Option<&Mask>) -> ErrorHistogram {
    assert!(a.same_size(b), "image sizes differ");
    if let Some(m) = mask {
        assert_eq!(
            (m.width, m.height),
            (a.width, a.height),
            "mask size differs"
        );
    }
    let mut h = ErrorHistogram::default();
    for (i, (pa, pb)) in a
        .rgba
        .chunks_exact(4)
        .zip(b.rgba.chunks_exact(4))
        .enumerate()
    {
        if let Some(m) = mask {
            if !m.bits[i] {
                continue;
            }
        }
        h.add(pixel_error(
            [pa[0], pa[1], pa[2], pa[3]],
            [pb[0], pb[1], pb[2], pb[3]],
        ));
    }
    h
}

/// Mean / max / p99 absolute error between two images over a mask.
pub fn abs_error(a: &Image, b: &Image, mask: Option<&Mask>) -> ErrorStats {
    error_histogram(a, b, mask).stats()
}

/// Per-pixel error image (grey = error, scaled by `gain`) for inspection.
pub fn error_image(a: &Image, b: &Image, gain: f32) -> Image {
    assert!(a.same_size(b));
    let mut out = Image::filled(a.width, a.height, [0, 0, 0, 255]);
    for y in 0..a.height {
        for x in 0..a.width {
            let e = pixel_error(a.get(x, y), b.get(x, y)) as f32 * gain;
            let v = e.round().clamp(0.0, 255.0) as u8;
            out.set(x, y, [v, v, v, 255]);
        }
    }
    out
}

/// Frame-to-frame difference statistics over a sequence (the flicker test,
/// 05 §4.3): one histogram per consecutive pair, plus the merged histogram
/// (whose p99 is the p99 over all pixel-pairs) and the mean of per-pair means.
#[derive(Clone, Debug, Default)]
pub struct SequenceDiff {
    pub per_pair: Vec<ErrorStats>,
    pub merged: ErrorStats,
    pub mean_of_means: f64,
    pub worst_pair_mean: f64,
}

pub fn frame_to_frame(frames: &[Image], mask: Option<&Mask>) -> SequenceDiff {
    let mut merged = ErrorHistogram::default();
    let mut per_pair = Vec::new();
    for pair in frames.windows(2) {
        let h = error_histogram(&pair[0], &pair[1], mask);
        per_pair.push(h.stats());
        merged.merge(&h);
    }
    let n = per_pair.len().max(1) as f64;
    SequenceDiff {
        mean_of_means: per_pair.iter().map(|s| s.mean).sum::<f64>() / n,
        worst_pair_mean: per_pair.iter().map(|s| s.mean).fold(0.0, f64::max),
        merged: merged.stats(),
        per_pair,
    }
}

// ── Coverage and the trail band ─────────────────────────────────────────────

/// Conservative coverage of a **convex** screen-space polygon (pixel units,
/// `y` down; vertices in either winding): a pixel is covered when its square
/// `[x, x+1) × [y, y+1)` overlaps the polygon (separating-axis test).
///
/// For a convex object (the test scenes' moving boxes) the screen silhouette
/// is the convex hull of its projected vertices, so this is the object's
/// exact conservative coverage: no id render or readback needed.
pub fn rasterize_convex(polygon: &[Vec2], width: u32, height: u32) -> Mask {
    let mut mask = Mask::new(width, height, false);
    if polygon.len() < 3 {
        return mask;
    }
    let (mut lo, mut hi) = (Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY));
    for p in polygon {
        lo = lo.min(*p);
        hi = hi.max(*p);
    }
    let x0 = lo.x.floor().max(0.0) as i64;
    let y0 = lo.y.floor().max(0.0) as i64;
    let x1 = (hi.x.ceil() as i64).min(width as i64 - 1);
    let y1 = (hi.y.ceil() as i64).min(height as i64 - 1);
    if x1 < x0 || y1 < y0 {
        return mask;
    }
    // Edge normals of the polygon.
    let n = polygon.len();
    let axes: Vec<Vec2> = (0..n)
        .map(|i| {
            let e = polygon[(i + 1) % n] - polygon[i];
            Vec2::new(-e.y, e.x)
        })
        .filter(|a| a.length_squared() > 0.0)
        .collect();
    let poly_proj: Vec<(f32, f32)> = axes
        .iter()
        .map(|a| {
            polygon
                .iter()
                .fold((f32::INFINITY, f32::NEG_INFINITY), |(mn, mx), p| {
                    let d = a.dot(*p);
                    (mn.min(d), mx.max(d))
                })
        })
        .collect();
    for y in y0..=y1 {
        for x in x0..=x1 {
            let (fx, fy) = (x as f32, y as f32);
            // Square axes: the polygon bbox already overlaps on x/y unless the
            // square lies entirely outside it.
            if fx + 1.0 <= lo.x || fx >= hi.x || fy + 1.0 <= lo.y || fy >= hi.y {
                continue;
            }
            let corners = [
                Vec2::new(fx, fy),
                Vec2::new(fx + 1.0, fy),
                Vec2::new(fx, fy + 1.0),
                Vec2::new(fx + 1.0, fy + 1.0),
            ];
            let separated = axes.iter().zip(&poly_proj).any(|(a, (pmn, pmx))| {
                let (smn, smx) =
                    corners
                        .iter()
                        .fold((f32::INFINITY, f32::NEG_INFINITY), |(mn, mx), c| {
                            let d = a.dot(*c);
                            (mn.min(d), mx.max(d))
                        });
                smx <= *pmn || smn >= *pmx
            });
            if !separated {
                mask.set(x as u32, y as u32, true);
            }
        }
    }
    mask
}

/// 2D convex hull (Andrew's monotone chain), counter-clockwise in a `y`-up
/// sense. Used to turn a box's 8 projected corners into its silhouette.
pub fn convex_hull(points: &[Vec2]) -> Vec<Vec2> {
    let mut pts: Vec<Vec2> = points.iter().copied().filter(|p| p.is_finite()).collect();
    pts.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    pts.dedup();
    if pts.len() < 3 {
        return pts;
    }
    let cross = |o: Vec2, a: Vec2, b: Vec2| (a - o).perp_dot(b - o);
    let mut lower: Vec<Vec2> = Vec::new();
    for p in &pts {
        while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], *p) <= 0.0 {
            lower.pop();
        }
        lower.push(*p);
    }
    let mut upper: Vec<Vec2> = Vec::new();
    for p in pts.iter().rev() {
        while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], *p) <= 0.0 {
            upper.pop();
        }
        upper.push(*p);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

/// Distance from `p` to a convex polygon (0 inside).
pub fn distance_to_convex(polygon: &[Vec2], p: Vec2) -> f32 {
    let n = polygon.len();
    if n == 0 {
        return f32::INFINITY;
    }
    if n >= 3 {
        // Inside iff on the same side of every edge.
        let mut pos = false;
        let mut neg = false;
        for i in 0..n {
            let (a, b) = (polygon[i], polygon[(i + 1) % n]);
            let c = (b - a).perp_dot(p - a);
            pos |= c > 0.0;
            neg |= c < 0.0;
        }
        if !(pos && neg) {
            return 0.0;
        }
    }
    let mut best = f32::INFINITY;
    for i in 0..n {
        let (a, b) = (polygon[i], polygon[(i + 1) % n]);
        let e = b - a;
        let t = if e.length_squared() > 0.0 {
            ((p - a).dot(e) / e.length_squared()).clamp(0.0, 1.0)
        } else {
            0.0
        };
        best = best.min((a + e * t - p).length());
    }
    best
}

/// Pixels whose centre is within `r` px of the convex polygon (inside
/// included).
pub fn within_distance_of_convex(polygon: &[Vec2], width: u32, height: u32, r: f32) -> Mask {
    let mut m = Mask::new(width, height, false);
    if polygon.is_empty() {
        return m;
    }
    let (mut lo, mut hi) = (Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY));
    for p in polygon {
        lo = lo.min(*p);
        hi = hi.max(*p);
    }
    let x0 = (lo.x - r - 1.0).floor().max(0.0) as u32;
    let y0 = (lo.y - r - 1.0).floor().max(0.0) as u32;
    let x1 = ((hi.x + r + 1.0).ceil().max(0.0) as u32).min(width.saturating_sub(1));
    let y1 = ((hi.y + r + 1.0).ceil().max(0.0) as u32).min(height.saturating_sub(1));
    for y in y0..=y1 {
        for x in x0..=x1 {
            let c = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
            if distance_to_convex(polygon, c) <= r {
                m.set(x, y, true);
            }
        }
    }
    m
}

/// Pixels whose centre is within Euclidean distance `r` of the centre of a
/// set pixel of `m` (inside included).
pub fn within_distance_of_mask(m: &Mask, r: f32) -> Mask {
    let ri = r.floor() as i64;
    let mut out = Mask::new(m.width, m.height, false);
    let (w, h) = (m.width as i64, m.height as i64);
    for y in 0..h {
        for x in 0..w {
            if !m.bits[(y * w + x) as usize] {
                continue;
            }
            for dy in -ri..=ri {
                for dx in -ri..=ri {
                    if ((dx * dx + dy * dy) as f32) > r * r {
                        continue;
                    }
                    let (nx, ny) = (x + dx, y + dy);
                    if nx >= 0 && ny >= 0 && nx < w && ny < h {
                        out.bits[(ny * w + nx) as usize] = true;
                    }
                }
            }
        }
    }
    out
}

/// The trail band (05 §4.2): pixels the moving object covered in any of the
/// `recent` frames (the last 0.5 s, i.e. 30 frames at 60 Hz, NOT including the
/// current one) but does not cover now.
pub fn trail_band(recent: &[&Mask], now: &Mask) -> Mask {
    let mut band = Mask::new(now.width, now.height, false);
    for m in recent {
        assert_eq!((m.width, m.height), (now.width, now.height));
        for (b, v) in band.bits.iter_mut().zip(&m.bits) {
            *b |= *v;
        }
    }
    for (b, n) in band.bits.iter_mut().zip(&now.bits) {
        *b &= !*n;
    }
    band
}

// ── Reprojection (05 §4.1) ──────────────────────────────────────────────────

/// A velocity field in **UV units** (`v = uv_now − uv_prev`, 03 §0), one
/// entry per pixel of the current frame, row-major.
#[derive(Clone, Debug)]
pub struct VelocityField {
    pub width: u32,
    pub height: u32,
    pub v: Vec<Vec2>,
}

impl VelocityField {
    pub fn zero(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            v: vec![Vec2::ZERO; (width * height) as usize],
        }
    }

    #[inline]
    pub fn get(&self, x: u32, y: u32) -> Vec2 {
        self.v[(y * self.width + x) as usize]
    }

    /// Velocity in **pixels** at a pixel.
    #[inline]
    pub fn get_px(&self, x: u32, y: u32) -> Vec2 {
        self.get(x, y) * Vec2::new(self.width as f32, self.height as f32)
    }
}

/// The result of warping the previous frame into the current one.
pub struct Warped {
    pub image: Image,
    /// Pixels whose source position lies inside the previous frame and that
    /// the caller's `visible_in_both` mask accepts. Measure only over these.
    pub valid: Mask,
}

/// Warp frame `k−1` into frame `k` by frame `k`'s velocity (05 §4.1): each
/// current pixel `q` (centre `uv_q = (q + 0.5) / size`) fetches the previous
/// frame **bilinearly** at `uv_q − v(q)`, exactly what a resolve's history
/// fetch does with a perfect history filter.
///
/// `visible_in_both` (optional) removes disoccluded pixels: pixels whose
/// surface was hidden in frame `k−1`. See [`depth_visibility_mask`] and
/// [`velocity_consistency_mask`] for the two ways to build it.
pub fn warp_by_velocity(
    prev: &Image,
    velocity: &VelocityField,
    visible_in_both: Option<&Mask>,
) -> Warped {
    warp_by_velocity_with(prev, velocity, visible_in_both, WarpFilter::Bilinear)
}

/// The resampling filter of a warp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WarpFilter {
    Bilinear,
    /// Separable 4×4 Catmull-Rom (bicubic): reproduces linear signals
    /// exactly and blurs a high-contrast texture far less than bilinear.
    CatmullRom,
}

/// [`warp_by_velocity`] with a chosen resampling filter.
pub fn warp_by_velocity_with(
    prev: &Image,
    velocity: &VelocityField,
    visible_in_both: Option<&Mask>,
    filter: WarpFilter,
) -> Warped {
    assert_eq!((prev.width, prev.height), (velocity.width, velocity.height));
    let (w, h) = (prev.width, prev.height);
    let size = Vec2::new(w as f32, h as f32);
    let mut image = Image::filled(w, h, [0, 0, 0, 255]);
    let mut valid = Mask::new(w, h, false);
    for y in 0..h {
        for x in 0..w {
            let uv_q = (Vec2::new(x as f32, y as f32) + 0.5) / size;
            let uv_prev = uv_q - velocity.get(x, y);
            let pos = uv_prev * size - 0.5; // texel-centre space
            let inside =
                uv_prev.x >= 0.0 && uv_prev.x <= 1.0 && uv_prev.y >= 0.0 && uv_prev.y <= 1.0;
            let vis = visible_in_both.map(|m| m.get(x, y)).unwrap_or(true);
            if inside && vis {
                valid.set(x, y, true);
            }
            image.set(
                x,
                y,
                match filter {
                    WarpFilter::Bilinear => sample_bilinear(prev, pos),
                    WarpFilter::CatmullRom => sample_catmull_rom(prev, pos),
                },
            );
        }
    }
    Warped { image, valid }
}

/// Catmull-Rom weights for the 4 taps at offsets −1, 0, 1, 2 from
/// `floor(pos)`, fraction `t`.
fn catmull_rom_weights(t: f32) -> [f32; 4] {
    let t2 = t * t;
    let t3 = t2 * t;
    [
        -0.5 * t3 + t2 - 0.5 * t,
        1.5 * t3 - 2.5 * t2 + 1.0,
        -1.5 * t3 + 2.0 * t2 + 0.5 * t,
        0.5 * t3 - 0.5 * t2,
    ]
}

/// Catmull-Rom (bicubic) fetch at texel-centre coordinates, clamp-to-edge,
/// clamped and rounded back to 8 bits.
pub fn sample_catmull_rom(img: &Image, pos: Vec2) -> [u8; 4] {
    let maxx = img.width as i64 - 1;
    let maxy = img.height as i64 - 1;
    let x0 = pos.x.floor();
    let y0 = pos.y.floor();
    let wx = catmull_rom_weights(pos.x - x0);
    let wy = catmull_rom_weights(pos.y - y0);
    let (x0, y0) = (x0 as i64, y0 as i64);
    let mut acc = [0.0f32; 4];
    for (j, wyj) in wy.iter().enumerate() {
        let yy = (y0 + j as i64 - 1).clamp(0, maxy) as u32;
        for (i, wxi) in wx.iter().enumerate() {
            let xx = (x0 + i as i64 - 1).clamp(0, maxx) as u32;
            let px = img.get(xx, yy);
            let w = wxi * wyj;
            for (a, v) in acc.iter_mut().zip(px.iter()) {
                *a += w * *v as f32;
            }
        }
    }
    acc.map(|v| v.round().clamp(0.0, 255.0) as u8)
}

/// Bilinear fetch at texel-centre coordinates (`(0,0)` = centre of pixel 0),
/// clamp-to-edge, rounded back to 8 bits.
pub fn sample_bilinear(img: &Image, pos: Vec2) -> [u8; 4] {
    let maxx = img.width as f32 - 1.0;
    let maxy = img.height as f32 - 1.0;
    let p = Vec2::new(pos.x.clamp(0.0, maxx), pos.y.clamp(0.0, maxy));
    let x0 = p.x.floor() as u32;
    let y0 = p.y.floor() as u32;
    let x1 = (x0 + 1).min(img.width - 1);
    let y1 = (y0 + 1).min(img.height - 1);
    let fx = p.x - x0 as f32;
    let fy = p.y - y0 as f32;
    let (a, b, c, d) = (
        img.get(x0, y0),
        img.get(x1, y0),
        img.get(x0, y1),
        img.get(x1, y1),
    );
    std::array::from_fn(|i| {
        let top = a[i] as f32 * (1.0 - fx) + b[i] as f32 * fx;
        let bot = c[i] as f32 * (1.0 - fx) + d[i] as f32 * fx;
        (top * (1.0 - fy) + bot * fy).round().clamp(0.0, 255.0) as u8
    })
}

/// "Visible in both frames" from depth: pixel `q` is visible in both when the
/// previous frame's depth at the reprojected position matches the depth the
/// same surface point had in the previous frame (`expected_prev_depth`, e.g.
/// `clip_prev.z / clip_prev.w` from the velocity pass), within `tolerance`
/// (relative, in the same units). `prev_depth` is sampled nearest.
pub fn depth_visibility_mask(
    velocity: &VelocityField,
    prev_depth: &[f32],
    expected_prev_depth: &[f32],
    tolerance: f32,
) -> Mask {
    let (w, h) = (velocity.width, velocity.height);
    let size = Vec2::new(w as f32, h as f32);
    let mut m = Mask::new(w, h, false);
    for y in 0..h {
        for x in 0..w {
            let uv_q = (Vec2::new(x as f32, y as f32) + 0.5) / size;
            let p = (uv_q - velocity.get(x, y)) * size;
            if p.x < 0.0 || p.y < 0.0 || p.x >= size.x || p.y >= size.y {
                continue;
            }
            let d_prev = prev_depth[(p.y as u32 * w + p.x as u32) as usize];
            let d_exp = expected_prev_depth[(y * w + x) as usize];
            let rel = (d_prev - d_exp).abs() / d_exp.abs().max(1e-6);
            m.set(x, y, rel <= tolerance);
        }
    }
    m
}

/// "Visible in both frames" without depth: forward-backward consistency.
/// Pixel `q` reprojects to `p = q − v_k(q)`; if `p` shows the same surface,
/// that surface's velocity in frame `k−1` agrees with `v_k(q)` (exact for
/// constant-velocity motion, the scripted scenes' case). Disagreement beyond
/// `tolerance_px` means `p` showed something else (an occluder).
pub fn velocity_consistency_mask(
    prev_velocity: &VelocityField,
    velocity: &VelocityField,
    tolerance_px: f32,
) -> Mask {
    let (w, h) = (velocity.width, velocity.height);
    let size = Vec2::new(w as f32, h as f32);
    let mut m = Mask::new(w, h, false);
    for y in 0..h {
        for x in 0..w {
            let v = velocity.get(x, y);
            let p = (Vec2::new(x as f32, y as f32) + 0.5) - v * size;
            if p.x < 0.0 || p.y < 0.0 || p.x >= size.x || p.y >= size.y {
                continue;
            }
            let vp = prev_velocity.get(p.x as u32, p.y as u32);
            m.set(x, y, ((vp - v) * size).length() <= tolerance_px);
        }
    }
    m
}

// ── Sub-pixel shift estimation (jitter-convention check) ───────────────────

/// Estimate the horizontal sub-pixel shift `s` (pixels, + = content moved
/// right) taking image `a` to image `b`, by minimising
/// `Σ (b(x) − a(x − s))²` over luma, searched in `[-range, range]` at `step`.
///
/// Both images are first blurred (Gaussian, σ = 2 px): on a 1-px-wide or
/// aliased edge, linear interpolation of the samples is a biased model of a
/// sub-pixel shift (≈0.1 px at s = 0.5); on the blurred signal it is
/// accurate, and the staircase of an aliased slanted edge averages out. Only
/// pixels whose blurred horizontal gradient exceeds `min_gradient` count.
pub fn estimate_shift_x(a: &Image, b: &Image, range: f32, step: f32, min_gradient: f32) -> f32 {
    let la = blur(&luma_plane(a), a.width, a.height, 2.0);
    let lb = blur(&luma_plane(b), b.width, b.height, 2.0);
    best_shift(&la, &lb, a.width, a.height, range, step, min_gradient)
}

/// Separable Gaussian blur, clamp-to-edge.
fn blur(p: &[f32], w: u32, h: u32, sigma: f32) -> Vec<f32> {
    let r = (3.0 * sigma).ceil() as i64;
    let k: Vec<f32> = (-r..=r)
        .map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp())
        .collect();
    let ks: f32 = k.iter().sum();
    let (w, h) = (w as i64, h as i64);
    let mut tmp = vec![0.0f32; p.len()];
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0;
            for (j, kv) in k.iter().enumerate() {
                let xx = (x + j as i64 - r).clamp(0, w - 1);
                acc += kv * p[(y * w + xx) as usize];
            }
            tmp[(y * w + x) as usize] = acc / ks;
        }
    }
    let mut out = vec![0.0f32; p.len()];
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0;
            for (j, kv) in k.iter().enumerate() {
                let yy = (y + j as i64 - r).clamp(0, h - 1);
                acc += kv * tmp[(yy * w + x) as usize];
            }
            out[(y * w + x) as usize] = acc / ks;
        }
    }
    out
}

/// [`estimate_shift_x`] along `y` (+ = content moved down). Both return NaN
/// when no pixel has a gradient above `min_gradient`.
pub fn estimate_shift_y(a: &Image, b: &Image, range: f32, step: f32, min_gradient: f32) -> f32 {
    let (w, h) = (a.width, a.height);
    let la = transpose(&blur(&luma_plane(a), w, h, 2.0), w, h);
    let lb = transpose(&blur(&luma_plane(b), w, h, 2.0), w, h);
    best_shift(&la, &lb, h, w, range, step, min_gradient)
}

fn luma_plane(img: &Image) -> Vec<f32> {
    img.rgba
        .chunks_exact(4)
        .map(|p| 0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32)
        .collect()
}

fn transpose(p: &[f32], w: u32, h: u32) -> Vec<f32> {
    let mut out = vec![0.0; p.len()];
    for y in 0..h {
        for x in 0..w {
            out[(x * h + y) as usize] = p[(y * w + x) as usize];
        }
    }
    out
}

fn best_shift(
    la: &[f32],
    lb: &[f32],
    w: u32,
    h: u32,
    range: f32,
    step: f32,
    min_gradient: f32,
) -> f32 {
    let margin = range.ceil() as u32 + 2;
    let mut best = (f64::INFINITY, f32::NAN);
    let steps = (2.0 * range / step).round() as i32;
    for i in 0..=steps {
        let s = -range + i as f32 * step;
        let mut err = 0.0f64;
        let mut used = 0usize;
        for y in 0..h {
            let row = (y * w) as usize;
            for x in margin..w.saturating_sub(margin) {
                let g = (la[row + x as usize + 1] - la[row + x as usize - 1]).abs();
                if g < min_gradient {
                    continue;
                }
                let src = x as f32 - s;
                let x0 = src.floor();
                let f = src - x0;
                let x0 = x0 as usize;
                let av = la[row + x0] * (1.0 - f) + la[row + x0 + 1] * f;
                let d = (lb[row + x as usize] - av) as f64;
                err += d * d;
                used += 1;
            }
        }
        if used > 0 && err < best.0 {
            best = (err, s);
        }
    }
    // NaN when no pixel had enough gradient to say anything.
    best.1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grey(w: u32, h: u32, v: u8) -> Image {
        Image::filled(w, h, [v, v, v, 255])
    }

    #[test]
    fn identical_images_have_zero_error() {
        let a = grey(8, 8, 100);
        let s = abs_error(&a, &a, None);
        assert_eq!(s.count, 64);
        assert_eq!(s.mean, 0.0);
        assert_eq!(s.max, 0);
        assert_eq!(s.p99, 0);
        assert_eq!(s.nonzero, 0);
    }

    #[test]
    fn error_is_max_channel_and_ignores_alpha() {
        let a = Image::filled(1, 1, [10, 20, 30, 0]);
        let b = Image::filled(1, 1, [13, 15, 30, 255]);
        assert_eq!(abs_error(&a, &b, None).max, 5);
    }

    #[test]
    fn mean_max_p99_over_a_mask() {
        // 100 pixels: 98 with error 1, 1 with 10, 1 with 200.
        let a = grey(10, 10, 50);
        let mut b = grey(10, 10, 51);
        b.set(0, 0, [60, 50, 50, 255]);
        b.set(9, 9, [250, 50, 50, 255]);
        let s = abs_error(&a, &b, None);
        assert_eq!(s.count, 100);
        assert!((s.mean - (98.0 + 10.0 + 200.0) / 100.0).abs() < 1e-9);
        assert_eq!(s.max, 200);
        // rank ceil(0.99*100) = 99 -> the 99th smallest is 10.
        assert_eq!(s.p99, 10);
        // Masking out the outlier.
        let mut m = Mask::new(10, 10, true);
        m.set(9, 9, false);
        let s = abs_error(&a, &b, Some(&m));
        assert_eq!(s.count, 99);
        assert_eq!(s.max, 10);
        // Empty mask -> zero stats, no panic.
        let s = abs_error(&a, &b, Some(&Mask::new(10, 10, false)));
        assert_eq!(s.count, 0);
    }

    #[test]
    fn frame_to_frame_merges_pairs() {
        let f = vec![grey(4, 4, 0), grey(4, 4, 2), grey(4, 4, 2), grey(4, 4, 6)];
        let d = frame_to_frame(&f, None);
        assert_eq!(d.per_pair.len(), 3);
        assert_eq!(d.per_pair[0].mean, 2.0);
        assert_eq!(d.per_pair[1].mean, 0.0);
        assert_eq!(d.per_pair[2].mean, 4.0);
        assert!((d.mean_of_means - 2.0).abs() < 1e-9);
        assert_eq!(d.worst_pair_mean, 4.0);
        assert_eq!(d.merged.count, 48);
        assert_eq!(d.merged.max, 4);
    }

    #[test]
    fn rasterize_axis_aligned_square_is_conservative() {
        // Square from (2.5, 2.5) to (5.0, 5.0): touches pixels 2..=4 on both axes.
        let poly = [
            Vec2::new(2.5, 2.5),
            Vec2::new(5.0, 2.5),
            Vec2::new(5.0, 5.0),
            Vec2::new(2.5, 5.0),
        ];
        let m = rasterize_convex(&poly, 10, 10);
        for y in 0..10 {
            for x in 0..10 {
                let expect = (2..=4).contains(&x) && (2..=4).contains(&y);
                assert_eq!(m.get(x, y), expect, "pixel {x},{y}");
            }
        }
        // Winding does not matter.
        let rev: Vec<Vec2> = poly.iter().rev().copied().collect();
        assert_eq!(rasterize_convex(&rev, 10, 10), m);
    }

    #[test]
    fn rasterize_triangle_excludes_far_corner() {
        // Right triangle (0,0),(4,0),(0,4): pixel (3,3) lies beyond the hypotenuse.
        let poly = [
            Vec2::new(0.0, 0.0),
            Vec2::new(4.0, 0.0),
            Vec2::new(0.0, 4.0),
        ];
        let m = rasterize_convex(&poly, 8, 8);
        assert!(m.get(0, 0));
        assert!(m.get(3, 0));
        assert!(m.get(1, 2)); // overlaps the hypotenuse
        assert!(!m.get(3, 3));
        assert!(!m.get(2, 2)); // square (2..3)² touches x+y=4 only at its corner
        assert!(!m.get(4, 0));
    }

    #[test]
    fn hull_of_cube_corners_is_its_outline() {
        let mut pts = vec![];
        for x in [0.0, 2.0] {
            for y in [0.0, 2.0] {
                pts.push(Vec2::new(x, y));
            }
        }
        pts.push(Vec2::new(1.0, 1.0)); // interior
        let hull = convex_hull(&pts);
        assert_eq!(hull.len(), 4);
        assert!(!hull.contains(&Vec2::new(1.0, 1.0)));
    }

    #[test]
    fn trail_band_is_past_minus_now() {
        let mut past1 = Mask::new(6, 1, false);
        past1.set(0, 0, true);
        past1.set(1, 0, true);
        let mut past2 = Mask::new(6, 1, false);
        past2.set(2, 0, true);
        let mut now = Mask::new(6, 1, false);
        now.set(2, 0, true);
        now.set(3, 0, true);
        let band = trail_band(&[&past1, &past2], &now);
        assert_eq!(band.bits, vec![true, true, false, false, false, false]);
    }

    #[test]
    fn dilate_grows_by_radius() {
        let mut m = Mask::new(7, 7, false);
        m.set(3, 3, true);
        let d = m.dilate(2);
        assert_eq!(d.count(), 25);
        assert!(d.get(1, 1) && d.get(5, 5) && !d.get(0, 3));
    }

    /// A horizontal ramp image: value = 10 * x.
    fn ramp(w: u32, h: u32) -> Image {
        let mut img = grey(w, h, 0);
        for y in 0..h {
            for x in 0..w {
                let v = (10 * x).min(255) as u8;
                img.set(x, y, [v, v, v, 255]);
            }
        }
        img
    }

    #[test]
    fn warp_by_integer_velocity_is_exact_and_marks_offscreen_invalid() {
        let prev = ramp(16, 4);
        // Content moved right by 2 px: v = +2 px in UV.
        let mut vel = VelocityField::zero(16, 4);
        for v in vel.v.iter_mut() {
            *v = Vec2::new(2.0 / 16.0, 0.0);
        }
        // The current frame is prev shifted right by 2.
        let mut cur = grey(16, 4, 0);
        for y in 0..4 {
            for x in 2..16 {
                cur.set(x, y, prev.get(x - 2, y));
            }
        }
        let w = warp_by_velocity(&prev, &vel, None);
        // Pixels 0 and 1 came from off-screen (uv < 0 only for the centre of
        // x = 0, 1 → x - 2 < 0 → uv_prev.x < 0).
        for y in 0..4 {
            assert!(!w.valid.get(0, y));
            assert!(!w.valid.get(1, y));
            assert!(w.valid.get(2, y));
        }
        let s = abs_error(&w.image, &cur, Some(&w.valid));
        assert_eq!(s.max, 0, "{s}");
    }

    #[test]
    fn warp_by_half_pixel_is_bilinear() {
        let prev = ramp(16, 1);
        let mut vel = VelocityField::zero(16, 1);
        for v in vel.v.iter_mut() {
            *v = Vec2::new(0.5 / 16.0, 0.0);
        }
        let w = warp_by_velocity(&prev, &vel, None);
        // Pixel 5 fetches position 4.5 → (40 + 50) / 2 = 45.
        assert_eq!(w.image.get(5, 0)[0], 45);
    }

    #[test]
    fn catmull_rom_is_exact_on_integer_shifts_and_linear_ramps() {
        let prev = ramp(16, 4);
        let mut vel = VelocityField::zero(16, 4);
        for v in vel.v.iter_mut() {
            *v = Vec2::new(2.0 / 16.0, 0.0);
        }
        let w = warp_by_velocity_with(&prev, &vel, None, WarpFilter::CatmullRom);
        for x in 3..14 {
            assert_eq!(w.image.get(x, 1)[0], prev.get(x - 2, 1)[0], "x {x}");
        }
        // Half-pixel on a linear ramp: the exact midpoint (bicubic reproduces
        // linear signals).
        for v in vel.v.iter_mut() {
            *v = Vec2::new(0.5 / 16.0, 0.0);
        }
        let w = warp_by_velocity_with(&prev, &vel, None, WarpFilter::CatmullRom);
        assert_eq!(w.image.get(6, 1)[0], 55);
    }

    #[test]
    fn catmull_rom_weights_sum_to_one() {
        for i in 0..=10 {
            let s: f32 = catmull_rom_weights(i as f32 / 10.0).iter().sum();
            assert!((s - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn warp_respects_visibility_mask() {
        let prev = ramp(8, 1);
        let vel = VelocityField::zero(8, 1);
        let mut vis = Mask::new(8, 1, true);
        vis.set(3, 0, false);
        let w = warp_by_velocity(&prev, &vel, Some(&vis));
        assert!(!w.valid.get(3, 0));
        assert_eq!(w.valid.count(), 7);
    }

    #[test]
    fn velocity_consistency_flags_occluder_edges() {
        // Background still (v = 0); an object moving +2 px/frame covers x 4..6 now.
        let size = 16.0;
        let mut prev_v = VelocityField::zero(16, 1);
        let mut cur_v = VelocityField::zero(16, 1);
        // object at 2..4 last frame
        for v in &mut prev_v.v[2..5] {
            *v = Vec2::new(2.0 / size, 0.0);
        }
        // object at 4..6 now
        for v in &mut cur_v.v[4..7] {
            *v = Vec2::new(2.0 / size, 0.0);
        }
        let m = velocity_consistency_mask(&prev_v, &cur_v, 0.1);
        // Object pixels reproject onto object pixels: consistent.
        assert!(m.get(4, 0) && m.get(5, 0) && m.get(6, 0));
        // Background just revealed at x = 2, 3 reprojects (v = 0) onto the
        // object's old position, whose velocity differs: disoccluded.
        assert!(!m.get(2, 0) && !m.get(3, 0));
        // Untouched background is consistent.
        assert!(m.get(10, 0));
    }

    #[test]
    fn depth_visibility_mask_rejects_depth_mismatch() {
        let vel = VelocityField::zero(4, 1);
        let prev_depth = vec![1.0, 1.0, 0.5, 1.0];
        let expected = vec![1.0, 1.0, 1.0, 1.0];
        let m = depth_visibility_mask(&vel, &prev_depth, &expected, 0.05);
        assert_eq!(m.bits, vec![true, true, false, true]);
    }

    /// A soft vertical edge at sub-pixel position `edge` (box-filtered).
    fn edge_image(w: u32, h: u32, edge: f32) -> Image {
        let mut img = grey(w, h, 0);
        for y in 0..h {
            for x in 0..w {
                let cover = ((x as f32 + 1.0) - edge).clamp(0.0, 1.0);
                let v = (cover * 200.0).round() as u8;
                img.set(x, y, [v, v, v, 255]);
            }
        }
        img
    }

    #[test]
    fn shift_estimate_recovers_half_pixel() {
        let a = edge_image(32, 8, 12.25);
        let b = edge_image(32, 8, 12.75);
        let s = estimate_shift_x(&a, &b, 1.0, 0.01, 1.0);
        assert!((s - 0.5).abs() < 0.03, "{s}");
        let s = estimate_shift_x(&b, &a, 1.0, 0.01, 1.0);
        assert!((s + 0.5).abs() < 0.03, "{s}");
    }

    #[test]
    fn shift_estimate_on_aliased_slanted_edge() {
        // Point-sampled (aliased) half-plane x < 20 + 0.3·y, sampled at pixel
        // centres p + 0.5 − j: jitter +0.25 vs −0.25 moves it by +0.5 px.
        let render = |j: f32| {
            let mut img = grey(64, 48, 0);
            for y in 0..48 {
                for x in 0..64 {
                    let sx = x as f32 + 0.5 - j;
                    let sy = y as f32 + 0.5;
                    if sx < 20.0 + 0.3 * sy {
                        img.set(x, y, [220, 220, 220, 255]);
                    }
                }
            }
            img
        };
        let s = estimate_shift_x(&render(-0.25), &render(0.25), 1.0, 0.01, 1.0);
        assert!((s - 0.5).abs() < 0.03, "{s}");
    }

    #[test]
    fn shift_estimate_y() {
        // Transposed edge: horizontal edge at y = 10.25 vs 10.75.
        let mk = |edge: f32| {
            let t = edge_image(24, 6, edge);
            let mut out = grey(6, 24, 0);
            for y in 0..24 {
                for x in 0..6 {
                    out.set(x, y, t.get(y, x));
                }
            }
            out
        };
        let s = estimate_shift_y(&mk(10.25), &mk(10.75), 1.0, 0.01, 1.0);
        assert!((s - 0.5).abs() < 0.03, "{s}");
    }

    #[test]
    fn distance_to_convex_square() {
        let sq = [
            Vec2::new(0.0, 0.0),
            Vec2::new(4.0, 0.0),
            Vec2::new(4.0, 4.0),
            Vec2::new(0.0, 4.0),
        ];
        assert_eq!(distance_to_convex(&sq, Vec2::new(2.0, 2.0)), 0.0);
        assert!((distance_to_convex(&sq, Vec2::new(5.5, 2.0)) - 1.5).abs() < 1e-6);
        assert!((distance_to_convex(&sq, Vec2::new(7.0, 8.0)) - 5.0).abs() < 1e-6);
        let m = within_distance_of_convex(&sq, 10, 10, 1.5);
        // Pixel 5 has centre 5.5: 1.5 from the edge x = 4 → in; pixel 6 out.
        assert!(m.get(5, 2) && !m.get(6, 2));
        let mut one = Mask::new(9, 9, false);
        one.set(4, 4, true);
        let d = within_distance_of_mask(&one, 1.5);
        assert!(d.get(5, 5) && d.get(4, 5) && !d.get(6, 4));
        assert_eq!(d.count(), 9);
    }
}
