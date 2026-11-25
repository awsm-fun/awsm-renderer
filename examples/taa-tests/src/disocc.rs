//! `?mode=disocclusion&scenes=taa-motion&k=32,55` (diagnostic, INFO lines):
//! the resolve's disocclusion test replayed on the CPU from the depth /
//! velocity readbacks of frames k−1 and k, against the shader's own verdict
//! (the G channel of the LIVE Rejection view).
//!
//! The CPU replica follows resolve.wgsl §4.3 / §4.5 / §4.7: per output pixel
//! q, the nearest (reverse-Z: max raw) depth of the 3×3 at k and the velocity
//! there; uv_hist = uv_q − v; the previous depth = nearest of the 2×2 at
//! uv_hist·R + prev_jitter − 0.5 in frame k−1's depth; d_expected = the view
//! distance of the k surface reprojected through frame k−1's view-projection;
//! disoccluded = d_prev < d_expected · (1 − 0.05).
//!
//! With `taa-debug-raw` it also captures the raw views 10 (0.5·d_prev /
//! d_expected), 11 (history length before the update), 12 (alpha) and 13
//! (motion-boundary factor) LIVE at k, compares view 10 on the age-1 strip
//! (covered by the object at k−1, not at k) with the static floor, and runs
//! the static sanity check (`?mode=disocclusion-static`): view 10 after 29
//! static frames must be uniform, and on the first frames the cube moves its
//! old footprint must turn dark.

// Index loops over several parallel per-pixel buffers.
#![allow(clippy::needless_range_loop)]

use anyhow::{anyhow, Result};
use awsm_renderer::taa::TaaDebugView;
use glam::{Mat4, Vec2, Vec4};

use crate::gates::tracked_coverage;
use crate::metrics::{abs_error, Image, Mask};
use crate::modes::reference_cached;
use crate::report::{set_hud, Report};
use crate::session::{gpu_idle, Aa, Session, SessionOpts};
use crate::Params;

fn opts(p: &Params, scene: &str, aa: Aa) -> SessionOpts {
    SessionOpts {
        scene: scene.to_string(),
        aa,
        size: p.size,
        origin: p.bundles.clone(),
        warmup: p.warmup,
    }
}

const THRESHOLD: f32 = 0.05;

pub(crate) struct DFrame {
    depth: Vec<f32>,
    vel: Vec<Vec2>,
    jitter: Vec2,
    vp: Mat4,
    inv_proj: Mat4,
    pub(crate) hull: Mask,
}

pub(crate) async fn read_frame(s: &Session) -> Result<DFrame> {
    let vf = s.renderer.debug_read_velocity();
    let df = s.renderer.debug_read_depth();
    let (_, _, v) = vf.await.map_err(|e| anyhow!("velocity: {e}"))?;
    let (_, _, depth) = df.await.map_err(|e| anyhow!("depth: {e}"))?;
    let m = s
        .renderer
        .camera_matrices()
        .ok_or_else(|| anyhow!("no camera"))?;
    Ok(DFrame {
        depth,
        vel: v.chunks_exact(2).map(|c| Vec2::new(c[0], c[1])).collect(),
        jitter: s.renderer.camera.jitter_px(),
        vp: m.view_projection(),
        inv_proj: m.projection.inverse(),
        hull: tracked_coverage(s).unwrap_or_else(|| Mask::new(s.opts.size.0, s.opts.size.1, false)),
    })
}

fn view_distance(depth: f32, inv_proj: &Mat4) -> f32 {
    if depth <= 0.0 {
        return 1.0e30;
    }
    let v = *inv_proj * Vec4::new(0.0, 0.0, depth, 1.0);
    -v.z / v.w
}

/// Which disocclusion definition the build under test has (`?disocc_def=`):
/// 1 = the nearest-surface ratio test only (5c325654 … 4809e43a); 2 = plus
/// the pixel's own-surface test with the slope-aware slack, previous depth =
/// NEAREST of the 2x2 (8298170a); 3 (default) = the same with the FARTHEST
/// of the 2x2 for the own-surface test (757ad193); 4 = and, for a dilated
/// pixel, test 2 only with relative motion |v − v_own|·O > 0.25 px
/// (8d87a418); 5 (default) = and in that dilated + relative-motion case the
/// footprint's NEAREST depth decides (a2be12b1).
pub(crate) fn definition() -> u32 {
    match crate::url_value("disocc_def").as_deref() {
        Some("v1") => 1,
        Some("v2") => 2,
        Some("v3") => 3,
        Some("v4") => 4,
        _ => 5,
    }
}

