//! `?mode=trail-diag&scenes=taa-motion&kfrom=31&kto=269[&kstep=1]` (diagnostic,
//! INFO lines): where TAA loses to the perfect-disocclusion oracle on the
//! trail band.
//!
//! Every evaluated frame, each static band pixel (covered by the tracked
//! object within the last 30 frames, not now, v = 0) is classified by
//! - ring: within 1.5 px of the current silhouette (the gate's ring);
//! - age: frames since the object last covered it (1, 2, 3-4, 5+);
//! - what the resolve did, from LIVE raw debug views (sessions in lockstep):
//!   D = disoccluded (test 1 / test 2 from raw view 14), F = kept history
//!   but alpha >= 0.99 (raw view 15: from the motion-edge term, the clip
//!   term, or other), H = blended with history (alpha < 0.99);
//! - dilated (raw view 14 R).
//!
//! Per cell over all evaluated frames: pixel count, mean TAA error and mean
//! ORACLE error against the 64-phase reference, and the cell's share of the
//! band-mean excess (sum of TAA - ORACLE error / sum of band pixels), so the
//! cells that cost the gate stand out. Needs `taa-debug-raw` and the raw
//! views 14 / 15 of the diagnostic shader.

#![allow(clippy::needless_range_loop)]

use std::collections::BTreeMap;

use anyhow::{anyhow, Result};
use glam::Vec2;

use crate::gates::{near_silhouette, tracked_coverage};
use crate::metrics::{pixel_error, Image};
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

const WINDOW: u32 = 30;
const ORACLE_CAP: usize = 16;

#[derive(Default, Clone, Copy)]
struct Cell {
    n: u64,
    taa: f64,
    oracle: f64,
    /// Px whose TAA error exceeds the ORACLE's by more than 16.
    ghosts: u64,
}

/// Age-1 estimators: (radius, sharpness) of a Gaussian reconstruction of ONE
/// jittered frame, in the Karis space, per-pixel normalised, clamped borders.
const ESTIMATORS: [(i32, f32); 7] = [
    (1, 2.29),
    (1, 1.5),
    (1, 1.0),
    (1, 0.6),
    (2, 1.0),
    (2, 0.6),
    (2, 0.35),
];

#[allow(clippy::too_many_arguments)]
fn estimate(
    frame: &[[f32; 3]],
    w: u32,
    h: u32,
    x: u32,
    y: u32,
    j: Vec2,
    rad: i32,
    sharp: f32,
) -> [f32; 3] {
    let mut acc = [0.0f32; 3];
    let mut ws = 0.0f32;
    for dy in -rad..=rad {
        for dx in -rad..=rad {
            let px = (x as i32 + dx).clamp(0, w as i32 - 1);
            let py = (y as i32 + dy).clamp(0, h as i32 - 1);
            let d = Vec2::new(dx as f32 - j.x, dy as f32 - j.y);
            let wt = (-sharp * d.length_squared()).exp();
            let c = frame[(py as u32 * w + px as u32) as usize];
            for t in 0..3 {
                acc[t] += wt * c[t];
            }
            ws += wt;
        }
    }
    [acc[0] / ws, acc[1] / ws, acc[2] / ws]
}

fn url_u32(key: &str) -> Option<u32> {
    crate::url_value(key).and_then(|v| v.parse().ok())
}

pub async fn trail_diag_mode(p: &Params, r: &mut Report) {
    let scenes: Vec<String> = if p.scenes_given {
        p.scenes.clone()
    } else {
        vec!["taa-motion".into()]
    };
    for scene in &scenes {
        if let Err(e) = trail_diag(p, scene, r).await {
            r.emit(
                &format!("trail-diag:{scene}"),
                false,
                &format!("error: {e:#}"),
            );
        }
    }
}

fn set_raw(s: &mut Session, id: u32) {
    #[cfg(feature = "taa-debug-raw")]
    s.renderer.set_taa_debug_view_raw(Some(id));
    #[cfg(not(feature = "taa-debug-raw"))]
    let _ = (s, id);
}

