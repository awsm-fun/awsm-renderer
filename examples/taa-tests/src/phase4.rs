//! Phase-4 (temporal upscaling) gate modes, 05 §4.5. The converged and pan
//! gates themselves are `?mode=gate&gates=converged,pan&scale=0.67&relax=1.5`
//! (and `scale=0.5&relax=2`): the TAA session renders at the scale, the
//! reference session is TAA-off at the FULL canvas size, so the comparison
//! is against the full-resolution reference.
//!
//! - `?mode=scale-change` (taa-orbit by default): TAA at 0.67, stepped to
//!   0.5 at k60 and back to 0.67 at k120 with `set_taa_render_scale`; every
//!   frame k40..160 against the full-resolution reference: no frame may have
//!   mean > 3 (no reset pop).
//! - `?mode=mip-bias` (taa-thin): for `mip_bias_extra` in `?mhm=`-style list
//!   `?bias=0,-0.25,-0.5,-1` at `?scale=` (default 0.67): converged error vs
//!   the reference over the whole image and over the textured panels
//!   (moiré shows as structured error there), static flicker, and the crops
//!   stored for viewing.
//! - `?mode=governor-hitch` (`?scenes=`, default kitchen-sink; use
//!   `size=1920x1435`): TAA, the render scale alternating 0.67 / 0.5 every
//!   120 frames (2 s); per-frame GPU frame time and CPU `render()` time, the
//!   step frames and the 3 after them against the median of the rest.

use anyhow::{anyhow, Result};

use crate::metrics::{abs_error, Image};
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

fn first_scene(p: &Params, default: &str) -> String {
    if p.scenes_given {
        p.scenes.first().cloned().unwrap_or_else(|| default.into())
    } else {
        default.into()
    }
}

pub async fn scale_change_mode(p: &Params, r: &mut Report) {
    let scene = first_scene(p, "taa-orbit");
    let res: Result<()> = async {
        // Switched session + two constant-scale controls (0.67 and 0.5), all
        // lockstep with the TAA-off reference session.
        crate::hooks::set_scale(0.67);
        let mut t = Session::open(opts(p, &scene, Aa::Taa)).await?;
        let mut c67 = Session::open(opts(p, &scene, Aa::Taa)).await?;
        crate::hooks::set_scale(0.5);
        let mut c50 = Session::open(opts(p, &scene, Aa::Taa)).await?;
        let mut rs = Session::open(opts(p, &scene, Aa::None)).await?;
        for s in [&t, &c67, &c50, &rs] {
            s.assert_lockstep_safe()?;
        }
        let mut worst = (0.0f64, 0u32);
        let mut over = Vec::new();
        let mut worst_excess = (f64::NEG_INFINITY, 0u32);
        let mut around = Vec::new();
        // Restated form (05 §4.5, "Restated"): against each scale's own
        // steady state. No frame may exceed the constant-scale run at the
        // same scale and frame by more than 1.5, and from 8 frames after a
        // step on, by more than 0.25 (no reset pop, and it settles).
        let mut over_control = Vec::new();
        let mut unsettled = Vec::new();
        let mut worst_settled = (f64::NEG_INFINITY, 0u32);
        while t.next_k <= 160 {
            let k = t.next_k;
            let step = match k {
                60 => Some(0.5f32),
                120 => Some(0.67f32),
                _ => None,
            };
            if let Some(sc) = step {
                t.renderer
                    .set_taa_render_scale(sc)
                    .map_err(|e| anyhow!("set_taa_render_scale({sc}): {e}"))?;
            }
            if k >= 40 {
                let (_, img) = t.render_next_captured().await?;
                let (_, i67) = c67.render_next_captured().await?;
                let (_, i50) = c50.render_next_captured().await?;
                rs.render_next()?;
                let reference = reference_cached(&mut rs, p).await?;
                let s = abs_error(&img, &reference, None);
                let control = if (60..120).contains(&k) { &i50 } else { &i67 };
                let sc = abs_error(control, &reference, None);
                let excess = s.mean - sc.mean;
                if s.mean > worst.0 {
                    worst = (s.mean, k);
                }
                if excess > worst_excess.0 {
                    worst_excess = (excess, k);
                }
                if s.mean > 3.0 {
                    over.push(k);
                }
                if excess > 1.5 {
                    over_control.push(k);
                }
                let settled = (68..120).contains(&k) || k >= 128;
                if settled {
                    if excess > worst_settled.0 {
                        worst_settled = (excess, k);
                    }
                    if excess > 0.25 {
                        unsettled.push(k);
                    }
                }
                if (58..=66).contains(&k) || (118..=126).contains(&k) {
                    around.push(format!("k{k} {:.3} (constant-scale {:.3})", s.mean, sc.mean));
                }
            } else {
                for s in [&mut t, &mut c67, &mut c50, &mut rs] {
                    s.render_next()?;
                    gpu_idle(&s.renderer).await;
                }
            }
            set_hud(&format!("taa-tests: scale-change {scene} k{k}"));
        }
        t.destroy();
        c67.destroy();
        c50.destroy();
        rs.destroy();
        r.emit(
            &format!("scale-change:{scene}"),
            over.is_empty(),
            &format!(
                "0.67 → 0.5 at k60 → 0.67 at k120, every frame k40..160 vs the full-resolution reference (05: no frame with mean > 3): worst mean {:.3} @k{}; frames with mean > 3: {over:?}; switched minus the constant-scale run at the same scale: worst {:+.3} @k{} (the pop itself); around the steps: {}",
                worst.0,
                worst.1,
                worst_excess.0,
                worst_excess.1,
                around.join(", ")
            ),
        );
        r.emit(
            &format!("scale-change-restated:{scene}"),
            over_control.is_empty() && unsettled.is_empty(),
            &format!(
                "switched minus the constant-scale run at the same scale and frame: worst {:+.3} @k{} (bound +1.5; frames over: {over_control:?}); from 8 frames after each step: worst {:+.3} @k{} (bound +0.25; frames over: {unsettled:?})",
                worst_excess.0, worst_excess.1, worst_settled.0, worst_settled.1
            ),
        );
        Ok(())
    }
    .await;
    crate::hooks::set_scale(p.scale);
    if let Err(e) = res {
        r.emit(
            &format!("scale-change:{scene}"),
            false,
            &format!("error: {e:#}"),
        );
    }
}