/// The CPU replica of resolve.wgsl's disocclusion test (§4.3 / §4.5 / §4.7):
/// (disoccluded mask, test-1 d_prev / d_expected per pixel, test-1 mask,
/// test-2 mask).
/// With `v2`, test 2 (the pixel's own surface) is ORed in.
pub(crate) fn replicate(
    prev: &DFrame,
    cur: &DFrame,
    w: u32,
    h: u32,
    def: u32,
) -> (Mask, Vec<f32>, Mask, Mask) {
    let size = Vec2::new(w as f32, h as f32);
    let (wi, hi) = (w as i64, h as i64);
    let inv_vp = cur.vp.inverse();
    let mut m = Mask::new(w, h, false);
    let mut m1 = Mask::new(w, h, false);
    let mut m2 = Mask::new(w, h, false);
    let mut ratio = vec![1.0f32; (w * h) as usize];
    let at = |f: &DFrame, x: i64, y: i64| {
        f.depth[(y.clamp(0, hi - 1) * wi + x.clamp(0, wi - 1)) as usize]
    };
    // (nearest, farthest) raw depth of the previous 2x2 footprint.
    let prev_footprint = |uv: Vec2| -> (f32, f32) {
        let pos = uv * size + prev.jitter - 0.5;
        let (x0, y0) = (pos.x.floor() as i64, pos.y.floor() as i64);
        let (mut near, mut far) = (-1.0f32, f32::INFINITY);
        for (ox, oy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            let d = at(prev, x0 + ox, y0 + oy);
            near = near.max(d);
            far = far.min(d);
        }
        (near, far)
    };
    let prev_nearest = |uv: Vec2| prev_footprint(uv).0;
    let prev_own = |uv: Vec2| {
        if def >= 3 {
            prev_footprint(uv).1
        } else {
            prev_footprint(uv).0
        }
    };
    // Where the point seen at uv with raw depth z was last frame if it did
    // not move: (previous uv, previous view distance).
    let static_reprojection = |uv: Vec2, z: f32| -> (Vec2, f32) {
        let ndc = Vec2::new(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0)
            + Vec2::new(2.0 * cur.jitter.x, -2.0 * cur.jitter.y) / size;
        let wh = inv_vp * Vec4::new(ndc.x, ndc.y, z, 1.0);
        let pc = prev.vp * Vec4::new(wh.x / wh.w, wh.y / wh.w, wh.z / wh.w, 1.0);
        let pn = pc.truncate() / pc.w;
        (
            Vec2::new(pn.x * 0.5 + 0.5, 0.5 - pn.y * 0.5),
            view_distance(pn.z, &prev.inv_proj),
        )
    };
    for y in 0..hi {
        for x in 0..wi {
            // Nearest of the 3×3 (reverse-Z: max raw depth), starting from
            // the centre so ties keep the centre (the shader's init).
            let z_own = at(cur, x, y);
            let (mut nd, mut np) = (z_own, (x, y));
            for dy in -1..=1i64 {
                for dx in -1..=1i64 {
                    let (px, py) = ((x + dx).clamp(0, wi - 1), (y + dy).clamp(0, hi - 1));
                    let d = cur.depth[(py * wi + px) as usize];
                    if d > nd {
                        nd = d;
                        np = (px, py);
                    }
                }
            }
            let i = (y * wi + x) as usize;
            let uv_q = (Vec2::new(x as f32, y as f32) + 0.5) / size;
            let sky = nd <= 0.0;
            let v = if sky {
                Vec2::ZERO
            } else {
                cur.vel[(np.1 * wi + np.0) as usize]
            };
            let uv_hist = uv_q - v;
            // Test 1: the surface whose vector the pixel uses.
            let d_expected = if sky {
                1.0e30
            } else {
                static_reprojection(uv_q, nd).1
            };
            let d_prev = view_distance(prev_nearest(uv_hist), &prev.inv_proj);
            ratio[i] = d_prev / d_expected;
            let t1 = d_prev < d_expected * (1.0 - THRESHOLD);
            m1.bits[i] = t1;
            let mut dis = t1;
            // Test 2: the pixel's own surface.
            if def >= 2 && z_own > 0.0 {
                let dilated = np != (x, y);
                let v_own = if dilated { cur.vel[i] } else { v };
                let (own_uv, own_z) = static_reprojection(uv_q, z_own);
                let uv_hist_own = uv_q - v_own;
                let is_static = ((uv_hist_own - own_uv) * size).length() < 0.1;
                let d_own = view_distance(z_own, &cur.inv_proj);
                let mut slope = 0.0f32;
                for (ox, oy) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
                    slope = slope
                        .max((view_distance(at(cur, x + ox, y + oy), &cur.inv_proj) - d_own).abs());
                }
                let slack_static = THRESHOLD.min(2.0 * slope / d_own.max(1.0e-6) + 0.002);
                let slack = if is_static { slack_static } else { THRESHOLD };
                let relative_motion = ((v - v_own) * size).length() > 0.25;
                let fp_uv = if dilated { uv_hist_own } else { uv_hist };
                let raw = if def >= 5 && dilated && relative_motion {
                    prev_footprint(fp_uv).0
                } else {
                    prev_own(fp_uv)
                };
                let d_prev_own = view_distance(raw, &prev.inv_proj);
                let gate = def < 4 || !dilated || relative_motion;
                if gate && d_prev_own < own_z * (1.0 - slack) {
                    dis = true;
                    m2.bits[i] = true;
                }
            }
            m.bits[i] = dis;
        }
    }
    (m, ratio, m1, m2)
}

