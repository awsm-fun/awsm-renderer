//! `?mode=ghost-track&scenes=taa-motion,taa-reveal,taa-skin[&gdump=21-41,126-146]`
//! (native scale): every pixel of every frame classified by what TAA's error
//! there is, and the stale-history pixels tracked across frames.
//!
//! The question it answers is the user's: is there a ghost trail the eye can
//! read, i.e. stale history that survives? For each pixel with a visible
//! TAA error (> T against the 64-phase reference; T = 16 and T = 8 both
//! reported), with C = the larger of the comparators' errors there:
//! - (a) GHOST: TAA used history there (alpha < 0.99, from the resolve's
//!   own raw view 15 in a lockstep session) and its error exceeds C + T;
//! - (b) CURRENT-FRAME: the pixel shows this frame only (alpha >= 0.99:
//!   reset, disoccluded, motion boundary, clip) and its error exceeds C + T:
//!   the single-frame reconstruction (kernel bleed, aliasing), not history;
//! - (c) ORACLE-LEVEL: within T of the perfect-information ORACLE's error
//!   (the oracle is as bad there);
//! - (d) NO-AA-LEVEL: within T of no-AA's error but the oracle does better.
//!
//! The comparators: no-AA always; the ORACLE on pixels whose surface does
//! not move (|v| < 0.01 px; the resolve-model mean of TAA-off frames
//! jittered exactly like TAA's since the moving object last covered the
//! pixel, at most 16), and MSAA 4x instead on moving pixels (the oracle
//! cannot reproject). Pixels are split into three regions: the trail band
//! (05 §4.2: covered by the moving objects within 30 frames, not now; the
//! trail gate's own band) where a ghost is a trail; the moving objects and
//! any moving surface (covered now, or |v| >= 0.01 px), where history error
//! is softness or lag of the surface's own detail, not a trail; and static
//! pixels elsewhere.
//!
//! Tracking: an (a) pixel continues the event of an (a) pixel within one
//! pixel of where its surface was last frame (its motion vector), else it
//! starts a new event. Per T: (a) pixel-frames, events, the distribution of
//! their lengths in frames, the longest and worst events. The bar (user,
//! round 2): no (a) event at T = 16 lasts more than one frame. The verdict
//! lines `ghost-persist:<scene>:trail` and `:static` (PASS iff so) judge
//! stale history where the object is not; `ghost-persist:<scene>:object` is
//! an INFO line (on the moving objects an (a) pixel is lag or softness of
//! their own detail), judged against MSAA in motion by
//! `ghost-region-error:<scene>` (mean error of TAA / MSAA / no-AA over every
//! pixel of each region).
//!
//! `gdump=a-b[,c-d]`: for those frames, store TAA / MSAA / ORACLE /
//! reference / no-AA and the T = 16 class map under
//! `ghost/<scene>/k<k>-<panel>.png` (the GIF tool reads them). Class map
//! colours: (a) red, (b) blue, (c) yellow, (d) grey, band without a visible
//! error dark green, else the TAA frame darkened.

#![allow(clippy::needless_range_loop)]

use std::collections::VecDeque;

use anyhow::{anyhow, Result};
use glam::Vec2;

use crate::gates::{moving_coverage, opts, Pair};
use crate::metrics::{pixel_error, trail_band, Mask};
use crate::report::{set_hud, Report};
use crate::session::{gpu_idle, Aa, Session};
use crate::Params;

const WINDOW: usize = 30;
const ORACLE_CAP: usize = 16;
const THRESHOLDS: [u8; 2] = [16, 8];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Ghost,
    Current,
    OracleLevel,
    NoAaLevel,
}

const REGIONS: [&str; 3] = [
    "trail band",
    "moving objects / surfaces",
    "static elsewhere",
];

#[derive(Default, Clone, Copy)]
struct Counts {
    /// Per region, [a, b, c, d] pixel-frames.
    px: [[u64; 4]; 3],
}

#[derive(Clone)]
struct Event {
    start: u32,
    last: u32,
    px_frames: u64,
    region: usize,
    /// (excess over C, k, x, y, [TAA, no-AA, MSAA, ORACLE or -1])
    worst: (i32, u32, u32, u32, [i32; 4]),
}

