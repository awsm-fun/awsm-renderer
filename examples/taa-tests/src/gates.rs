//! The 05 §4.2–4.3 gates: ghosting (trail, stop, cut) and convergence
//! (converged quality, flicker, slow-pan blur), as `?mode=gate&gates=…`.
//!
//! Every comparison is against the jittered ground-truth reference (05 §3)
//! built from a SECOND session of the same scene with TAA off, played in
//! lockstep with the session under test, so both see identical scene state at
//! frame `k` and the reference never perturbs the run under test. Errors are
//! the display-encoded 0–255 per-pixel max-channel errors of `metrics`.
//!
//! Thresholds are 05's, multiplied by `?relax=` (1.5 at render scale 0.67,
//! 2 at 0.5 per 05 §4.5; default 1).
//!
//! A failing line is followed by debug-view evidence in the 05 §6 order
//! (FreezeJitter, Velocity, ReprojectionError, Rejection, HistoryLength) at
//! its worst frame, stored as PNGs under `phase2/` in the capture store.

use anyhow::{anyhow, Result};
use awsm_renderer::taa::TaaDebugView;
use glam::{Vec2, Vec4};

use crate::metrics::{self, abs_error, ErrorHistogram, ErrorStats, Image, Mask};
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

/// The AA under test (first of `?aa=`, default TAA).
fn test_aa(p: &Params) -> Aa {
    if p.aa_given {
        p.aas.first().copied().unwrap_or(Aa::Taa)
    } else {
        Aa::Taa
    }
}

/// Exact conservative coverage of the scenario's tracked box at the frame the
/// session last rendered, from its 8 projected corners (unjittered camera).
pub fn tracked_coverage(s: &Session) -> Option<Mask> {
    let hull = tracked_hull(s)?;
    let (w, h) = s.opts.size;
    Some(metrics::rasterize_convex(&hull, w, h))
}

/// Pixels whose centre is within `r` px of the moving object's current
/// silhouette (inside included): exact for a tracked box (its projected
/// hull), else `now` grown by `r`.
pub fn near_silhouette(s: &Session, now: &Mask, r: f32) -> Mask {
    match tracked_hull(s) {
        Some(hull) => {
            let (w, h) = s.opts.size;
            metrics::within_distance_of_convex(&hull, w, h, r).or(now)
        }
        None => metrics::within_distance_of_mask(now, r),
    }
}

/// The screen-space convex hull (pixels) of the tracked box.
pub fn tracked_hull(s: &Session) -> Option<Vec<Vec2>> {
    let (name, half) = s.scenario.tracked?;
    let corners = s.node_box_corners(name, half)?;
    let vp = s.view_projection()?;
    let (w, h) = s.opts.size;
    let pts: Vec<Vec2> = corners
        .iter()
        .filter_map(|c| {
            let clip = vp * Vec4::new(c.x, c.y, c.z, 1.0);
            (clip.w > 1e-6).then(|| {
                let ndc = clip.truncate() / clip.w;
                Vec2::new(
                    (ndc.x * 0.5 + 0.5) * w as f32,
                    (0.5 - ndc.y * 0.5) * h as f32,
                )
            })
        })
        .collect();
    Some(metrics::convex_hull(&pts))
}

/// Coverage of "the moving objects" at the frame just rendered: the tracked
/// box's exact hull when the scene has one, else every pixel whose motion
/// vector (read back from the TAA session) exceeds 0.05 px/frame.
async fn moving_coverage(s: &Session) -> Result<Mask> {
    if let Some(m) = tracked_coverage(s) {
        return Ok(m);
    }
    let fut = s.renderer.debug_read_velocity();
    let (w, h, v) = fut.await.map_err(|e| anyhow!("debug_read_velocity: {e}"))?;
    let size = Vec2::new(w as f32, h as f32);
    let mut m = Mask::new(w, h, false);
    for (i, c) in v.chunks_exact(2).enumerate() {
        let vv = Vec2::new(c[0], c[1]);
        let marker = (vv.x - 2.0).abs() < 1e-3 && (vv.y - 2.0).abs() < 1e-3;
        m.bits[i] = !marker && (vv * size).length() > 0.05;
    }
    // Upscaling: the readback is at render size; resample (nearest) to the
    // output size the images are compared at.
    let (ow, oh) = s.opts.size;
    if (w, h) != (ow, oh) {
        let mut o = Mask::new(ow, oh, false);
        for y in 0..oh {
            for x in 0..ow {
                let sx = ((x as u64 * w as u64) / ow as u64) as u32;
                let sy = ((y as u64 * h as u64) / oh as u64) as u32;
                o.set(x, y, m.get(sx.min(w - 1), sy.min(h - 1)));
            }
        }
        return Ok(o);
    }
    Ok(m)
}

