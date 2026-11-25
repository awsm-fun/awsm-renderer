//! Diagnostics for failing phase-2 lines (not gates; INFO lines only).
//!
//! `?mode=trail-evidence&scenes=taa-motion&k=32,55`: at each frame k, the
//! trail band split by age (frames since the tracked object last covered the
//! pixel), by cause (within 2 px of the current silhouette; inside the
//! object's shadow footprint now or in the last 30 frames), for TAA, no-AA
//! and MSAA 4× against the same 64-phase reference; the worst pixel with its
//! debug-view values; and full-frame tiles (TAA, reference, |error|×8, no-AA,
//! MSAA, Rejection, HistoryLength, Velocity, band mask) under
//! `phase2/evidence/` plus the crop rectangle for compositing.
//!
//! The shadow footprint is measured, not modelled: a lockstep TAA-off session
//! with the tracked node hidden; pixels that differ from the plain TAA-off
//! frame, minus the object's own (2 px dilated) hull, are what the object
//! changes elsewhere (its shadow, and any reflection).
//!
//! `?mode=speeds&scenes=…`: per scenario phase, the screen speed (px/frame)
//! from the velocity readback: mean / p50 / max over moving pixels, and the
//! speed at the screen centre.

// Per-pixel index loops over several parallel masks / images read clearer
// with an index than zipped iterators.
#![allow(clippy::needless_range_loop)]

use anyhow::{anyhow, Result};
use awsm_renderer::taa::TaaDebugView;
use glam::Vec2;

use crate::gates::{near_silhouette, tracked_coverage};
use crate::metrics::{self, abs_error, pixel_error, Image, Mask};
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

/// Stats of `img` vs `reference` over `mask`, as a short string.
fn st(img: &Image, reference: &Image, mask: &Mask) -> String {
    let s = abs_error(img, reference, Some(mask));
    format!(
        "n {} mean {:.3} p99 {} max {}",
        s.count, s.mean, s.p99, s.max
    )
}

pub async fn trail_evidence_mode(p: &Params, r: &mut Report) {
    let scenes: Vec<String> = if p.scenes_given {
        p.scenes.clone()
    } else {
        vec!["taa-motion".into()]
    };
    let ks: Vec<u32> = if p.ks_given {
        p.ks.clone()
    } else {
        vec![32, 55]
    };
    for scene in &scenes {
        if let Err(e) = trail_evidence(p, scene, &ks, r).await {
            r.emit(
                &format!("trail-evidence:{scene}"),
                false,
                &format!("error: {e:#}"),
            );
        }
    }
}

const DEBUG_VIEWS: [(&str, TaaDebugView); 4] = [
    ("rejection", TaaDebugView::Rejection),
    ("history", TaaDebugView::HistoryLength),
    ("velocity", TaaDebugView::Velocity),
    ("reproj", TaaDebugView::ReprojectionError),
];

/// Oracle history depth: the resolve's native cap.
const ORACLE_CAP: usize = 16;

