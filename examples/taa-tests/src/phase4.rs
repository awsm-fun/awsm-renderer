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
        crate::hooks::set_scale(0.67);
        let mut t = Session::open(opts(p, &scene, Aa::Taa)).await?;
        let mut rs = Session::open(opts(p, &scene, Aa::None)).await?;
        t.assert_lockstep_safe()?;
        rs.assert_lockstep_safe()?;
        let mut worst = (0.0f64, 0u32);
        let mut over = Vec::new();
        let mut around = Vec::new();
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
                rs.render_next()?;
                let reference = reference_cached(&mut rs, p).await?;
                let s = abs_error(&img, &reference, None);
                if s.mean > worst.0 {
                    worst = (s.mean, k);
                }
                if s.mean > 3.0 {
                    over.push(k);
                }
                if (58..=66).contains(&k) || (118..=126).contains(&k) {
                    around.push(format!("k{k} {:.3}", s.mean));
                }
            } else {
                t.render_next()?;
                rs.render_next()?;
                gpu_idle(&t.renderer).await;
                gpu_idle(&rs.renderer).await;
            }
            set_hud(&format!("taa-tests: scale-change {scene} k{k}"));
        }
        t.destroy();
        rs.destroy();
        r.emit(
            &format!("scale-change:{scene}"),
            over.is_empty(),
            &format!(
                "0.67 → 0.5 at k60 → 0.67 at k120, every frame k40..160 vs the full-resolution reference: worst mean {:.3} @k{}; frames with mean > 3: {over:?}; around the steps: {}",
                worst.0,
                worst.1,
                around.join(", ")
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