/// Lockstep pair: the session under test and a TAA-off reference session.
struct Pair {
    test: Session,
    refs: Session,
}

impl Pair {
    async fn open(p: &Params, scene: &str) -> Result<Pair> {
        let test = Session::open(opts(p, scene, test_aa(p))).await?;
        let refs = Session::open(opts(p, scene, Aa::None)).await?;
        test.assert_lockstep_safe()?;
        refs.assert_lockstep_safe()?;
        // Ghosting / convergence gates run with the sharpening off (05 §4.5).
        if let Some(taa) = &test.renderer.anti_aliasing.taa {
            if taa.sharpen != 0.0 {
                return Err(anyhow!(
                    "TAA sharpen is {} (must be 0 in the gates)",
                    taa.sharpen
                ));
            }
        }
        Ok(Pair { test, refs })
    }

    /// Render frame `next_k` in both (no capture).
    async fn step(&mut self) -> Result<u32> {
        let k = self.test.render_next()?;
        self.refs.render_next()?;
        gpu_idle(&self.test.renderer).await;
        gpu_idle(&self.refs.renderer).await;
        Ok(k)
    }

    /// Render frame `next_k` in both; capture the test frame; build the
    /// reference (display-linear) for it. Returns (k, test, reference, plain
    /// TAA-off frame).
    async fn step_with_reference(&mut self, p: &Params) -> Result<(u32, Image, Image, Image)> {
        let (k, test) = self.test.render_next_captured().await?;
        let (_, plain) = self.refs.render_next_captured().await?;
        let lin = reference_cached(&mut self.refs, p).await?;
        Ok((k, test, lin, plain))
    }

    fn destroy(self) {
        self.test.destroy();
        self.refs.destroy();
    }
}

fn thr(p: &Params, v: f64) -> f64 {
    v * p.relax
}

/// A per-frame gate's verdict: counted only at stride 1 (every frame). With
/// `stride` > 1 the line is INFO, labelled "every Nth frame — not the gate".
fn stride_verdict(p: &Params, r: &mut Report, name: &str, ok: bool, detail: &str) {
    if p.stride <= 1 {
        r.emit(name, ok, &format!("every frame: {detail}"));
    } else {
        r.info(
            name,
            &format!(
                "{} — every {}th frame only, NOT the gate: {detail}",
                if ok { "would PASS" } else { "would FAIL" },
                p.stride
            ),
        );
    }
}

/// Track the worst of a series of stats.
#[derive(Default)]
struct Worst {
    frames: u32,
    failing: Vec<u32>,
    worst_mean: (f64, u32),
    worst_max: (u8, u32),
}

impl Worst {
    fn add(&mut self, k: u32, s: &ErrorStats, ok: bool) {
        self.frames += 1;
        if !ok {
            self.failing.push(k);
        }
        if s.mean >= self.worst_mean.0 {
            self.worst_mean = (s.mean, k);
        }
        if s.max >= self.worst_max.0 {
            self.worst_max = (s.max, k);
        }
    }

    /// The frame to show evidence for: the worst-mean frame of a failing line.
    fn evidence_frame(&self) -> Option<u32> {
        (!self.failing.is_empty()).then_some(self.worst_mean.1)
    }

    fn summary(&self) -> String {
        let fails = if self.failing.is_empty() {
            String::new()
        } else {
            let shown: Vec<String> = self
                .failing
                .iter()
                .take(16)
                .map(|k| k.to_string())
                .collect();
            format!(
                "; failing frames ({}): {}{}",
                self.failing.len(),
                shown.join(","),
                if self.failing.len() > 16 { ",…" } else { "" }
            )
        };
        format!(
            "{} frames; worst mean {:.3} @k{}, worst max {} @k{}{}",
            self.frames,
            self.worst_mean.0,
            self.worst_mean.1,
            self.worst_max.0,
            self.worst_max.1,
            fails
        )
    }
}

