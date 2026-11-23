//! `?mode=tune-sweep&refcache=<name>`: the motion-tuning grid
//! `max_history_moving` × `motion_edge_alpha` (`?mhm=2,4,8,16`,
//! `?mea=0,0.3,0.6,1`; `?combos=a-b` runs a slice of the grid, row-major), on
//! top of `?tune=`. Per combination (INFO lines; the gates are separate):
//!
//! - taa-motion trail at every frame with a band: worst mean / max over the
//!   literal band, split into "within 2 px of the current silhouette" and
//!   "the rest", plus the 1.5 px ring and band-minus-ring;
//! - taa-orbit: stop (mean 8 frames after the strafe stops) and pan (every
//!   strafe frame: worst and average mean);
//! - taa-thin slow-pan phase: shimmer = TAA's frame-to-frame mean difference
//!   minus the reference's own frame-to-frame mean difference (per frame;
//!   average and worst), next to TAA's error against the reference (blur).
//!
//! References come from the `refcache` (they do not depend on the tuning),
//! so only the first combination pays for them.

use anyhow::Result;

use crate::gates::{near_silhouette, tracked_coverage};
use crate::metrics::{abs_error, trail_band, Image, Mask};
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

fn floats(key: &str, default: &[f32]) -> Vec<f32> {
    crate::url_value(key)
        .map(|v| v.split(',').filter_map(|x| x.trim().parse().ok()).collect())
        .unwrap_or_else(|| default.to_vec())
}

/// Worst mean / worst max / failing-frame count of a per-frame series.
#[derive(Default)]
struct Series {
    n: u32,
    worst_mean: (f64, u32),
    worst_max: (u8, u32),
    sum_mean: f64,
    failing: u32,
}

impl Series {
    fn add(&mut self, k: u32, img: &Image, reference: &Image, m: &Mask) {
        if m.count() == 0 {
            return;
        }
        let s = abs_error(img, reference, Some(m));
        self.n += 1;
        self.sum_mean += s.mean;
        if s.mean >= self.worst_mean.0 {
            self.worst_mean = (s.mean, k);
        }
        if s.max >= self.worst_max.0 {
            self.worst_max = (s.max, k);
        }
        if !(s.mean < 2.0 && s.max < 16) {
            self.failing += 1;
        }
    }

    fn show(&self) -> String {
        format!(
            "worst mean {:.3} @k{}, worst max {} @k{}, avg mean {:.3}, {} / {} frames fail (mean < 2, max < 16)",
            self.worst_mean.0,
            self.worst_mean.1,
            self.worst_max.0,
            self.worst_max.1,
            self.sum_mean / self.n.max(1) as f64,
            self.failing,
            self.n
        )
    }
}

async fn open_pair(p: &Params, scene: &str) -> Result<(Session, Session)> {
    let t = Session::open(opts(p, scene, Aa::Taa)).await?;
    let rs = Session::open(opts(p, scene, Aa::None)).await?;
    t.assert_lockstep_safe()?;
    rs.assert_lockstep_safe()?;
    Ok((t, rs))
}

async fn step(t: &mut Session, rs: &mut Session, capture: bool) -> Result<Option<Image>> {
    if capture {
        let (_, img) = t.render_next_captured().await?;
        rs.render_next()?;
        gpu_idle(&rs.renderer).await;
        Ok(Some(img))
    } else {
        t.render_next()?;
        rs.render_next()?;
        gpu_idle(&t.renderer).await;
        gpu_idle(&rs.renderer).await;
        Ok(None)
    }
}

async fn trail_motion(p: &Params) -> Result<String> {
    let (mut t, mut rs) = open_pair(p, "taa-motion").await?;
    let frames = t.scenario.frames;
    let mut history: Vec<Mask> = Vec::new();
    let (mut lit, mut near2, mut rest2, mut ring15, mut minus15) = (
        Series::default(),
        Series::default(),
        Series::default(),
        Series::default(),
        Series::default(),
    );
    while t.next_k < frames {
        let k = t.next_k;
        let band_possible = history.iter().any(|m| m.count() > 0);
        let img = step(&mut t, &mut rs, band_possible).await?;
        let now = tracked_coverage(&t).unwrap_or_else(|| Mask::new(p.size.0, p.size.1, false));
        if let Some(img) = img {
            let recent: Vec<&Mask> = history.iter().rev().take(30).collect();
            let band = trail_band(&recent, &now);
            if band.count() > 0 {
                let reference = reference_cached(&mut rs, p).await?;
                let n2 = near_silhouette(&t, &now, 2.0).and(&band);
                let n15 = near_silhouette(&t, &now, 1.5).and(&band);
                lit.add(k, &img, &reference, &band);
                near2.add(k, &img, &reference, &n2);
                rest2.add(k, &img, &reference, &band.and(&n2.not()));
                ring15.add(k, &img, &reference, &n15);
                minus15.add(k, &img, &reference, &band.and(&n15.not()));
            }
        }
        history.push(now);
        if history.len() > 31 {
            history.remove(0);
        }
        set_hud(&format!("taa-tests: tune-sweep taa-motion k{k}"));
    }
    t.destroy();
    rs.destroy();
    Ok(format!(
        "taa-motion trail, every frame: literal band {}\n      within 2 px of the silhouette: {}\n      the rest (> 2 px): {}\n      1.5 px ring: {}\n      band minus the 1.5 px ring: {}",
        lit.show(),
        near2.show(),
        rest2.show(),
        ring15.show(),
        minus15.show()
    ))
}

