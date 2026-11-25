//! `?mode=trail-oracle&scenes=taa-skin[&moving=fox,morph-cube][&k=…]`: the
//! trail test (05 §4.2) for a scene whose moving things are not a tracked
//! box, with the band measured from real coverage and a perfect-information
//! oracle beside every number (INFO lines; the gate stays `?mode=gate`).
//!
//! Why: for a scene without a tracked box the gate's band comes from the
//! motion vectors ("covered" = |v| > 0.05 px). A planted foot, a limb at the
//! turn of its swing or a morph at the end of its stroke has |v| ≈ 0 while it
//! still covers its pixels, so that band also holds pixels the object covers
//! NOW (and misses pixels a slow limb just left). This mode measures
//! coverage itself ([`CoverageProbe`]): two lockstep TAA sessions, one with
//! the moving nodes hidden; a pixel is covered when its depth differs between
//! them at the pixel centre or at any of its four corners (conservative, like
//! the tracked box's hull). Both bands are reported. `?mode=gate`'s trail
//! uses the same probe for scenes listed in [`untracked_moving_nodes`].
//!
//! The oracle is the one of `trail-evidence`: per band pixel, the
//! resolve-model mean of TAA-off frames jittered exactly like the TAA
//! session's, from the first frame after the object uncovered the pixel,
//! capped at the native history length. Perfect vectors, perfect
//! disocclusion, no clip; it knows nothing about shading changes (a shadow
//! that moves over a still pixel), exactly like a history method.

#![allow(clippy::needless_range_loop)]

use anyhow::{anyhow, Result};
use awsm_renderer::taa::TaaDebugView;
use glam::Vec2;

use crate::metrics::{self, error_histogram, ErrorHistogram, Image, Mask};
use crate::modes::reference_cached;
use crate::reference::{encode_space, resolve_frame};
use crate::report::{set_hud, Report};
use crate::session::{finish_capture, Aa, Session, SessionOpts};
use crate::Params;

/// The trail window: 0.5 s at 60 Hz (05 §4.2).
const WINDOW: u32 = 30;
/// Oracle history depth: the resolve's native cap.
const ORACLE_CAP: usize = 16;

fn opts(p: &Params, scene: &str, aa: Aa) -> SessionOpts {
    SessionOpts {
        scene: scene.to_string(),
        aa,
        size: p.size,
        origin: p.bundles.clone(),
        warmup: p.warmup,
    }
}

/// One series (an image source over one mask family): per-frame worst and a
/// histogram pooled over every frame.
#[derive(Default)]
struct Series {
    frames: u32,
    failing: u32,
    worst_mean: (f64, u32),
    worst_max: (u8, u32),
    sum_mean: f64,
    pooled: ErrorHistogram,
}

impl Series {
    fn add(&mut self, k: u32, img: &Image, reference: &Image, mask: &Mask) -> Option<f64> {
        if mask.count() == 0 {
            return None;
        }
        let h = error_histogram(img, reference, Some(mask));
        let s = h.stats();
        self.frames += 1;
        self.sum_mean += s.mean;
        if s.mean >= 2.0 {
            self.failing += 1;
        }
        if s.mean > self.worst_mean.0 {
            self.worst_mean = (s.mean, k);
        }
        if s.max > self.worst_max.0 {
            self.worst_max = (s.max, k);
        }
        self.pooled.merge(&h);
        Some(s.mean)
    }

    fn summary(&self) -> String {
        if self.frames == 0 {
            return "no frames".into();
        }
        let p = self.pooled.stats();
        format!(
            "worst mean {:.3} @k{}, worst max {} @k{}, frames with mean >= 2: {}/{}, average of frame means {:.3}, pooled mean {:.3} p99 {} over {} px",
            self.worst_mean.0,
            self.worst_mean.1,
            self.worst_max.0,
            self.worst_max.1,
            self.failing,
            self.frames,
            self.sum_mean / self.frames as f64,
            p.mean,
            p.p99,
            self.pooled.count(),
        )
    }
}

/// Real coverage of a set of nodes, frame by frame: two lockstep TAA sessions,
/// one with the nodes hidden; a pixel is covered when its depth differs.
///
/// Conservative, like the tracked box's exact hull: besides the pixel centre
/// (jitter frozen at 0) the frame is re-rendered at the four pixel corners
/// (jitter ±0.49), and a pixel counts as covered when any of the five samples
/// hits the object. A pixel the object only partly covers, with its centre
/// outside, is then "covered now" and stays out of the band.
pub struct CoverageProbe {
    with: Session,
    without: Session,
    pub nodes: Vec<String>,
}