/// 05 §6 evidence for a failing line at frame `k`: the frame with
/// `FreezeJitter` (a whole run with the jitter frozen), then the frame-`k`
/// normal / Velocity / ReprojectionError / Rejection / HistoryLength views
/// (each a replay of the same deterministic run, switched to the view just
/// before frame `k`). PNGs under `phase2/<gate>-<scene>-k<k>-<view>.png`.
async fn evidence(p: &Params, gate: &str, scene: &str, k: u32, r: &mut Report) -> Result<()> {
    let base = format!("phase2/{gate}-{}-k{k:03}", scene.replace('@', "_"));
    let mut views: Vec<String> = Vec::new();
    {
        let mut s = Session::open(opts(p, scene, Aa::Taa)).await?;
        s.renderer.set_taa_debug_view(TaaDebugView::FreezeJitter);
        if k > 0 {
            s.play_to(k - 1).await?;
        }
        let (_, img) = s.render_next_captured().await?;
        crate::store::put(&p.store, &format!("{base}-freeze-jitter.png"), &img).await?;
        s.destroy();
        views.push("freeze-jitter".into());
    }
    for (name, view) in [
        ("normal", TaaDebugView::None),
        ("velocity", TaaDebugView::Velocity),
        ("reprojection-error", TaaDebugView::ReprojectionError),
        ("rejection", TaaDebugView::Rejection),
        ("history-length", TaaDebugView::HistoryLength),
    ] {
        let mut s = Session::open(opts(p, scene, Aa::Taa)).await?;
        if k > 0 {
            s.play_to(k - 1).await?;
        }
        s.renderer.set_taa_debug_view(view);
        let (_, img) = s.render_next_captured().await?;
        crate::store::put(&p.store, &format!("{base}-{name}.png"), &img).await?;
        s.destroy();
        views.push(name.into());
    }
    r.info(
        &format!("evidence:{gate}:{scene}:k{k}"),
        &format!("stored {base}-{{{}}}.png", views.join(",")),
    );
    Ok(())
}

/// The frame-`k` TAA image with the jitter frozen, against the reference at
/// `k` (05 §6 step 1: does the failure survive FreezeJitter?).
async fn freeze_jitter_error(
    p: &Params,
    scene: &str,
    k: u32,
    mask: Option<&Mask>,
) -> Result<ErrorStats> {
    let mut s = Session::open(opts(p, scene, Aa::Taa)).await?;
    s.renderer.set_taa_debug_view(TaaDebugView::FreezeJitter);
    let mut rs = Session::open(opts(p, scene, Aa::None)).await?;
    if k > 0 {
        s.play_to(k - 1).await?;
        rs.play_to(k - 1).await?;
    }
    let (_, img) = s.render_next_captured().await?;
    rs.render_next()?;
    let reference = reference_cached(&mut rs, p).await?;
    s.destroy();
    rs.destroy();
    Ok(abs_error(&img, &reference, mask))
}