pub async fn mip_bias_mode(p: &Params, r: &mut Report) {
    let scene = first_scene(p, "taa-thin");
    let biases: Vec<f32> = crate::url_value("bias")
        .map(|v| v.split(',').filter_map(|x| x.trim().parse().ok()).collect())
        .unwrap_or_else(|| vec![0.0, -0.25, -0.5, -1.0]);
    let scale = if (p.scale - 1.0).abs() < 1e-6 {
        0.67
    } else {
        p.scale
    };
    crate::hooks::set_scale(scale);
    for b in biases {
        let mut tune = p.tune.clone();
        tune.retain(|(k, _)| k != "mip_bias_extra");
        tune.push(("mip_bias_extra".into(), b));
        let name = format!("mip-bias:{scene}:{b}@{scale}");
        if let Err(e) = crate::hooks::set_tune(&tune) {
            r.emit(&name, false, &e);
            continue;
        }
        let res: Result<String> = async {
            // Converged: static, k = 40 (taa-thin's static phase).
            let k = 40;
            let mut t = Session::open(opts(p, &scene, Aa::Taa)).await?;
            let mut rs = Session::open(opts(p, &scene, Aa::None)).await?;
            t.play_to(k - 1).await?;
            rs.play_to(k - 1).await?;
            let (_, img) = t.render_next_captured().await?;
            rs.render_next()?;
            let reference = reference_cached(&mut rs, p).await?;
            let whole = abs_error(&img, &reference, None);
            // Panels: the tracked node's hull (the scenario's textured
            // spinner) grown, if any.
            let panel = crate::gates::tracked_coverage(&t).map(|m| m.dilate(40));
            let pstat = panel.as_ref().map(|m| abs_error(&img, &reference, Some(m)));
            // Flicker: 60 pairs, time frozen.
            let mut prev: Option<Image> = None;
            let mut sum = 0.0f64;
            let mut hist = crate::metrics::ErrorHistogram::default();
            for _ in 0..61 {
                let f = t.render_frozen_captured(None).await?;
                if let Some(pv) = &prev {
                    let h = crate::metrics::error_histogram(&f, pv, None);
                    sum += h.stats().mean / 60.0;
                    hist.merge(&h);
                }
                prev = Some(f);
            }
            crate::store::put(&p.store, &format!("phase4/mip-bias-{scene}-{b}-{scale}.png"), &img).await?;
            t.destroy();
            rs.destroy();
            let fl = hist.stats();
            Ok(format!(
                "converged k{k} vs full-res reference: whole {whole}; spinner region {}; static flicker mean {sum:.4} p99 {} max {} (05: < 0.5 / < 4); image stored phase4/mip-bias-{scene}-{b}-{scale}.png",
                pstat.map(|s| s.to_string()).unwrap_or_else(|| "n/a".into()),
                fl.p99,
                fl.max
            ))
        }
        .await;
        match res {
            Ok(d) => r.info(&name, &d),
            Err(e) => r.emit(&name, false, &format!("error: {e:#}")),
        }
    }
    let _ = crate::hooks::set_tune(&p.tune);
    crate::hooks::set_scale(p.scale);
}