async fn trail_diag(p: &Params, scene: &str, r: &mut Report) -> Result<()> {
    use crate::reference::{encode_space, resolve_frame};
    let kfrom = url_u32("kfrom").unwrap_or(31);
    let kstep = url_u32("kstep").unwrap_or(1).max(1);
    let mut t = Session::open(opts(p, scene, Aa::Taa)).await?;
    let mut rs = Session::open(opts(p, scene, Aa::None)).await?;
    let mut os = Session::open(opts(p, scene, Aa::None)).await?;
    let kto = url_u32("kto").unwrap_or(t.scenario.frames.saturating_sub(1));
    let mut raws: Vec<(u32, Session)> = Vec::new();
    for id in [14u32, 15u32] {
        let mut s = Session::open(opts(p, scene, Aa::Taa)).await?;
        set_raw(&mut s, id);
        crate::session::wait_taa_debug_variant(&s.renderer).await;
        raws.push((id, s));
    }
    for sess in [&t, &rs, &os] {
        sess.assert_lockstep_safe()?;
    }
    let (w, h) = p.size;
    let n = (w * h) as usize;
    let mut last_cov: Vec<Option<u32>> = vec![None; n];
    let mut ring: std::collections::VecDeque<(u32, Vec<[f32; 3]>)> = Default::default();
    let mut cells: BTreeMap<String, Cell> = BTreeMap::new();
    let mut band_total: u64 = 0;
    let mut frame_lines: Vec<String> = Vec::new();
    let mut worst_excess: (f64, u32) = (0.0, 0);
    let mut far_acc = (0u64, 0f64, 0f64);
    let mut est_sum = [0.0f64; ESTIMATORS.len()];
    let mut est_ring = [0.0f64; ESTIMATORS.len()];
    let (mut est_n, mut est_ring_n) = (0u64, 0u64);
    while t.next_k <= kto {
        let k = t.next_k;
        let target = k >= kfrom && (k - kfrom) % kstep == 0;
        let (_, test) = t.render_next_captured().await?;
        let j = t.renderer.camera.jitter_px();
        os.render_next_jittered(Some(j))?;
        let ojit = {
            let rb = os.begin_capture()?;
            crate::session::finish_capture(rb, os.opts.size).await?
        };
        // The jittered frame in the reference's accumulation space (for the
        // age-1 estimators).
        let okar: Vec<[f32; 3]> = ojit
            .rgba
            .chunks_exact(4)
            .map(|px| {
                let c = [
                    crate::reference::srgb_to_linear(px[0]),
                    crate::reference::srgb_to_linear(px[1]),
                    crate::reference::srgb_to_linear(px[2]),
                ];
                let kk = if p.karis {
                    1.0 / (1.0 + c[0].max(c[1]).max(c[2]))
                } else {
                    1.0
                };
                [c[0] * kk, c[1] * kk, c[2] * kk]
            })
            .collect();
        ring.push_back((k, resolve_frame(&ojit, j, p.karis)));
        if ring.len() > ORACLE_CAP {
            ring.pop_front();
        }
        let mut raw_imgs: Vec<(u32, Image)> = Vec::new();
        for (id, s) in raws.iter_mut() {
            if target {
                raw_imgs.push((*id, s.render_next_captured().await?.1));
            } else {
                s.render_next()?;
                gpu_idle(&s.renderer).await;
            }
        }
        let hull = tracked_coverage(&t).ok_or_else(|| anyhow!("no tracked coverage"))?;
        if target {
            rs.render_next()?;
            gpu_idle(&rs.renderer).await;
            let reference = reference_cached(&mut rs, p).await?;
            let (vw, _vh, vel) = t
                .renderer
                .debug_read_velocity()
                .await
                .map_err(|e| anyhow!("debug_read_velocity: {e}"))?;
            let size = Vec2::new(w as f32, h as f32);
            let ring15 = near_silhouette(&t, &hull, 1.5);
            let v14 = &raw_imgs.iter().find(|(id, _)| *id == 14).unwrap().1;
            let v15 = &raw_imgs.iter().find(|(id, _)| *id == 15).unwrap().1;
            let (mut fb_n, mut fb_t, mut fb_o) = (0u64, 0f64, 0f64);
            let (mut fr_n, mut fr_t, mut fr_o) = (0u64, 0f64, 0f64);
            // Per frame: band mean if age-1 px used each estimator (others: oracle).
            let mut fe = [0.0f64; ESTIMATORS.len()];
            // Band ages this frame: 1, 2, 3-4, 5-16, 17+.
            let mut ages = [0u32; 5];
            let dump = url_u32("dumpk") == Some(k);
            let mut overlay = if dump { Some(test.clone()) } else { None };
            for i in 0..n {
                if hull.bits[i] {
                    continue;
                }
                let Some(c) = last_cov[i] else { continue };
                let age = k - c;
                if !(1..=WINDOW).contains(&age) {
                    continue;
                }
                let (x, y) = (i as u32 % w, i as u32 / w);
                let vi = (y * vw + x) as usize;
                let v = Vec2::new(vel[vi * 2], vel[vi * 2 + 1]) * size;
                if v.length() >= 0.01 {
                    continue;
                }
                // Oracle at this pixel.
                let nfr = (age as usize).min(ORACLE_CAP);
                let first = k + 1 - nfr as u32;
                let mut acc = [0.0f32; 3];
                let mut cnt = 0.0f32;
                for (fk, f) in ring.iter() {
                    if *fk >= first {
                        for (a, c) in acc.iter_mut().zip(f[i]) {
                            *a += c;
                        }
                        cnt += 1.0;
                    }
                }
                let oc = encode_space([acc[0] / cnt, acc[1] / cnt, acc[2] / cnt], p.karis);
                let refp = reference.get(x, y);
                let et = pixel_error(test.get(x, y), refp) as f64;
                let eo = pixel_error(oc, refp) as f64;
                let a14 = v14.get(x, y);
                let a15 = v15.get(x, y);
                let dil = a14[0] > 127;
                let t1 = a14[1] > 127;
                let t2 = a14[2] > 127;
                let full = a15[0] > 127;
                let edge = a15[1] > 127;
                let clip = a15[2] > 127;
                let what = if t1 || t2 {
                    format!(
                        "D{}{}",
                        if t1 { "1" } else { "" },
                        if t2 { "2" } else { "" }
                    )
                } else if full {
                    format!(
                        "F{}{}",
                        if edge { "e" } else { "" },
                        if clip { "c" } else { "" }
                    )
                } else {
                    "H".to_string()
                };
                let ab = match age {
                    1 => "a1",
                    2 => "a2",
                    3 | 4 => "a3-4",
                    _ => "a5+",
                };
                for (ei, (rad, sh)) in ESTIMATORS.iter().enumerate() {
                    let e = if age == 1 {
                        let c = estimate(&okar, w, h, x, y, j, *rad, *sh);
                        let ee = pixel_error(encode_space(c, p.karis), refp) as f64;
                        est_sum[ei] += ee;
                        if ring15.bits[i] {
                            est_ring[ei] += ee;
                        }
                        ee
                    } else {
                        eo
                    };
                    fe[ei] += e;
                }
                if age == 1 {
                    est_n += 1;
                    if ring15.bits[i] {
                        est_ring_n += 1;
                    }
                }
                ages[match age {
                    1 => 0,
                    2 => 1,
                    3 | 4 => 2,
                    5..=16 => 3,
                    _ => 4,
                }] += 1;
                let rg = if ring15.bits[i] { "ring" } else { "rest" };
                if let Some(o) = overlay.as_mut() {
                    let col = if age == 1 && !(t1 || t2) && !full {
                        if ring15.bits[i] {
                            [255, 0, 0, 255]
                        } else {
                            [255, 255, 0, 255]
                        }
                    } else if age == 1 && (t1 || t2) {
                        [0, 200, 0, 255]
                    } else if age == 1 {
                        [0, 120, 255, 255]
                    } else {
                        let c = o.get(x, y);
                        [c[0] / 2, c[1] / 2, c[2] / 2, 255]
                    };
                    o.set(x, y, col);
                }
                let key = format!("{rg} {ab} {what} {}", if dil { "dil" } else { "own" });
                let e = cells.entry(key).or_default();
                e.n += 1;
                e.taa += et;
                e.oracle += eo;
                if et > eo + 16.0 {
                    e.ghosts += 1;
                    if let Some(o) = overlay.as_mut() {
                        o.set(x, y, [255, 0, 255, 255]);
                    }
                }
                band_total += 1;
                fb_n += 1;
                fb_t += et;
                fb_o += eo;
                if ring15.bits[i] {
                    fr_n += 1;
                    fr_t += et;
                    fr_o += eo;
                }
            }
            // Control: static px far from the object (> 40 px from its hull,
            // never covered), TAA vs the full 16-frame oracle.
            if ring.len() == ORACLE_CAP {
                let far = near_silhouette(&t, &hull, 40.0).not();
                let (mut fn_, mut ft, mut fo) = (0u64, 0f64, 0f64);
                for i in (0..n).step_by(7) {
                    if !far.bits[i] || last_cov[i].is_some() {
                        continue;
                    }
                    let (x, y) = (i as u32 % w, i as u32 / w);
                    let mut acc = [0.0f32; 3];
                    for (_, f) in ring.iter() {
                        for (a, c) in acc.iter_mut().zip(f[i]) {
                            *a += c;
                        }
                    }
                    let m = ring.len() as f32;
                    let oc = encode_space([acc[0] / m, acc[1] / m, acc[2] / m], p.karis);
                    let refp = reference.get(x, y);
                    fn_ += 1;
                    ft += pixel_error(test.get(x, y), refp) as f64;
                    fo += pixel_error(oc, refp) as f64;
                }
                far_acc.0 += fn_;
                far_acc.1 += ft;
                far_acc.2 += fo;
            }
            if let Some(o) = overlay {
                crate::store::put(&p.store, &format!("r2/diag/{scene}-k{k:03}-cells.png"), &o)
                    .await?;
                crate::store::put(&p.store, &format!("r2/diag/{scene}-k{k:03}-taa.png"), &test)
                    .await?;
                crate::store::put(
                    &p.store,
                    &format!("r2/diag/{scene}-k{k:03}-ref.png"),
                    &reference,
                )
                .await?;
            }
            if fb_n > 0 {
                let ex = (fb_t - fb_o) / fb_n as f64;
                if ex > worst_excess.0 {
                    worst_excess = (ex, k);
                }
                let fes: Vec<String> = fe
                    .iter()
                    .map(|v| format!("{:.2}", v / fb_n as f64))
                    .collect();
                frame_lines.push(format!(
                    "k{k}: band {fb_n} TAA {:.2} ORACLE {:.2} | ring {fr_n} TAA {:.2} ORACLE {:.2} | oracle with age-1 estimators [{}] | ages 1/2/3-4/5-16/17+: {:?}",
                    fb_t / fb_n as f64,
                    fb_o / fb_n as f64,
                    if fr_n > 0 { fr_t / fr_n as f64 } else { 0.0 },
                    if fr_n > 0 { fr_o / fr_n as f64 } else { 0.0 },
                    fes.join(" "),
                    ages,
                ));
            }
        } else {
            rs.render_next()?;
            gpu_idle(&rs.renderer).await;
        }
        for i in 0..n {
            if hull.bits[i] {
                last_cov[i] = Some(k);
            }
        }
        set_hud(&format!("taa-tests: trail-diag {scene} k{k}/{kto}"));
    }
    t.destroy();
    rs.destroy();
    os.destroy();
    for (_, s) in raws {
        s.destroy();
    }
    let el: Vec<String> = ESTIMATORS
        .iter()
        .enumerate()
        .map(|(ei, (rad, sh))| {
            format!(
                "{}x{} k={sh}: age-1 band {:.3}, ring {:.3}",
                2 * rad + 1,
                2 * rad + 1,
                est_sum[ei] / est_n.max(1) as f64,
                est_ring[ei] / est_ring_n.max(1) as f64
            )
        })
        .collect();
    r.info(
        &format!("trail-diag:{scene}:age1-estimators"),
        &format!("{est_n} age-1 px-frames ({est_ring_n} in the ring), one jittered frame vs the reference:\n    {}", el.join("\n    ")),
    );
    r.info(
        &format!("trail-diag:{scene}:far-control"),
        &format!(
            "static px > 40 px from the object, never covered (every 7th px): {} px-frames, TAA {:.3} vs 16-frame ORACLE {:.3}",
            far_acc.0,
            far_acc.1 / far_acc.0.max(1) as f64,
            far_acc.2 / far_acc.0.max(1) as f64
        ),
    );
    r.info(
        &format!("trail-diag:{scene}:frames"),
        &format!("\n    {}", frame_lines.join("\n    ")),
    );
    let mut rows: Vec<(String, Cell)> = cells.into_iter().collect();
    rows.sort_by(|a, b| {
        let ea = a.1.taa - a.1.oracle;
        let eb = b.1.taa - b.1.oracle;
        eb.partial_cmp(&ea).unwrap_or(std::cmp::Ordering::Equal)
    });
    let bt = band_total.max(1) as f64;
    let (mut st, mut so) = (0.0, 0.0);
    let lines: Vec<String> = rows
        .iter()
        .map(|(key, c)| {
            st += c.taa;
            so += c.oracle;
            format!(
                "{key:28} n {:8} ({:5.2}%) TAA {:6.2} ORACLE {:6.2} | share of band-mean excess {:+.4} | ghosts (TAA > ORACLE + 16) {}",
                c.n,
                100.0 * c.n as f64 / bt,
                c.taa / c.n as f64,
                c.oracle / c.n as f64,
                (c.taa - c.oracle) / bt,
                c.ghosts
            )
        })
        .collect();
    r.info(
        &format!("trail-diag:{scene}:cells"),
        &format!(
            "static band px over frames {kfrom}..{kto} step {kstep}: {band_total} px-frames; TAA mean {:.3} ORACLE mean {:.3} (excess {:+.3}); worst frame excess {:+.3} @k{}\n    {}",
            st / bt,
            so / bt,
            (st - so) / bt,
            worst_excess.0,
            worst_excess.1,
            lines.join("\n    ")
        ),
    );
    Ok(())
}

