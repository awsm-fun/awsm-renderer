//! `?mode=tune-sweep2&refcache=<name>` — the motion tuning, by COORDINATE
//! SEARCH from the defaults (the full grid is 54 configurations × ~6 min):
//! each knob of `?knobs=` alone at each of its values (the others at their
//! defaults), then `?best=name:v,…` if given. `?configs=a-b` slices the list.
//! Default knobs: max_history_moving 2|4|8, max_history_moving_speed 0.5|1,
//! motion_edge_alpha 0.3|0.6|1, clip_alpha 0.5|0.75|1 (experimental).
//!
//! Per configuration (INFO lines):
//! - trail taa-motion and taa-reveal, every frame with a band: worst / avg
//!   mean and worst max over the literal band, band minus the 1.5 px ring,
//!   the ring, CONTACT px (d_prev/d_expected in [0.95, 1) on the uncovering
//!   frame) and the rest;
//! - taa-orbit: stop (+8 after the strafe), pan (every strafe frame: worst
//!   and avg mean), motion flicker during the strafe (TAA frame-to-frame
//!   mean minus the reference's own);
//! - taa-thin converged (k32, vs reference) and static flicker (scripted
//!   static phase); taa-orbit static flicker (time frozen at k0).
//!
//! All masks (bands, rings, contact) are tuning-independent and computed
//! once in a first pass; references come from the refcache.

use anyhow::{anyhow, Result};

use crate::disocc::{definition, read_frame, replicate};
use crate::gates::{near_silhouette, tracked_coverage};
use crate::metrics::{
    abs_error, error_histogram, pixel_error, trail_band, ErrorHistogram, ErrorStats, Image, Mask,
};
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

fn indices(m: &Mask) -> Vec<u32> {
    m.bits
        .iter()
        .enumerate()
        .filter(|(_, b)| **b)
        .map(|(i, _)| i as u32)
        .collect()
}

fn stats_at(a: &Image, b: &Image, idx: &[u32]) -> ErrorStats {
    let mut h = ErrorHistogram::default();
    for i in idx {
        let (x, y) = (*i % a.width, *i / a.width);
        h.add(pixel_error(a.get(x, y), b.get(x, y)));
    }
    h.stats()
}

#[derive(Default, Clone)]
struct Series {
    n: u32,
    sum: f64,
    worst_mean: (f64, u32),
    worst_max: (u8, u32),
}

impl Series {
    fn add(&mut self, k: u32, s: &ErrorStats) {
        if s.count == 0 {
            return;
        }
        self.n += 1;
        self.sum += s.mean;
        if s.mean >= self.worst_mean.0 {
            self.worst_mean = (s.mean, k);
        }
        if s.max >= self.worst_max.0 {
            self.worst_max = (s.max, k);
        }
    }

    fn show(&self) -> String {
        if self.n == 0 {
            return "n/a".into();
        }
        format!(
            "{:.2}@k{}/{:.2}/{}",
            self.worst_mean.0,
            self.worst_mean.1,
            self.sum / self.n as f64,
            self.worst_max.0
        )
    }
}

const TRAIL_SETS: [&str; 5] = ["literal", "minus-ring", "ring", "contact", "rest"];

/// Per evaluated frame: index lists of the five trail sets.
pub(crate) struct TrailPlan {
    scene: String,
    frames: Vec<(u32, Vec<Vec<u32>>)>,
    total: u32,
}