pub async fn governor_hitch_mode(p: &Params, r: &mut Report) {
    use awsm_renderer::debug::TimingTier;
    use awsm_renderer::profiling;
    let scene = first_scene(p, "kitchen-sink");
    let res: Result<String> = async {
        crate::hooks::set_scale(0.67);
        let mut s = Session::open(opts(p, &scene, Aa::Taa)).await?;
        s.renderer.logging.gpu = TimingTier::Frame;
        for _ in 0..60 {
            s.render_next()?;
            gpu_idle(&s.renderer).await;
        }
        let mut gpu = Vec::new();
        let mut cpu = Vec::new();
        let mut steps = Vec::new();
        let mut last = profiling::gpu_frame_timing()
            .map(|t| (t.count, t.total_ms))
            .unwrap_or((0, 0.0));
        let mut scale = 0.67f32;
        for i in 0..480u32 {
            if i > 0 && i % 120 == 0 {
                scale = if scale > 0.6 { 0.5 } else { 0.67 };
                s.renderer
                    .set_taa_render_scale(scale)
                    .map_err(|e| anyhow!("set_taa_render_scale: {e}"))?;
                steps.push(i as usize);
            }
            let t0 = crate::now_ms();
            s.render_next()?;
            let t1 = crate::now_ms();
            gpu_idle(&s.renderer).await;
            // Let the timestamp readback fold this frame in.
            crate::session::sleep_ms(4).await;
            let now = profiling::gpu_frame_timing()
                .map(|t| (t.count, t.total_ms))
                .unwrap_or(last);
            let dc = now.0.saturating_sub(last.0);
            gpu.push(if dc > 0 { (now.1 - last.1) / dc as f64 } else { f64::NAN });
            last = now;
            cpu.push(t1 - t0);
            set_hud(&format!("taa-tests: governor-hitch {scene} {i}"));
        }
        s.renderer.logging.gpu = TimingTier::Off;
        s.destroy();
        let median = |v: &[f64]| {
            let mut x: Vec<f64> = v.iter().copied().filter(|f| f.is_finite()).collect();
            x.sort_by(|a, b| a.total_cmp(b));
            x.get(x.len() / 2).copied().unwrap_or(f64::NAN)
        };
        let (gm, cm) = (median(&gpu), median(&cpu));
        let mut rows = Vec::new();
        for st in &steps {
            let g: Vec<String> = (0..4).map(|d| format!("{:.2}", gpu.get(st + d).copied().unwrap_or(f64::NAN))).collect();
            let c: Vec<String> = (0..4).map(|d| format!("{:.2}", cpu.get(st + d).copied().unwrap_or(f64::NAN))).collect();
            rows.push(format!("step @frame {st}: GPU [{}] ms, CPU render() [{}] ms", g.join(", "), c.join(", ")));
        }
        Ok(format!(
            "scale 0.67 ↔ 0.5 every 120 frames at {}x{}; median GPU frame {gm:.2} ms, median CPU render() {cm:.2} ms (the GPU timing folds a frame or two late: the hitch can show on the step frame or the next); {}",
            p.size.0,
            p.size.1,
            rows.join("; ")
        ))
    }
    .await;
    crate::hooks::set_scale(p.scale);
    match res {
        Ok(d) => r.info(&format!("governor-hitch:{scene}"), &d),
        Err(e) => r.emit(
            &format!("governor-hitch:{scene}"),
            false,
            &format!("error: {e:#}"),
        ),
    }
}