async fn orbit_stop_pan(p: &Params) -> Result<String> {
    let (mut t, mut rs) = open_pair(p, "taa-orbit").await?;
    let (a, b) = t.scenario.phase_range("strafe").unwrap_or((150, 240));
    let stop_k = t.scenario.phase_range("stopped").map(|x| x.0).unwrap_or(b) + 8;
    let (mut worst, mut sum, mut cnt) = ((0.0f64, 0u32), 0.0f64, 0u32);
    let mut stop = String::new();
    while t.next_k <= stop_k {
        let k = t.next_k;
        let want = (k >= a && k < b) || k == stop_k;
        let img = step(&mut t, &mut rs, want).await?;
        if let Some(img) = img {
            let reference = reference_cached(&mut rs, p).await?;
            let s = abs_error(&img, &reference, None);
            if k == stop_k {
                stop = format!("stop k{k}: mean {:.4} p99 {} (< 1 required)", s.mean, s.p99);
            } else {
                sum += s.mean;
                cnt += 1;
                if s.mean >= worst.0 {
                    worst = (s.mean, k);
                }
            }
        }
        set_hud(&format!("taa-tests: tune-sweep taa-orbit k{k}"));
    }
    t.destroy();
    rs.destroy();
    Ok(format!(
        "taa-orbit {stop}; pan k{a}..{b} (every frame): worst mean {:.4} @k{}, avg mean {:.4} (< 3 required)",
        worst.0,
        worst.1,
        sum / cnt.max(1) as f64
    ))
}

async fn thin_pan_shimmer(p: &Params) -> Result<String> {
    let (mut t, mut rs) = open_pair(p, "taa-thin").await?;
    let (a, b) = t.scenario.phase_range("pan").unwrap_or((210, 300));
    let mut prev: Option<(Image, Image)> = None;
    let (mut excess_sum, mut excess_worst, mut n) = (0.0f64, (f64::NEG_INFINITY, 0u32), 0u32);
    let (mut taa_f2f, mut ref_f2f, mut blur) = (0.0f64, 0.0f64, 0.0f64);
    while t.next_k < b {
        let k = t.next_k;
        let img = step(&mut t, &mut rs, k >= a).await?;
        if let Some(img) = img {
            let reference = reference_cached(&mut rs, p).await?;
            blur += abs_error(&img, &reference, None).mean;
            if let Some((pi, pr)) = &prev {
                let ft = abs_error(&img, pi, None).mean;
                let fr = abs_error(&reference, pr, None).mean;
                let e = ft - fr;
                excess_sum += e;
                taa_f2f += ft;
                ref_f2f += fr;
                n += 1;
                if e > excess_worst.0 {
                    excess_worst = (e, k);
                }
            }
            prev = Some((img, reference));
        }
        set_hud(&format!("taa-tests: tune-sweep taa-thin k{k}"));
    }
    t.destroy();
    rs.destroy();
    let nn = n.max(1) as f64;
    Ok(format!(
        "taa-thin pan k{a}..{b}: shimmer (TAA f2f − reference f2f) avg {:.4}, worst {:.4} @k{} (TAA f2f avg {:.4}, reference f2f avg {:.4}); blur (TAA vs reference) avg mean {:.4}",
        excess_sum / nn,
        excess_worst.0,
        excess_worst.1,
        taa_f2f / nn,
        ref_f2f / nn,
        blur / (n + 1) as f64
    ))
}

pub async fn tune_sweep_mode(p: &Params, r: &mut Report) {
    let mhm = floats("mhm", &[2.0, 4.0, 8.0, 16.0]);
    let mea = floats("mea", &[0.0, 0.3, 0.6, 1.0]);
    let grid: Vec<(f32, f32)> = mhm
        .iter()
        .flat_map(|m| mea.iter().map(move |e| (*m, *e)))
        .collect();
    let (lo, hi) = p.combos.unwrap_or((0, grid.len().saturating_sub(1)));
    if p.refcache.is_none() {
        r.info(
            "tune-sweep",
            "no ?refcache= given: every combination rebuilds every reference (slow)",
        );
    }
    for (i, (m, e)) in grid.iter().enumerate() {
        if i < lo || i > hi {
            continue;
        }
        let mut tune = p.tune.clone();
        tune.retain(|(k, _)| k != "max_history_moving" && k != "motion_edge_alpha");
        tune.push(("max_history_moving".into(), *m));
        tune.push(("motion_edge_alpha".into(), *e));
        let name = format!("tune-sweep:#{i}:max_history_moving={m},motion_edge_alpha={e}");
        if let Err(err) = crate::hooks::set_tune(&tune) {
            r.emit(&name, false, &err);
            continue;
        }
        let mut parts = Vec::new();
        for res in [
            trail_motion(p).await,
            orbit_stop_pan(p).await,
            thin_pan_shimmer(p).await,
        ] {
            match res {
                Ok(s) => parts.push(s),
                Err(err) => parts.push(format!("error: {err:#}")),
            }
        }
        r.info(
            &name,
            &format!(
                "({})\n    {}",
                crate::hooks::tune_label(),
                parts.join("\n    ")
            ),
        );
    }
    let _ = crate::hooks::set_tune(&p.tune);
}
