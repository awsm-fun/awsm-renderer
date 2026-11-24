//! `?mode=reactive-sweep&refcache=<name>` — the reactive weights
//! `reactive_transparent` (`?rt=0.1,0.2,0.3,0.5,0.8`) × `reactive_material`
//! (`?rm=0.2,0.4,0.6`) on taa-transparent (INFO lines; `?combos=a-b` slices
//! the grid, row-major). Per combination:
//!
//! 1. static flicker (05 §4.3, mean < 0.5 / p99 < 4): time frozen at k0, 40
//!    frames to converge, 60 frame pairs; split by content region
//!    (particles = `fountain`, glass, water, lines = `zigzag`, rest);
//! 2. the 05 §4.4 trail over each content region while the backdrop moves
//!    (backdrop trail band ∩ region), every frame, vs the 64-phase reference;
//! 3. each region's own trail (covered by that content within 30 frames, not
//!    now: moving particles, flowing water), every frame.
//!
//! The regions are measured (a lockstep TAA-off session per region with that
//! content hidden; pixels that change, dilated 1 px) and, like the MSAA and
//! no-AA columns, do not depend on the tuning: a first pass computes them
//! once (as pixel index lists) and every combination only renders TAA.
//! `particles`: the static flicker of the `particles` scene per combination
//! (whole image and the emitters' region).

use anyhow::{anyhow, Result};

use crate::gates::tracked_coverage;
use crate::metrics::{pixel_error, trail_band, ErrorHistogram, ErrorStats, Image, Mask};
use crate::modes::reference_cached;
use crate::report::{set_hud, Report};
use crate::session::{gpu_idle, Aa, Session, SessionOpts};
use crate::velocity::content_mask;
use crate::Params;

const SCENE: &str = "taa-transparent";
const REGIONS: [(&str, &str); 4] = [
    ("particles", "fountain"),
    ("glass", "glass"),
    ("water", "water"),
    ("lines", "zigzag"),
];

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

/// Worst / average over the frames of a per-frame series, 05 trail rule.
#[derive(Default, Clone)]
struct Series {
    n: u32,
    sum: f64,
    worst_mean: (f64, u32),
    worst_max: (u8, u32),
    failing: u32,
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
        if !(s.mean < 2.0 && s.max < 16) {
            self.failing += 1;
        }
    }

    fn show(&self) -> String {
        if self.n == 0 {
            return "n/a".into();
        }
        format!(
            "worst mean {:.2}@k{} max {}@k{} avg {:.2}, {}/{} fail",
            self.worst_mean.0,
            self.worst_mean.1,
            self.worst_max.0,
            self.worst_max.1,
            self.sum / self.n as f64,
            self.failing,
            self.n
        )
    }
}

/// Per frame: per region, (band ∩ region, region's own trail) index lists.
struct FrameMasks {
    k: u32,
    lists: Vec<(Vec<u32>, Vec<u32>)>,
}

/// Static per-region masks at k0 (for the flicker split).
async fn static_regions(
    p: &Params,
    scene: &str,
    regions: &[(&str, &str)],
) -> Result<Vec<(String, Mask)>> {
    let mut plain_s = Session::open(opts(p, scene, Aa::None)).await?;
    let (_, plain) = plain_s.render_next_captured().await?;
    plain_s.destroy();
    let mut out = Vec::new();
    let mut any = Mask::new(plain.width, plain.height, false);
    for (label, name) in regions {
        let mut h = Session::open(opts(p, scene, Aa::None)).await?;
        let hidden = if name.is_empty() {
            h.hide_no_vector_content(&[])
        } else {
            h.hide_named(&[name])
        };
        if hidden.is_empty() {
            h.destroy();
            return Err(anyhow!("{scene}: region `{label}` ({name}) not found"));
        }
        let (_, img) = h.render_next_captured().await?;
        h.destroy();
        let m = content_mask(&plain, &img);
        any = any.or(&m);
        out.push((label.to_string(), m));
    }
    out.push(("rest".into(), any.not()));
    Ok(out)
}