/// The moving nodes of a scene without a tracked box (`?moving=a,b`
/// overrides); `None` when the scene has no such list.
pub fn untracked_moving_nodes(scene: &str) -> Option<Vec<String>> {
    if let Some(v) = crate::url_value("moving") {
        let v: Vec<String> = v
            .split(',')
            .filter(|x| !x.is_empty())
            .map(str::to_string)
            .collect();
        return (!v.is_empty()).then_some(v);
    }
    match crate::session::base_scene_name(scene) {
        "taa-skin" => Some(vec!["fox".into(), "morph-cube".into()]),
        // The pop-in cube: its pixels' content changes when it appears (the
        // oracle restarts there), though nothing moves.
        _ if scene.ends_with("@popin") || scene.ends_with("@popjump") => Some(vec!["cube".into()]),
        _ => None,
    }
}

impl CoverageProbe {
    pub async fn open(p: &Params, scene: &str, nodes: Vec<String>) -> Result<Self> {
        let mut with = Session::open(opts(p, scene, Aa::Taa)).await?;
        with.renderer.set_taa_debug_view(TaaDebugView::FreezeJitter);
        let mut without = Session::open(opts(p, scene, Aa::Taa)).await?;
        without
            .renderer
            .set_taa_debug_view(TaaDebugView::FreezeJitter);
        with.assert_lockstep_safe()?;
        without.assert_lockstep_safe()?;
        let names: Vec<&str> = nodes.iter().map(String::as_str).collect();
        let hidden = without.hide_named(&names);
        if hidden.len() != names.len() || names.is_empty() {
            return Err(anyhow!("moving nodes {names:?}: only {hidden:?} found"));
        }
        Ok(Self {
            with,
            without,
            nodes,
        })
    }

    /// Render the next scenario frame and return its conservative coverage.
    pub async fn step(&mut self) -> Result<Mask> {
        let (w, h) = self.with.opts.size;
        let n = (w * h) as usize;
        self.with.render_next()?;
        self.without.render_next()?;
        let mut cov = Mask::new(w, h, false);
        let mut corner = 0usize;
        loop {
            let a = read_depth(&self.with).await?;
            let b = read_depth(&self.without).await?;
            if a.len() != n || b.len() != n {
                return Err(anyhow!(
                    "depth readback is not at the output size (native scale only)"
                ));
            }
            for i in 0..n {
                if (a[i] - b[i]).abs() > 1.0e-7 {
                    cov.bits[i] = true;
                }
            }
            if corner == 4 {
                break;
            }
            let j = Vec2::new(
                if corner & 1 == 0 { -0.49 } else { 0.49 },
                if corner & 2 == 0 { -0.49 } else { 0.49 },
            );
            self.with.render_frozen(Some(j))?;
            self.without.render_frozen(Some(j))?;
            corner += 1;
        }
        Ok(cov)
    }

    pub fn destroy(self) {
        self.with.destroy();
        self.without.destroy();
    }
}

async fn read_depth(s: &Session) -> Result<Vec<f32>> {
    let fut = s.renderer.debug_read_depth();
    let (_, _, d) = fut.await.map_err(|e| anyhow!("debug_read_depth: {e}"))?;
    Ok(d)
}

async fn capture_now(s: &mut Session) -> Result<Image> {
    let rb = s.begin_capture()?;
    finish_capture(rb, s.opts.size).await
}

pub async fn trail_oracle_mode(p: &Params, r: &mut Report) {
    let scenes: Vec<String> = if p.scenes_given {
        p.scenes.clone()
    } else {
        vec!["taa-skin".into()]
    };
    for scene in &scenes {
        if let Err(e) = trail_oracle(p, scene, r).await {
            r.emit(
                &format!("trail-oracle:{scene}"),
                false,
                &format!("error: {e:#}"),
            );
        }
    }
}