struct Tracker {
    t: u8,
    counts: Counts,
    frames_with_a: Vec<(u32, u64)>,
    /// Per frame listed in `gframes=`: [region][class] px.
    listed: Vec<(u32, [[u64; 4]; 3])>,
    /// Event id per pixel at the previous evaluated frame (0 = none).
    prev: Vec<u32>,
    prev_k: Option<u32>,
    events: Vec<Event>,
}

impl Tracker {
    fn new(t: u8, n: usize) -> Self {
        Self {
            t,
            counts: Counts::default(),
            frames_with_a: Vec::new(),
            listed: Vec::new(),
            prev: vec![0; n],
            prev_k: None,
            events: Vec::new(),
        }
    }
}

fn parse_ranges(v: &str) -> Vec<(u32, u32)> {
    v.split(',')
        .filter_map(|r| {
            let mut it = r.split('-');
            let a = it.next()?.trim().parse().ok()?;
            let b = it
                .next()
                .map(|b| b.trim().parse().ok())
                .unwrap_or(Some(a))?;
            Some((a, b))
        })
        .collect()
}

pub async fn ghost_track_mode(p: &Params, r: &mut Report) {
    if (p.scale - 1.0).abs() > 1e-6 {
        r.emit(
            "ghost-track",
            false,
            "native scale only (velocity and depth at output size)",
        );
        return;
    }
    let scenes: Vec<String> = if p.scenes_given {
        p.scenes.clone()
    } else {
        vec!["taa-motion".into(), "taa-reveal".into(), "taa-skin".into()]
    };
    for scene in &scenes {
        if let Err(e) = ghost_track(p, scene, r).await {
            r.emit(
                &format!("ghost-track:{scene}"),
                false,
                &format!("error: {e:#}"),
            );
        }
    }
}