async fn trail_evidence(p: &Params, scene: &str, ks: &[u32], r: &mut Report) -> Result<()> {
    use crate::reference::{encode_space, resolve_frame};
    let mut t = Session::open(opts(p, scene, Aa::Taa)).await?;
    let mut rs = Session::open(opts(p, scene, Aa::None)).await?;
    let mut ms = Session::open(opts(p, scene, Aa::Msaa)).await?;
    let mut sm = Session::open(opts(p, scene, Aa::Smaa)).await?;
    let mut hs = Session::open(opts(p, scene, Aa::None)).await?;
    // The oracle's input: TAA-off frames jittered exactly like the TAA
    // session's frame (its packed jitter, read after it renders).
    let mut os = Session::open(opts(p, scene, Aa::None)).await?;
    // LIVE debug views: TAA sessions with the view set from the start, played
    // in lockstep (the view replaces the output; history accumulates as usual).
    let mut dv: Vec<(&str, Session)> = Vec::new();
    for (name, view) in DEBUG_VIEWS {
        let mut d = Session::open(opts(p, scene, Aa::Taa)).await?;
        d.renderer.set_taa_debug_view(view);
        crate::session::wait_taa_debug_variant(&d.renderer).await;
        dv.push((name, d));
    }
    for sess in [&t, &rs, &ms, &hs, &os] {
        sess.assert_lockstep_safe()?;
    }
    let (tracked, _) = t
        .scenario
        .tracked
        .ok_or_else(|| anyhow!("{scene} has no tracked object"))?;
    let hidden = hs.hide_no_vector_content(&[tracked]);
    if !hidden.iter().any(|h| h == tracked) {
        return Err(anyhow!("could not hide `{tracked}`"));
    }
    let (w, h) = p.size;
    let n = (w * h) as usize;
    let mut last_cov: Vec<Option<u32>> = vec![None; n];
    let mut last_shadow: Vec<Option<u32>> = vec![None; n];
    // Ring of the last ORACLE_CAP per-frame resolve-model reconstructions.
    let mut ring: std::collections::VecDeque<(u32, Vec<[f32; 3]>)> = Default::default();
    let last = *ks.iter().max().unwrap_or(&0);
    let mut jitter_check: Option<String> = None;
    let mut prev_df: Option<crate::disocc::DFrame> = None;
    let mut uncover_ratio = vec![f32::NAN; n];
    while t.next_k <= last {
        let k = t.next_k;
        let target = ks.contains(&k);
        let (_, test) = t.render_next_captured().await?;
        let j = t.renderer.camera.jitter_px();
        // The disocclusion test's own ratio on each pixel's uncovering frame
        // (CPU replica of the resolve, see disocc.rs): "contact" pixels are
        // those with d_prev / d_expected in [0.95, 1.0), which the 5 % ratio
        // cannot flag.
        let df = crate::disocc::read_frame(&t).await?;
        if let Some(pf) = &prev_df {
            let (_, ratio, _, _) =
                crate::disocc::replicate(pf, &df, w, h, crate::disocc::definition());
            for i in 0..n {
                if pf.hull.bits[i] && !df.hull.bits[i] {
                    uncover_ratio[i] = ratio[i];
                }
            }
        }
        prev_df = Some(df);
        let (_, plain) = rs.render_next_captured().await?;
        let (_, hidden_img) = hs.render_next_captured().await?;
        os.render_next_jittered(Some(j))?;
        let ojit = finish_capture_now(&mut os).await?;
        ring.push_back((k, resolve_frame(&ojit, j, p.karis)));
        if ring.len() > ORACLE_CAP {
            ring.pop_front();
        }
        let msaa = if target {
            Some(ms.render_next_captured().await?.1)
        } else {
            ms.render_next()?;
            gpu_idle(&ms.renderer).await;
            None
        };
        let smaa = if target {
            Some(sm.render_next_captured().await?.1)
        } else {
            sm.render_next()?;
            gpu_idle(&sm.renderer).await;
            None
        };
        let mut dimgs: Vec<(&str, Option<Image>)> = Vec::new();
        for (name, d) in dv.iter_mut() {
            if target {
                dimgs.push((name, Some(d.render_next_captured().await?.1)));
            } else {
                d.render_next()?;
                gpu_idle(&d.renderer).await;
            }
        }
        let hull = tracked_coverage(&t).ok_or_else(|| anyhow!("no tracked coverage"))?;
        let hull2 = near_silhouette(&t, &hull, 2.0);
        let mut shadow = Mask::new(w, h, false);
        for i in 0..n {
            let a = &plain.rgba[i * 4..i * 4 + 3];
            let b = &hidden_img.rgba[i * 4..i * 4 + 3];
            shadow.bits[i] = a != b && !hull2.bits[i];
        }
        set_hud(&format!("taa-tests: trail-evidence {scene} k{k}"));
        if target {
            let reference = reference_cached(&mut rs, p).await?;
            let msaa = msaa.unwrap_or_else(|| plain.clone());
            let smaa = smaa.unwrap_or_else(|| plain.clone());
            let get = |name: &str| -> Image {
                dimgs
                    .iter()
                    .find(|(n, _)| *n == name)
                    .and_then(|(_, i)| i.clone())
                    .unwrap_or_else(|| Image::filled(w, h, [0, 0, 0, 255]))
            };
            let (rej, hist, velv, reproj) = (
                get("rejection"),
                get("history"),
                get("velocity"),
                get("reproj"),
            );
            let (vw, _vh, vel) = t
                .renderer
                .debug_read_velocity()
                .await
                .map_err(|e| anyhow!("debug_read_velocity: {e}"))?;
            let mut band = Mask::new(w, h, false);
            let mut age = vec![0u32; n];
            for i in 0..n {
                if hull.bits[i] {
                    continue;
                }
                if let Some(c) = last_cov[i] {
                    let a = k - c;
                    if (1..=WINDOW).contains(&a) {
                        band.bits[i] = true;
                        age[i] = a;
                    }
                }
            }
            let mut shadow_win = shadow.clone();
            for i in 0..n {
                if let Some(c) = last_shadow[i] {
                    if k - c <= WINDOW {
                        shadow_win.bits[i] = true;
                    }
                }
            }
            // Static band pixels (v = 0) and the oracle there.
            let size = Vec2::new(w as f32, h as f32);
            let mut stat = Mask::new(w, h, false);
            for i in 0..n {
                let (x, y) = (i as u32 % w, i as u32 / w);
                let vi = (y * vw + x) as usize;
                let v = Vec2::new(vel[vi * 2], vel[vi * 2 + 1]) * size;
                stat.bits[i] = band.bits[i] && v.length() < 0.01;
            }
            let mut oracle = test.clone();
            for i in 0..n {
                if !stat.bits[i] {
                    continue;
                }
                let nfr = (age[i] as usize).min(ORACLE_CAP);
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
                let c = [acc[0] / cnt, acc[1] / cnt, acc[2] / cnt];
                let px = encode_space(c, p.karis);
                oracle.rgba[i * 4..i * 4 + 4].copy_from_slice(&px);
            }
            // Jitter / oracle self-check once: on static pixels far from the
            // object and its shadow, the 16-frame oracle must sit at the
            // resolve-model floor against the reference.
            if jitter_check.is_none() && ring.len() == ORACLE_CAP {
                let mut far = near_silhouette(&t, &hull, 40.0)
                    .or(&shadow_win.dilate(3))
                    .not();
                // Only pixels the object has not covered within the oracle's
                // 16 frames (the self-check averages all 16).
                for i in 0..n {
                    if last_cov[i].is_some_and(|c| k - c <= ORACLE_CAP as u32) {
                        far.bits[i] = false;
                    }
                }
                let mut full = test.clone();
                for i in 0..n {
                    let mut acc = [0.0f32; 3];
                    for (_, f) in ring.iter() {
                        for (a, c) in acc.iter_mut().zip(f[i]) {
                            *a += c;
                        }
                    }
                    let m = ring.len() as f32;
                    full.rgba[i * 4..i * 4 + 4].copy_from_slice(&encode_space(
                        [acc[0] / m, acc[1] / m, acc[2] / m],
                        p.karis,
                    ));
                }
                let msg = format!(
                    "16-frame oracle on pixels > 40 px from the object, off its shadow, and not covered by it within 16 frames: {} (the resolve-model floor is ~0.1 on taa-motion; a wrong jitter sign or pixel-centre convention would show here as edge errors)",
                    st(&full, &reference, &far)
                );
                r.info(
                    &format!("trail-evidence:{scene}:oracle-self-check:k{k}"),
                    &msg,
                );
                jitter_check = Some(msg);
            }
            let ring15 = near_silhouette(&t, &hull, 1.5).and(&band);
            let rest15 = band.and(&ring15.not());
            r.info(
                &format!("trail-evidence:{scene}:k{k}:band"),
                &format!(
                    "band {} px ({} static, v = 0); object hull now {} px; shadow footprint now {} px, within the last {WINDOW} frames {} px.\n    literal band: TAA {} | no-AA {} | MSAA {}\n    band minus the 1.5 px ring: TAA {} | no-AA {} | MSAA {}\n    the 1.5 px ring alone: TAA {} | no-AA {} | MSAA {}",
                    band.count(),
                    stat.count(),
                    hull.count(),
                    shadow.count(),
                    shadow_win.count(),
                    st(&test, &reference, &band),
                    st(&plain, &reference, &band),
                    st(&msaa, &reference, &band),
                    st(&test, &reference, &rest15),
                    st(&plain, &reference, &rest15),
                    st(&msaa, &reference, &rest15),
                    st(&test, &reference, &ring15),
                    st(&plain, &reference, &ring15),
                    st(&msaa, &reference, &ring15),
                ),
            );
            // Oracle vs TAA / no-AA / MSAA on the static band pixels.
            let mut contact = Mask::new(w, h, false);
            for i in 0..n {
                let q = uncover_ratio[i];
                contact.bits[i] = stat.bits[i] && (0.95..1.0).contains(&q);
            }
            let mut lines = Vec::new();
            for (label, m) in [
                ("static band (literal)", stat.clone()),
                ("static band minus the 1.5 px ring", stat.and(&ring15.not())),
                ("static band, 1.5 px ring alone", stat.and(&ring15)),
                (
                    "CONTACT px (d_prev/d_expected in [0.95, 1.0) on the uncovering frame)",
                    contact.clone(),
                ),
                ("the rest (not contact)", stat.and(&contact.not())),
                ("contact, minus the 1.5 px ring", contact.and(&ring15.not())),
            ] {
                lines.push(format!(
                    "{label} ({} px): ORACLE {} | TAA {} | MSAA {} | SMAA {} | no-AA {}",
                    m.count(),
                    st(&oracle, &reference, &m),
                    st(&test, &reference, &m),
                    st(&msaa, &reference, &m),
                    st(&smaa, &reference, &m),
                    st(&plain, &reference, &m)
                ));
            }
            for a in 1..=WINDOW {
                let mut m = Mask::new(w, h, false);
                for i in 0..n {
                    m.bits[i] = stat.bits[i] && age[i] == a;
                }
                if m.count() == 0 {
                    continue;
                }
                lines.push(format!(
                    "age {a:2}: ORACLE {} | TAA {} | MSAA {} | SMAA {} | no-AA {}",
                    st(&oracle, &reference, &m),
                    st(&test, &reference, &m),
                    st(&msaa, &reference, &m),
                    st(&smaa, &reference, &m),
                    st(&plain, &reference, &m)
                ));
            }
            r.info(
                &format!("trail-evidence:{scene}:k{k}:oracle"),
                &format!(
                    "(oracle: per pixel, the resolve-model mean of the TAA-off frames jittered like TAA's, from the first frame after the object uncovered it, capped at {ORACLE_CAP}; perfect disocclusion, perfect vectors, no clip)\n    {}",
                    lines.join("\n    ")
                ),
            );
            // By cause (whole band).
            let near = band.and(&hull2);
            let far = band.and(&hull2.not());
            let in_sh = band.and(&shadow_win);
            let not_sh = band.and(&shadow_win.not());
            let mut lines = Vec::new();
            for (label, m) in [
                ("within 2 px of the current silhouette", &near),
                ("farther than 2 px", &far),
                ("in the shadow footprint (now or last 30 frames)", &in_sh),
                ("not in the shadow footprint", &not_sh),
            ] {
                lines.push(format!(
                    "{label}: TAA {} | no-AA {} | MSAA {}",
                    st(&test, &reference, m),
                    st(&plain, &reference, m),
                    st(&msaa, &reference, m)
                ));
            }
            r.info(
                &format!("trail-evidence:{scene}:k{k}:by-cause"),
                &format!("\n    {}", lines.join("\n    ")),
            );
            // Worst band pixel.
            let mut worst = (0u8, 0usize);
            for i in 0..n {
                if band.bits[i] {
                    let e = pixel_error(
                        test.get(i as u32 % w, i as u32 / w),
                        reference.get(i as u32 % w, i as u32 / w),
                    );
                    if e > worst.0 {
                        worst = (e, i);
                    }
                }
            }
            let (x, y) = (worst.1 as u32 % w, worst.1 as u32 / w);
            let vi = (y * vw + x) as usize;
            let v = Vec2::new(vel[vi * 2], vel[vi * 2 + 1]) * size;
            let rgb = |img: &Image| {
                let c = img.get(x, y);
                format!("({},{},{})", c[0], c[1], c[2])
            };
            r.info(
                &format!("trail-evidence:{scene}:k{k}:worst-pixel"),
                &format!(
                    "({x},{y}) error {} (TAA vs ref); age {}; within 2 px of silhouette {}; in shadow footprint {}; TAA {} ref {} oracle {} no-AA {} MSAA {}; velocity {:+.3},{:+.3} px; LIVE debug views (display-encoded bytes): Rejection {} (R off-screen, G disocclusion, B clip amount), HistoryLength {} (n/N_max, after tonemap + sRGB), ReprojectionError {}, Velocity {}",
                    worst.0,
                    age[worst.1],
                    hull2.bits[worst.1],
                    shadow_win.bits[worst.1],
                    rgb(&test),
                    rgb(&reference),
                    rgb(&oracle),
                    rgb(&plain),
                    rgb(&msaa),
                    v.x,
                    v.y,
                    rgb(&rej),
                    rgb(&hist),
                    rgb(&reproj),
                    rgb(&velv)
                ),
            );
            // Tiles.
            let both = band.or(&hull);
            let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0u32, 0u32);
            for yy in 0..h {
                for xx in 0..w {
                    if both.get(xx, yy) {
                        x0 = x0.min(xx);
                        y0 = y0.min(yy);
                        x1 = x1.max(xx);
                        y1 = y1.max(yy);
                    }
                }
            }
            let (cx, cy) = ((x0 + x1) / 2, (y0 + y1) / 2);
            let (cw, ch) = (320u32.min(w), 240u32.min(h));
            let crop_x = cx.saturating_sub(cw / 2).min(w - cw);
            let crop_y = cy.saturating_sub(ch / 2).min(h - ch);
            let mut band_img = Image::filled(w, h, [0, 0, 0, 255]);
            for i in 0..n {
                let (xx, yy) = (i as u32 % w, i as u32 / w);
                if band.bits[i] {
                    let g = (255 - (age[i] * 6).min(200)) as u8;
                    band_img.set(xx, yy, [g, g, 0, 255]);
                } else if hull.bits[i] {
                    band_img.set(xx, yy, [80, 80, 80, 255]);
                } else if shadow_win.bits[i] {
                    band_img.set(xx, yy, [0, 0, 140, 255]);
                }
            }
            band_img.set(x, y, [255, 0, 0, 255]);
            let base = format!("phase2/evidence/{scene}-k{k:03}");
            let tiles: [(&str, &Image); 11] = [
                ("taa", &test),
                ("ref", &reference),
                ("err8", &metrics::error_image(&test, &reference, 8.0)),
                ("noaa", &plain),
                ("msaa", &msaa),
                ("oracle", &oracle),
                ("rejection", &rej),
                ("history", &hist),
                ("velocity", &velv),
                ("reproj", &reproj),
                ("band", &band_img),
            ];
            for (name, img) in tiles {
                crate::store::put(&p.store, &format!("{base}-{name}.png"), img).await?;
            }
            r.info(
                &format!("trail-evidence:{scene}:k{k}:tiles"),
                &format!(
                    "stored {base}-{{taa,ref,err8,noaa,msaa,oracle,rejection,history,velocity,reproj,band}}.png (debug views LIVE); crop {crop_x},{crop_y} {cw}x{ch}"
                ),
            );
        }
        for i in 0..n {
            if hull.bits[i] {
                last_cov[i] = Some(k);
            }
            if shadow.bits[i] {
                last_shadow[i] = Some(k);
            }
        }
    }
    t.destroy();
    rs.destroy();
    ms.destroy();
    sm.destroy();
    hs.destroy();
    os.destroy();
    for (_, d) in dv {
        d.destroy();
    }
    Ok(())
}