/// `?mode=newobj-cost&scenes=a,b` (round 3, `reactive_new_objects`): the
/// worst case of the new-object flags, every mesh of the scene shown at
/// once. Every 60 frames all visible meshes are hidden for one frame and
/// then shown again (all of them new objects on the next frame, cleared on
/// the one after). CPU `render()` and GPU frame per frame; the report gives
/// the steady medians and, per show, the show frame and the three after.
/// Run with `tune=reactive_new_objects:0` and `:1` to price the flags;
/// `clones=N` adds N copies of the first visible mesh (many meshes at once).
pub async fn newobj_cost_mode(p: &Params, r: &mut Report) {
    use awsm_renderer::debug::TimingTier;
    use awsm_renderer::profiling;
    let scenes = if !p.scenes_given {
        vec!["kitchen-sink".to_string()]
    } else {
        p.scenes.clone()
    };
    for scene in scenes {
        let res: Result<String> = async {
            let mut s = Session::open(opts(p, &scene, Aa::Taa)).await?;
            s.renderer.logging.gpu = TimingTier::Frame;
            // `clones=N`: N extra copies of the first visible mesh (same
            // place), to make the worst case many meshes at once.
            let clones: usize = crate::url_value("clones")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            if clones > 0 {
                let src = s
                    .renderer
                    .meshes
                    .iter()
                    .find(|(_, m)| !m.hidden)
                    .map(|(k, _)| k)
                    .ok_or_else(|| anyhow!("no visible mesh to clone"))?;
                for _ in 0..clones {
                    s.renderer
                        .clone_mesh(src)
                        .map_err(|e| anyhow!("clone_mesh: {e}"))?;
                }
            }
            let shown: Vec<awsm_renderer::meshes::MeshKey> = s
                .renderer
                .meshes
                .iter()
                .filter(|(_, m)| !m.hidden)
                .map(|(k, _)| k)
                .collect();
            for _ in 0..60 {
                s.render_next()?;
                gpu_idle(&s.renderer).await;
            }
            let mut gpu = Vec::new();
            let mut cpu = Vec::new();
            let mut shows = Vec::new();
            let mut last = profiling::gpu_frame_timing()
                .map(|t| (t.count, t.total_ms))
                .unwrap_or((0, 0.0));
            for i in 0..300u32 {
                if i % 60 == 10 {
                    for k in &shown {
                        let _ = s.renderer.set_mesh_hidden(*k, true);
                    }
                }
                if i % 60 == 11 {
                    for k in &shown {
                        let _ = s.renderer.set_mesh_hidden(*k, false);
                    }
                    shows.push(i as usize);
                }
                let t0 = crate::now_ms();
                s.render_next()?;
                let t1 = crate::now_ms();
                gpu_idle(&s.renderer).await;
                crate::session::sleep_ms(4).await;
                let now = profiling::gpu_frame_timing()
                    .map(|t| (t.count, t.total_ms))
                    .unwrap_or(last);
                let dc = now.0.saturating_sub(last.0);
                gpu.push(if dc > 0 { (now.1 - last.1) / dc as f64 } else { f64::NAN });
                last = now;
                cpu.push(t1 - t0);
                set_hud(&format!("taa-tests: newobj-cost {scene} {i}"));
            }
            s.renderer.logging.gpu = TimingTier::Off;
            s.destroy();
            let median = |v: &[f64]| {
                let mut x: Vec<f64> = v.iter().copied().filter(|f| f.is_finite()).collect();
                x.sort_by(|a, b| a.total_cmp(b));
                x.get(x.len() / 2).copied().unwrap_or(f64::NAN)
            };
            let steady: Vec<usize> = (0..gpu.len()).filter(|i| i % 60 >= 20).collect();
            let gm = median(&steady.iter().map(|i| gpu[*i]).collect::<Vec<_>>());
            let cm = median(&steady.iter().map(|i| cpu[*i]).collect::<Vec<_>>());
            let mut rows = Vec::new();
            for st in &shows {
                let g: Vec<String> = (0..4).map(|d| format!("{:.2}", gpu.get(st + d).copied().unwrap_or(f64::NAN))).collect();
                let c: Vec<String> = (0..4).map(|d| format!("{:.2}", cpu.get(st + d).copied().unwrap_or(f64::NAN))).collect();
                rows.push(format!("show @{st}: GPU [{}] ms, CPU render() [{}] ms", g.join(", "), c.join(", ")));
            }
            Ok(format!(
                "{} meshes shown at once every 60 frames at {}x{}; steady median GPU frame {gm:.3} ms, CPU render() {cm:.3} ms; {}",
                shown.len(),
                p.size.0,
                p.size.1,
                rows.join("; ")
            ))
        }
        .await;
        match res {
            Ok(d) => r.info(&format!("newobj-cost:{scene}"), &d),
            Err(e) => r.emit(
                &format!("newobj-cost:{scene}"),
                false,
                &format!("error: {e:#}"),
            ),
        }
    }
}