/// 05 §4.2 trail: error vs the reference inside the trail band (covered within
/// the last 0.5 s, not now): mean < 2 and max < 16 at every evaluated frame.
async fn trail(p: &Params, scene: &str, r: &mut Report) -> Result<()> {
    let mut pair = Pair::open(p, scene).await?;
    let frames = pair.test.scenario.frames;
    let window = 30usize; // 0.5 s at 60 Hz
    let mut history: Vec<Mask> = Vec::new();
    let mut worst = Worst::default();
    let mut band_px = 0usize;
    let mut worst_band: Option<Mask> = None;
    let has_tracked_motion = pair.test.scenario.tracked.is_some();
    let mut worst_rest = Worst::default();
    let mut worst_ring = Worst::default();
    while pair.test.next_k < frames {
        let k = pair.test.next_k;
        // Only frames whose band can be non-empty need a reference.
        let band_possible = history.iter().any(|m| m.count() > 0);
        let evaluate = k >= 1 && k % p.stride == 0 && band_possible;
        let now = if evaluate {
            let (_, test, reference, _) = pair.step_with_reference(p).await?;
            let now = moving_coverage(&pair.test).await?;
            let recent: Vec<&Mask> = history.iter().rev().take(window).collect();
            let band = metrics::trail_band(&recent, &now);
            let n = band.count();
            if n > 0 {
                band_px = band_px.max(n);
                let s = abs_error(&test, &reference, Some(&band));
                let ok = s.mean < thr(p, 2.0) && (s.max as f64) < thr(p, 16.0);
                worst.add(k, &s, ok);
                // 05 review (A): the reconstruction kernel reaches 1.5 px, so
                // the ring within 1.5 px of the CURRENT silhouette is still
                // partly "covered" in the reference itself.
                let ring = near_silhouette(&pair.test, &now, 1.5).and(&band);
                let rest = band.and(&ring.not());
                for (m, wt) in [(&rest, &mut worst_rest), (&ring, &mut worst_ring)] {
                    if m.count() > 0 {
                        let st = abs_error(&test, &reference, Some(m));
                        let ok = st.mean < thr(p, 2.0) && (st.max as f64) < thr(p, 16.0);
                        wt.add(k, &st, ok);
                    }
                }
                if worst.worst_mean.1 == k {
                    worst_band = Some(band.clone());
                }
                if p.keep && (!ok || k % 30 == 0) {
                    let mut overlay = test.clone();
                    for (i, b) in band.bits.iter().enumerate() {
                        if *b {
                            overlay.rgba[i * 4] = 255;
                        }
                    }
                    let base = format!("phase2/trail-{scene}-k{k:03}");
                    crate::store::put(&p.store, &format!("{base}-band.png"), &overlay).await?;
                    crate::store::put(&p.store, &format!("{base}-taa.png"), &test).await?;
                    crate::store::put(&p.store, &format!("{base}-ref.png"), &reference).await?;
                }
            }
            now
        } else {
            pair.step().await?;
            moving_coverage(&pair.test).await?
        };
        history.push(now);
        if history.len() > window + 1 {
            history.remove(0);
        }
        set_hud(&format!("taa-tests: trail {scene} k{k}/{frames}"));
    }
    pair.destroy();
    if worst.frames == 0 {
        if has_tracked_motion {
            r.emit(
                &format!("trail:{scene}"),
                false,
                "no trail band: the tracked object moves in this scenario but no frame produced a band",
            );
        } else {
            r.info(
                &format!("trail:{scene}"),
                "n/a: no frame had a trail band (no tracked object; nothing moved against a still camera)",
            );
        }
        return Ok(());
    }
    let ok = worst.failing.is_empty();
    let detail = format!(
        "{} (thresholds mean < {:.1}, max < {:.0}; largest band {band_px} px)",
        worst.summary(),
        thr(p, 2.0),
        thr(p, 16.0),
    );
    stride_verdict(p, r, &format!("trail:{scene}"), ok, &detail);
    r.info(
        &format!("trail-band-minus-ring:{scene}"),
        &format!(
            "band minus the pixels within 1.5 px of the current silhouette: {} ({} at these thresholds)",
            worst_rest.summary(),
            if worst_rest.failing.is_empty() { "would PASS" } else { "would FAIL" }
        ),
    );
    r.info(
        &format!("trail-ring:{scene}"),
        &format!(
            "the ring within 1.5 px of the current silhouette alone: {} ({})",
            worst_ring.summary(),
            if worst_ring.failing.is_empty() {
                "would PASS"
            } else {
                "would FAIL"
            }
        ),
    );
    if let Some(k) = worst.evidence_frame() {
        let fj = freeze_jitter_error(p, scene, k, worst_band.as_ref()).await?;
        r.info(
            &format!("evidence:trail:{scene}:k{k}"),
            &format!("FreezeJitter, same band: {fj} (still failing ⇒ not a jitter bug)"),
        );
        evidence(p, "trail", scene, k, r).await?;
    }
    Ok(())
}

