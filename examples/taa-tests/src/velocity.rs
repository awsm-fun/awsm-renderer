//! The phase-1 gate, 05 §4.1: motion vectors.
//!
//! `?mode=vectors&scenes=…` runs, per scene and per evaluated frame `k`:
//!
//! - **analytic** (scenes with known motion: `taa-motion`, `taa-orbit`, and
//!   any scene whose tracked object is a rigid box): every drawn pixel's
//!   expected vector is computed on the CPU from its depth (unprojected with
//!   this frame's view-projection), the tracked object's world matrix now and
//!   last frame, and last frame's view-projection. |measured − expected| in
//!   pixels: max ≤ 0.02.
//! - **reprojection**: warp frame `k−1` by frame `k`'s vectors (bilinear) and
//!   compare with frame `k` over pixels visible in both frames: mean < 2 and
//!   p99 < 12 (display 0–255). Visible-in-both = in bounds, drawn (depth > 0,
//!   vector not the (2, 2) "behind last camera" marker), forward-backward
//!   vector consistency (|v_{k−1}(p) − v_k(q)| ≤ 1 px at the reprojected
//!   position p) and depth continuity (relative raw-depth change ≤ 5 %).
//! - **G2**: a lockstep session renders the same frames with a Halton jitter
//!   override; on smooth pixels its vectors must equal the jitter-free ones
//!   (max ≤ 0.02 px). A leaked jitter shows up as ~|Δj| ≈ 0.1–0.6 px.
//!
//! Both sessions run TAA (`AntiAliasing::taa`, which the readbacks need) with
//! `TaaDebugView::FreezeJitter`; in phase 1 there is no resolve, so the image
//! is a plain no-AA render.

use anyhow::{anyhow, Result};
use awsm_renderer::taa::TaaDebugView;
use glam::{Mat4, Vec2, Vec3, Vec4};

use crate::metrics::{self, abs_error, Image, Mask, VelocityField};
use crate::modes::build_reference;
use crate::report::{set_hud, Report};
use crate::session::{gpu_idle, Aa, Session, SessionOpts};
use crate::Params;

/// One rendered frame with everything the vector tests need.
struct VFrame {
    vel: VelocityField,
    /// Raw depth (reverse-Z: 0 = far / nothing drawn).
    depth: Vec<f32>,
    vp: Mat4,
    tracked_world: Option<Mat4>,
}

fn opts(p: &Params, scene: &str) -> SessionOpts {
    SessionOpts {
        scene: scene.to_string(),
        aa: Aa::Taa,
        size: p.size,
        origin: p.bundles.clone(),
        warmup: p.warmup,
    }
}

/// Render the next frame (optionally jittered) and read vectors + depth. (Its
/// image is not used: with the phase-2 resolve it is the accumulated TAA
/// output; plain frames come from the TAA-off session.)
async fn next_vframe(s: &mut Session, jitter: Option<Vec2>) -> Result<VFrame> {
    s.render_next_jittered(jitter)?;
    let vel_fut = s.renderer.debug_read_velocity();
    let depth_fut = s.renderer.debug_read_depth();
    let (vw, vh, v) = vel_fut
        .await
        .map_err(|e| anyhow!("debug_read_velocity: {e}"))?;
    let (dw, dh, depth) = depth_fut
        .await
        .map_err(|e| anyhow!("debug_read_depth: {e}"))?;
    if (vw, vh) != s.opts.size || (dw, dh) != s.opts.size {
        return Err(anyhow!(
            "readback size velocity {vw}x{vh} depth {dw}x{dh} != canvas {:?} (render scale must be 1 in phase 1)",
            s.opts.size
        ));
    }
    let vel = VelocityField {
        width: vw,
        height: vh,
        v: v.chunks_exact(2).map(|c| Vec2::new(c[0], c[1])).collect(),
    };
    let vp = s.view_projection().ok_or_else(|| anyhow!("no camera"))?;
    let tracked_world = s.scenario.tracked.and_then(|(n, _)| s.node_world(n));
    Ok(VFrame {
        vel,
        depth,
        vp,
        tracked_world,
    })
}

fn is_marker(v: Vec2) -> bool {
    (v.x - 2.0).abs() < 1e-3 && (v.y - 2.0).abs() < 1e-3
}

/// The analytic motion model of a scene: what moves, and how.
#[derive(Clone, Copy)]
enum Model {
    /// Only the camera and (optionally) the scenario's tracked rigid box move.
    Rigid(Option<Vec3>),
    /// taa-vertex-hook: the `wave` plane (3 × 1.5 m at (0, 0.5, 0), no
    /// rotation) displaced by y += a·sin(k·x − ω·t); everything else static.
    Wave { t_now: f32, t_prev: f32 },
}

const WAVE_K: f32 = 12.566371;
const WAVE_W: f32 = 4.0;
const WAVE_A: f32 = 0.15;
/// `wave`'s plane segments along x (author.js: segments_x 96 over 3 m).
const WAVE_SEGMENTS_X: u32 = 96;

/// Which scenes have an analytic model, and which.
fn model_for(scene: &str, half: Option<Vec3>, k: u32) -> Option<Model> {
    match scene {
        "taa-motion" | "taa-orbit" | "taa-reveal" | "taa-thin" | "taa-cut"
        | "instancing-stress" | "lod-classic" | "lod-nanite" | "taa-cluster" => {
            Some(Model::Rigid(half))
        }
        "taa-vertex-hook" => Some(Model::Wave {
            t_now: crate::scenario::time_at(k) as f32,
            t_prev: crate::scenario::time_at(k - 1) as f32,
        }),
        _ => None,
    }
}

/// Analytic expected vector (UV) of the surface at pixel `(x, y)` of `cur`.
/// `jitter`: the camera jitter `cur` was rendered with (render px). Pixel `q`
/// then holds the surface at the unjittered position `q + 0.5 − j`, and its
/// vector (unjittered on both ends, G2) is that point's motion.
fn expected_vector(
    cur: &VFrame,
    prev: &VFrame,
    x: u32,
    y: u32,
    model: Model,
    jitter: Vec2,
) -> Option<Vec2> {
    let half = match model {
        Model::Rigid(h) => h,
        Model::Wave { .. } => None,
    };
    let (w, h) = (cur.vel.width as f32, cur.vel.height as f32);
    let d = cur.depth[(y * cur.vel.width + x) as usize];
    if d <= 0.0 {
        return None;
    }
    let uv_now = Vec2::new(
        (x as f32 + 0.5 - jitter.x) / w,
        (y as f32 + 0.5 - jitter.y) / h,
    );
    let ndc = Vec4::new(uv_now.x * 2.0 - 1.0, 1.0 - uv_now.y * 2.0, d, 1.0);
    let world = cur.vp.inverse() * ndc;
    let world = world.truncate() / world.w;
    // A point on the tracked box moves with it; everything else is static.
    let world_prev = match (half, cur.tracked_world, prev.tracked_world) {
        (Some(half), Some(m_now), Some(m_prev)) => {
            let local = m_now.inverse().transform_point3(world);
            let eps = 2e-3;
            let a = local.abs();
            // A point within eps of TWO of the box's face planes sits on an
            // edge, where a surface touching the box (the floor under the
            // resting cube) is indistinguishable from the box by position:
            // ambiguous, so not tested.
            let near_faces = (0..3).filter(|i| (a[*i] - half[*i]).abs() <= eps).count();
            if near_faces >= 2 {
                return None;
            }
            if a.cmple(half + Vec3::splat(eps)).all() {
                m_prev.transform_point3(local)
            } else {
                world
            }
        }
        _ => world,
    };
    let world_prev = match model {
        Model::Wave { t_now, t_prev } => {
            // On the wave plane (within its footprint and displacement band):
            // the point moved vertically by h(x, t_prev) − h(x, t_now).
            // The GPU displaces the 96 x-segments' VERTICES and rasterises flat
            // facets between them, so the surface (and its motion) is the
            // piecewise-linear interpolation of the vertex heights in x (h
            // does not depend on z, so every quad is planar).
            let l = world - Vec3::new(0.0, 0.5, 0.0);
            let seg = 3.0 / WAVE_SEGMENTS_X as f32;
            let fi = ((l.x + 1.5) / seg).clamp(0.0, WAVE_SEGMENTS_X as f32 - 1e-4);
            let i0 = fi.floor();
            let u = fi - i0;
            let (x0, x1) = (-1.5 + i0 * seg, -1.5 + (i0 + 1.0) * seg);
            let hv = |x: f32, t: f32| WAVE_A * (WAVE_K * x - WAVE_W * t).sin();
            let facet = |t: f32| hv(x0, t) * (1.0 - u) + hv(x1, t) * u;
            let h_now = facet(t_now);
            let on_wave =
                l.x.abs() <= 1.5 + 1e-3 && l.z.abs() <= 0.75 + 1e-3 && (l.y - h_now).abs() <= 1e-3;
            if on_wave {
                world + Vec3::new(0.0, facet(t_prev) - h_now, 0.0)
            } else if l.x.abs() <= 1.5 + 0.02
                && l.z.abs() <= 0.75 + 0.02
                && l.y.abs() <= WAVE_A + 0.02
            {
                // Near the wave but not on it (its silhouette): ambiguous.
                return None;
            } else {
                world_prev
            }
        }
        Model::Rigid(_) => world_prev,
    };
    let clip = prev.vp * world_prev.extend(1.0);
    if clip.w <= 1e-6 {
        return None;
    }
    let n = clip.truncate() / clip.w;
    let uv_prev = Vec2::new(n.x * 0.5 + 0.5, 0.5 - n.y * 0.5);
    Some(uv_now - uv_prev)
}