/// `?mode=sharpen-sweep&values=0,0.15,0.25,0.35` (taa-thin, `?scale=`,
/// default 0.67): per RCAS strength, the converged error at k40 against the
/// full-resolution reference and the static flicker; images stored.
pub async fn sharpen_sweep_mode(p: &Params, r: &mut Report) {
    let scene = first_scene(p, "taa-thin");
    let values: Vec<f32> = crate::url_value("values")
        .map(|v| v.split(',').filter_map(|x| x.trim().parse().ok()).collect())
        .unwrap_or_else(|| vec![0.0, 0.15, 0.25, 0.35]);
    let scale = if (p.scale - 1.0).abs() < 1e-6 {
        0.67
    } else {
        p.scale
    };
    crate::hooks::set_scale(scale);
    for v in values {
        crate::hooks::set_sharpen(v);
        let name = format!("sharpen:{scene}:{v}@{scale}");
        let res: Result<String> = async {
            let k = 40;
            let mut t = Session::open(opts(p, &scene, Aa::Taa)).await?;
            let mut rs = Session::open(opts(p, &scene, Aa::None)).await?;
            t.play_to(k - 1).await?;
            rs.play_to(k - 1).await?;
            let (_, img) = t.render_next_captured().await?;
            rs.render_next()?;
            let reference = reference_cached(&mut rs, p).await?;
            let whole = abs_error(&img, &reference, None);
            let mut prev: Option<Image> = None;
            let mut sum = 0.0f64;
            let mut hist = crate::metrics::ErrorHistogram::default();
            for _ in 0..61 {
                let f = t.render_frozen_captured(None).await?;
                if let Some(pv) = &prev {
                    let h = crate::metrics::error_histogram(&f, pv, None);
                    sum += h.stats().mean / 60.0;
                    hist.merge(&h);
                }
                prev = Some(f);
            }
            crate::store::put(&p.store, &format!("phase4/sharpen-{scene}-{v}-{scale}.png"), &img).await?;
            t.destroy();
            rs.destroy();
            let fl = hist.stats();
            Ok(format!(
                "converged k{k} vs full-res reference: {whole}; static flicker mean {sum:.4} p99 {} max {}",
                fl.p99, fl.max
            ))
        }
        .await;
        match res {
            Ok(d) => r.info(&name, &d),
            Err(e) => r.emit(&name, false, &format!("error: {e:#}")),
        }
    }
    crate::hooks::set_sharpen(0.0);
    crate::hooks::set_scale(p.scale);
}