pub(crate) async fn plan_trail(p: &Params, scene: &str) -> Result<TrailPlan> {
    let (w, h) = p.size;
    let n = (w * h) as usize;
    let mut t = Session::open(opts(p, scene, Aa::Taa)).await?;
    let mut rs = Session::open(opts(p, scene, Aa::None)).await?;
    let total = t.scenario.frames;
    let mut hist: Vec<Mask> = Vec::new();
    let mut prev_df = None;
    let mut uncover = vec![f32::NAN; n];
    let mut frames = Vec::new();
    while t.next_k < total {
        let k = t.next_k;
        t.render_next()?;
        rs.render_next()?;
        gpu_idle(&t.renderer).await;
        let df = read_frame(&t).await?;
        if let Some(pf) = &prev_df {
            let (_, ratio, _, _) = replicate(pf, &df, w, h, definition());
            for i in 0..n {
                if pf.hull.bits[i] && !df.hull.bits[i] {
                    uncover[i] = ratio[i];
                }
            }
        }
        let now = tracked_coverage(&t).ok_or_else(|| anyhow!("{scene}: no tracked object"))?;
        let recent: Vec<&Mask> = hist.iter().rev().take(30).collect();
        let band = trail_band(&recent, &now);
        if band.count() > 0 {
            // Fill the reference cache now (later passes only read it).
            reference_cached(&mut rs, p).await?;
            let ring = near_silhouette(&t, &now, 1.5).and(&band);
            let mut contact = Mask::new(w, h, false);
            for i in 0..n {
                contact.bits[i] = band.bits[i] && (0.95..1.0).contains(&uncover[i]);
            }
            frames.push((
                k,
                vec![
                    indices(&band),
                    indices(&band.and(&ring.not())),
                    indices(&ring),
                    indices(&contact),
                    indices(&band.and(&contact.not())),
                ],
            ));
        } else {
            gpu_idle(&rs.renderer).await;
        }
        hist.push(now);
        if hist.len() > 31 {
            hist.remove(0);
        }
        prev_df = Some(df);
        set_hud(&format!("taa-tests: tune-sweep2 plan {scene} k{k}"));
    }
    t.destroy();
    rs.destroy();
    Ok(TrailPlan {
        scene: scene.to_string(),
        frames,
        total,
    })
}

fn ref_path(p: &Params, scene: &str, k: u32) -> String {
    crate::modes::ref_cache_path(p, scene, k, p.karis).unwrap_or_default()
}

async fn cached_ref(p: &Params, scene: &str, k: u32) -> Result<Image> {
    let path = ref_path(p, scene, k);
    crate::store::get(&p.store, &path)
        .await?
        .ok_or_else(|| anyhow!("{path} missing"))
}

pub(crate) async fn run_trail(p: &Params, plan: &TrailPlan) -> Result<String> {
    let mut t = Session::open(opts(p, &plan.scene, Aa::Taa)).await?;
    let mut series = vec![Series::default(); TRAIL_SETS.len()];
    let mut fi = 0;
    while t.next_k < plan.total {
        let k = t.next_k;
        if fi < plan.frames.len() && plan.frames[fi].0 == k {
            let (_, img) = t.render_next_captured().await?;
            let reference = cached_ref(p, &plan.scene, k).await?;
            for (si, idx) in plan.frames[fi].1.iter().enumerate() {
                if !idx.is_empty() {
                    series[si].add(k, &stats_at(&img, &reference, idx));
                }
            }
            fi += 1;
        } else {
            t.render_next()?;
            gpu_idle(&t.renderer).await;
        }
        set_hud(&format!("taa-tests: tune-sweep2 {} k{k}", plan.scene));
    }
    t.destroy();
    Ok(format!(
        "trail {} (worst mean@k / avg mean / worst max): {}",
        plan.scene,
        TRAIL_SETS
            .iter()
            .zip(&series)
            .map(|(n, s)| format!("{n} {}", s.show()))
            .collect::<Vec<_>>()
            .join(" | ")
    ))
}