/// One rg16float ulp of each component of a UV vector, in pixels (the
/// velocity target's quantization step at that magnitude: 10 mantissa bits).
fn half_ulp_px(v: Vec2, size: Vec2) -> f32 {
    let ulp = |c: f32| {
        let a = c.abs();
        if a < 6.1e-5 {
            2f32.powi(-24)
        } else {
            2f32.powi(a.log2().floor() as i32 - 10)
        }
    };
    Vec2::new(ulp(v.x) * size.x, ulp(v.y) * size.y).length()
}

/// Per-pixel tolerance: the 05 §4.1 ±0.02 px, or one rg16float ulp of the
/// vector when that is larger (|v| ≳ 20 px/frame: teleports, fast pans), the
/// format's own resolution.
fn tolerance_px(v: Vec2, size: Vec2) -> f32 {
    0.02f32.max(half_ulp_px(v, size))
}

/// Float stats of a list of pixel errors.
fn fstats(mut e: Vec<f32>) -> (usize, f32, f32, f32) {
    if e.is_empty() {
        return (0, 0.0, 0.0, 0.0);
    }
    e.sort_by(|a, b| a.total_cmp(b));
    let n = e.len();
    let mean = e.iter().sum::<f32>() / n as f32;
    let p99 = e[((0.99 * n as f32).ceil() as usize).clamp(1, n) - 1];
    (n, mean, p99, e[n - 1])
}

/// 05 §4.1 analytic.
struct Analytic {
    n: usize,
    mean: f32,
    p99: f32,
    max: f32,
    at: Option<(u32, u32)>,
    /// Pixels above the strict 0.02 px, and above the precision-aware tolerance.
    over_strict: usize,
    over_tol: usize,
    /// |v| (px) at the worst pixel.
    v_at_max: f32,
    field: VelocityField,
}

fn analytic(prev: &VFrame, cur: &VFrame, model: Model, jitter: Vec2) -> Analytic {
    let size = Vec2::new(cur.vel.width as f32, cur.vel.height as f32);
    let mut errs = Vec::new();
    let mut worst = (0.0f32, None);
    let (mut over_strict, mut over_tol, mut v_at_max) = (0usize, 0usize, 0.0f32);
    // The expected field (measured vector where no expectation exists), for
    // the reprojection control.
    let mut field = cur.vel.clone();
    for y in 0..cur.vel.height {
        for x in 0..cur.vel.width {
            let v = cur.vel.get(x, y);
            if is_marker(v) {
                continue;
            }
            if let Some(e) = expected_vector(cur, prev, x, y, model, jitter) {
                field.v[(y * cur.vel.width + x) as usize] = e;
                let err = ((v - e) * size).length();
                if err > worst.0 {
                    worst = (err, Some((x, y)));
                    v_at_max = (e * size).length();
                }
                if err > 0.02 {
                    over_strict += 1;
                }
                if err > tolerance_px(e, size) {
                    over_tol += 1;
                }
                errs.push(err);
            }
        }
    }
    let (n, mean, p99, max) = fstats(errs);
    Analytic {
        n,
        mean,
        p99,
        max,
        at: worst.1,
        over_strict,
        over_tol,
        v_at_max,
        field,
    }
}

/// Visible-in-both mask for reprojection: (with the forward-backward
/// consistency test, without it). Both require drawn, non-marker, in-bounds
/// reprojection and depth continuity (≤ 5 %).
fn visible_in_both(prev: &VFrame, cur: &VFrame) -> (Mask, Mask) {
    let (w, h) = (cur.vel.width, cur.vel.height);
    let consistent = metrics::velocity_consistency_mask(&prev.vel, &cur.vel, 1.0);
    let size = Vec2::new(w as f32, h as f32);
    let mut m = Mask::new(w, h, false);
    let mut nc = Mask::new(w, h, false);
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) as usize;
            let v = cur.vel.v[i];
            let d = cur.depth[i];
            if d <= 0.0 || is_marker(v) {
                continue;
            }
            let p = (Vec2::new(x as f32, y as f32) + 0.5) - v * size;
            if p.x < 0.0 || p.y < 0.0 || p.x >= size.x || p.y >= size.y {
                continue;
            }
            let dp = prev.depth[(p.y as u32 * w + p.x as u32) as usize];
            if dp <= 0.0 || (dp - d).abs() > 0.05 * d.max(dp) {
                continue;
            }
            nc.bits[i] = true;
            m.bits[i] = consistent.bits[i];
        }
    }
    (m, nc)
}

/// Moving pixels of the current frame: drawn, not a marker, |v| > 0.5 px.
fn moving_mask(f: &VFrame) -> Mask {
    let (w, h) = (f.vel.width, f.vel.height);
    let size = Vec2::new(w as f32, h as f32);
    let mut m = Mask::new(w, h, false);
    for (i, v) in f.vel.v.iter().enumerate() {
        m.bits[i] = f.depth[i] > 0.0 && !is_marker(*v) && (*v * size).length() > 0.5;
    }
    m
}

/// 4-connected components of `m` with at least `min_px` pixels.
fn components(m: &Mask, min_px: usize) -> (Vec<Mask>, usize) {
    let (w, h) = (m.width, m.height);
    let mut seen = vec![false; m.bits.len()];
    let mut out = Vec::new();
    let mut small = 0usize;
    for start in 0..m.bits.len() {
        if !m.bits[start] || seen[start] {
            continue;
        }
        let mut stack = vec![start];
        seen[start] = true;
        let mut px = Vec::new();
        while let Some(i) = stack.pop() {
            px.push(i);
            let (x, y) = ((i as u32 % w) as i64, (i as u32 / w) as i64);
            for (dx, dy) in [(1i64, 0i64), (-1, 0), (0, 1), (0, -1)] {
                let (nx, ny) = (x + dx, y + dy);
                if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                    continue;
                }
                let j = (ny * w as i64 + nx) as usize;
                if m.bits[j] && !seen[j] {
                    seen[j] = true;
                    stack.push(j);
                }
            }
        }
        if px.len() >= min_px {
            let mut c = Mask::new(w, h, false);
            for i in px {
                c.bits[i] = true;
            }
            out.push(c);
        } else {
            small += px.len();
        }
    }
    (out, small)
}

/// Per-scene bound on the fraction of MOVING pixels excluded ONLY by the
/// forward-backward consistency test (05 review item 1(ii)), chosen from the
/// measured data on 5c325654 (see README). `None`: not bounded yet (INFO).
fn consistency_bound(scene: &str) -> Option<f64> {
    // Measured on 5c325654 (vectors run with this accounting): 0.00 % in
    // every scene except taa-skin (0.45 % at k30, 2.60 % at k60), taa-hud
    // (7 px) and the cadence variant (36-39 %: skins stepping every 2nd /
    // 4th frame are honestly not constant-velocity).
    Some(match scene {
        "taa-skin" => 0.05,
        "prefab-skinned-morph@cadence" => 0.45,
        _ => 0.01,
    })
}

/// Bilinear sample of a vector field at texel-centre coordinates; `None` if
/// any of the four texels is undrawn, a marker, or not smooth.
fn sample_field(f: &VelocityField, depth: &[f32], smooth: &Mask, pos: Vec2) -> Option<Vec2> {
    let (w, h) = (f.width as i64, f.height as i64);
    let x0 = pos.x.floor() as i64;
    let y0 = pos.y.floor() as i64;
    if x0 < 0 || y0 < 0 || x0 + 1 >= w || y0 + 1 >= h {
        return None;
    }
    let fx = pos.x - x0 as f32;
    let fy = pos.y - y0 as f32;
    let at = |x: i64, y: i64| -> Option<Vec2> {
        let i = (y * w + x) as usize;
        let v = f.v[i];
        (depth[i] > 0.0 && !is_marker(v) && smooth.bits[i]).then_some(v)
    };
    let (a, b, c, d) = (
        at(x0, y0)?,
        at(x0 + 1, y0)?,
        at(x0, y0 + 1)?,
        at(x0 + 1, y0 + 1)?,
    );
    Some((a * (1.0 - fx) + b * fx) * (1.0 - fy) + (c * (1.0 - fx) + d * fx) * fy)
}