/// `?mode=upscale-sweep&scale=0.67&mhu=24,32,48`: `max_history_upscaling`
/// at an upscaling scale: taa-thin static flicker + converged (k32, full-res
/// reference), taa-orbit static flicker, pan (every strafe frame) and stop
/// (+8); plus the Confidence debug view statistics on taa-thin at k32
/// (display-encoded bytes: mean, p5; the view is w_best through the
/// display pass). Needs `?refcache=` holding the native refs.
#[cfg(feature = "taa-tuning-v2")]
pub async fn upscale_sweep_mode(p: &Params, r: &mut Report) {
    let values: Vec<f32> = crate::url_value("mhu")
        .map(|v| v.split(',').filter_map(|x| x.trim().parse().ok()).collect())
        .unwrap_or_else(|| vec![24.0, 32.0, 48.0]);
    let scale = if (p.scale - 1.0).abs() < 1e-6 {
        0.67
    } else {
        p.scale
    };
    crate::hooks::set_scale(scale);
    for v in values {
        let mut tune = p.tune.clone();
        tune.retain(|(k, _)| k != "max_history_upscaling");
        tune.push(("max_history_upscaling".into(), v));
        let name = format!("upscale-sweep:max_history_upscaling={v}@{scale}");
        if let Err(e) = crate::hooks::set_tune(&tune) {
            r.emit(&name, false, &e);
            continue;
        }
        let res: Result<String> = async {
            let (tf, tp) = crate::sweep2::static_flicker(p, "taa-thin").await?;
            let (of, op) = crate::sweep2::static_flicker(p, "taa-orbit").await?;
            // Converged taa-thin k32 + the Confidence view.
            let mut t = Session::open(opts(p, "taa-thin", Aa::Taa)).await?;
            let mut cf = Session::open(opts(p, "taa-thin", Aa::Taa)).await?;
            cf.renderer.set_taa_debug_view(awsm_renderer::taa::TaaDebugView::Confidence);
            crate::session::wait_taa_debug_variant(&cf.renderer).await;
            let mut rs = Session::open(opts(p, "taa-thin", Aa::None)).await?;
            t.play_to(31).await?;
            cf.play_to(31).await?;
            rs.play_to(31).await?;
            let (_, img) = t.render_next_captured().await?;
            let (_, conf) = cf.render_next_captured().await?;
            rs.render_next()?;
            let reference = reference_cached(&mut rs, p).await?;
            t.destroy();
            cf.destroy();
            rs.destroy();
            let conv = abs_error(&img, &reference, None);
            let mut vals: Vec<u8> = conf.rgba.chunks_exact(4).map(|c| c[0]).collect();
            vals.sort_unstable();
            let mean = vals.iter().map(|v| *v as f64).sum::<f64>() / vals.len().max(1) as f64;
            let p5 = vals[vals.len() / 20];
            let orbit = crate::sweep2::run_orbit(p).await?;
            Ok(format!(
                "taa-thin static flicker {tf:.4} p99 {tp}, converged k32 {:.3} p99 {}; taa-orbit static flicker {of:.4} p99 {op}; {orbit}; Confidence view (taa-thin k32, R byte after the display pass): mean {mean:.1}, p5 {p5}",
                conv.mean, conv.p99
            ))
        }
        .await;
        match res {
            Ok(d) => r.info(&name, &d),
            Err(e) => r.emit(&name, false, &format!("error: {e:#}")),
        }
    }
    let _ = crate::hooks::set_tune(&p.tune);
    crate::hooks::set_scale(p.scale);
}

/// "Thin detail": reference pixels whose 3×3 luma range (bytes) is ≥ 48 —
/// the high-contrast single-pixel features (wires, dots, text strokes).
fn thin_detail_mask(r: &Image) -> crate::metrics::Mask {
    let (w, h) = (r.width, r.height);
    let luma: Vec<f32> = r
        .rgba
        .chunks_exact(4)
        .map(|c| 0.299 * c[0] as f32 + 0.587 * c[1] as f32 + 0.114 * c[2] as f32)
        .collect();
    let mut m = crate::metrics::Mask::new(w, h, false);
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            let (mut lo, mut hi) = (f32::MAX, f32::MIN);
            for dy in 0..3 {
                for dx in 0..3 {
                    let v = luma[((y + dy - 1) * w + x + dx - 1) as usize];
                    lo = lo.min(v);
                    hi = hi.max(v);
                }
            }
            m.set(x, y, hi - lo >= 48.0);
        }
    }
    m
}

fn luma(r: &Image) -> Vec<f32> {
    r.rgba
        .chunks_exact(4)
        .map(|c| 0.299 * c[0] as f32 + 0.587 * c[1] as f32 + 0.114 * c[2] as f32)
        .collect()
}