/// 05 §4.2 stop: 8 frames after motion stops, the whole image is within mean
/// < 1 of the reference. Scenes with a scripted `stopped` phase use it; the
/// others stop by freezing time at frame 60 (everything halts at once, the
/// jitter keeps cycling) and are measured 8 frames later.
async fn stop(p: &Params, scene: &str, r: &mut Report) -> Result<()> {
    let mut pair = Pair::open(p, scene).await?;
    let scripted = pair.test.scenario.phase_range("stopped").map(|(a, _)| a);
    let (k_stop, how) = match scripted {
        Some(a) => (a, "scripted stop"),
        None => (60, "time frozen"),
    };
    let (test, reference, k_meas) = if scripted.is_some() {
        let k = k_stop + 8;
        while pair.test.next_k < k {
            pair.step().await?;
        }
        let (_, test, reference, _) = pair.step_with_reference(p).await?;
        (test, reference, k)
    } else {
        while pair.test.next_k <= k_stop {
            pair.step().await?;
        }
        // 7 frozen frames, then the 8th captured.
        for _ in 0..7 {
            pair.test.render_frozen(None)?;
            gpu_idle(&pair.test.renderer).await;
        }
        let test = pair.test.render_frozen_captured(None).await?;
        let reference = reference_cached(&mut pair.refs, p).await?;
        (test, reference, k_stop)
    };
    // Convergence-relative (INFO): the converged error at the same view (32
    // more still frames), so "+8" reads against what TAA converges to there.
    let converged = if scripted.is_some() {
        for _ in 0..31 {
            pair.step().await?;
        }
        let (k, img, refc, _) = pair.step_with_reference(p).await?;
        Some((k, abs_error(&img, &refc, None)))
    } else {
        for _ in 0..31 {
            pair.test.render_frozen(None)?;
            gpu_idle(&pair.test.renderer).await;
        }
        let img = pair.test.render_frozen_captured(None).await?;
        Some((k_stop, abs_error(&img, &reference, None)))
    };
    pair.destroy();
    let s = abs_error(&test, &reference, None);
    if let Some((kc, c)) = &converged {
        r.info(
            &format!("stop-relative:{scene}"),
            &format!(
                "+8 mean {:.4} minus converged mean {:.4} (k{kc}, 32 frames later, same view) = {:+.4}",
                s.mean,
                c.mean,
                s.mean - c.mean
            ),
        );
    }
    let ok = s.mean < thr(p, 1.0);
    r.emit(
        &format!("stop:{scene}"),
        ok,
        &format!(
            "{how} at k{k_stop}, measured 8 frames later (k{k_meas}{}): {s} (threshold mean < {:.2})",
            if scripted.is_some() { "" } else { " frozen" },
            thr(p, 1.0)
        ),
    );
    if p.keep || !ok {
        let base = format!("phase2/stop-{}", scene.replace('@', "_"));
        crate::store::put(&p.store, &format!("{base}-taa.png"), &test).await?;
        crate::store::put(&p.store, &format!("{base}-ref.png"), &reference).await?;
        crate::store::put(
            &p.store,
            &format!("{base}-error.png"),
            &metrics::error_image(&test, &reference, 8.0),
        )
        .await?;
    }
    if !ok && scripted.is_some() {
        let fj = freeze_jitter_error(p, scene, k_meas, None).await?;
        r.info(
            &format!("evidence:stop:{scene}:k{k_meas}"),
            &format!("FreezeJitter: {fj}"),
        );
        evidence(p, "stop", scene, k_meas, r).await?;
    }
    Ok(())
}