async fn trail_oracle(p: &Params, scene: &str, r: &mut Report) -> Result<()> {
    let (w, h) = p.size;
    let n = (w * h) as usize;
    let mut t = Session::open(opts(p, scene, Aa::Taa)).await?;
    let mut rs = Session::open(opts(p, scene, Aa::None)).await?;
    let mut ms = Session::open(opts(p, scene, Aa::Msaa)).await?;
    let mut os = Session::open(opts(p, scene, Aa::None)).await?;
    let moving = match untracked_moving_nodes(scene) {
        Some(m) => m,
        None => t
            .scenario
            .tracked
            .map(|(name, _)| vec![name.to_string()])
            .ok_or_else(|| anyhow!("{scene}: no moving nodes known (pass ?moving=a,b)"))?,
    };
    let mut probe = CoverageProbe::open(p, scene, moving.clone()).await?;
    for sess in [&t, &rs, &ms, &os] {
        sess.assert_lockstep_safe()?;
    }
    let frames = t.scenario.frames;
    let evidence_ks: Vec<u32> = if p.ks_given { p.ks.clone() } else { Vec::new() };

    // Last frame each pixel was covered, by real coverage and by the gate's
    // vector threshold.
    let mut last_cov: Vec<Option<u32>> = vec![None; n];
    let mut last_cov_v: Vec<Option<u32>> = vec![None; n];
    let mut ring: std::collections::VecDeque<(u32, Vec<[f32; 3]>)> = Default::default();

    let mut s_taa = Series::default();
    let mut s_oracle = Series::default();
    let mut s_none = Series::default();
    let mut s_msaa = Series::default();
    let mut s_taa_ring = Series::default();
    let mut s_oracle_ring = Series::default();
    let mut s_taa_rest = Series::default();
    let mut s_oracle_rest = Series::default();
    let mut s_taa_vband = Series::default();
    // The vector band split by what it really is.
    let mut s_taa_vband_covered = Series::default();
    let mut s_taa_vband_uncovered = Series::default();
    // By age (frames since the object left the pixel): 1, 2-3, 4-8, 9+.
    let age_names = ["age 1", "age 2-3", "age 4-8", "age 9-30"];
    let mut s_taa_age: Vec<Series> = (0..4).map(|_| Series::default()).collect();
    let mut s_oracle_age: Vec<Series> = (0..4).map(|_| Series::default()).collect();
    let mut per_frame: Vec<String> = Vec::new();
    let mut covered_px_sum = 0u64;
    let mut vcov_px_sum = 0u64;
    let mut vcov_and_cov_sum = 0u64;

    while t.next_k < frames {
        let k = t.next_k;
        let (_, test) = t.render_next_captured().await?;
        let j = t.renderer.camera.jitter_px();
        let (_, plain) = rs.render_next_captured().await?;
        let (_, msaa) = ms.render_next_captured().await?;
        os.render_next_jittered(Some(j))?;
        let ojit = capture_now(&mut os).await?;
        ring.push_back((k, resolve_frame(&ojit, j, p.karis)));
        if ring.len() > ORACLE_CAP {
            ring.pop_front();
        }
        let cov = probe.step().await?;
        // The gate's coverage: |v| > 0.05 px in the TAA session's vectors.
        let (vw, vh, vel) = t
            .renderer
            .debug_read_velocity()
            .await
            .map_err(|e| anyhow!("debug_read_velocity: {e}"))?;
        if (vw, vh) != (w, h) {
            return Err(anyhow!("velocity readback is not at the output size"));
        }
        let size = Vec2::new(w as f32, h as f32);
        let mut vcov = Mask::new(w, h, false);
        for i in 0..n {
            let vv = Vec2::new(vel[i * 2], vel[i * 2 + 1]);
            let marker = (vv.x - 2.0).abs() < 1e-3 && (vv.y - 2.0).abs() < 1e-3;
            vcov.bits[i] = !marker && (vv * size).length() > 0.05;
        }
        covered_px_sum += cov.count() as u64;
        vcov_px_sum += vcov.count() as u64;
        vcov_and_cov_sum += vcov.and(&cov).count() as u64;

        if k >= 1 {
            let mut band = Mask::new(w, h, false);
            let mut vband = Mask::new(w, h, false);
            let mut age = vec![0u32; n];
            for i in 0..n {
                if !cov.bits[i] {
                    if let Some(c) = last_cov[i] {
                        let a = k - c;
                        if (1..=WINDOW).contains(&a) {
                            band.bits[i] = true;
                            age[i] = a;
                        }
                    }
                }
                if !vcov.bits[i] {
                    if let Some(c) = last_cov_v[i] {
                        if (1..=WINDOW).contains(&(k - c)) {
                            vband.bits[i] = true;
                        }
                    }
                }
            }
            if band.count() > 0 || vband.count() > 0 {
                let reference = reference_cached(&mut rs, p).await?;
                let mut oracle = test.clone();
                for i in 0..n {
                    if !band.bits[i] {
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
                    oracle.rgba[i * 4..i * 4 + 4].copy_from_slice(&encode_space(c, p.karis));
                }
                let ring15 = metrics::within_distance_of_mask(&cov, 1.5).and(&band);
                let rest = band.and(&ring15.not());
                let mt = s_taa.add(k, &test, &reference, &band);
                let mo = s_oracle.add(k, &oracle, &reference, &band);
                s_none.add(k, &plain, &reference, &band);
                s_msaa.add(k, &msaa, &reference, &band);
                s_taa_ring.add(k, &test, &reference, &ring15);
                s_oracle_ring.add(k, &oracle, &reference, &ring15);
                s_taa_rest.add(k, &test, &reference, &rest);
                s_oracle_rest.add(k, &oracle, &reference, &rest);
                let mv = s_taa_vband.add(k, &test, &reference, &vband);
                s_taa_vband_covered.add(k, &test, &reference, &vband.and(&cov));
                s_taa_vband_uncovered.add(k, &test, &reference, &vband.and(&cov.not()));
                for (ai, (lo, hi)) in [(1, 1), (2, 3), (4, 8), (9, WINDOW)]
                    .into_iter()
                    .enumerate()
                {
                    let mut m = Mask::new(w, h, false);
                    for i in 0..n {
                        m.bits[i] = band.bits[i] && age[i] >= lo && age[i] <= hi;
                    }
                    s_taa_age[ai].add(k, &test, &reference, &m);
                    s_oracle_age[ai].add(k, &oracle, &reference, &m);
                }
                per_frame.push(format!(
                    "k{k}:{}/{}/{}/{}",
                    mt.map(|v| format!("{v:.2}")).unwrap_or_else(|| "-".into()),
                    mo.map(|v| format!("{v:.2}")).unwrap_or_else(|| "-".into()),
                    band.count(),
                    mv.map(|v| format!("{v:.2}")).unwrap_or_else(|| "-".into()),
                ));
                if evidence_ks.contains(&k) {
                    let base = format!("r2/trail-oracle/{scene}-k{k:03}");
                    let mut overlay = test.clone();
                    for i in 0..n {
                        if band.bits[i] {
                            overlay.rgba[i * 4] = 255;
                        }
                        if vband.bits[i] {
                            overlay.rgba[i * 4 + 2] = 255;
                        }
                    }
                    for (name, img) in [
                        ("taa", &test),
                        ("ref", &reference),
                        ("oracle", &oracle),
                        ("noaa", &plain),
                        ("msaa", &msaa),
                        ("bands", &overlay),
                    ] {
                        crate::store::put(&p.store, &format!("{base}-{name}.png"), img).await?;
                    }
                    let err = metrics::error_image(&test, &reference, 8.0);
                    crate::store::put(&p.store, &format!("{base}-err8.png"), &err).await?;
                    let erro = metrics::error_image(&oracle, &reference, 8.0);
                    crate::store::put(&p.store, &format!("{base}-oracle-err8.png"), &erro).await?;
                }
            }
        }
        for i in 0..n {
            if cov.bits[i] {
                last_cov[i] = Some(k);
            }
            if vcov.bits[i] {
                last_cov_v[i] = Some(k);
            }
        }
        set_hud(&format!("taa-tests: trail-oracle {scene} k{k}/{frames}"));
    }
    for s in [t, rs, ms, os] {
        s.destroy();
    }
    probe.destroy();
    r.info(
        &format!("trail-oracle:{scene}:coverage"),
        &format!(
            "moving nodes {moving:?}; per frame on average {} px covered (depth with vs without them at the pixel centre and its four corners), {} px with |v| > 0.05 px, {} px both: the vector threshold misses {:.1}% of the covered px",
            covered_px_sum / frames as u64,
            vcov_px_sum / frames as u64,
            vcov_and_cov_sum / frames as u64,
            100.0 * (1.0 - vcov_and_cov_sum as f64 / covered_px_sum.max(1) as f64),
        ),
    );
    for (name, s) in [
        ("TAA", &s_taa),
        ("ORACLE", &s_oracle),
        ("no-AA", &s_none),
        ("MSAA", &s_msaa),
        ("TAA, 1.5 px ring", &s_taa_ring),
        ("ORACLE, 1.5 px ring", &s_oracle_ring),
        ("TAA, minus ring", &s_taa_rest),
        ("ORACLE, minus ring", &s_oracle_rest),
    ] {
        r.info(
            &format!("trail-oracle:{scene}:coverage-band:{name}"),
            &s.summary(),
        );
    }
    for ai in 0..4 {
        r.info(
            &format!("trail-oracle:{scene}:coverage-band:{}", age_names[ai]),
            &format!(
                "TAA {} | ORACLE {}",
                s_taa_age[ai].summary(),
                s_oracle_age[ai].summary()
            ),
        );
    }
    r.info(
        &format!("trail-oracle:{scene}:vector-band:TAA"),
        &format!(
            "(the gate's band: |v| > 0.05 px within 30 frames, not now) {} || of it, px the object covers NOW: {} || px it does not cover: {}",
            s_taa_vband.summary(),
            s_taa_vband_covered.summary(),
            s_taa_vband_uncovered.summary()
        ),
    );
    r.info(
        &format!("trail-oracle:{scene}:per-frame"),
        &format!(
            "k:TAA/ORACLE/band px/TAA on the vector band — {}",
            per_frame.join(" ")
        ),
    );
    Ok(())
}