/// Capture the frame a session just rendered.
async fn finish_capture_now(s: &mut Session) -> Result<Image> {
    let rb = s.begin_capture()?;
    crate::session::finish_capture(rb, s.opts.size).await
}

/// `?mode=speeds`: screen speeds per scenario phase.
pub async fn speeds_mode(p: &Params, r: &mut Report) {
    for scene in &p.scenes {
        if let Err(e) = speeds(p, scene, r).await {
            r.emit(&format!("speeds:{scene}"), false, &format!("error: {e:#}"));
        }
    }
}

async fn speeds(p: &Params, scene: &str, r: &mut Report) -> Result<()> {
    let mut s = Session::open(opts(p, scene, Aa::Taa)).await?;
    s.renderer.set_taa_debug_view(TaaDebugView::FreezeJitter);
    crate::session::wait_taa_debug_variant(&s.renderer).await;
    let frames = s.scenario.frames;
    let phases = s.scenario.phases.clone();
    let tracked = s.scenario.tracked.is_some();
    // Per phase: per-frame (mean moving, p50 moving, max, centre, tracked mean).
    let mut per: Vec<Vec<[f32; 5]>> = vec![Vec::new(); phases.len()];
    while s.next_k < frames {
        let k = s.render_next()?;
        gpu_idle(&s.renderer).await;
        let (w, h, v) = s
            .renderer
            .debug_read_velocity()
            .await
            .map_err(|e| anyhow!("debug_read_velocity: {e}"))?;
        let size = Vec2::new(w as f32, h as f32);
        let mut sp: Vec<f32> = v
            .chunks_exact(2)
            .filter(|c| !((c[0] - 2.0).abs() < 1e-3 && (c[1] - 2.0).abs() < 1e-3))
            .map(|c| (Vec2::new(c[0], c[1]) * size).length())
            .filter(|l| *l > 0.05)
            .collect();
        sp.sort_by(|a, b| a.total_cmp(b));
        let mean = sp.iter().sum::<f32>() / sp.len().max(1) as f32;
        let p50 = sp.get(sp.len() / 2).copied().unwrap_or(0.0);
        let max = sp.last().copied().unwrap_or(0.0);
        let ci = ((h / 2) * w + w / 2) as usize;
        let centre = (Vec2::new(v[ci * 2], v[ci * 2 + 1]) * size).length();
        let tr = if tracked {
            tracked_coverage(&s)
                .map(|m| {
                    let mut acc = (0.0f32, 0usize);
                    for (i, b) in m.bits.iter().enumerate() {
                        if *b {
                            let l = (Vec2::new(v[i * 2], v[i * 2 + 1]) * size).length();
                            if l < 100.0 {
                                acc = (acc.0 + l, acc.1 + 1);
                            }
                        }
                    }
                    acc.0 / acc.1.max(1) as f32
                })
                .unwrap_or(0.0)
        } else {
            0.0
        };
        if let Some(pi) = phases.iter().position(|(_, a, b)| k >= *a && k < *b) {
            per[pi].push([mean, p50, max, centre, tr]);
        }
        set_hud(&format!("taa-tests: speeds {scene} k{k}"));
    }
    s.destroy();
    let mut rows = Vec::new();
    for (pi, (name, a, b)) in phases.iter().enumerate() {
        let f = &per[pi];
        if f.is_empty() {
            continue;
        }
        let avg = |j: usize| f.iter().map(|x| x[j]).sum::<f32>() / f.len() as f32;
        let mx = |j: usize| f.iter().map(|x| x[j]).fold(0.0f32, f32::max);
        rows.push(format!(
            "{name} k{a}..{b}: moving px mean {:.2} (p50 {:.2}, max {:.2}) px/frame; screen centre {:.2} px/frame{}",
            avg(0),
            avg(1),
            mx(2),
            avg(3),
            if tracked {
                format!("; tracked object mean {:.2} px/frame (max over frames {:.2})", avg(4), mx(4))
            } else {
                String::new()
            }
        ));
    }
    r.info(
        &format!("speeds:{scene}"),
        &format!("\n    {}", rows.join("\n    ")),
    );
    Ok(())
}