async fn ghost_track(p: &Params, scene: &str, r: &mut Report) -> Result<()> {
    let dumps = crate::url_value("gdump")
        .map(|v| parse_ranges(&v))
        .unwrap_or_default();
    let listed_frames: Vec<u32> = crate::url_value("gframes")
        .map(|v| v.split(',').filter_map(|x| x.trim().parse().ok()).collect())
        .unwrap_or_default();
    let mut pair = Pair::open(p, scene).await?;
    let frames = pair.test.scenario.frames;
    let mut ms = Session::open(opts(p, scene, Aa::Msaa)).await?;
    let mut os = Session::open(opts(p, scene, Aa::None)).await?;
    // The resolve's own alpha, from raw view 15 (R = final alpha >= 0.99) of
    // a TAA session in lockstep with the one under test.
    let mut alpha_s = Session::open(opts(p, scene, Aa::Taa)).await?;
    alpha_s.renderer.set_taa_debug_view_raw(Some(15));
    crate::session::wait_taa_debug_variant(&alpha_s.renderer).await;
    for s in [&ms, &os, &alpha_s] {
        s.assert_lockstep_safe()?;
    }
    // `gviews=13,14,16`: more raw views, in their own lockstep sessions,
    // stored with the dumped frames (diagnosis).
    let mut view_s: Vec<(u32, Session)> = Vec::new();
    for id in crate::url_value("gviews")
        .map(|v| {
            v.split(',')
                .filter_map(|x| x.trim().parse::<u32>().ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
    {
        let mut s = Session::open(opts(p, scene, Aa::Taa)).await?;
        s.renderer.set_taa_debug_view_raw(Some(id));
        crate::session::wait_taa_debug_variant(&s.renderer).await;
        s.assert_lockstep_safe()?;
        view_s.push((id, s));
    }
    let has_tracked = pair.test.scenario.tracked.is_some();
    let mut probe = match crate::oracle::untracked_moving_nodes(scene) {
        Some(nodes) if !has_tracked => {
            Some(crate::oracle::CoverageProbe::open(p, scene, nodes).await?)
        }
        _ => None,
    };
    let (w, h) = p.size;
    let n = (w * h) as usize;
    let size = Vec2::new(w as f32, h as f32);
    let mut history: Vec<Mask> = Vec::new();
    let mut oring: VecDeque<(u32, Vec<[f32; 3]>)> = Default::default();
    let mut trackers: Vec<Tracker> = THRESHOLDS.iter().map(|t| Tracker::new(*t, n)).collect();
    let mut evaluated = 0u32;
    let mut band_px_frames = 0u64;
    // Per region, every pixel: (px-frames, sum of TAA / MSAA / no-AA error).
    let mut region_err = [(0u64, 0f64, 0f64, 0f64); 3];
    while pair.test.next_k < frames {
        let k = pair.test.next_k;
        let (_, test, reference, plain) = pair.step_with_reference(p).await?;
        let msaa = ms.render_next_captured().await?.1;
        let alpha_img = alpha_s.render_next_captured().await?.1;
        let dump_now = dumps.iter().any(|(a, b)| (*a..=*b).contains(&k));
        let mut view_imgs: Vec<(u32, crate::metrics::Image)> = Vec::new();
        for (id, s) in view_s.iter_mut() {
            if dump_now {
                view_imgs.push((*id, s.render_next_captured().await?.1));
            } else {
                s.render_next()?;
                gpu_idle(&s.renderer).await;
            }
        }
        let j = pair.test.renderer.camera.jitter_px();
        os.render_next_jittered(Some(j))?;
        let ojit = {
            let rb = os.begin_capture()?;
            crate::session::finish_capture(rb, os.opts.size).await?
        };
        oring.push_back((k, crate::reference::resolve_frame(&ojit, j, p.karis)));
        if oring.len() > ORACLE_CAP {
            oring.pop_front();
        }
        let now = match probe.as_mut() {
            Some(pr) => pr.step().await?,
            None => moving_coverage(&pair.test).await?,
        };
        let (vw, vh, vel) = pair
            .test
            .renderer
            .debug_read_velocity()
            .await
            .map_err(|e| anyhow!("debug_read_velocity: {e}"))?;
        if (vw, vh) != (w, h) {
            return Err(anyhow!(
                "velocity readback {vw}x{vh} is not the output size"
            ));
        }
        let recent: Vec<&Mask> = history.iter().rev().take(WINDOW).collect();
        let band = trail_band(&recent, &now);
        let dump = dumps.iter().any(|(a, b)| (*a..=*b).contains(&k));
        // Every frame from k1; early on the oracle has only the frames so far
        // (as does TAA's history).
        if k >= 1 && !oring.is_empty() {
            evaluated += 1;
            band_px_frames += band.count() as u64;
            let mut oracle_img = if dump { Some(test.clone()) } else { None };
            let mut class_img = if dump { Some(test.clone()) } else { None };
            // Per pixel: errors and the comparator, shared by both trackers.
            // (class at T = 16, class at T = 8, excess over the comparator,
            // [TAA, no-AA, MSAA, ORACLE (-1: none)] errors).
            type PerPx = (Option<Class>, Option<Class>, i32, [i32; 4], usize);
            let mut per_px: Vec<Option<PerPx>> = vec![None; n];
            for i in 0..n {
                let (x, y) = (i as u32 % w, i as u32 / w);
                let v = Vec2::new(vel[i * 2], vel[i * 2 + 1]);
                let marker = (v.x - 2.0).abs() < 1e-3 && (v.y - 2.0).abs() < 1e-3;
                let moving = !marker && (v * size).length() >= 0.01;
                let rp = reference.get(x, y);
                let et = pixel_error(test.get(x, y), rp);
                let en = pixel_error(plain.get(x, y), rp);
                let em = pixel_error(msaa.get(x, y), rp);
                // The oracle: frames since the moving objects last covered
                // the pixel (capped), or this frame alone on a moving or
                // covered pixel.
                let age = if moving || now.bits[i] {
                    1
                } else {
                    history
                        .iter()
                        .rev()
                        .position(|m| m.bits[i])
                        .map(|a| a + 1)
                        .unwrap_or(ORACLE_CAP)
                        .min(ORACLE_CAP)
                };
                let need_oracle = dump || (!moving && et > THRESHOLDS[1]);
                let oc = if need_oracle {
                    let first = (k + 1).saturating_sub(age as u32);
                    let mut acc = [0.0f32; 3];
                    let mut cnt = 0.0f32;
                    for (fk, f) in oring.iter() {
                        if *fk >= first {
                            for (a, c) in acc.iter_mut().zip(f[i]) {
                                *a += c;
                            }
                            cnt += 1.0;
                        }
                    }
                    Some(crate::reference::encode_space(
                        [acc[0] / cnt, acc[1] / cnt, acc[2] / cnt],
                        p.karis,
                    ))
                } else {
                    None
                };
                if let (Some(o), Some(c)) = (oracle_img.as_mut(), oc) {
                    o.set(x, y, c);
                }
                let eo = oc.map(|c| pixel_error(c, rp) as i32).unwrap_or(-1);
                let full = alpha_img.get(x, y)[0] > 127;
                let comparator = if moving {
                    en.max(em) as i32
                } else {
                    (en as i32).max(eo)
                };
                let classify = |t: u8| -> Option<Class> {
                    if et <= t {
                        return None;
                    }
                    let et = et as i32;
                    let t = t as i32;
                    Some(if et > comparator + t {
                        if full {
                            Class::Current
                        } else {
                            Class::Ghost
                        }
                    } else if !moving && eo >= et - t {
                        Class::OracleLevel
                    } else {
                        Class::NoAaLevel
                    })
                };
                let (c16, c8) = (classify(THRESHOLDS[0]), classify(THRESHOLDS[1]));
                let region = if band.bits[i] {
                    0
                } else if moving || now.bits[i] {
                    1
                } else {
                    2
                };
                let re = &mut region_err[region];
                re.0 += 1;
                re.1 += et as f64;
                re.2 += em as f64;
                re.3 += en as f64;
                if let Some(ci) = class_img.as_mut() {
                    let col = match c16 {
                        Some(Class::Ghost) => [255, 0, 0, 255],
                        Some(Class::Current) => [0, 90, 255, 255],
                        Some(Class::OracleLevel) => [255, 220, 0, 255],
                        Some(Class::NoAaLevel) => [150, 150, 150, 255],
                        None if band.bits[i] => [0, 90, 0, 255],
                        None => {
                            let c = test.get(x, y);
                            [c[0] / 3, c[1] / 3, c[2] / 3, 255]
                        }
                    };
                    ci.set(x, y, col);
                }
                if c8.is_some() {
                    per_px[i] = Some((
                        c16,
                        c8,
                        et as i32 - comparator,
                        [et as i32, en as i32, em as i32, eo],
                        region,
                    ));
                }
            }
            for (ti, tr) in trackers.iter_mut().enumerate() {
                let mut cur = vec![0u32; n];
                let mut a_count = 0u64;
                let mut frame_px = [[0u64; 4]; 3];
                let contiguous = tr.prev_k == Some(k.wrapping_sub(1));
                for i in 0..n {
                    let Some((c16, c8, excess, errs, region)) = per_px[i] else {
                        continue;
                    };
                    let Some(class) = (if ti == 0 { c16 } else { c8 }) else {
                        continue;
                    };
                    let slot = match class {
                        Class::Ghost => 0,
                        Class::Current => 1,
                        Class::OracleLevel => 2,
                        Class::NoAaLevel => 3,
                    };
                    tr.counts.px[region][slot] += 1;
                    frame_px[region][slot] += 1;
                    if class != Class::Ghost {
                        continue;
                    }
                    a_count += 1;
                    let (x, y) = (i as u32 % w, i as u32 / w);
                    // Where this surface was last frame.
                    let v = Vec2::new(vel[i * 2], vel[i * 2 + 1]) * size;
                    let src = Vec2::new(x as f32 + 0.5, y as f32 + 0.5) - v;
                    let (sx, sy) = (src.x.floor() as i32, src.y.floor() as i32);
                    let mut inherit = 0u32;
                    if contiguous {
                        for dy in -1..=1 {
                            for dx in -1..=1 {
                                let (qx, qy) = (sx + dx, sy + dy);
                                if qx < 0 || qy < 0 || qx >= w as i32 || qy >= h as i32 {
                                    continue;
                                }
                                let id = tr.prev[(qy as u32 * w + qx as u32) as usize];
                                if id != 0 && (inherit == 0 || id < inherit) {
                                    inherit = id;
                                }
                            }
                        }
                    }
                    let id = if inherit != 0 {
                        inherit
                    } else {
                        tr.events.push(Event {
                            start: k,
                            last: k,
                            px_frames: 0,
                            region,
                            worst: (i32::MIN, k, x, y, [0, 0, 0, -1]),
                        });
                        tr.events.len() as u32
                    };
                    let e = &mut tr.events[id as usize - 1];
                    e.last = k;
                    e.px_frames += 1;
                    if excess > e.worst.0 {
                        e.worst = (excess, k, x, y, errs);
                    }
                    cur[i] = id;
                }
                if a_count > 0 {
                    tr.frames_with_a.push((k, a_count));
                }
                if listed_frames.contains(&k) {
                    tr.listed.push((k, frame_px));
                }
                tr.prev = cur;
                tr.prev_k = Some(k);
            }
            if dump {
                let base = format!("ghost/{scene}/k{k:03}");
                crate::store::put(&p.store, &format!("{base}-taa.png"), &test).await?;
                crate::store::put(&p.store, &format!("{base}-msaa.png"), &msaa).await?;
                crate::store::put(&p.store, &format!("{base}-ref.png"), &reference).await?;
                crate::store::put(&p.store, &format!("{base}-noaa.png"), &plain).await?;
                if let Some(o) = oracle_img {
                    crate::store::put(&p.store, &format!("{base}-oracle.png"), &o).await?;
                }
                if let Some(c) = class_img {
                    crate::store::put(&p.store, &format!("{base}-class.png"), &c).await?;
                }
                for (id, img) in &view_imgs {
                    crate::store::put(&p.store, &format!("{base}-view{id}.png"), img).await?;
                }
            }
        }
        history.push(now);
        if history.len() > WINDOW + 1 {
            history.remove(0);
        }
        gpu_idle(&ms.renderer).await;
        set_hud(&format!("taa-tests: ghost-track {scene} k{k}/{frames}"));
    }
    pair.destroy();
    ms.destroy();
    os.destroy();
    alpha_s.destroy();
    for (_, s) in view_s {
        s.destroy();
    }
    report(r, scene, evaluated, band_px_frames, &trackers);
    r.info(
        &format!("ghost-region-error:{scene}"),
        &format!(
            "mean error over every pixel-frame of the region, TAA / MSAA 4x / no-AA: {}",
            REGIONS
                .iter()
                .zip(region_err)
                .map(|(name, (n, t, m, a))| {
                    let d = n.max(1) as f64;
                    format!(
                        "{name} ({n} px-frames) {:.3} / {:.3} / {:.3}",
                        t / d,
                        m / d,
                        a / d
                    )
                })
                .collect::<Vec<_>>()
                .join("; ")
        ),
    );
    Ok(())
}

fn report(r: &mut Report, scene: &str, evaluated: u32, band_px_frames: u64, trackers: &[Tracker]) {
    let order = ["1", "2", "3-4", "5-8", "9-16", "17+"];
    let bucket = |len: u32| match len {
        1 => 0,
        2 => 1,
        3..=4 => 2,
        5..=8 => 3,
        9..=16 => 4,
        _ => 5,
    };
    for tr in trackers {
        let t = tr.t;
        let c = tr.counts;
        let per_region: Vec<String> = REGIONS
            .iter()
            .enumerate()
            .map(|(ri, name)| {
                let px = c.px[ri];
                format!(
                    "{name}: (a) ghost {} / (b) current-frame {} / (c) oracle-level {} / (d) no-AA-level {}",
                    px[0], px[1], px[2], px[3]
                )
            })
            .collect();
        r.info(
            &format!("ghost-classes:{scene}:T{t}"),
            &format!(
                "{evaluated} frames, {band_px_frames} band px-frames; visible-error px-frames (TAA error > {t}):\n    {}\n    frames with any (a): {}",
                per_region.join("\n    "),
                tr.frames_with_a.len()
            ),
        );
        let describe = |e: &Event| -> String {
            let (ex, k, x, y, [et, en, em, eo]) = e.worst;
            format!(
                "k{}-k{} ({} frames, {} px-frames) worst @k{k} ({x},{y}): TAA {et} / no-AA {en} / MSAA {em} / ORACLE {} (excess {ex:+})",
                e.start,
                e.last,
                e.last - e.start + 1,
                e.px_frames,
                if eo < 0 { "n/a".to_string() } else { eo.to_string() },
            )
        };
        let mut lines = Vec::new();
        for (ri, name) in REGIONS.iter().enumerate() {
            let evs: Vec<&Event> = tr.events.iter().filter(|e| e.region == ri).collect();
            let mut hist = [0u32; 6];
            for e in &evs {
                hist[bucket(e.last - e.start + 1)] += 1;
            }
            let hist_s: Vec<String> = order
                .iter()
                .zip(hist)
                .map(|(b, n)| format!("{b}: {n}"))
                .collect();
            let mut by_len = evs.clone();
            by_len.sort_by_key(|e| std::cmp::Reverse((e.last - e.start, e.px_frames)));
            let mut by_err = evs.clone();
            by_err.sort_by_key(|e| std::cmp::Reverse(e.worst.0));
            lines.push(format!(
                "{name}: {} events, {} px-frames; length in frames [{}]\n      longest: {}\n      worst: {}",
                evs.len(),
                evs.iter().map(|e| e.px_frames).sum::<u64>(),
                hist_s.join(", "),
                by_len.iter().take(6).map(|e| describe(e)).collect::<Vec<_>>().join("\n               "),
                by_err.iter().take(6).map(|e| describe(e)).collect::<Vec<_>>().join("\n             "),
            ));
        }
        r.info(
            &format!("ghost-events:{scene}:T{t}"),
            &format!(
                "(a) events by region (where each started):\n    {}",
                lines.join("\n    ")
            ),
        );
        if !tr.listed.is_empty() {
            r.info(
                &format!("ghost-listed:{scene}:T{t}"),
                &format!(
                    "per frame, px by region [trail band | moving objects | static] as (a)/(b)/(c)/(d):\n    {}",
                    tr.listed
                        .iter()
                        .map(|(k, px)| format!(
                            "k{k}: {}",
                            px.iter()
                                .map(|c| format!("{}/{}/{}/{}", c[0], c[1], c[2], c[3]))
                                .collect::<Vec<_>>()
                                .join(" | ")
                        ))
                        .collect::<Vec<_>>()
                        .join("\n    ")
                ),
            );
        }
        r.info(
            &format!("ghost-frames:{scene}:T{t}"),
            &format!(
                "(a) px per frame: [{}]",
                tr.frames_with_a
                    .iter()
                    .map(|(k, a)| format!("k{k} {a}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
        if t == THRESHOLDS[0] {
            for (ri, name) in REGIONS.iter().enumerate() {
                let evs: Vec<&Event> = tr.events.iter().filter(|e| e.region == ri).collect();
                let persistent = evs.iter().filter(|e| e.last > e.start).count();
                let longest = evs.iter().map(|e| e.last - e.start + 1).max().unwrap_or(0);
                let detail = format!(
                    "{name}: no ghost (a) event at T = {t} lasts more than one frame: {persistent} of {} events last longer (longest {longest} frames)",
                    evs.len()
                );
                let key = ["trail", "object", "static"][ri];
                // The verdict is on stale history where the object is NOT:
                // the trail band and static pixels elsewhere. On the moving
                // objects themselves an (a) pixel is lag or softness of the
                // object's own detail (or its own silhouette): reported on
                // its own line, judged against MSAA in motion
                // (`ghost-region-error`).
                if ri == 1 {
                    r.info(&format!("ghost-persist:{scene}:{key}"), &detail);
                } else if let Some(why) = verdict_is_info(scene, ri) {
                    r.info(
                        &format!("ghost-persist:{scene}:{key}"),
                        &format!("{detail} [INFO: {why}]"),
                    );
                } else {
                    r.emit(
                        &format!("ghost-persist:{scene}:{key}"),
                        persistent == 0,
                        &detail,
                    );
                }
            }
        }
    }
}

/// Regions whose `ghost-persist` line is INFO for one scene (lead decisions,
/// round 3), with the reason printed on the line.
/// - `taa-orbit` trail and static: no object moves, only the camera, so the
///   "trail band" is just the pixels whose motion fell below 0.05 px (near
///   the pivot and after the stop) and holds no trail; its events are single
///   pixels on checker edges softening under camera motion. Judged by
///   `ghost-region-error` (moving region) against MSAA and no AA.
/// - `taa-thin` static: sub-pixel wires in a still image, whose history the
///   clip cuts on some jitter phases: events repeat at the same pixels with
///   the 8-frame jitter cycle. That is shimmer, judged by the flicker gate,
///   not history of something that moved.
fn verdict_is_info(scene: &str, region: usize) -> Option<&'static str> {
    match (scene, region) {
        ("taa-orbit", 0) | ("taa-orbit", 2) => Some(
            "camera-only scene: no object leaves a trail; judged by the moving-region error vs MSAA",
        ),
        ("taa-thin", 2) => Some(
            "still sub-pixel wires repeating with the jitter cycle: shimmer, judged by the flicker gate",
        ),
        _ => None,
    }
}