/// Static flicker of a scene per region: (whole, per-region) strings.
async fn static_flicker(p: &Params, scene: &str, regions: &[(String, Mask)]) -> Result<String> {
    let mut s = Session::open(opts(p, scene, Aa::Taa)).await?;
    s.play_to(0).await?;
    for _ in 0..40 {
        s.render_frozen(None)?;
        gpu_idle(&s.renderer).await;
    }
    let mut prev = s.render_frozen_captured(None).await?;
    let mut hs: Vec<ErrorHistogram> = vec![ErrorHistogram::default(); regions.len() + 1];
    let mut means = vec![0.0f64; regions.len() + 1];
    for _ in 0..60 {
        let img = s.render_frozen_captured(None).await?;
        for (ri, m) in regions
            .iter()
            .map(|(_, m)| Some(m))
            .chain(std::iter::once(None))
            .enumerate()
        {
            let h = crate::metrics::error_histogram(&img, &prev, m);
            means[ri] += h.stats().mean / 60.0;
            hs[ri].merge(&h);
        }
        prev = img;
    }
    s.destroy();
    let mut parts = Vec::new();
    let labels: Vec<String> = regions
        .iter()
        .map(|(l, _)| l.clone())
        .chain(std::iter::once("whole image".to_string()))
        .collect();
    for (ri, l) in labels.iter().enumerate() {
        let st = hs[ri].stats();
        let ok = means[ri] < 0.5 && st.p99 < 4;
        parts.push(format!(
            "{l} mean {:.3} p99 {} max {}{}",
            means[ri],
            st.p99,
            st.max,
            if ok { "" } else { " (FAIL)" }
        ));
    }
    Ok(parts.join(" | "))
}

pub async fn reactive_sweep_mode(p: &Params, r: &mut Report) {
    if let Err(e) = reactive_sweep(p, r).await {
        r.emit("reactive-sweep", false, &format!("error: {e:#}"));
    }
    let _ = crate::hooks::set_tune(&p.tune);
}