fn green_mask(img: &Image) -> Mask {
    let mut m = Mask::new(img.width, img.height, false);
    for (i, px) in img.rgba.chunks_exact(4).enumerate() {
        m.bits[i] = px[1] > 64 && px[1] as i32 > px[0] as i32 + 32;
    }
    m
}

fn luma(img: &Image, m: &Mask) -> (usize, f64, u8, u8) {
    let (mut n, mut sum, mut lo, mut hi) = (0usize, 0.0f64, 255u8, 0u8);
    for (i, px) in img.rgba.chunks_exact(4).enumerate() {
        if m.bits[i] {
            n += 1;
            sum += px[0] as f64;
            lo = lo.min(px[0]);
            hi = hi.max(px[0]);
        }
    }
    (n, sum / n.max(1) as f64, lo, hi)
}

fn crop_box(m: &Mask, w: u32, h: u32) -> (u32, u32, u32, u32) {
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0u32, 0u32);
    for y in 0..h {
        for x in 0..w {
            if m.get(x, y) {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    let (cw, ch) = (320u32.min(w), 240u32.min(h));
    let cx = (x0 + x1) / 2;
    let cy = (y0 + y1) / 2;
    (
        cx.saturating_sub(cw / 2).min(w - cw),
        cy.saturating_sub(ch / 2).min(h - ch),
        cw,
        ch,
    )
}

/// Tiles side by side (2× nearest), 4 px dark separators.
fn side_by_side(tiles: &[Image]) -> Image {
    let (tw, th) = (tiles[0].width * 2, tiles[0].height * 2);
    let n = tiles.len() as u32;
    let mut out = Image::filled(tw * n + 4 * (n - 1), th, [32, 32, 32, 255]);
    for (ti, t) in tiles.iter().enumerate() {
        let ox = ti as u32 * (tw + 4);
        for y in 0..th {
            for x in 0..tw {
                out.set(ox + x, y, t.get(x / 2, y / 2));
            }
        }
    }
    out
}

fn mask_image(m: &Mask, base: &Image, color: [u8; 4]) -> Image {
    let mut img = base.clone();
    for (i, px) in img.rgba.chunks_exact_mut(4).enumerate() {
        // Darkened frame under the mask colour.
        if m.bits[i] {
            px.copy_from_slice(&color);
        } else {
            for c in px.iter_mut().take(3) {
                *c /= 3;
            }
        }
    }
    img
}

#[cfg(feature = "taa-debug-raw")]
const RAW_VIEWS: [u32; 4] = [10, 11, 12, 13];
#[cfg(not(feature = "taa-debug-raw"))]
const RAW_VIEWS: [u32; 0] = [];

fn set_raw(s: &mut Session, id: u32) {
    #[cfg(feature = "taa-debug-raw")]
    s.renderer.set_taa_debug_view_raw(Some(id));
    #[cfg(not(feature = "taa-debug-raw"))]
    let _ = (s, id);
}

pub async fn disocclusion_mode(p: &Params, r: &mut Report) {
    let scene = if p.scenes_given {
        p.scenes
            .first()
            .cloned()
            .unwrap_or_else(|| "taa-motion".into())
    } else {
        "taa-motion".into()
    };
    let ks: Vec<u32> = if p.ks_given {
        p.ks.clone()
    } else {
        vec![32, 55]
    };
    if let Err(e) = disocclusion(p, &scene, &ks, r).await {
        r.emit(
            &format!("disocclusion:{scene}"),
            false,
            &format!("error: {e:#}"),
        );
    }
}

async fn disocclusion(p: &Params, scene: &str, ks: &[u32], r: &mut Report) -> Result<()> {
    let (w, h) = p.size;
    let mut t = Session::open(opts(p, scene, Aa::Taa)).await?;
    let mut rej = Session::open(opts(p, scene, Aa::Taa)).await?;
    rej.renderer.set_taa_debug_view(TaaDebugView::Rejection);
    crate::session::wait_taa_debug_variant(&rej.renderer).await;
    let mut raws: Vec<(u32, Session)> = Vec::new();
    for id in RAW_VIEWS {
        let mut s = Session::open(opts(p, scene, Aa::Taa)).await?;
        set_raw(&mut s, id);
        crate::session::wait_taa_debug_variant(&s.renderer).await;
        raws.push((id, s));
    }
    let last = *ks.iter().max().unwrap_or(&0);
    let mut prev: Option<DFrame> = None;
    while t.next_k <= last {
        let k = t.next_k;
        let target = ks.contains(&k);
        let need = target || ks.contains(&(k + 1));
        let (_, plain_taa) = t.render_next_captured().await?;
        let cur = if need {
            Some(read_frame(&t).await?)
        } else {
            None
        };
        let rej_img = if target {
            Some(rej.render_next_captured().await?.1)
        } else {
            rej.render_next()?;
            gpu_idle(&rej.renderer).await;
            None
        };
        let mut raw_imgs: Vec<(u32, Image)> = Vec::new();
        for (id, s) in raws.iter_mut() {
            if target {
                raw_imgs.push((*id, s.render_next_captured().await?.1));
            } else {
                s.render_next()?;
                gpu_idle(&s.renderer).await;
            }
        }
        set_hud(&format!("taa-tests: disocclusion {scene} k{k}"));
        if target {
            let pf = prev.as_ref().ok_or_else(|| anyhow!("no k−1 readback"))?;
            let cf = cur.as_ref().ok_or_else(|| anyhow!("no k readback"))?;
            let rej_img = rej_img.ok_or_else(|| anyhow!("no Rejection capture"))?;
            let def = definition();
            let (expected, ratio, test1, _) = replicate(pf, cf, w, h, def);
            let shader = green_mask(&rej_img);
            let both = expected.and(&shader);
            // The age-1 strip: covered by the object at k−1, not at k.
            let strip = pf.hull.and(&cf.hull.not());
            let drawn = {
                let mut m = Mask::new(w, h, false);
                for (i, d) in cf.depth.iter().enumerate() {
                    m.bits[i] = *d > 0.0;
                }
                m
            };
            // Static floor far from the object: drawn, outside both hulls
            // grown by 20 px.
            let far = drawn.and(&pf.hull.or(&cf.hull).dilate(20).not());
            let ratio_strip: Vec<f32> = (0..(w * h) as usize)
                .filter(|i| strip.bits[*i])
                .map(|i| ratio[i])
                .collect();
            let mean_ratio = ratio_strip.iter().sum::<f32>() / ratio_strip.len().max(1) as f32;
            r.info(
                &format!("disocclusion:{scene}:k{k}"),
                &format!(
                    "definition v{}; test 1 alone {} px (strip {}); jitter k−1 ({:+.3},{:+.3}) k ({:+.3},{:+.3}); CPU replica disoccluded {} px (in the age-1 strip of {} px: {}); shader G (live Rejection) {} px (in the strip: {}); both {} px; CPU-only {} px; shader-only {} px; CPU d_prev/d_expected on the strip: mean {:.3}",
                    def,
                    test1.count(),
                    test1.and(&strip).count(),
                    pf.jitter.x,
                    pf.jitter.y,
                    cf.jitter.x,
                    cf.jitter.y,
                    expected.count(),
                    strip.count(),
                    expected.and(&strip).count(),
                    shader.count(),
                    shader.and(&strip).count(),
                    both.count(),
                    expected.and(&shader.not()).count(),
                    shader.and(&expected.not()).count(),
                    mean_ratio
                ),
            );
            for (id, img) in &raw_imgs {
                let (ns, ms, los, his) = luma(img, &strip);
                let (nf, mf, lof, hif) = luma(img, &far);
                r.info(
                    &format!("disocclusion:{scene}:k{k}:raw{id}"),
                    &format!(
                        "LIVE raw view {id} (display-encoded R byte): age-1 strip {ns} px mean {ms:.1} [{los}..{his}] | static floor > 20 px away {nf} px mean {mf:.1} [{lof}..{hif}]"
                    ),
                );
                crate::store::put(
                    &p.store,
                    &format!("phase2/disocc/{scene}-k{k:03}-raw{id}.png"),
                    img,
                )
                .await?;
            }
            let (cx, cy, cw, ch) = crop_box(&pf.hull.or(&cf.hull), w, h);
            let tiles = vec![
                mask_image(&expected, &plain_taa, [0, 255, 0, 255]).crop(cx, cy, cw, ch),
                mask_image(&shader, &plain_taa, [0, 255, 0, 255]).crop(cx, cy, cw, ch),
                {
                    let mut o = Image::filled(w, h, [0, 0, 0, 255]);
                    for i in 0..(w * h) as usize {
                        let px = match (expected.bits[i], shader.bits[i], strip.bits[i]) {
                            (true, true, _) => [255, 255, 0, 255],
                            (true, false, _) => [255, 0, 0, 255],
                            (false, true, _) => [0, 255, 0, 255],
                            (false, false, true) => [60, 60, 160, 255],
                            _ => [0, 0, 0, 255],
                        };
                        o.rgba[i * 4..i * 4 + 4].copy_from_slice(&px);
                    }
                    o.crop(cx, cy, cw, ch)
                },
                rej_img.crop(cx, cy, cw, ch),
            ];
            crate::store::put(
                &p.store,
                &format!("phase2/disocc/disocclusion-k{k:03}.png"),
                &side_by_side(&tiles),
            )
            .await?;
        }
        prev = cur;
    }
    t.destroy();
    rej.destroy();
    for (_, s) in raws {
        s.destroy();
    }
    Ok(())
}

/// `?mode=disocclusion-static`: raw view 10 after 29 static frames (must be
/// uniform 0.5·exposure grey over geometry), then on the first moving frames
/// of taa-motion (k30..33) the cube's old footprint must turn dark.
pub async fn disocclusion_static_mode(p: &Params, r: &mut Report) {
    if let Err(e) = disocclusion_static(p, r).await {
        r.emit("disocclusion-static", false, &format!("error: {e:#}"));
    }
}

async fn disocclusion_static(p: &Params, r: &mut Report) -> Result<()> {
    if RAW_VIEWS.is_empty() {
        return Err(anyhow!("needs the taa-debug-raw build"));
    }
    let (w, h) = p.size;
    let scene = "taa-motion";
    let mut s = Session::open(opts(p, scene, Aa::Taa)).await?;
    set_raw(&mut s, 10);
    crate::session::wait_taa_debug_variant(&s.renderer).await;
    let mut prev_hull: Option<Mask> = None;
    while s.next_k <= 33 {
        let (k, img) = s.render_next_captured().await?;
        let hull = tracked_coverage(&s).unwrap_or_else(|| Mask::new(w, h, false));
        let df = s.renderer.debug_read_depth();
        let (_, _, depth) = df.await.map_err(|e| anyhow!("depth: {e}"))?;
        let mut drawn = Mask::new(w, h, false);
        for (i, d) in depth.iter().enumerate() {
            drawn.bits[i] = *d > 0.0;
        }
        if k == 29 {
            let (n, m, lo, hi) = luma(&img, &drawn);
            r.info(
                "disocclusion-static:k29",
                &format!("after 29 static frames, raw view 10 over the {n} drawn px: mean {m:.1}, range [{lo}..{hi}] (must be uniform)"),
            );
            crate::store::put(&p.store, "phase2/disocc/static-k029-raw10.png", &img).await?;
        }
        if k >= 30 {
            if let Some(ph) = &prev_hull {
                let old = ph.and(&hull.not());
                let far = drawn.and(&ph.or(&hull).dilate(20).not());
                let (no, mo, loo, hio) = luma(&img, &old);
                let (nf, mf, lof, hif) = luma(&img, &far);
                r.info(
                    &format!("disocclusion-static:k{k}"),
                    &format!(
                        "raw view 10: the cube's old footprint (covered at k−1, not at k) {no} px mean {mo:.1} [{loo}..{hio}] vs static floor {nf} px mean {mf:.1} [{lof}..{hif}] (old footprint must be darker)"
                    ),
                );
                crate::store::put(
                    &p.store,
                    &format!("phase2/disocc/static-k{k:03}-raw10.png"),
                    &img,
                )
                .await?;
            }
        }
        prev_hull = Some(hull);
    }
    s.destroy();
    Ok(())
}

/// `?mode=static-rejection&scenes=…`: false disocclusions on a static scene.
/// Time frozen at k0 (camera and scene still, the jitter cycles), 40 frames
/// to converge, then 30 frames of the LIVE Rejection view: the number of
/// pixels flagged disoccluded (G) per frame must be ~0.
pub async fn static_rejection_mode(p: &Params, r: &mut Report) {
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
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    };
    for scene in &scenes {
        let res: Result<String> = async {
            let mut s = Session::open(opts(p, scene, Aa::Taa)).await?;
            s.renderer.set_taa_debug_view(TaaDebugView::Rejection);
            crate::session::wait_taa_debug_variant(&s.renderer).await;
            s.play_to(0).await?;
            for _ in 0..40 {
                s.render_frozen(None)?;
                gpu_idle(&s.renderer).await;
            }
            let (w, h) = p.size;
            let mut counts = Vec::new();
            let mut hits = vec![0u16; (w * h) as usize];
            let base = s.render_frozen_captured(None).await?;
            let mut jit = vec![s.renderer.camera.jitter_px()];
            let mut burst: Option<Image> = None;
            let mut burst_split = String::new();
            let mut prev_df = Some(read_frame(&s).await?);
            for _ in 0..30 {
                let img = s.render_frozen_captured(None).await?;
                jit.push(s.renderer.camera.jitter_px());
                let df = read_frame(&s).await?;
                let g = green_mask(&img);
                if g.count() > counts.iter().copied().max().unwrap_or(0) {
                    burst = Some(mask_image(&g, &base, [0, 255, 0, 255]));
                    if let Some(pf) = &prev_df {
                        let (all, _, t1, t2) = replicate(pf, &df, w, h, definition());
                        let only1 = t1.and(&t2.not()).count();
                        let only2 = t2.and(&t1.not()).count();
                        let both = t1.and(&t2).count();
                        burst_split = format!(
                            "; burst frame, CPU replica (def v{}): {} px (test 1 only {only1}, test 2 only {only2}, both {both}); shader {} px; overlap {}",
                            definition(),
                            all.count(),
                            g.count(),
                            all.and(&g).count()
                        );
                    }
                }
                prev_df = Some(df);
                counts.push(g.count());
                for (i, b) in g.bits.iter().enumerate() {
                    if *b {
                        hits[i] += 1;
                    }
                }
                set_hud(&format!("taa-tests: static-rejection {scene}"));
            }
            s.destroy();
            // Hit map: how often each pixel was flagged (red, brighter =
            // more often) over the darkened Rejection frame.
            let mut map = base.clone();
            for (i, px) in map.rgba.chunks_exact_mut(4).enumerate() {
                if hits[i] > 0 {
                    let v = (80 + hits[i] as u32 * 175 / 30).min(255) as u8;
                    px.copy_from_slice(&[v, 0, 0, 255]);
                } else {
                    for c in px.iter_mut().take(3) {
                        *c /= 4;
                    }
                }
            }
            crate::store::put(&p.store, &format!("phase2/static-rejection/{scene}.png"), &map).await?;
            if let Some(b) = &burst {
                crate::store::put(&p.store, &format!("phase2/static-rejection/{scene}-burst.png"), b)
                    .await?;
            }
            let total: usize = counts.iter().sum();
            let max = counts.iter().max().copied().unwrap_or(0);
            let bi = counts.iter().position(|c| *c == max).unwrap_or(0);
            let jit_note = format!(
                "; burst frame jitter k−1 ({:+.3},{:+.3}) → k ({:+.3},{:+.3}); jitter sequence {:?}",
                jit[bi].x,
                jit[bi].y,
                jit[bi + 1].x,
                jit[bi + 1].y,
                jit.iter().take(9).map(|j| format!("({:+.3},{:+.3})", j.x, j.y)).collect::<Vec<_>>()
            );
            let distinct = hits.iter().filter(|c| **c > 0).count();
            let always = hits.iter().filter(|c| **c >= 30).count();
            Ok(format!(
                "after 40 static frames, 30 frames of the live Rejection view: disoccluded (G) px per frame mean {:.1}, max {max}; {distinct} distinct px ever flagged, {always} flagged on all 30 frames; per frame {:?}{jit_note}{burst_split}",
                total as f64 / counts.len().max(1) as f64,
                counts
            ))
        }
        .await;
        match res {
            Ok(d) => r.info(&format!("static-rejection:{scene}"), &d),
            Err(e) => r.emit(
                &format!("static-rejection:{scene}"),
                false,
                &format!("error: {e:#}"),
            ),
        }
    }
}

/// `?mode=rejection-motion` (taa-orbit by default): under camera motion, per
/// frame of the orbit and strafe phases, the pixels flagged disoccluded by
/// test 1 only / test 2 only / both (CPU replica, `disocc_def`) and by the
/// shader (LIVE Rejection view); and on every strafe frame the TAA error vs
/// the 64-phase reference split into "within 2 px of a parallax silhouette"
/// (3×3 depth jump > 5 % in the current frame, grown by 2 px) and the rest.
pub async fn rejection_motion_mode(p: &Params, r: &mut Report) {
    let scene = if p.scenes_given {
        p.scenes
            .first()
            .cloned()
            .unwrap_or_else(|| "taa-orbit".into())
    } else {
        "taa-orbit".into()
    };
    if let Err(e) = rejection_motion(p, &scene, r).await {
        r.emit(
            &format!("rejection-motion:{scene}"),
            false,
            &format!("error: {e:#}"),
        );
    }
}

fn depth_edges(f: &DFrame, w: u32, h: u32) -> Mask {
    let (wi, hi) = (w as i64, h as i64);
    let mut m = Mask::new(w, h, false);
    for y in 0..hi {
        for x in 0..wi {
            let c = f.depth[(y * wi + x) as usize];
            let dc = view_distance(c, &f.inv_proj);
            let mut edge = false;
            for (ox, oy) in [
                (-1, 0),
                (1, 0),
                (0, -1),
                (0, 1),
                (-1, -1),
                (1, 1),
                (-1, 1),
                (1, -1),
            ] {
                let (nx, ny) = ((x + ox).clamp(0, wi - 1), (y + oy).clamp(0, hi - 1));
                let dn = view_distance(f.depth[(ny * wi + nx) as usize], &f.inv_proj);
                if (dn - dc).abs() > 0.05 * dn.min(dc) {
                    edge = true;
                    break;
                }
            }
            m.bits[(y * wi + x) as usize] = edge;
        }
    }
    m
}

async fn rejection_motion(p: &Params, scene: &str, r: &mut Report) -> Result<()> {
    let (w, h) = p.size;
    let mut t = Session::open(opts(p, scene, Aa::Taa)).await?;
    let mut rej = Session::open(opts(p, scene, Aa::Taa)).await?;
    rej.renderer.set_taa_debug_view(TaaDebugView::Rejection);
    crate::session::wait_taa_debug_variant(&rej.renderer).await;
    let mut rs = Session::open(opts(p, scene, Aa::None)).await?;
    for s in [&t, &rej, &rs] {
        s.assert_lockstep_safe()?;
    }
    let phases = t.scenario.phases.clone();
    let (strafe_a, strafe_b) = t.scenario.phase_range("strafe").unwrap_or((u32::MAX, 0));
    let last = phases
        .iter()
        .filter(|(n, _, _)| *n == "orbit" || *n == "strafe")
        .map(|(_, _, b)| *b)
        .max()
        .unwrap_or(0);
    let first = phases
        .iter()
        .filter(|(n, _, _)| *n == "orbit" || *n == "strafe")
        .map(|(_, a, _)| *a)
        .min()
        .unwrap_or(0);
    let def = definition();
    let mut prev: Option<DFrame> = None;
    // per phase: (frames, Σ t1-only, Σ t2-only, Σ both, Σ shader, max t2)
    let mut acc: std::collections::BTreeMap<&str, (u32, usize, usize, usize, usize, usize)> =
        Default::default();
    let (mut near_w, mut rest_w, mut near_sum, mut rest_sum, mut nf) =
        ((0.0f64, 0u32), (0.0f64, 0u32), 0.0f64, 0.0f64, 0u32);
    while t.next_k < last {
        let k = t.next_k;
        let active = k + 1 >= first;
        let (_, img) = t.render_next_captured().await?;
        let rimg = if k >= first {
            Some(rej.render_next_captured().await?.1)
        } else {
            rej.render_next()?;
            gpu_idle(&rej.renderer).await;
            None
        };
        rs.render_next()?;
        let cur = if active {
            Some(read_frame(&t).await?)
        } else {
            None
        };
        if let (Some(pf), Some(cf), Some(ri)) = (&prev, &cur, &rimg) {
            let (_, _, t1, t2) = replicate(pf, cf, w, h, def);
            let shader = green_mask(ri).count();
            let phase = phases
                .iter()
                .find(|(_, a, b)| k >= *a && k < *b)
                .map(|(n, _, _)| *n)
                .unwrap_or("?");
            let e = acc.entry(phase).or_default();
            let (o1, o2, b) = (
                t1.and(&t2.not()).count(),
                t2.and(&t1.not()).count(),
                t1.and(&t2).count(),
            );
            e.0 += 1;
            e.1 += o1;
            e.2 += o2;
            e.3 += b;
            e.4 += shader;
            e.5 = e.5.max(o2 + b);
            if k >= strafe_a && k < strafe_b {
                let reference = reference_cached(&mut rs, p).await?;
                let near = depth_edges(cf, w, h).dilate(2);
                let sn = abs_error(&img, &reference, Some(&near));
                let sr = abs_error(&img, &reference, Some(&near.not()));
                nf += 1;
                near_sum += sn.mean;
                rest_sum += sr.mean;
                if sn.mean > near_w.0 {
                    near_w = (sn.mean, k);
                }
                if sr.mean > rest_w.0 {
                    rest_w = (sr.mean, k);
                }
            } else {
                gpu_idle(&rs.renderer).await;
            }
        } else {
            gpu_idle(&rs.renderer).await;
        }
        prev = cur;
        set_hud(&format!("taa-tests: rejection-motion {scene} k{k}"));
    }
    t.destroy();
    rej.destroy();
    rs.destroy();
    let rows: Vec<String> = acc
        .iter()
        .map(|(ph, (n, o1, o2, b, sh, m2))| {
            let n = (*n).max(1) as f64;
            format!(
                "{ph}: per frame test 1 only {:.0}, test 2 only {:.0}, both {:.0} (test 2 max {m2}); shader {:.0}",
                *o1 as f64 / n,
                *o2 as f64 / n,
                *b as f64 / n,
                *sh as f64 / n
            )
        })
        .collect();
    r.info(
        &format!("rejection-motion:{scene}"),
        &format!(
            "(replica def v{def})\n    {}\n    strafe k{strafe_a}..{strafe_b}, TAA vs reference: within 2 px of a parallax silhouette avg mean {:.3}, worst {:.3}@k{} | rest avg {:.3}, worst {:.3}@k{}",
            rows.join("\n    "),
            near_sum / nf.max(1) as f64,
            near_w.0,
            near_w.1,
            rest_sum / nf.max(1) as f64,
            rest_w.0,
            rest_w.1
        ),
    );
    Ok(())
}