/// 05 §4.2 cut: the first frame after each camera switch has no pixel from
/// before it. A switch is a camera position jump (> 0.5 world units), a
/// rotation (forward axes > 10° apart) or a projection change between
/// consecutive frames. taa-cut's first switch is flagged
/// (`reset_temporal_history`), the second is not (the camera-jump guard must
/// catch it); each line reports whether the guard fired.
///
/// Checks: (1) the 05 threshold, mean < 3 against a TAA-off render of the new
/// frame; (2) over the pixels whose colour differs (> 16) between the TAA-off
/// renders of the old and the new camera, the TAA frame is closer to the new
/// render than to the old one (mean), with max / p99 reported.
async fn cut(p: &Params, scene: &str, r: &mut Report) -> Result<()> {
    let mut test = Session::open(opts(p, scene, test_aa(p))).await?;
    let mut plain = Session::open(opts(p, scene, Aa::None)).await?;
    let setup = test.setup.clone();
    let sc = test.scenario.clone();
    let mut switches: Vec<(u32, bool, String)> = Vec::new();
    for k in 1..sc.frames {
        let a = crate::scenario::drive(&sc, k - 1, &setup);
        let b = crate::scenario::drive(&sc, k, &setup);
        let jump = (a.camera_world.w_axis - b.camera_world.w_axis).length();
        let fa = -a.camera_world.z_axis.truncate().normalize_or_zero();
        let fb = -b.camera_world.z_axis.truncate().normalize_or_zero();
        let angle = fa.dot(fb).clamp(-1.0, 1.0).acos().to_degrees();
        let proj = format!("{:?}", a.camera_params) != format!("{:?}", b.camera_params);
        if jump > 0.5 || angle > 10.0 || proj {
            switches.push((
                k,
                b.cut,
                format!("position jump {jump:.2}, rotation {angle:.1}°, projection changed {proj}"),
            ));
        }
    }
    if switches.is_empty() {
        return Err(anyhow!("{scene} has no camera switch"));
    }
    let mut old_plain: Option<Image> = None;
    for (k, flagged, why) in &switches {
        let k = *k;
        while test.next_k < k {
            let last = test.next_k + 1 == k;
            test.render_next()?;
            gpu_idle(&test.renderer).await;
            if last {
                old_plain = Some(plain.render_next_captured().await?.1);
            } else {
                plain.render_next()?;
                gpu_idle(&plain.renderer).await;
            }
        }
        let (_, a) = test.render_next_captured().await?;
        let guard = test.renderer.camera.jumped();
        let (_, b) = plain.render_next_captured().await?;
        let old = old_plain
            .take()
            .ok_or_else(|| anyhow!("no TAA-off frame before the switch at k{k}"))?;
        let s = abs_error(&a, &b, None);
        let mut changed = Mask::new(a.width, a.height, false);
        for y in 0..a.height {
            for x in 0..a.width {
                changed.set(x, y, metrics::pixel_error(old.get(x, y), b.get(x, y)) > 16);
            }
        }
        let to_new = abs_error(&a, &b, Some(&changed));
        let to_old = abs_error(&a, &old, Some(&changed));
        let mut closer_old = 0usize;
        for y in 0..a.height {
            for x in 0..a.width {
                if changed.get(x, y)
                    && metrics::pixel_error(a.get(x, y), old.get(x, y))
                        < metrics::pixel_error(a.get(x, y), b.get(x, y))
                {
                    closer_old += 1;
                }
            }
        }
        let ok = s.mean < thr(p, 3.0) && changed.count() > 0 && to_new.mean < to_old.mean;
        r.emit(
            &format!("cut:{scene}:k{k}"),
            ok,
            &format!(
                "{} switch ({why}); camera-jump guard {}; first frame vs TAA-off render of it: {s} (threshold mean < {:.1}); on the {} px that differ (> 16) between the old and new cameras' TAA-off renders: vs new {to_new}, vs old {to_old} (must be closer to new), {closer_old} px closer to the old frame",
                if *flagged {
                    "FLAGGED (reset_temporal_history)"
                } else {
                    "UNFLAGGED"
                },
                if guard { "FIRED" } else { "did not fire" },
                thr(p, 3.0),
                changed.count()
            ),
        );
        if p.keep || !ok {
            let base = format!("phase2/cut-{scene}-k{k:03}");
            crate::store::put(&p.store, &format!("{base}-taa.png"), &a).await?;
            crate::store::put(&p.store, &format!("{base}-plain.png"), &b).await?;
            crate::store::put(&p.store, &format!("{base}-plain-before.png"), &old).await?;
        }
        if !ok {
            evidence(p, "cut", scene, k, r).await?;
        }
        // Adjacent switches: this frame is the next one's "before".
        old_plain = Some(b);
    }
    test.destroy();
    plain.destroy();
    Ok(())
}