async fn reactive_sweep(p: &Params, r: &mut Report) -> Result<()> {
    let rts = floats("rt", &[0.1, 0.2, 0.3, 0.5, 0.8]);
    let rms = floats("rm", &[0.2, 0.4, 0.6]);
    // reactive_gamma_scale (experimental knob; NaN = not set).
    let rgs = if cfg!(feature = "taa-experimental") {
        floats("rg", &[0.5, 1.0])
    } else {
        vec![f32::NAN]
    };
    let grid: Vec<(f32, f32, f32)> = rts
        .iter()
        .flat_map(|a| rms.iter().map(move |b| (*a, *b)))
        .flat_map(|(a, b)| rgs.iter().map(move |g| (a, b, *g)))
        .collect();
    let (lo, hi) = p.combos.unwrap_or((0, grid.len().saturating_sub(1)));
    let (w, h) = p.size;
    // ---- Pass 0: tuning-independent masks, references, MSAA / no-AA ----
    let mut rs = Session::open(opts(p, SCENE, Aa::None)).await?;
    let mut ms = Session::open(opts(p, SCENE, Aa::Msaa)).await?;
    let mut hss: Vec<Session> = Vec::new();
    for (label, name) in REGIONS {
        let mut s = Session::open(opts(p, SCENE, Aa::None)).await?;
        if s.hide_named(&[name]).is_empty() {
            return Err(anyhow!("region `{label}` ({name}) not found"));
        }
        hss.push(s);
    }
    let tracked_probe = Session::open(opts(p, SCENE, Aa::None)).await?;
    let frames = tracked_probe.scenario.frames;
    tracked_probe.destroy();
    let mut hull_hist: Vec<Mask> = Vec::new();
    let mut region_hist: Vec<Vec<Mask>> = vec![Vec::new(); REGIONS.len()];
    let mut fm: Vec<FrameMasks> = Vec::new();
    // [region][band∩region, own trail][MSAA, no-AA]
    let mut base: Vec<[[Series; 2]; 2]> = vec![Default::default(); REGIONS.len()];
    while rs.next_k < frames {
        let k = rs.next_k;
        let (_, plain) = rs.render_next_captured().await?;
        let (_, msaa) = ms.render_next_captured().await?;
        let hull = tracked_coverage(&rs).unwrap_or_else(|| Mask::new(w, h, false));
        let recent: Vec<&Mask> = hull_hist.iter().rev().take(30).collect();
        let band = trail_band(&recent, &hull);
        let mut lists = Vec::new();
        let mut regions_now = Vec::new();
        for (ri, s) in hss.iter_mut().enumerate() {
            let (_, hid) = s.render_next_captured().await?;
            let region = content_mask(&plain, &hid);
            let rec: Vec<&Mask> = region_hist[ri].iter().rev().take(30).collect();
            let own = trail_band(&rec, &region);
            lists.push((indices(&band.and(&region)), indices(&own)));
            regions_now.push(region);
        }
        if lists.iter().any(|(a, b)| !a.is_empty() || !b.is_empty()) {
            let reference = reference_cached(&mut rs, p).await?;
            for (ri, (a, b)) in lists.iter().enumerate() {
                for (mi, idx) in [a, b].into_iter().enumerate() {
                    if idx.is_empty() {
                        continue;
                    }
                    base[ri][mi][0].add(k, &stats_at(&msaa, &reference, idx));
                    base[ri][mi][1].add(k, &stats_at(&plain, &reference, idx));
                }
            }
            fm.push(FrameMasks { k, lists });
        }
        hull_hist.push(hull);
        for (ri, m) in regions_now.into_iter().enumerate() {
            region_hist[ri].push(m);
            if region_hist[ri].len() > 31 {
                region_hist[ri].remove(0);
            }
        }
        if hull_hist.len() > 31 {
            hull_hist.remove(0);
        }
        set_hud(&format!("taa-tests: reactive-sweep pass 0 k{k}/{frames}"));
    }
    rs.destroy();
    ms.destroy();
    for s in hss {
        s.destroy();
    }
    let mut lines = Vec::new();
    for (ri, (label, _)) in REGIONS.iter().enumerate() {
        lines.push(format!(
            "{label}: trail (band ∩ region) MSAA {} | no-AA {}; own trail MSAA {} | no-AA {}",
            base[ri][0][0].show(),
            base[ri][0][1].show(),
            base[ri][1][0].show(),
            base[ri][1][1].show()
        ));
    }
    r.info(
        "reactive-sweep:baseline",
        &format!(
            "tuning-independent columns, {SCENE}:\n    {}",
            lines.join("\n    ")
        ),
    );
    let regions_static = static_regions(p, SCENE, &REGIONS).await?;
    let particles_regions = static_regions(p, "particles", &[("emitters", "")]).await?;
    // ---- Per combination: TAA only ----
    for (gi, (rt, rm, rg)) in grid.iter().enumerate() {
        if gi < lo || gi > hi {
            continue;
        }
        let mut tune = p.tune.clone();
        tune.retain(|(k, _)| {
            k != "reactive_transparent" && k != "reactive_material" && k != "reactive_gamma_scale"
        });
        tune.push(("reactive_transparent".into(), *rt));
        tune.push(("reactive_material".into(), *rm));
        if rg.is_finite() {
            tune.push(("reactive_gamma_scale".into(), *rg));
        }
        let name = format!(
            "reactive-sweep:#{gi}:reactive_transparent={rt},reactive_material={rm}{}",
            if rg.is_finite() {
                format!(",reactive_gamma_scale={rg}")
            } else {
                String::new()
            }
        );
        if let Err(e) = crate::hooks::set_tune(&tune) {
            r.emit(&name, false, &e);
            continue;
        }
        let flick = static_flicker(p, SCENE, &regions_static).await?;
        let pflick = static_flicker(p, "particles", &particles_regions).await?;
        let mut t = Session::open(opts(p, SCENE, Aa::Taa)).await?;
        let mut series: Vec<[Series; 2]> = vec![Default::default(); REGIONS.len()];
        let mut fi = 0usize;
        while t.next_k < frames {
            let k = t.next_k;
            let want = fi < fm.len() && fm[fi].k == k;
            if want {
                let (_, img) = t.render_next_captured().await?;
                let path = crate::modes::ref_cache_path(p, SCENE, k, p.karis).unwrap_or_default();
                let reference = crate::store::get(&p.store, &path)
                    .await?
                    .ok_or_else(|| anyhow!("reference {path} missing (needs ?refcache=)"))?;
                for (ri, (a, b)) in fm[fi].lists.iter().enumerate() {
                    if !a.is_empty() {
                        series[ri][0].add(k, &stats_at(&img, &reference, a));
                    }
                    if !b.is_empty() {
                        series[ri][1].add(k, &stats_at(&img, &reference, b));
                    }
                }
                fi += 1;
            } else {
                t.render_next()?;
                gpu_idle(&t.renderer).await;
            }
            set_hud(&format!("taa-tests: reactive-sweep #{gi} k{k}/{frames}"));
        }
        t.destroy();
        let mut lines = vec![
            format!("static flicker {SCENE}: {flick}"),
            format!("static flicker particles: {pflick}"),
        ];
        for (ri, (label, _)) in REGIONS.iter().enumerate() {
            lines.push(format!(
                "{label}: trail (band ∩ region) TAA {} | own trail TAA {}",
                series[ri][0].show(),
                series[ri][1].show()
            ));
        }
        r.info(&name, &format!("\n    {}", lines.join("\n    ")));
    }
    Ok(())
}