pub(crate) async fn run_orbit(p: &Params) -> Result<String> {
    let scene = "taa-orbit";
    let mut t = Session::open(opts(p, scene, Aa::Taa)).await?;
    let (a, b) = t.scenario.phase_range("strafe").unwrap_or((150, 240));
    let stop_k = t.scenario.phase_range("stopped").map(|x| x.0).unwrap_or(b) + 8;
    let mut pan = Series::default();
    let mut prev: Option<(Image, Image)> = None;
    let (mut ex_sum, mut ex_n, mut ex_w) = (0.0f64, 0u32, f64::NEG_INFINITY);
    let mut stop = String::new();
    while t.next_k <= stop_k {
        let k = t.next_k;
        let want = (k >= a && k < b) || k == stop_k;
        if want {
            let (_, img) = t.render_next_captured().await?;
            let reference = cached_ref(p, scene, k).await?;
            let s = abs_error(&img, &reference, None);
            if k == stop_k {
                stop = format!("stop k{k} {:.3}", s.mean);
            } else {
                pan.add(k, &s);
                if let Some((pi, pr)) = &prev {
                    let e = abs_error(&img, pi, None).mean - abs_error(&reference, pr, None).mean;
                    ex_sum += e;
                    ex_n += 1;
                    ex_w = ex_w.max(e);
                }
                prev = Some((img, reference));
            }
        } else {
            t.render_next()?;
            gpu_idle(&t.renderer).await;
        }
        set_hud(&format!("taa-tests: tune-sweep2 {scene} k{k}"));
    }
    t.destroy();
    Ok(format!(
        "taa-orbit {stop}; pan worst {:.3}@k{} avg {:.3}; strafe motion flicker (TAA f2f − ref f2f) avg {:.4} worst {:.4}",
        pan.worst_mean.0,
        pan.worst_mean.1,
        pan.sum / pan.n.max(1) as f64,
        ex_sum / ex_n.max(1) as f64,
        ex_w
    ))
}

pub(crate) async fn static_flicker(p: &Params, scene: &str) -> Result<(f64, u8)> {
    let mut s = Session::open(opts(p, scene, Aa::Taa)).await?;
    let scripted = s
        .scenario
        .phase_range("static")
        .filter(|(a, b)| b - a >= 101);
    let mut imgs = Vec::new();
    if let Some((a, _)) = scripted {
        s.play_to(a + 39).await?;
        for _ in 0..61 {
            imgs.push(s.render_next_captured().await?.1);
        }
    } else {
        s.play_to(0).await?;
        for _ in 0..40 {
            s.render_frozen(None)?;
            gpu_idle(&s.renderer).await;
        }
        for _ in 0..61 {
            imgs.push(s.render_frozen_captured(None).await?);
        }
    }
    s.destroy();
    let mut h = ErrorHistogram::default();
    let mut m = 0.0;
    for w in imgs.windows(2) {
        let e = error_histogram(&w[1], &w[0], None);
        m += e.stats().mean / 60.0;
        h.merge(&e);
    }
    Ok((m, h.stats().p99))
}

async fn run_static(p: &Params) -> Result<String> {
    // taa-thin converged at k32 vs its (cached) reference.
    let mut t = Session::open(opts(p, "taa-thin", Aa::Taa)).await?;
    t.play_to(31).await?;
    let (_, img) = t.render_next_captured().await?;
    t.destroy();
    let reference = match crate::store::get(&p.store, &ref_path(p, "taa-thin", 32)).await? {
        Some(r) => r,
        None => {
            let mut rs = Session::open(opts(p, "taa-thin", Aa::None)).await?;
            rs.play_to(32).await?;
            let r = reference_cached(&mut rs, p).await?;
            rs.destroy();
            r
        }
    };
    let conv = abs_error(&img, &reference, None);
    let (tf, tp) = static_flicker(p, "taa-thin").await?;
    let (of, op) = static_flicker(p, "taa-orbit").await?;
    Ok(format!(
        "converged taa-thin k32 {:.3} p99 {}; static flicker taa-thin {tf:.4} p99 {tp}, taa-orbit {of:.4} p99 {op}",
        conv.mean, conv.p99
    ))
}