/// 05 §4.3 converged quality: on the static scene after 32 frames, TAA's
/// error against the reference is lower than MSAA 4×'s (SMAA and no-AA for
/// the table).
async fn converged(p: &Params, scene: &str, r: &mut Report) -> Result<()> {
    let k = 32;
    let mut others = Vec::new();
    for aa in [Aa::Msaa, Aa::Smaa, Aa::None] {
        let mut s = Session::open(opts(p, scene, aa)).await?;
        s.play_to(k - 1).await?;
        let (_, img) = s.render_next_captured().await?;
        s.destroy();
        others.push((aa, img));
    }
    let mut pair = Pair::open(p, scene).await?;
    while pair.test.next_k < k {
        pair.step().await?;
    }
    let (_, test, reference, _) = pair.step_with_reference(p).await?;
    pair.destroy();
    let et = abs_error(&test, &reference, None);
    let rows: Vec<(Aa, ErrorStats)> = others
        .iter()
        .map(|(aa, img)| (*aa, abs_error(img, &reference, None)))
        .collect();
    let em = rows
        .iter()
        .find(|(aa, _)| *aa == Aa::Msaa)
        .map(|(_, s)| *s)
        .unwrap_or_default();
    let table: Vec<String> = rows
        .iter()
        .map(|(aa, s)| format!("{}: {s}", aa.name()))
        .collect();
    r.emit(
        &format!("converged:{scene}"),
        et.mean < thr(p, em.mean),
        &format!(
            "k{k}: {} vs ref {et}; {} (PASS iff TAA mean < {} × MSAA mean)",
            test_aa(p).name(),
            table.join("; "),
            p.relax
        ),
    );
    if p.keep {
        crate::store::put(
            &p.store,
            &format!("phase2/converged-{scene}-taa.png"),
            &test,
        )
        .await?;
        crate::store::put(
            &p.store,
            &format!("phase2/converged-{scene}-ref.png"),
            &reference,
        )
        .await?;
    }
    Ok(())
}

/// 05 §4.3 flicker: 60 frames of a static scene after convergence: mean
/// frame-to-frame difference < 0.5, 99th percentile < 4. A scripted `static`
/// phase is used when the scene has one long enough (taa-thin); every other
/// scene is made static by freezing time at frame 0 (the jitter keeps
/// cycling), converged for 40 frames, then measured.
async fn flicker(p: &Params, scene: &str, r: &mut Report) -> Result<()> {
    let mut s = Session::open(opts(p, scene, test_aa(p))).await?;
    let mut imgs: Vec<Image> = Vec::new();
    let how;
    if let Some((a, b)) = s
        .scenario
        .phase_range("static")
        .filter(|(a, b)| b - a >= 101)
    {
        let first = a + 40;
        let last = (first + 60).min(b - 1);
        s.play_to(first - 1).await?;
        while s.next_k <= last {
            imgs.push(s.render_next_captured().await?.1);
        }
        how = format!("scripted static phase, frames {first}..={last}");
    } else {
        s.play_to(0).await?;
        for _ in 0..40 {
            s.render_frozen(None)?;
            gpu_idle(&s.renderer).await;
        }
        for _ in 0..61 {
            imgs.push(s.render_frozen_captured(None).await?);
        }
        how = "time frozen at k0, 40 frames to converge, then 61 frames".to_string();
    }
    s.destroy();
    let mut merged = ErrorHistogram::default();
    let mut means = Vec::new();
    for w in imgs.windows(2) {
        let h = metrics::error_histogram(&w[0], &w[1], None);
        means.push(h.stats().mean);
        merged.merge(&h);
    }
    let mean = means.iter().sum::<f64>() / means.len().max(1) as f64;
    let st = merged.stats();
    let ok = mean < thr(p, 0.5) && (st.p99 as f64) < thr(p, 4.0);
    r.emit(
        &format!("flicker:{scene}"),
        ok,
        &format!(
            "{how}: mean frame-to-frame {mean:.4}, p99 {}, max {} (thresholds mean < {:.2}, p99 < {:.1})",
            st.p99,
            st.max,
            thr(p, 0.5),
            thr(p, 4.0)
        ),
    );
    if !ok {
        if let (Some(a), Some(b)) = (imgs.first(), imgs.get(1)) {
            let base = format!("phase2/flicker-{}", scene.replace('@', "_"));
            crate::store::put(&p.store, &format!("{base}-a.png"), a).await?;
            crate::store::put(
                &p.store,
                &format!("{base}-diff.png"),
                &metrics::error_image(a, b, 16.0),
            )
            .await?;
        }
    }
    Ok(())
}

