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
use crate::metrics::{Image, Mask};
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

struct DFrame {
    depth: Vec<f32>,
    vel: Vec<Vec2>,
    jitter: Vec2,
    vp: Mat4,
    inv_proj: Mat4,
    hull: Mask,
}

async fn read_frame(s: &Session) -> Result<DFrame> {
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

/// The CPU replica: (disoccluded mask, d_prev / d_expected per pixel).
fn replicate(prev: &DFrame, cur: &DFrame, w: u32, h: u32) -> (Mask, Vec<f32>) {
    let size = Vec2::new(w as f32, h as f32);
    let (wi, hi) = (w as i64, h as i64);
    let inv_vp = cur.vp.inverse();
    let mut m = Mask::new(w, h, false);
    let mut ratio = vec![1.0f32; (w * h) as usize];
    for y in 0..hi {
        for x in 0..wi {
            // Nearest of the 3×3 (reverse-Z: max raw depth).
            let (mut nd, mut np) = (-1.0f32, (x, y));
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
            if nd <= 0.0 {
                continue; // sky: the shader's d_expected is 1e30, never disoccluded
            }
            let v = cur.vel[(np.1 * wi + np.0) as usize];
            let uv_q = (Vec2::new(x as f32, y as f32) + 0.5) / size;
            let uv_hist = uv_q - v;
            if uv_hist.x < 0.0 || uv_hist.y < 0.0 || uv_hist.x > 1.0 || uv_hist.y > 1.0 {
                continue;
            }
            let pos = uv_hist * size + prev.jitter - 0.5;
            let (x0, y0) = (pos.x.floor() as i64, pos.y.floor() as i64);
            let mut pd = -1.0f32;
            for (ox, oy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let (px, py) = ((x0 + ox).clamp(0, wi - 1), (y0 + oy).clamp(0, hi - 1));
                pd = pd.max(prev.depth[(py * wi + px) as usize]);
            }
            let ndc = Vec2::new(uv_q.x * 2.0 - 1.0, 1.0 - uv_q.y * 2.0)
                + Vec2::new(2.0 * cur.jitter.x, -2.0 * cur.jitter.y) / size;
            let wh = inv_vp * Vec4::new(ndc.x, ndc.y, nd, 1.0);
            let pc = prev.vp * Vec4::new(wh.x / wh.w, wh.y / wh.w, wh.z / wh.w, 1.0);
            let d_expected = view_distance(pc.z / pc.w, &prev.inv_proj);
            let d_prev = view_distance(pd, &prev.inv_proj);
            let i = (y * wi + x) as usize;
            ratio[i] = d_prev / d_expected;
            m.bits[i] = d_prev < d_expected * (1.0 - THRESHOLD);
        }
    }
    (m, ratio)
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
    let mut raws: Vec<(u32, Session)> = Vec::new();
    for id in RAW_VIEWS {
        let mut s = Session::open(opts(p, scene, Aa::Taa)).await?;
        set_raw(&mut s, id);
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
            let (expected, ratio) = replicate(pf, cf, w, h);
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
                    "jitter k−1 ({:+.3},{:+.3}) k ({:+.3},{:+.3}); CPU replica disoccluded {} px (in the age-1 strip of {} px: {}); shader G (live Rejection) {} px (in the strip: {}); both {} px; CPU-only {} px; shader-only {} px; CPU d_prev/d_expected on the strip: mean {:.3}",
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