fn parse_knobs() -> Vec<(String, Vec<f32>)> {
    let spec = crate::url_value("knobs").unwrap_or_else(|| {
        "max_history_moving:2|4|8;max_history_moving_speed:0.5|1;motion_edge_alpha:0.3|0.6|1;clip_alpha:0.5|0.75|1".into()
    });
    spec.split(';')
        .filter_map(|kv| {
            let (k, vs) = kv.split_once(':')?;
            Some((
                k.trim().to_string(),
                vs.split('|')
                    .filter_map(|v| v.trim().parse().ok())
                    .collect(),
            ))
        })
        .collect()
}

pub async fn tune_sweep2_mode(p: &Params, r: &mut Report) {
    if let Err(e) = tune_sweep2(p, r).await {
        r.emit("tune-sweep2", false, &format!("error: {e:#}"));
    }
    let _ = crate::hooks::set_tune(&p.tune);
}

async fn tune_sweep2(p: &Params, r: &mut Report) -> Result<()> {
    if p.refcache.is_none() {
        return Err(anyhow!("tune-sweep2 needs ?refcache="));
    }
    // Configurations: defaults, each knob value alone, then ?best=.
    let mut configs: Vec<Vec<(String, f32)>> = vec![vec![]];
    for (k, vs) in parse_knobs() {
        for v in vs {
            configs.push(vec![(k.clone(), v)]);
        }
    }
    if let Some(best) = crate::url_value("best") {
        configs.push(
            best.split(',')
                .filter_map(|kv| {
                    let (k, v) = kv.split_once(':')?;
                    Some((k.trim().to_string(), v.trim().parse().ok()?))
                })
                .collect(),
        );
    }
    let (lo, hi) = p.combos.unwrap_or((0, configs.len() - 1));
    crate::hooks::set_tune(&p.tune).map_err(|e| anyhow!(e))?;
    let plans = [
        plan_trail(p, "taa-motion").await?,
        plan_trail(p, "taa-reveal").await?,
    ];
    // Fill the orbit / thin reference caches.
    {
        let mut rs = Session::open(opts(p, "taa-orbit", Aa::None)).await?;
        let (a, b) = rs.scenario.phase_range("strafe").unwrap_or((150, 240));
        let stop_k = rs.scenario.phase_range("stopped").map(|x| x.0).unwrap_or(b) + 8;
        while rs.next_k <= stop_k {
            let k = rs.next_k;
            rs.render_next()?;
            if (k >= a && k < b) || k == stop_k {
                reference_cached(&mut rs, p).await?;
            } else {
                gpu_idle(&rs.renderer).await;
            }
            set_hud(&format!("taa-tests: tune-sweep2 refs taa-orbit k{k}"));
        }
        rs.destroy();
    }
    for (ci, cfg) in configs.iter().enumerate() {
        if ci < lo || ci > hi {
            continue;
        }
        let mut tune = p.tune.clone();
        for (k, v) in cfg {
            tune.retain(|(n, _)| n != k);
            tune.push((k.clone(), *v));
        }
        let label = if cfg.is_empty() {
            "defaults".to_string()
        } else {
            cfg.iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(",")
        };
        let name = format!("tune-sweep2:#{ci}:{label}");
        if let Err(e) = crate::hooks::set_tune(&tune) {
            r.emit(&name, false, &e);
            continue;
        }
        let mut parts = Vec::new();
        for plan in &plans {
            parts.push(
                run_trail(p, plan)
                    .await
                    .unwrap_or_else(|e| format!("error {e:#}")),
            );
        }
        parts.push(
            run_orbit(p)
                .await
                .unwrap_or_else(|e| format!("error {e:#}")),
        );
        parts.push(
            run_static(p)
                .await
                .unwrap_or_else(|e| format!("error {e:#}")),
        );
        r.info(
            &name,
            &format!(
                "({})\n    {}",
                crate::hooks::tune_label(),
                parts.join("\n    ")
            ),
        );
    }
    Ok(())
}