/// "Visible in both" without mixed pixels: q is kept only if its 3×3
/// neighbourhood in frame k, and the 3×3 around its reprojected position in
/// frame k−1, are all drawn, free of depth discontinuities (> 5 % relative)
/// and of vector spread (> 0.25 px). A pixel containing two surfaces has no
/// single "previous position".
fn pure_mask(prev: &VFrame, cur: &VFrame) -> Mask {
    let (w, h) = (cur.vel.width, cur.vel.height);
    let size = Vec2::new(w as f32, h as f32);
    let clean = |f: &VFrame, cx: i64, cy: i64| -> bool {
        if cx < 1 || cy < 1 || cx >= w as i64 - 1 || cy >= h as i64 - 1 {
            return false;
        }
        let ci = (cy * w as i64 + cx) as usize;
        let (dc, vc) = (f.depth[ci], f.vel.v[ci]);
        if dc <= 0.0 || is_marker(vc) {
            return false;
        }
        for dy in -1..=1i64 {
            for dx in -1..=1i64 {
                let i = ((cy + dy) * w as i64 + cx + dx) as usize;
                let (d, v) = (f.depth[i], f.vel.v[i]);
                if d <= 0.0 || is_marker(v) {
                    return false;
                }
                if (d - dc).abs() > 0.05 * d.max(dc) {
                    return false;
                }
                if ((v - vc) * size).length() > 0.25 {
                    return false;
                }
            }
        }
        true
    };
    let mut m = Mask::new(w, h, false);
    for y in 0..h {
        for x in 0..w {
            if !clean(cur, x as i64, y as i64) {
                continue;
            }
            let v = cur.vel.get(x, y);
            let p = (Vec2::new(x as f32, y as f32) + 0.5) - v * size;
            if clean(prev, p.x.floor() as i64, p.y.floor() as i64) {
                m.set(x, y, true);
            }
        }
    }
    m
}

/// Pixels of `cur_plain` that change when the no-vector content is hidden,
/// dilated by 1 px.
fn content_mask(plain: &Image, hidden: &Image) -> Mask {
    let mut m = Mask::new(plain.width, plain.height, false);
    for (i, (a, b)) in plain
        .rgba
        .chunks_exact(4)
        .zip(hidden.rgba.chunks_exact(4))
        .enumerate()
    {
        m.bits[i] = a[..3] != b[..3];
    }
    m.dilate(1)
}

/// Names (beyond emitters and lines) of no-vector content per scene.
fn no_vector_names(scene: &str) -> &'static [&'static str] {
    match scene {
        "taa-transparent" => &["glass", "water"],
        _ => &[],
    }
}

/// Smooth pixels (3×3 vector spread ≤ 0.05 px), drawn, not markers.
fn smooth_mask(f: &VFrame) -> Mask {
    let (w, h) = (f.vel.width, f.vel.height);
    let size = Vec2::new(w as f32, h as f32);
    let mut m = Mask::new(w, h, false);
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let c = f.vel.get(x, y);
            if is_marker(c) || f.depth[(y * w + x) as usize] <= 0.0 {
                continue;
            }
            let mut ok = true;
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let n = f.vel.get((x as i32 + dx) as u32, (y as i32 + dy) as u32);
                    if ((n - c) * size).length() > 0.05 {
                        ok = false;
                    }
                }
            }
            m.set(x, y, ok);
        }
    }
    m
}

/// Default evaluation frames per scene (motion phases; ≥ 1).
fn default_frames(s: &Session) -> Vec<u32> {
    match crate::session::base_scene_name(&s.opts.scene) {
        "taa-motion" => vec![60, 120, 240],
        "taa-orbit" => vec![90, 200],
        "taa-reveal" => vec![60, 150],
        "taa-thin" => vec![150, 250],
        "taa-transparent" => vec![60, 150],
        "taa-hud" => vec![90],
        "taa-cut" => vec![60, 130],
        _ => vec![30, 60],
    }
}

/// Resample a JITTERED frame's vector field onto the unjittered pixel grid:
/// jittered pixel q holds the surface at unjittered position q + 0.5 − j, so
/// the unjittered pixel p (centre p + 0.5) is the jittered field at
/// texel-centre coordinates p + j (bilinear; undrawn / marker texels make the
/// pixel invalid).
fn unjitter_field(f: &VFrame, j: Vec2) -> (VelocityField, Mask) {
    let (w, h) = (f.vel.width, f.vel.height);
    let all = Mask::new(w, h, true);
    let mut out = VelocityField::zero(w, h);
    let mut ok = Mask::new(w, h, false);
    for y in 0..h {
        for x in 0..w {
            if let Some(v) = sample_field(&f.vel, &f.depth, &all, Vec2::new(x as f32, y as f32) + j)
            {
                out.v[(y * w + x) as usize] = v;
                ok.set(x, y, true);
            }
        }
    }
    (out, ok)
}

/// Discrete-LOD level per chain and (cluster LOD) a signature of the GPU cut:
/// `(levels, Some((selected clusters, hash)))`.
async fn lod_state(s: &mut Session) -> (Vec<usize>, Option<(u32, u64)>) {
    let levels: Vec<usize> = s
        .renderer
        .lod
        .iter_mut()
        .map(|(_, c)| c.current_level)
        .collect();
    let cut = cluster_cut_signature(&s.renderer).await;
    (levels, cut)
}

/// Read back the cluster-LOD pass's `selected` flags (written by the GPU cut
/// shader each frame) and summarise them.
async fn cluster_cut_signature(r: &awsm_renderer::AwsmRenderer) -> Option<(u32, u64)> {
    use awsm_renderer_core::buffers::{BufferDescriptor, BufferUsage};
    let pass = r.render_passes.cluster_lod.as_ref()?;
    if pass.states.is_empty() {
        return None;
    }
    let mut count = 0u32;
    let mut hash = 0xcbf29ce484222325u64;
    for (mi, st) in pass.states.iter().enumerate() {
        let size = st.cluster_count * 4;
        if size == 0 {
            continue;
        }
        let src = &st.buffers.selected_buffer;
        let dst = r
            .gpu
            .create_buffer(
                &BufferDescriptor::new(
                    Some("taa-tests cut readback"),
                    size as usize,
                    BufferUsage::new().with_copy_dst().with_map_read(),
                )
                .into(),
            )
            .ok()?;
        let enc = r.gpu.create_command_encoder(Some("taa-tests cut readback"));
        enc.copy_buffer_to_buffer(src, 0, &dst, 0, size).ok()?;
        r.gpu.submit_commands(&enc.finish());
        let bytes = awsm_renderer_core::buffers::extract_buffer_vec(&dst, Some(size))
            .await
            .ok()?;
        dst.destroy();
        for (i, c) in bytes.chunks_exact(4).enumerate() {
            if u32::from_le_bytes([c[0], c[1], c[2], c[3]]) != 0 {
                count += 1;
                hash = (hash ^ ((mi as u64) << 32 | i as u64)).wrapping_mul(0x100000001b3);
            }
        }
    }
    Some((count, hash))
}

/// Split the drawn pixels into `k` objects by world position (k-means,
/// initialised by splitting along the axis of largest extent into equal
/// thirds), e.g. the three skinned prefab clones.
fn object_clusters(f: &VFrame, k: usize) -> Vec<Mask> {
    let (w, h) = (f.vel.width, f.vel.height);
    let size = Vec2::new(w as f32, h as f32);
    let inv = f.vp.inverse();
    let mut pts: Vec<(usize, Vec3)> = Vec::new();
    for (i, d) in f.depth.iter().enumerate() {
        if *d <= 0.0 {
            continue;
        }
        let q = Vec2::new((i as u32 % w) as f32 + 0.5, (i as u32 / w) as f32 + 0.5) / size;
        let c = inv * Vec4::new(q.x * 2.0 - 1.0, 1.0 - q.y * 2.0, *d, 1.0);
        pts.push((i, c.truncate() / c.w));
    }
    if pts.len() < k {
        return vec![];
    }
    let (mut lo, mut hi) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
    for (_, p) in &pts {
        lo = lo.min(*p);
        hi = hi.max(*p);
    }
    let ext = hi - lo;
    let axis = if ext.x >= ext.y && ext.x >= ext.z {
        0
    } else if ext.y >= ext.z {
        1
    } else {
        2
    };
    pts.sort_by(|a, b| a.1[axis].total_cmp(&b.1[axis]));
    let mut centres: Vec<Vec3> = (0..k)
        .map(|c| {
            let chunk = &pts[c * pts.len() / k..(c + 1) * pts.len() / k];
            chunk.iter().map(|p| p.1).sum::<Vec3>() / chunk.len().max(1) as f32
        })
        .collect();
    let mut label = vec![0usize; pts.len()];
    for _ in 0..20 {
        for (li, (_, p)) in label.iter_mut().zip(&pts) {
            *li = (0..k)
                .min_by(|a, b| {
                    (*p - centres[*a])
                        .length_squared()
                        .total_cmp(&(*p - centres[*b]).length_squared())
                })
                .unwrap_or(0);
        }
        for (c, centre) in centres.iter_mut().enumerate() {
            let (sum, n) = label
                .iter()
                .zip(&pts)
                .filter(|(l, _)| **l == c)
                .fold((Vec3::ZERO, 0usize), |(s, n), (_, p)| (s + p.1, n + 1));
            if n > 0 {
                *centre = sum / n as f32;
            }
        }
    }
    (0..k)
        .map(|c| {
            let mut m = Mask::new(w, h, false);
            for (l, (i, _)) in label.iter().zip(&pts) {
                if *l == c {
                    m.bits[*i] = true;
                }
            }
            m
        })
        .collect()
}