/// Isolated detail: reference pixels whose luma differs from the median of
/// their 3×3 by ≥ 48 (single-pixel dots, 1-px strokes). Returns the mask and
/// the per-pixel median.
fn isolated_mask(r: &Image) -> (crate::metrics::Mask, Vec<f32>) {
    let (w, h) = (r.width, r.height);
    let l = luma(r);
    let mut med = l.clone();
    let mut m = crate::metrics::Mask::new(w, h, false);
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            let mut v = [0f32; 9];
            for dy in 0..3 {
                for dx in 0..3 {
                    v[(dy * 3 + dx) as usize] = l[((y + dy - 1) * w + x + dx - 1) as usize];
                }
            }
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let i = (y * w + x) as usize;
            med[i] = v[4];
            m.set(x, y, (l[i] - v[4]).abs() >= 48.0);
        }
    }
    (m, med)
}

/// Over the isolated mask: the error, and the fraction of those pixels where
/// the image's luma is closer to the 3×3 median than to the reference (the
/// feature dropped out).
fn isolated_stats(img: &Image, reference: &Image, m: &crate::metrics::Mask, med: &[f32]) -> String {
    let (li, lr) = (luma(img), luma(reference));
    let mut n = 0usize;
    let mut drop = 0usize;
    for i in 0..li.len() {
        if m.bits[i] {
            n += 1;
            if (li[i] - med[i]).abs() < (li[i] - lr[i]).abs() {
                drop += 1;
            }
        }
    }
    let e = abs_error(img, reference, Some(m));
    format!(
        "isolated {:.3} p99 {} dropped {:.1}%",
        e.mean,
        e.p99,
        100.0 * drop as f64 / n.max(1) as f64
    )
}