/// `?mode=flag-count&scenes=taa-orbit&k=28,29,30,…[&views=14,16]` (INFO): per
/// listed frame, the number of px with each channel of the given raw debug
/// views set (> 127): view 14 = (dilated, test 1, test 2), view 16 = test 2's
/// parts (depth, MIXED, COVERED). One TAA session per view, in lockstep.
pub async fn flag_count_mode(p: &Params, r: &mut Report) {
    let scenes: Vec<String> = if p.scenes_given {
        p.scenes.clone()
    } else {
        vec!["taa-orbit".into()]
    };
    let views: Vec<u32> = crate::url_value("views")
        .map(|v| v.split(',').filter_map(|x| x.parse().ok()).collect())
        .unwrap_or_else(|| vec![14, 16]);
    for scene in &scenes {
        if let Err(e) = flag_count(p, scene, &views, r).await {
            r.emit(
                &format!("flag-count:{scene}"),
                false,
                &format!("error: {e:#}"),
            );
        }
    }
}

async fn flag_count(p: &Params, scene: &str, views: &[u32], r: &mut Report) -> Result<()> {
    let mut sessions: Vec<(u32, Session)> = Vec::new();
    for id in views {
        let mut s = Session::open(opts(p, scene, Aa::Taa)).await?;
        s.assert_lockstep_safe()?;
        set_raw(&mut s, *id);
        crate::session::wait_taa_debug_variant(&s.renderer).await;
        sessions.push((*id, s));
    }
    let last = p.ks.iter().copied().max().unwrap_or(0);
    let frames = sessions
        .first()
        .map(|(_, s)| s.scenario.frames)
        .unwrap_or(0);
    let mut lines = Vec::new();
    for k in 0..=last.min(frames.saturating_sub(1)) {
        let target = p.ks.contains(&k);
        let mut parts = Vec::new();
        for (id, s) in sessions.iter_mut() {
            if target {
                let (_, img) = s.render_next_captured().await?;
                let mut c = [0usize; 3];
                for px in img.rgba.chunks_exact(4) {
                    for ch in 0..3 {
                        if px[ch] > 127 {
                            c[ch] += 1;
                        }
                    }
                }
                parts.push(format!("view{id} {c:?}"));
                if p.keep {
                    crate::store::put(
                        &p.store,
                        &format!("r2/flags/{scene}-k{k:03}-view{id}.png"),
                        &img,
                    )
                    .await?;
                }
            } else {
                s.render_next()?;
                gpu_idle(&s.renderer).await;
            }
        }
        if target {
            lines.push(format!("k{k}: {}", parts.join(" ")));
        }
        set_hud(&format!("taa-tests: flag-count {scene} k{k}"));
    }
    for (_, s) in sessions {
        s.destroy();
    }
    r.info(
        &format!("flag-count:{scene}"),
        &format!("px with the channel set (view 14 = dilated/test1/test2; 16 = depth/MIXED/COVERED):\n    {}", lines.join("\n    ")),
    );
    Ok(())
}