fn scaled(f: &VelocityField, s: f32) -> VelocityField {
    VelocityField {
        width: f.width,
        height: f.height,
        v: f.v.iter().map(|v| *v * s).collect(),
    }
}

/// Deterministic pre-pass for LOD scenes: the frames at which the discrete
/// level or the cluster cut changes (TAA off, no image readbacks).
async fn lod_switch_frames(p: &Params, scene: &str, max_frames: u32) -> Result<Vec<u32>> {
    let mut s = Session::open(SessionOpts {
        aa: Aa::None,
        ..opts(p, scene)
    })
    .await?;
    let mut prev = None;
    let mut out = Vec::new();
    while s.next_k < max_frames {
        let k = s.render_next()?;
        gpu_idle(&s.renderer).await;
        let st = lod_state(&mut s).await;
        if let Some(pv) = &prev {
            if *pv != st && k > 1 {
                out.push(k);
            }
        }
        prev = Some(st);
    }
    s.destroy();
    Ok(out)
}

async fn vectors_scene(p: &Params, scene: &str, r: &mut Report) -> Result<()> {
    let is_lod = scene.starts_with("lod-") || scene == "taa-cluster";
    let lod_frames = if is_lod && !p.ks_given {
        let f = lod_switch_frames(p, scene, 120).await?;
        r.info(
            &format!("vectors-lod-switches:{scene}"),
            &format!("level / cut changes at frames {f:?} (dolly 1.2× → 12× radius over 2 s)"),
        );
        Some(f)
    } else {
        None
    };
    let mut a = Session::open(opts(p, scene)).await?;
    a.renderer.set_taa_debug_view(TaaDebugView::FreezeJitter);
    let mut b = Session::open(opts(p, scene)).await?;
    b.renderer.set_taa_debug_view(TaaDebugView::None);
    // The reference session: TAA off, jittered 64-phase accumulation (05 §3).
    let mut rs = Session::open(SessionOpts {
        aa: Aa::None,
        ..opts(p, scene)
    })
    .await?;
    // The no-vector-content session: TAA off, emitters / lines / glass /
    // UV-flow hidden, so diffing it against `rs` finds their pixels.
    let mut hs = Session::open(SessionOpts {
        aa: Aa::None,
        ..opts(p, scene)
    })
    .await?;
    for sess in [&a, &b, &rs, &hs] {
        sess.assert_lockstep_safe()?;
    }
    let hidden_names = hs.hide_no_vector_content(no_vector_names(scene));
    let mut hs = if hidden_names.is_empty() {
        hs.destroy();
        None
    } else {
        Some(hs)
    };
    let frames: Vec<u32> = if p.ks_given {
        p.ks.clone()
    } else if let Some(f) = &lod_frames {
        // Up to three switch frames (or the defaults if none switched).
        if f.is_empty() {
            default_frames(&a)
        } else {
            f.iter().take(3).copied().collect()
        }
    } else {
        default_frames(&a)
    };
    let half = a.scenario.tracked.map(|(_, h)| h);
    let last = *frames.iter().max().unwrap_or(&0);
    let size = Vec2::new(p.size.0 as f32, p.size.1 as f32);
    r.info(
        &format!("vectors-setup:{scene}"),
        &format!(
            "frames {frames:?}; drawn-mesh bounds centre {:?} radius {:.3}; all bounds radius {:.3}; no-vector content hidden for the mask: {hidden_names:?}",
            a.setup.visible_center, a.setup.visible_radius, a.setup.bounds_radius
        ),
    );
    let mut prev_a: Option<VFrame> = None;
    let mut prev_b: Option<VFrame> = None;
    let mut prev_ref: Option<Image> = None;
    let mut prev_plain: Option<Image> = None;
    let mut prev_content: Option<Mask> = None;
    let mut prev_lod = None;
    while a.next_k <= last {
        let k = a.next_k;
        let need = frames.contains(&k) || frames.contains(&(k + 1));
        if !need {
            a.render_next()?;
            // B keeps its own Halton jitter every frame.
            b.render_next_jittered(Some(crate::reference::jitter_offset(k, 8)))?;
            rs.render_next()?;
            if let Some(h) = hs.as_mut() {
                h.render_next()?;
                gpu_idle(&h.renderer).await;
            }
            gpu_idle(&a.renderer).await;
            gpu_idle(&b.renderer).await;
            gpu_idle(&rs.renderer).await;
            prev_a = None;
            prev_b = None;
            prev_ref = None;
            prev_plain = None;
            prev_content = None;
            continue;
        }
        let fa = next_vframe(&mut a, None).await?;
        let lod_now = if is_lod {
            Some(lod_state(&mut a).await)
        } else {
            None
        };
        let jb = crate::reference::jitter_offset(k, 8);
        let fb = next_vframe(&mut b, Some(jb)).await?;
        // The plain TAA-off frame (literal no-AA variant), then the reference.
        let (_, plain) = rs.render_next_captured().await?;
        let content = match hs.as_mut() {
            Some(h) => {
                let (_, hidden) = h.render_next_captured().await?;
                content_mask(&plain, &hidden)
            }
            None => Mask::new(plain.width, plain.height, false),
        };
        let (refk, _) = build_reference(&mut rs, p.phases, false).await?;
        set_hud(&format!("taa-tests: vectors {scene} k{k}"));
        // A scripted teleport resets the node's motion history (G10): its
        // expected motion on this frame is zero, i.e. prev = current.
        let teleported = a
            .last_drive
            .as_ref()
            .is_some_and(|d| !d.teleports.is_empty());
        if teleported {
            if let Some(pa) = prev_a.as_mut() {
                pa.tracked_world = fa.tracked_world;
            }
            if let Some(pb) = prev_b.as_mut() {
                pb.tracked_world = fb.tracked_world;
            }
        }
        if frames.contains(&k) {
            let pa = prev_a
                .as_ref()
                .ok_or_else(|| anyhow!("no previous frame for k{k}"))?;
            let pb = prev_b
                .as_ref()
                .ok_or_else(|| anyhow!("no previous frame for k{k}"))?;
            let pref = prev_ref
                .as_ref()
                .ok_or_else(|| anyhow!("no previous reference for k{k}"))?;
            let pplain = prev_plain
                .as_ref()
                .ok_or_else(|| anyhow!("no previous plain frame for k{k}"))?;
            let pcontent = prev_content
                .as_ref()
                .ok_or_else(|| anyhow!("no previous content mask for k{k}"))?;
            if let (Some(before), Some(after)) = (&prev_lod, &lod_now) {
                let (lb, cb): &(Vec<usize>, Option<(u32, u64)>) = before;
                let (la, ca) = after;
                r.emit(
                    &format!("vectors-lod-switch:{scene}:k{k}"),
                    lb != la || cb != ca,
                    &format!("k{} → k{k}: discrete levels {lb:?} → {la:?}; cluster cut (selected, hash) {cb:?} → {ca:?} (must change)", k - 1),
                );
            }
            // (a) analytic.
            let mut control: Option<VelocityField> = None;
            if let Some(model) = model_for(scene, half, k) {
                let an = analytic(pa, &fa, model, Vec2::ZERO);
                r.emit(
                    &format!("vectors-analytic:{scene}:k{k}"),
                    an.n > 0 && an.over_tol == 0,
                    &format!(
                        "{} px: |v − v_expected| mean {:.5} p99 {:.5} max {:.5} px{} (|v| there {:.1} px); {} px > 0.02 px [literal 05 bound: {}], {} px > max(0.02, 1 rg16float ulp of v) (PASS iff the latter is 0)",
                        an.n,
                        an.mean,
                        an.p99,
                        an.max,
                        an.at.map(|(x, y)| format!(" @({x},{y})")).unwrap_or_default(),
                        an.v_at_max,
                        an.over_strict,
                        if an.over_strict == 0 { "PASS" } else { "FAIL-by-precision" },
                        an.over_tol
                    ),
                );
                control = Some(an.field);
            }
            let (vis, vis_nc) = visible_in_both(pa, &fa);
            let drawn = fa.depth.iter().filter(|d| **d > 0.0).count();
            // The 05 §4.1 mask, refined: visible in both (in bounds, drawn,
            // forward-backward consistent, depth-continuous), no mixed pixel
            // (pure_mask), and no no-vector content in either frame.
            let pure = pure_mask(pa, &fa);
            let mut keep = vis.and(&pure);
            // The same without the consistency test (for 1(ii)).
            let mut keep_nc = vis_nc.and(&pure);
            let mut content_px = 0usize;
            for y in 0..fa.vel.height {
                for x in 0..fa.vel.width {
                    let i = (y * fa.vel.width + x) as usize;
                    let v = fa.vel.v[i];
                    let q = (Vec2::new(x as f32, y as f32) + 0.5) - v * size;
                    let prev_hit = q.x >= 0.0
                        && q.y >= 0.0
                        && q.x < size.x
                        && q.y < size.y
                        && pcontent.get(q.x as u32, q.y as u32);
                    if content.bits[i] || prev_hit {
                        if keep.bits[i] {
                            content_px += 1;
                        }
                        keep.bits[i] = false;
                        keep_nc.bits[i] = false;
                    }
                }
            }
            let coverage = keep.count() as f64 / drawn.max(1) as f64;
            // Coverage over MOVING pixels (|v| > 0.5 px), per object
            // (4-connected component of the moving pixels, ≥ 200 px).
            let moving = moving_mask(&fa);
            let (objects, small_px) = components(&moving, 200);
            let mut obj_rows = Vec::new();
            let mut obj_ok = true;
            for (oi, o) in objects.iter().enumerate() {
                let c = o.and(&keep).count() as f64 / o.count().max(1) as f64;
                if c < 0.5 {
                    obj_ok = false;
                }
                obj_rows.push(format!("#{} {} px {:.1}%", oi + 1, o.count(), 100.0 * c));
            }
            let moving_cov = moving.and(&keep).count() as f64 / moving.count().max(1) as f64;
            if !obj_ok || p.keep {
                // Which moving objects lost their coverage: red = moving px
                // excluded, green = moving px kept, over the plain frame.
                let mut img = plain.clone();
                for o in objects.iter() {
                    let c = o.and(&keep).count() as f64 / o.count().max(1) as f64;
                    for (i, b) in o.bits.iter().enumerate() {
                        if *b {
                            let px = if keep.bits[i] {
                                [0, 200, 0, 255]
                            } else if c < 0.5 {
                                [255, 0, 0, 255]
                            } else {
                                [255, 160, 0, 255]
                            };
                            img.rgba[i * 4..i * 4 + 4].copy_from_slice(&px);
                        }
                    }
                }
                crate::store::put(
                    &p.store,
                    &format!(
                        "vectors/{}-k{k:03}-moving-coverage.png",
                        scene.replace('@', "_")
                    ),
                    &img,
                )
                .await?;
            }
            let enough = coverage >= 0.5 && obj_ok;
            let row =
                |st: &metrics::ErrorStats| st.count > 0 && st.mean < 2.0 && (st.p99 as f64) < 12.0;
            let ss_of = |src: &Image,
                         dst: &Image,
                         field: &VelocityField,
                         filter: metrics::WarpFilter,
                         m: &Mask| {
                let w = metrics::warp_by_velocity_with(src, field, Some(m), filter);
                let st = abs_error(&w.image, dst, Some(&w.valid));
                (w, st)
            };
            use metrics::WarpFilter::{Bilinear, CatmullRom};
            // Moving pixels excluded ONLY by the consistency test.
            let cons_only = moving.and(&keep_nc).and(&keep.not());
            let cons_frac = cons_only.count() as f64 / moving.count().max(1) as f64;
            let bound = consistency_bound(scene);
            let cons_ok = bound.is_none_or(|b| cons_frac <= b);
            // (b1) THE GATE: supersampled references, Catmull-Rom, refined mask.
            let (w_gate, gate) = ss_of(pref, &refk, &fa.vel, CatmullRom, &keep);
            let ok_ss = enough && cons_ok && row(&gate);
            let zero = VelocityField::zero(fa.vel.width, fa.vel.height);
            let (_, z) = ss_of(pref, &refk, &zero, CatmullRom, &keep);
            r.emit(
                &format!("vectors-reprojection-ss:{scene}:k{k}"),
                ok_ss,
                &format!(
                    "{}64-phase refs, Catmull-Rom, refined mask: {gate}; coverage {:.1}% of {drawn} drawn px (visible-in-both {:.1}%, then mixed px removed, then {content_px} no-vector-content px removed); MOVING px (|v| > 0.5): {} px, {:.1}% kept, per object [{}]{}; thresholds mean < 2, p99 < 12; v = 0 on the same px: {z}",
                    if !enough {
                        "INSUFFICIENT COVERAGE (< 50 % of drawn px or of a moving object) — "
                    } else if !cons_ok {
                        "TOO MANY MOVING PX EXCLUDED BY CONSISTENCY — "
                    } else {
                        ""
                    },
                    100.0 * coverage,
                    100.0 * vis.count() as f64 / drawn.max(1) as f64,
                    moving.count(),
                    100.0 * moving_cov,
                    obj_rows.join(", "),
                    if small_px > 0 {
                        format!(" (+{small_px} px in components < 200 px, not counted per object)")
                    } else {
                        String::new()
                    }
                ),
            );
            // 1(ii)/(iii): what the consistency test removes, and the warp
            // error there with v_k as-is (control: a wrong vector shows here).
            let (_, cons_err) = ss_of(pref, &refk, &fa.vel, CatmullRom, &cons_only);
            let (_, cons_err0) = ss_of(pref, &refk, &zero, CatmullRom, &cons_only);
            r.info(
                &format!("vectors-consistency-excluded:{scene}:k{k}"),
                &format!(
                    "{} moving px excluded ONLY by forward-backward consistency = {:.2}% of moving px (bound: {}); reference warp error there with v_k as-is: {cons_err} (v = 0: {cons_err0})",
                    cons_only.count(),
                    100.0 * cons_frac,
                    bound
                        .map(|b| format!("{:.1}% → {}", 100.0 * b, if cons_ok { "within" } else { "EXCEEDED" }))
                        .unwrap_or_else(|| "none yet".into())
                ),
            );
            let (_, bil_keep) = ss_of(pref, &refk, &fa.vel, Bilinear, &keep);
            let (_, bil_vis) = ss_of(pref, &refk, &fa.vel, Bilinear, &vis);
            let (_, cr_vis) = ss_of(pref, &refk, &fa.vel, CatmullRom, &vis);
            r.info(
                &format!("vectors-reprojection-ss-variants:{scene}:k{k}"),
                &format!("bilinear+refined: {bil_keep} | bilinear+visible-in-both (previous gate): {bil_vis} | Catmull-Rom+visible-in-both: {cr_vis}"),
            );
            if let Some(field) = &control {
                let (_, c1) = ss_of(pref, &refk, field, CatmullRom, &keep);
                let (_, c2) = ss_of(pref, &refk, field, Bilinear, &keep);
                r.info(
                    &format!("vectors-reprojection-ss-control:{scene}:k{k}"),
                    &format!(
                        "ANALYTIC vectors, same refs + mask: Catmull-Rom {c1} | bilinear {c2}"
                    ),
                );
            }
            // (b2) the literal 05 §4.1 test on no-AA frames (TAA-off session
            // images): kept, labelled as the aliasing floor.
            let (_, lit_cr) = ss_of(pplain, &plain, &fa.vel, CatmullRom, &keep);
            let (_, lit_bil_vis) = ss_of(pplain, &plain, &fa.vel, Bilinear, &vis);
            let mut note = String::new();
            if let Some(field) = &control {
                let (_, sc) = ss_of(pplain, &plain, field, CatmullRom, &keep);
                note = format!("; ANALYTIC vectors, Catmull-Rom+refined: {sc}");
            }
            r.info(
                &format!("vectors-reprojection-noaa:{scene}:k{k}"),
                &format!(
                    "literal no-AA frames (aliasing floor): Catmull-Rom+refined {lit_cr} ({}) | bilinear+visible-in-both {lit_bil_vis}{note}",
                    if row(&lit_cr) { "would PASS" } else { "would FAIL" }
                ),
            );
            if !ok_ss || p.keep {
                let base = format!("vectors/{scene}-k{k:03}");
                crate::store::put(&p.store, &format!("{base}-ref.png"), &refk).await?;
                crate::store::put(&p.store, &format!("{base}-ref-warped.png"), &w_gate.image)
                    .await?;
                let mut err = metrics::error_image(&w_gate.image, &refk, 8.0);
                for (i, v) in w_gate.valid.bits.iter().enumerate() {
                    if !*v {
                        err.rgba[i * 4 + 2] = 90; // excluded pixels tinted blue
                    }
                }
                crate::store::put(&p.store, &format!("{base}-ref-error.png"), &err).await?;
                if let Some(field) = &control {
                    let (wc, _) = ss_of(pref, &refk, field, CatmullRom, &keep);
                    let mut err = metrics::error_image(&wc.image, &refk, 8.0);
                    for (i, v) in wc.valid.bits.iter().enumerate() {
                        if !*v {
                            err.rgba[i * 4 + 2] = 90;
                        }
                    }
                    crate::store::put(&p.store, &format!("{base}-ref-error-analytic.png"), &err)
                        .await?;
                }
            }
            let warped_ss = w_gate;
            // Per-object breakdown + vector-scale sweep (skinned clones, and
            // any scene where `?sweep` asks for it).
            if matches!(
                crate::session::base_scene_name(scene),
                "prefab-skinned-morph" | "taa-skin"
            ) || p.sweep
            {
                let clusters = object_clusters(&fa, if scene == "taa-skin" { 2 } else { 3 });
                for (ci, m) in clusters.iter().enumerate() {
                    // Moving pixels of this object only (|v| > 0.05 px/frame):
                    // static pixels are indifferent to the scale.
                    let mut mm = m.and(&warped_ss.valid);
                    for (i, b) in mm.bits.iter_mut().enumerate() {
                        *b = *b && (fa.vel.v[i] * size).length() > 0.05;
                    }
                    let moving_here = m.and(&moving).count();
                    if moving_here < 200 {
                        r.info(
                            &format!("vectors-object{}:{scene}:k{k}", ci + 1),
                            &format!(
                                "not a moving object: {moving_here} px of its {} px move > 0.5 px/frame",
                                m.count()
                            ),
                        );
                        continue;
                    }
                    if mm.count() < 200 {
                        r.emit(
                            &format!("vectors-object{}:{scene}:k{k}", ci + 1),
                            false,
                            &format!(
                                "insufficient moving pixels: {} kept moving px of {} object px (need 200)",
                                mm.count(),
                                m.count()
                            ),
                        );
                        continue;
                    }
                    let mut row = Vec::new();
                    let mut best = (f64::INFINITY, 0.0f32);
                    let mut curve: Vec<(f32, f64)> = Vec::new();
                    for sc in [0.0f32, 0.5, 0.9, 0.95, 1.0, 1.05, 1.1, 1.5, 2.0] {
                        let wv = metrics::warp_by_velocity_with(
                            pref,
                            &scaled(&fa.vel, sc),
                            Some(&keep),
                            metrics::WarpFilter::CatmullRom,
                        );
                        let st = abs_error(&wv.image, &refk, Some(&mm));
                        if st.mean < best.0 {
                            best = (st.mean, sc);
                        }
                        curve.push((sc, st.mean));
                        row.push(format!("{sc}×v: mean {:.3} p99 {}", st.mean, st.p99));
                    }
                    // Parabola through the 0.95 / 1.0 / 1.05 samples: the
                    // continuous best scale (flat minima make the argmin noisy).
                    let m = |x: f32| {
                        curve
                            .iter()
                            .find(|c| (c.0 - x).abs() < 1e-6)
                            .map(|c| c.1)
                            .unwrap_or(0.0)
                    };
                    let (y0, y1, y2) = (m(0.95), m(1.0), m(1.05));
                    let denom = y0 - 2.0 * y1 + y2;
                    let fit = if denom > 1e-9 {
                        1.0 + 0.05 * 0.5 * (y0 - y2) / denom
                    } else {
                        best.1 as f64
                    };
                    let stat = abs_error(&warped_ss.image, &refk, Some(&mm));
                    r.emit(
                        &format!("vectors-object{}:{scene}:k{k}", ci + 1),
                        best.1 == 1.0,
                        &format!(
                            "{} moving px; at 1×v {stat}; sweep [{}]; sampled minimum at {}×v (must be 1×); parabolic best scale {fit:.3} (1× is {:.2}% above that minimum)",
                            mm.count(),
                            row.join(", "),
                            best.1,
                            100.0 * (y1 - best.0) / best.0.max(1e-9)
                        ),
                    );
                }
            }
            // (c) G2. Exact where the motion is known.
            let dj = jb - crate::reference::jitter_offset(k.wrapping_sub(1), 8);
            if let Some(model) = model_for(scene, half, k) {
                let an = analytic(pb, &fb, model, jb);
                r.emit(
                    &format!("vectors-g2-analytic:{scene}:k{k}"),
                    an.n > 0 && an.over_tol == 0,
                    &format!(
                        "jittered frame (j = ({:+.3}, {:+.3}), Δj = ({:+.3}, {:+.3}) px), {} px: |v − v_expected(q + 0.5 − j)| mean {:.5} p99 {:.5} max {:.5} px (|v| there {:.1} px); {} px > 0.02 [literal: {}], {} px > max(0.02, 1 rg16float ulp) (PASS iff 0)",
                        jb.x, jb.y, dj.x, dj.y, an.n, an.mean, an.p99, an.max, an.v_at_max, an.over_strict,
                        if an.over_strict == 0 { "PASS" } else { "FAIL-by-precision" },
                        an.over_tol
                    ),
                );
            }
            // G2 on every scene: the JITTERED session's vectors, resampled to
            // the unjittered grid, drive the supersampled reprojection.
            let (vj, okj) = unjitter_field(&fb, jb);
            let visj = keep.and(&okj);
            let wj = metrics::warp_by_velocity_with(
                pref,
                &vj,
                Some(&visj),
                metrics::WarpFilter::CatmullRom,
            );
            let sj = abs_error(&wj.image, &refk, Some(&wj.valid));
            r.emit(
                &format!("vectors-g2-ss:{scene}:k{k}"),
                sj.count > 0 && sj.mean < 2.0 && (sj.p99 as f64) < 12.0,
                &format!(
                    "jittered vectors (j = ({:+.3}, {:+.3}), Δj = ({:+.3}, {:+.3}) px) resampled at q + 0.5 − j, supersampled warp: {sj} (frozen-vector warp on these px: {}) (thresholds mean < 2, p99 < 12)",
                    jb.x, jb.y, dj.x, dj.y,
                    abs_error(&warped_ss.image, &refk, Some(&wj.valid))
                ),
            );
            // The earlier sampled comparison, as information (method noise).
            let sm_a = smooth_mask(&fa);
            let sm = sm_a.and(&smooth_mask(&fb));
            let mut diffs = Vec::new();
            for (i, on) in sm.bits.iter().enumerate() {
                if *on {
                    let q = Vec2::new((i as u32 % p.size.0) as f32, (i as u32 / p.size.0) as f32);
                    if let Some(vf) = sample_field(&fa.vel, &fa.depth, &sm_a, q - jb) {
                        diffs.push(((fb.vel.v[i] - vf) * size).length());
                    }
                }
            }
            let over = diffs.iter().filter(|d| **d > 0.02).count();
            let (n, mean, p99, max) = fstats(diffs);
            r.info(
                &format!("vectors-g2-sampled:{scene}:k{k}"),
                &format!("method-noise reference: {n} smooth px |v_jit(q) − v_frozen(q + 0.5 − j)| mean {mean:.5} p99 {p99:.5} max {max:.5} px, {over} px > 0.02"),
            );
        }
        prev_a = Some(fa);
        prev_b = Some(fb);
        prev_ref = Some(refk);
        prev_plain = Some(plain);
        prev_content = Some(content);
        prev_lod = lod_now;
    }
    a.destroy();
    b.destroy();
    rs.destroy();
    if let Some(h) = hs {
        h.destroy();
    }
    Ok(())
}