/// `?mode=reference-floor` (05 review item 5): on a static frame, the
/// distance between the 64-phase reference (one normalisation over all
/// taps, dropped border taps) and the resolve's own model of convergence
/// (8 phases, per-frame normalisation, clamped borders), in both
/// accumulation spaces; plus the converged TAA frame against each. The first
/// number is the floor inside the stop (< 1) and trail (< 2) thresholds.
pub async fn reference_floor_mode(p: &Params, r: &mut Report) {
    let scenes: Vec<String> = if p.scenes_given {
        p.scenes.clone()
    } else {
        vec!["taa-thin".into(), "taa-motion".into()]
    };
    for scene in &scenes {
        if let Err(e) = reference_floor(p, scene, r).await {
            r.emit(
                &format!("reference-floor:{scene}"),
                false,
                &format!("error: {e:#}"),
            );
        }
    }
}

async fn reference_floor(p: &Params, scene: &str, r: &mut Report) -> Result<()> {
    use crate::reference::{jitter_offset, AccumSpace, ReferenceAccumulator};
    // A frame of a still phase: taa-thin's static phase, taa-motion's still.
    let k = if p.ks_given {
        p.ks.first().copied().unwrap_or(20)
    } else {
        match crate::session::base_scene_name(scene) {
            "taa-thin" => 40,
            _ => 20,
        }
    };
    let mut s = Session::open(opts(p, scene, Aa::None)).await?;
    s.play_to(k).await?;
    let size = p.size;
    let mut rows = Vec::new();
    let mut refs = Vec::new();
    for space in [AccumSpace::DisplayLinear, AccumSpace::Karis] {
        let mut r64 = ReferenceAccumulator::new(size, size, space);
        let mut m8 = ReferenceAccumulator::resolve_model(size, size, space);
        let mut m64 = ReferenceAccumulator::resolve_model(size, size, space);
        for i in 0..p.phases {
            let j = jitter_offset(i, p.phases);
            let img = s.render_frozen_captured(Some(j)).await?;
            r64.add_frame(&img, j);
            m64.add_frame(&img, j);
        }
        for i in 0..8 {
            let j = jitter_offset(i, 8);
            let img = s.render_frozen_captured(Some(j)).await?;
            m8.add_frame(&img, j);
        }
        let (a, b, c) = (r64.finish(), m8.finish(), m64.finish());
        rows.push(format!(
            "{}: reference64 vs resolve-model(8 phases) {} | reference64 vs resolve-model(64 phases: normalisation + borders only) {}",
            space.name(),
            abs_error(&a, &b, None),
            abs_error(&a, &c, None)
        ));
        refs.push((space, a, b));
    }
    s.destroy();
    // The converged TAA frame (held static for 40 frames after k).
    let mut t = Session::open(opts(p, scene, Aa::Taa)).await?;
    t.play_to(k).await?;
    for _ in 0..39 {
        t.render_frozen(None)?;
        gpu_idle(&t.renderer).await;
    }
    let taa = t.render_frozen_captured(None).await?;
    t.destroy();
    for (space, a, b) in &refs {
        rows.push(format!(
            "converged TAA (k{k} + 40 frozen frames) vs reference64 ({}) {} | vs resolve-model(8) {}",
            space.name(),
            abs_error(&taa, a, None),
            abs_error(&taa, b, None)
        ));
    }
    r.info(
        &format!("reference-floor:{scene}:k{k}"),
        &format!("\n    {}", rows.join("\n    ")),
    );
    Ok(())
}