/// `?mode=gamma-sweep&values=1.25,1.75,2.5,4&scale=0.67[&motion]`:
/// `gamma_still` at an upscaling scale. Static taa-thin converged (k40)
/// against the full-res reference over the whole image and over the
/// thin-detail mask (reference 3×3 luma range ≥ 48), static flicker
/// (taa-thin, taa-orbit); baselines at the same k: native TAA (scale 1,
/// default tuning) and MSAA on the same mask. With `&motion`: the ghosting
/// side at the same scale — trail on taa-motion (bands planned at native
/// size), taa-motion stop (+8), taa-orbit pan + stop.
#[cfg(feature = "taa-tuning-v2")]
pub async fn gamma_sweep_mode(p: &Params, r: &mut Report) {
    let values: Vec<f32> = crate::url_value("values")
        .map(|v| v.split(',').filter_map(|x| x.trim().parse().ok()).collect())
        .unwrap_or_else(|| vec![1.25, 1.75, 2.5, 4.0]);
    let scale = if (p.scale - 1.0).abs() < 1e-6 {
        0.67
    } else {
        p.scale
    };
    let motion = crate::url_value("motion").is_some();
    let k = 40;
    // Baselines (native TAA at default tuning, MSAA) and the mask.
    let base: Result<(crate::metrics::Mask, crate::metrics::Mask, Vec<f32>, String)> = async {
        crate::hooks::set_scale(1.0);
        let _ = crate::hooks::set_tune(&[]);
        let mut t = Session::open(opts(p, "taa-thin", Aa::Taa)).await?;
        let mut ms = Session::open(opts(p, "taa-thin", Aa::Msaa)).await?;
        let mut rs = Session::open(opts(p, "taa-thin", Aa::None)).await?;
        t.play_to(k - 1).await?;
        ms.play_to(k - 1).await?;
        rs.play_to(k - 1).await?;
        let (_, ti) = t.render_next_captured().await?;
        let (_, mi) = ms.render_next_captured().await?;
        rs.render_next()?;
        let reference = reference_cached(&mut rs, p).await?;
        t.destroy();
        ms.destroy();
        rs.destroy();
        let m = thin_detail_mask(&reference);
        let (iso, med) = isolated_mask(&reference);
        let d_iso = format!(
            "isolated mask {} px: native TAA {}; MSAA {}",
            iso.count(),
            isolated_stats(&ti, &reference, &iso, &med),
            isolated_stats(&mi, &reference, &iso, &med)
        );
        Ok((
            m.clone(),
            iso,
            med,
            format!(
                "thin-detail mask {} px; native TAA whole {:.3} thin {:.3}; MSAA whole {:.3} thin {:.3}; {d_iso}",
                m.count(),
                abs_error(&ti, &reference, None).mean,
                abs_error(&ti, &reference, Some(&m)).mean,
                abs_error(&mi, &reference, None).mean,
                abs_error(&mi, &reference, Some(&m)).mean
            ),
        ))
    }
    .await;
    let (mask, iso, med) = match base {
        Ok((m, iso, med, d)) => {
            r.info(&format!("gamma-sweep:baseline:taa-thin:k{k}"), &d);
            (m, iso, med)
        }
        Err(e) => {
            r.emit("gamma-sweep:baseline", false, &format!("error: {e:#}"));
            return;
        }
    };
    // Trail bands are geometry: plan once at native size.
    let plan = if motion {
        crate::hooks::set_scale(1.0);
        match crate::sweep2::plan_trail(p, "taa-motion").await {
            Ok(pl) => Some(pl),
            Err(e) => {
                r.emit("gamma-sweep:plan", false, &format!("error: {e:#}"));
                None
            }
        }
    } else {
        None
    };
    crate::hooks::set_scale(scale);
    for v in values {
        let mut tune = p.tune.clone();
        tune.retain(|(n, _)| n != "gamma_still");
        tune.push(("gamma_still".into(), v));
        let name = format!("gamma-sweep:gamma_still={v}@{scale}");
        if let Err(e) = crate::hooks::set_tune(&tune) {
            r.emit(&name, false, &e);
            continue;
        }
        let res: Result<String> = async {
            let mut t = Session::open(opts(p, "taa-thin", Aa::Taa)).await?;
            let mut rs = Session::open(opts(p, "taa-thin", Aa::None)).await?;
            t.play_to(k - 1).await?;
            rs.play_to(k - 1).await?;
            let (_, img) = t.render_next_captured().await?;
            rs.render_next()?;
            let reference = reference_cached(&mut rs, p).await?;
            t.destroy();
            rs.destroy();
            crate::store::put(&p.store, &format!("phase4/gamma-still-taa-thin-{v}-{scale}.png"), &img).await?;
            let whole = abs_error(&img, &reference, None);
            let thin = abs_error(&img, &reference, Some(&mask));
            let (tf, tp) = crate::sweep2::static_flicker(p, "taa-thin").await?;
            let (of, op) = crate::sweep2::static_flicker(p, "taa-orbit").await?;
            let mut out = format!(
                "taa-thin k{k} whole {:.3} p99 {}, thin {:.3} p99 {}, {}; static flicker taa-thin {tf:.4} p99 {tp}, taa-orbit {of:.4} p99 {op}",
                whole.mean,
                whole.p99,
                thin.mean,
                thin.p99,
                isolated_stats(&img, &reference, &iso, &med)
            );
            if let Some(pl) = &plan {
                let trail = crate::sweep2::run_trail(p, pl).await?;
                // taa-motion stop: scripted stop + 8.
                let mut t = Session::open(opts(p, "taa-motion", Aa::Taa)).await?;
                let mut rs = Session::open(opts(p, "taa-motion", Aa::None)).await?;
                let sk = t
                    .scenario
                    .phase_range("stopped")
                    .map(|x| x.0)
                    .ok_or_else(|| anyhow!("taa-motion has no `stopped` phase"))?
                    + 8;
                t.play_to(sk - 1).await?;
                rs.play_to(sk - 1).await?;
                let (_, img) = t.render_next_captured().await?;
                rs.render_next()?;
                let reference = reference_cached(&mut rs, p).await?;
                t.destroy();
                rs.destroy();
                let stop = abs_error(&img, &reference, None);
                let orbit = crate::sweep2::run_orbit(p).await?;
                out.push_str(&format!(
                    "; {trail}; taa-motion stop k{sk} {:.3}; {orbit}",
                    stop.mean
                ));
            }
            Ok(out)
        }
        .await;
        set_hud(&format!("taa-tests: gamma-sweep {v}@{scale}"));
        match res {
            Ok(d) => r.info(&name, &d),
            Err(e) => r.emit(&name, false, &format!("error: {e:#}")),
        }
    }
    let _ = crate::hooks::set_tune(&p.tune);
    crate::hooks::set_scale(p.scale);
}