/// `?mode=vectors`: the 05 §4.1 gate over `scenes`.
pub async fn vectors_mode(p: &Params, r: &mut Report) {
    let scenes: Vec<String> = if p.scenes_given {
        p.scenes.clone()
    } else {
        [
            "taa-motion",
            "taa-orbit",
            "taa-skin",
            "taa-reveal",
            "taa-thin",
            "taa-transparent",
            "taa-hud",
            "taa-cut",
            "taa-vertex-hook",
            "taa-cluster",
            "anim-skinned",
            "anim-morph",
            "prefab-skinned-morph",
            "instancing-stress",
            "lod-classic",
            // lod-nanite's committed bundle has no .clusters.bin (it renders the
            // discrete chain); cluster-LOD vectors are covered by taa-cluster.
            "particles",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    };
    for scene in &scenes {
        if let Err(e) = vectors_scene(p, scene, r).await {
            r.emit(&format!("vectors:{scene}"), false, &format!("error: {e:#}"));
        }
    }
}

/// `?mode=aa-toggle&scenes=taa-vertex-hook`: toggling the AA mode at runtime
/// (none → TAA → MSAA → TAA, each `set_anti_aliasing` + `commit_load`, the
/// editor's sequence) leaves the custom-vertex mesh displaced: each
/// toggled frame equals a fresh session of that mode at the same frame, and
/// the hook mesh visibly moves between two times.
pub async fn aa_toggle_mode(p: &Params, r: &mut Report) {
    let scenes: Vec<String> = if p.scenes_given {
        p.scenes.clone()
    } else {
        vec!["taa-vertex-hook".into()]
    };
    for scene in &scenes {
        if let Err(e) = aa_toggle_scene(p, scene, r).await {
            r.emit(
                &format!("aa-toggle:{scene}"),
                false,
                &format!("error: {e:#}"),
            );
        }
    }
}

async fn fresh_frame(p: &Params, scene: &str, aa: Aa, k: u32) -> Result<Image> {
    let mut s = Session::open(SessionOpts {
        scene: scene.to_string(),
        aa,
        size: p.size,
        origin: p.bundles.clone(),
        warmup: p.warmup,
    })
    .await?;
    if aa == Aa::Taa {
        s.renderer.set_taa_debug_view(TaaDebugView::FreezeJitter);
    }
    s.play_to(k - 1).await?;
    let (_, img) = s.render_next_captured().await?;
    s.destroy();
    Ok(img)
}

async fn aa_toggle_scene(p: &Params, scene: &str, r: &mut Report) -> Result<()> {
    use awsm_renderer::anti_alias::AntiAliasing;
    let k = 20;
    let mut s = Session::open(SessionOpts {
        scene: scene.to_string(),
        aa: Aa::None,
        size: p.size,
        origin: p.bundles.clone(),
        warmup: p.warmup,
    })
    .await?;
    s.play_to(k - 1).await?;
    let (_, none0) = s.render_next_captured().await?;
    // The hook mesh moves with time (compare against a later frame).
    let later = fresh_frame(p, scene, Aa::None, k + 15).await?;
    let mask = crate::gates::tracked_coverage(&s);
    let moved = abs_error(&none0, &later, mask.as_ref());
    r.emit(
        &format!("aa-toggle:{scene}:hook-moves"),
        moved.mean > 1.0,
        &format!(
            "k{k} vs k{} inside the hook mesh's hull: {moved} (must differ: mean > 1)",
            k + 15
        ),
    );
    let fresh_none = fresh_frame(p, scene, Aa::None, k).await?;
    let st = abs_error(&none0, &fresh_none, None);
    r.emit(
        &format!("aa-toggle:{scene}:none"),
        st.max == 0,
        &format!("vs fresh no-AA session: {st}"),
    );
    for (i, aa) in [Aa::Taa, Aa::Msaa, Aa::Taa].into_iter().enumerate() {
        let cfg = match aa {
            Aa::Taa => crate::hooks::taa_native(),
            Aa::Msaa => AntiAliasing::msaa4(),
            Aa::Smaa => AntiAliasing::smaa(),
            Aa::None => AntiAliasing::none(),
        };
        s.renderer
            .set_anti_aliasing(cfg)
            .await
            .map_err(|e| anyhow!("set_anti_aliasing {}: {e}", aa.name()))?;
        // The supported runtime flip (what the editor does): set, then commit.
        s.renderer
            .commit_load(|_| {})
            .await
            .map_err(|e| anyhow!("commit_load after set_anti_aliasing {}: {e}", aa.name()))?;
        if aa == Aa::Taa {
            s.renderer.set_taa_debug_view(TaaDebugView::FreezeJitter);
        }
        s.settle_compiles(20_000).await?;
        // Re-render the same instant a few times so lazily compiled
        // pipelines land, then capture.
        for _ in 0..4 {
            s.render_frozen(None)?;
            gpu_idle(&s.renderer).await;
            s.settle_compiles(20_000).await?;
        }
        let img = s.render_frozen_captured(None).await?;
        let fresh = fresh_frame(p, scene, aa, k).await?;
        let st = abs_error(&img, &fresh, None);
        let in_hook = abs_error(&img, &fresh, mask.as_ref());
        r.emit(
            &format!("aa-toggle:{scene}:step{}-{}", i + 1, aa.name()),
            in_hook.max <= 2 && st.mean < 0.5,
            &format!(
                "toggled vs fresh {} session at k{k}: whole {st}; hook hull {in_hook} (thresholds hull max ≤ 2, whole mean < 0.5)",
                aa.name()
            ),
        );
        if p.keep {
            crate::store::put(
                &p.store,
                &format!("toggle/{scene}-step{}-{}.png", i + 1, aa.name()),
                &img,
            )
            .await?;
        }
    }
    s.destroy();
    Ok(())
}

/// Every test scene with a player bundle.
pub const ALL_BUNDLE_SCENES: &[&str] = &[
    "aa-edges",
    "alpha-cutoff",
    "anim-blend",
    "anim-morph",
    "anim-skinned",
    "atmosphere",
    "bloom-post",
    "builtin-overrides",
    "contact-shadows",
    "cutoff-anim-shadow",
    "cutoff-dynamic",
    "decals",
    "dynamic-material-attributes",
    "dynamic-material-textures",
    "dynamic-materials",
    "env-bc6h-spheres",
    "env-ibl",
    "instancing-stress",
    "isaac-robots",
    "kitchen-sink",
    "lights-many",
    "lod-classic",
    "lod-nanite-open",
    "lod-nanite",
    "mirror",
    "mujoco-capture",
    "mujoco-flex",
    "particles",
    "pbr-extensions",
    "player-cameras",
    "prefab-skinned-morph",
    "prefab-static",
    "shadows-all",
    "sprite",
    "ssr-arena",
    "ssr",
    "taa-cluster",
    "taa-cut",
    "taa-hud",
    "taa-motion",
    "taa-orbit",
    "taa-reveal",
    "taa-skin",
    "taa-thin",
    "taa-transparent",
    "taa-vertex-hook",
    "transparent",
    "volumetrics",
];

/// `?mode=motion-history`: the renderer's byte-exact previous-state
/// self-check (`debug_verify_motion_history`) for `frames` (default 120)
/// frames of every scene, TAA on. PASS only with 0 mismatching bytes in every
/// region on every frame AND a non-zero number of bytes compared.
pub async fn motion_history_mode(p: &Params, r: &mut Report) {
    let scenes: Vec<String> = if p.scenes_given {
        p.scenes.clone()
    } else {
        ALL_BUNDLE_SCENES.iter().map(|s| s.to_string()).collect()
    };
    let frames = p
        .frames
        .iter()
        .copied()
        .max()
        .filter(|_| p.frames_given)
        .unwrap_or(120);
    for scene in &scenes {
        set_hud(&format!("taa-tests: motion-history {scene}…"));
        let res = motion_history_scene(p, scene, frames).await;
        if let Err(e) = res {
            r.emit(
                &format!("motion-history:{scene}"),
                false,
                &format!("error: {e:#}"),
            );
            continue;
        }
        let (ok, detail) = res.unwrap_or_default();
        r.emit(&format!("motion-history:{scene}"), ok, &detail);
    }
}

#[derive(Default)]
struct RegionTotals {
    compared: usize,
    mismatching: usize,
    fresh_compared: usize,
    fresh_mismatching: usize,
    first_bad: Option<String>,
}

async fn motion_history_scene(p: &Params, scene: &str, frames: u32) -> Result<(bool, String)> {
    let mut s = Session::open(opts(p, scene)).await?;
    s.renderer.set_debug_motion_history_check(true);
    let mut totals: std::collections::BTreeMap<&'static str, RegionTotals> = Default::default();
    let mut bad_frames: Vec<u32> = Vec::new();
    let mut no_previous = 0u32;
    for _ in 0..frames {
        let k = s.render_next()?;
        let fut = s.renderer.debug_verify_motion_history();
        let check = fut
            .await
            .map_err(|e| anyhow!("debug_verify_motion_history k{k}: {e}"))?;
        if !check.previous_available {
            no_previous += 1;
        }
        if !check.passed() {
            bad_frames.push(k);
        }
        for reg in &check.regions {
            let t = totals.entry(reg.name).or_default();
            t.compared += reg.bytes_compared;
            t.mismatching += reg.bytes_mismatching;
            t.fresh_compared += reg.fresh_bytes_compared;
            t.fresh_mismatching += reg.fresh_bytes_mismatching;
            if t.first_bad.is_none()
                && (reg.bytes_mismatching > 0 || reg.fresh_bytes_mismatching > 0)
            {
                t.first_bad = Some(format!(
                    "k{k}: prev slot {:?} (byte {:?}), fresh slot {:?} (byte {:?})",
                    reg.first_mismatch_slot,
                    reg.first_mismatch_offset,
                    reg.first_fresh_mismatch_slot,
                    reg.first_fresh_mismatch_offset
                ));
            }
        }
    }
    // Skin cadence of the scene's meshes (diagnostics).
    let mut periods: std::collections::BTreeMap<u8, usize> = Default::default();
    for (_, m) in s.renderer.meshes.iter() {
        *periods.entry(m.skin_update_period).or_default() += 1;
    }
    s.renderer.set_debug_motion_history_check(false);
    s.destroy();
    let compared: usize = totals.values().map(|t| t.compared + t.fresh_compared).sum();
    let mismatching: usize = totals
        .values()
        .map(|t| t.mismatching + t.fresh_mismatching)
        .sum();
    let regions: Vec<String> = totals
        .iter()
        .map(|(n, t)| {
            format!(
                "{n}: {} B compared / {} mismatching, fresh {} / {}{}",
                t.compared,
                t.mismatching,
                t.fresh_compared,
                t.fresh_mismatching,
                t.first_bad
                    .as_ref()
                    .map(|b| format!(" [first: {b}]"))
                    .unwrap_or_default()
            )
        })
        .collect();
    // The cadence variant must actually run skins at periods > 1.
    let cadence_ok = !scene.ends_with("@cadence") || periods.keys().any(|p| *p > 1);
    let ok = mismatching == 0 && compared > 0 && bad_frames.is_empty() && cadence_ok;
    Ok((
        ok,
        format!(
            "{frames} frames ({no_previous} without a previous snapshot); {} B compared in total, {mismatching} mismatching{}; {}; failing frames {:?}; mesh skin_update_period histogram {:?}{}",
            compared,
            if compared == 0 { " — NOTHING COMPARED (cannot pass)" } else { "" },
            regions.join("; "),
            bad_frames.iter().take(12).collect::<Vec<_>>(),
            periods,
            if cadence_ok { "" } else { " — CADENCE VARIANT RAN NO SKIN AT PERIOD > 1" }
        ),
    ))
}

/// One motion-history self-check, summarised.
async fn history_check(s: &Session) -> Result<(bool, usize, usize, String)> {
    let fut = s.renderer.debug_verify_motion_history();
    let c = fut
        .await
        .map_err(|e| anyhow!("debug_verify_motion_history: {e}"))?;
    let compared: usize = c
        .regions
        .iter()
        .map(|r| r.bytes_compared + r.fresh_bytes_compared)
        .sum();
    let bad: usize = c
        .regions
        .iter()
        .map(|r| r.bytes_mismatching + r.fresh_bytes_mismatching)
        .sum();
    let detail: Vec<String> = c
        .regions
        .iter()
        .filter(|r| r.bytes_mismatching + r.fresh_bytes_mismatching > 0)
        .map(|r| {
            format!(
                "{}: {} B prev-mismatch (first slot {:?}), {} B fresh-mismatch",
                r.name, r.bytes_mismatching, r.first_mismatch_slot, r.fresh_bytes_mismatching
            )
        })
        .collect();
    Ok((
        c.passed() && c.previous_available,
        compared,
        bad,
        detail.join("; "),
    ))
}

/// Mean |v| (px) over a mask.
fn mean_speed(f: &VFrame, m: &Mask) -> f32 {
    let size = Vec2::new(f.vel.width as f32, f.vel.height as f32);
    let (mut sum, mut n) = (0.0f32, 0usize);
    for (i, on) in m.bits.iter().enumerate() {
        if *on && !is_marker(f.vel.v[i]) {
            sum += (f.vel.v[i] * size).length();
            n += 1;
        }
    }
    if n == 0 {
        0.0
    } else {
        sum / n as f32
    }
}

/// `?mode=history-burst`: the fresh-slot widening cases (05 §4.1 G10 side):
/// `burst` — taa-motion, 40 new transforms+meshes in NON-ADJACENT recycled
/// slots on frame 60 while the cube moves (> 32 fresh ranges); `density` —
/// taa-motion reduced to 3 transforms, `burst_n` (default 64) new ADJACENT
/// ones on frame 60 (fresh bytes ≥ 60 % of the pre-allocated buffer, ~105
/// slots; 2–5 inserts stay far below it); `reset` —
/// prefab-skinned-morph, `reset_motion_history` on two of the three skinned
/// clones on frame 30 (joint buffer mostly fresh) while the third walks.
/// Every case: the self-check reads 0 mismatching bytes on frames k−1…k+3,
/// and the moving object's vectors on frame k are right (analytic cube within
/// ±0.02 px; the un-reset clone's mean speed within 0.5–2× of frame k−1).
pub async fn history_burst_mode(p: &Params, r: &mut Report) {
    let cases: Vec<String> = if p
        .gates
        .iter()
        .any(|g| g == "ghosting" || g == "convergence")
    {
        vec!["burst".into(), "density".into(), "reset".into()]
    } else {
        p.gates.clone()
    };
    for case in &cases {
        let res = history_burst_case(p, case, r).await;
        if let Err(e) = res {
            r.emit(&format!("history-{case}"), false, &format!("error: {e:#}"));
        }
    }
}

async fn history_burst_case(p: &Params, case: &str, r: &mut Report) -> Result<()> {
    use awsm_renderer::transforms::Transform;
    let scene = if case == "reset" {
        "prefab-skinned-morph"
    } else {
        "taa-motion"
    };
    let kb: u32 = if case == "reset" { 30 } else { 60 };
    let mut s = Session::open(opts(p, scene)).await?;
    s.renderer.set_taa_debug_view(TaaDebugView::FreezeJitter);
    s.renderer.set_debug_motion_history_check(true);
    let half = s.scenario.tracked.map(|(_, h)| h);
    let cube_mesh = s.node("cube").and_then(|h| h.meshes.first().copied());
    let mut spare: Vec<(
        awsm_renderer::transforms::TransformKey,
        awsm_renderer::meshes::MeshKey,
    )> = Vec::new();
    let mut lines = Vec::new();
    let mut ok_all = true;
    let mut prev: Option<VFrame> = None;
    let mut prev_clusters: Option<Vec<f32>> = None;
    while s.next_k <= kb + 3 {
        let k = s.next_k;
        // Setup steps (well before the event).
        if case == "burst" && k == 10 {
            // 80 transforms + meshes, hidden under the floor.
            let cube = cube_mesh.ok_or_else(|| anyhow!("no cube mesh"))?;
            for i in 0..80 {
                let tk = s.renderer.transforms.insert(
                    Transform::IDENTITY.with_translation(Vec3::new(
                        (i % 10) as f32 - 5.0,
                        -3.0,
                        (i / 10) as f32 - 4.0,
                    )),
                    None,
                );
                let mk = s
                    .renderer
                    .duplicate_mesh_with_transform(cube, tk)
                    .map_err(|e| anyhow!("duplicate: {e}"))?;
                spare.push((tk, mk));
            }
        }
        if case == "burst" && k == 20 {
            // Free every other slot: the burst then refills NON-adjacent slots.
            let mut kept = Vec::new();
            for (i, (tk, mk)) in spare.drain(..).enumerate() {
                if i % 2 == 0 {
                    s.renderer.remove_mesh(mk);
                    s.renderer.transforms.remove(tk);
                } else {
                    kept.push((tk, mk));
                }
            }
            spare = kept;
        }
        if case == "density" && k == 5 {
            // Reduce the scene to the cube, the camera and the light.
            if let Some(h) = s.node("floor").cloned() {
                for m in &h.meshes {
                    s.renderer.remove_mesh(*m);
                }
                s.renderer.transforms.remove(h.transform);
            }
        }
        // The event, applied before rendering frame kb.
        if k == kb {
            match case {
                "burst" => {
                    let cube = cube_mesh.ok_or_else(|| anyhow!("no cube mesh"))?;
                    for i in 0..40 {
                        let tk = s.renderer.transforms.insert(
                            Transform::IDENTITY.with_translation(Vec3::new(
                                (i % 10) as f32 - 5.0,
                                -4.0,
                                (i / 10) as f32 - 4.0,
                            )),
                            None,
                        );
                        s.renderer
                            .duplicate_mesh_with_transform(cube, tk)
                            .map_err(|e| anyhow!("duplicate: {e}"))?;
                    }
                }
                "density" => {
                    let cube = cube_mesh.ok_or_else(|| anyhow!("no cube mesh"))?;
                    for i in 0..p.burst_n {
                        let tk = s.renderer.transforms.insert(
                            Transform::IDENTITY.with_translation(Vec3::new(
                                i as f32 - 2.0,
                                -4.0,
                                0.0,
                            )),
                            None,
                        );
                        s.renderer
                            .duplicate_mesh_with_transform(cube, tk)
                            .map_err(|e| anyhow!("duplicate: {e}"))?;
                    }
                }
                _ => {
                    let roots = s.nodes_named("Z_UP");
                    if roots.len() < 3 {
                        return Err(anyhow!(
                            "expected 3+ Z_UP clone roots, found {}",
                            roots.len()
                        ));
                    }
                    for h in roots.iter().take(2) {
                        s.renderer.reset_motion_history(h.transform);
                    }
                }
            }
        }
        if k + 1 < kb {
            s.render_next()?;
            gpu_idle(&s.renderer).await;
            continue;
        }
        let f = next_vframe(&mut s, None).await?;
        let (passed, compared, bad, detail) = history_check(&s).await?;
        let mut line = format!(
            "k{k}: self-check {} ({compared} B compared, {bad} mismatching{})",
            if passed { "clean" } else { "MISMATCH" },
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        );
        if compared == 0 || !passed {
            ok_all = false;
        }
        if let Some(pf) = &prev {
            if case == "reset" {
                let cl = object_clusters(&f, 3);
                let speeds: Vec<f32> = cl.iter().map(|m| mean_speed(&f, m)).collect();
                if k == kb {
                    if let Some(ps) = &prev_clusters {
                        // The clone that was NOT reset must keep moving.
                        let ratios: Vec<f32> = speeds
                            .iter()
                            .zip(ps)
                            .map(|(a, b)| a / b.max(1e-6))
                            .collect();
                        let moving_ok = ratios.iter().any(|q| (0.5..=2.0).contains(q));
                        line += &format!("; per-clone mean |v| {speeds:.3?} px (k−1: {ps:.3?}), ratio {ratios:.2?} — the un-reset clone must stay within 0.5–2×: {}", if moving_ok { "yes" } else { "NO" });
                        ok_all &= moving_ok;
                    }
                }
                prev_clusters = Some(speeds);
            } else if let Some(model) = model_for("taa-motion", half, k) {
                let an = analytic(pf, &f, model, Vec2::ZERO);
                line += &format!(
                    "; analytic vectors: {} px, max err {:.4} px, {} px over tolerance",
                    an.n, an.max, an.over_tol
                );
                if k == kb {
                    ok_all &= an.over_tol == 0 && an.n > 0;
                }
            }
        } else if case == "reset" {
            let cl = object_clusters(&f, 3);
            prev_clusters = Some(cl.iter().map(|m| mean_speed(&f, m)).collect());
        }
        lines.push(line);
        prev = Some(f);
    }
    s.renderer.set_debug_motion_history_check(false);
    s.destroy();
    r.emit(
        &format!("history-{case}:{scene}"),
        ok_all,
        &lines.join(" | "),
    );
    Ok(())
}