/// 05 §4.3 camera-motion blur: during the slow (1 px/frame) strafe, error
/// against the reference mean < 3.
async fn pan(p: &Params, scene: &str, r: &mut Report) -> Result<()> {
    let mut pair = Pair::open(p, scene).await?;
    let (a, b) = pair
        .test
        .scenario
        .phase_range("strafe")
        .ok_or_else(|| anyhow!("{scene} has no `strafe` phase"))?;
    let mut worst = Worst::default();
    // Every strafe frame (every `stride`-th with `stride` > 1: not the gate).
    while pair.test.next_k < b {
        let k = pair.test.next_k;
        if k >= a && k % p.stride == 0 {
            let (_, test, reference, _) = pair.step_with_reference(p).await?;
            let s = abs_error(&test, &reference, None);
            worst.add(k, &s, s.mean < thr(p, 3.0));
        } else {
            pair.step().await?;
        }
        set_hud(&format!("taa-tests: pan {scene} k{k}"));
    }
    pair.destroy();
    stride_verdict(
        p,
        r,
        &format!("pan:{scene}"),
        worst.failing.is_empty() && worst.frames > 0,
        &format!(
            "strafe k{a}..{b}: {} (threshold mean < {:.1})",
            worst.summary(),
            thr(p, 3.0)
        ),
    );
    if let Some(k) = worst.evidence_frame() {
        evidence(p, "pan", scene, k, r).await?;
    }
    Ok(())
}

/// `?mode=gate&gates=…` — `ghosting` = trail + stop + cut; `convergence` =
/// converged + flicker + pan; or any list of the individual gates. Each gate
/// runs on its 05 scenes unless `?scenes=` overrides.
pub async fn gate_mode(p: &Params, r: &mut Report) {
    let mut gates: Vec<&str> = Vec::new();
    for g in &p.gates {
        match g.as_str() {
            "ghosting" => gates.extend(["trail", "stop", "cut"]),
            "convergence" => gates.extend(["converged", "flicker", "pan"]),
            "trail" => gates.push("trail"),
            "stop" => gates.push("stop"),
            "cut" => gates.push("cut"),
            "converged" => gates.push("converged"),
            "flicker" => gates.push("flicker"),
            "pan" => gates.push("pan"),
            other => r.emit(&format!("gate:{other}"), false, "unknown gate"),
        }
    }
    for g in gates {
        let default_scenes: &[&str] = match g {
            "trail" => &["taa-motion", "taa-reveal"],
            "stop" => &["taa-motion", "taa-orbit"],
            "cut" => &["taa-cut"],
            "converged" => &["taa-thin"],
            "flicker" => &[
                "taa-thin",
                "taa-motion",
                "taa-orbit",
                "taa-skin",
                "taa-reveal",
                "taa-cut",
                "taa-vertex-hook",
                "taa-cluster",
            ],
            "pan" => &["taa-orbit"],
            _ => &[],
        };
        let scenes: Vec<String> = if p.scenes_given {
            p.scenes.clone()
        } else {
            default_scenes.iter().map(|s| s.to_string()).collect()
        };
        for scene in &scenes {
            set_hud(&format!("taa-tests: {g} {scene}…"));
            let res = match g {
                "trail" => trail(p, scene, r).await,
                "stop" => stop(p, scene, r).await,
                "cut" => cut(p, scene, r).await,
                "converged" => converged(p, scene, r).await,
                "flicker" => flicker(p, scene, r).await,
                "pan" => pan(p, scene, r).await,
                _ => Ok(()),
            };
            if let Err(e) = res {
                r.emit(&format!("{g}:{scene}"), false, &format!("error: {e:#}"));
            }
        }
    }
}
