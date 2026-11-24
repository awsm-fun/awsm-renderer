//! `?mode=phase3` — the 05 §4.4 gate (reactive pixels and the HUD).
//!
//! - `taa-transparent`: the trail test over particle, glass and UV-flow
//!   regions, same thresholds (mean < 2, max < 16, every frame). Those
//!   regions are measured, not modelled: a lockstep TAA-off session with the
//!   no-vector content hidden (emitters, lines, glass, water); pixels that
//!   differ from the plain TAA-off frame (dilated 1 px) are the content.
//!   Two bands are gated: (1) the moving backdrop's trail band ∩ the content
//!   (what the 05 line literally names), (2) the content's OWN trail: pixels
//!   the content covered within the last 30 frames but not now (particles and
//!   UV flow move without vectors). The whole content region now is reported
//!   too. TAA, MSAA 4× and no-AA against the same 64-phase reference; no
//!   oracle (it is not defined for content without vectors).
//! - `taa-hud`: HUD pixels (the scenario's HUD nodes, measured the same way:
//!   plain vs HUD-hidden TAA-off) match the TAA-off render of the same frame,
//!   mean < 1, at every frame.

use anyhow::{anyhow, Result};

use crate::gates::tracked_coverage;
use crate::metrics::{abs_error, trail_band, ErrorStats, Image, Mask};
use crate::modes::reference_cached;
use crate::report::{set_hud, Report};
use crate::session::{gpu_idle, Aa, Session, SessionOpts};
use crate::velocity::{content_mask, no_vector_names};
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

#[derive(Default)]
struct Track {
    frames: u32,
    failing: Vec<u32>,
    worst_mean: (f64, u32),
    worst_max: (u8, u32),
    sum: f64,
}

impl Track {
    fn add(&mut self, k: u32, s: &ErrorStats, ok: bool) {
        if s.count == 0 {
            return;
        }
        self.frames += 1;
        self.sum += s.mean;
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

    fn show(&self) -> String {
        let f: Vec<String> = self
            .failing
            .iter()
            .take(12)
            .map(|k| k.to_string())
            .collect();
        format!(
            "{} frames; worst mean {:.3} @k{}, worst max {} @k{}, avg mean {:.3}; failing {}{}",
            self.frames,
            self.worst_mean.0,
            self.worst_mean.1,
            self.worst_max.0,
            self.worst_max.1,
            self.sum / self.frames.max(1) as f64,
            self.failing.len(),
            if f.is_empty() {
                String::new()
            } else {
                format!(
                    " ({}{})",
                    f.join(","),
                    if self.failing.len() > 12 { ",…" } else { "" }
                )
            }
        )
    }
}

fn trail_ok(s: &ErrorStats) -> bool {
    s.mean < 2.0 && s.max < 16
}

pub async fn phase3_mode(p: &Params, r: &mut Report) {
    let scenes: Vec<String> = if p.scenes_given {
        p.scenes.clone()
    } else {
        vec!["taa-transparent".into(), "taa-hud".into()]
    };
    for scene in &scenes {
        let res = if scene == "taa-hud" {
            hud(p, scene, r).await
        } else {
            reactive_trail(p, scene, r).await
        };
        if let Err(e) = res {
            r.emit(&format!("phase3:{scene}"), false, &format!("error: {e:#}"));
        }
    }
}

async fn reactive_trail(p: &Params, scene: &str, r: &mut Report) -> Result<()> {
    let mut t = Session::open(opts(p, scene, Aa::Taa)).await?;
    let mut rs = Session::open(opts(p, scene, Aa::None)).await?;
    let mut ms = Session::open(opts(p, scene, Aa::Msaa)).await?;
    let mut hs = Session::open(opts(p, scene, Aa::None)).await?;
    for s in [&t, &rs, &ms, &hs] {
        s.assert_lockstep_safe()?;
    }
    let hidden = hs.hide_no_vector_content(no_vector_names(scene));
    if hidden.is_empty() {
        return Err(anyhow!("{scene}: no no-vector content to measure"));
    }
    let frames = t.scenario.frames;
    let (w, h) = p.size;
    let mut hull_hist: Vec<Mask> = Vec::new();
    let mut content_hist: Vec<Mask> = Vec::new();
    // [band∩content, content trail, content now, whole backdrop band] × [TAA, MSAA, no-AA]
    let mut tr: Vec<[Track; 3]> = (0..4).map(|_| Default::default()).collect();
    while t.next_k < frames {
        let k = t.next_k;
        let (_, test) = t.render_next_captured().await?;
        let (_, plain) = rs.render_next_captured().await?;
        let (_, hid) = hs.render_next_captured().await?;
        let (_, msaa) = ms.render_next_captured().await?;
        let content = content_mask(&plain, &hid);
        let hull = tracked_coverage(&t).unwrap_or_else(|| Mask::new(w, h, false));
        let recent_hull: Vec<&Mask> = hull_hist.iter().rev().take(30).collect();
        let recent_content: Vec<&Mask> = content_hist.iter().rev().take(30).collect();
        let band = trail_band(&recent_hull, &hull);
        let ctrail = trail_band(&recent_content, &content);
        let masks = [band.and(&content), ctrail, content.clone(), band];
        if masks.iter().any(|m| m.count() > 0) {
            let reference = reference_cached(&mut rs, p).await?;
            for (mi, m) in masks.iter().enumerate() {
                if m.count() == 0 {
                    continue;
                }
                for (ii, img) in [&test, &msaa, &plain].into_iter().enumerate() {
                    let s = abs_error(img, &reference, Some(m));
                    tr[mi][ii].add(k, &s, trail_ok(&s));
                }
            }
        }
        hull_hist.push(hull);
        content_hist.push(content);
        if hull_hist.len() > 31 {
            hull_hist.remove(0);
            content_hist.remove(0);
        }
        set_hud(&format!("taa-tests: phase3 {scene} k{k}/{frames}"));
    }
    t.destroy();
    rs.destroy();
    ms.destroy();
    hs.destroy();
    let labels = [
        "backdrop trail band ∩ particle/glass/UV-flow content",
        "the content's own trail (covered by content within 30 frames, not now)",
        "the content region now",
        "the whole backdrop trail band",
    ];
    for (mi, label) in labels.iter().enumerate() {
        let detail = format!(
            "{label} (hidden for the mask: {hidden:?}); TAA: {} | MSAA: {} | no-AA: {} (thresholds mean < 2, max < 16, every frame)",
            tr[mi][0].show(),
            tr[mi][1].show(),
            tr[mi][2].show()
        );
        let name = format!(
            "phase3-trail:{scene}:{}",
            ["band-content", "content-trail", "content-now", "band"][mi]
        );
        if mi < 2 {
            let ok = tr[mi][0].frames > 0 && tr[mi][0].failing.is_empty();
            r.emit(&name, ok, &detail);
        } else {
            r.info(&name, &detail);
        }
    }
    Ok(())
}

async fn hud(p: &Params, scene: &str, r: &mut Report) -> Result<()> {
    let mut t = Session::open(opts(p, scene, Aa::Taa)).await?;
    let mut rs = Session::open(opts(p, scene, Aa::None)).await?;
    let mut hs = Session::open(opts(p, scene, Aa::None)).await?;
    let names: Vec<&str> = t.scenario.hud_nodes.clone();
    if names.is_empty() {
        return Err(anyhow!("{scene} has no HUD nodes in its scenario"));
    }
    let hidden = hs.hide_named(&names);
    if hidden.len() != names.len() {
        return Err(anyhow!("HUD nodes {names:?}: only {hidden:?} found"));
    }
    let frames = t.scenario.frames;
    let mut tr = Track::default();
    let mut px = 0usize;
    while t.next_k < frames {
        let k = t.next_k;
        let (_, test) = t.render_next_captured().await?;
        let (_, plain) = rs.render_next_captured().await?;
        let (_, hid) = hs.render_next_captured().await?;
        // HUD pixels: where hiding the HUD changes the TAA-off frame (no
        // dilation: the HUD's own edge pixels are HUD pixels).
        let mut m = Mask::new(plain.width, plain.height, false);
        for (i, (a, b)) in plain
            .rgba
            .chunks_exact(4)
            .zip(hid.rgba.chunks_exact(4))
            .enumerate()
        {
            m.bits[i] = a[..3] != b[..3];
        }
        px = px.max(m.count());
        let s = abs_error(&test, &plain, Some(&m));
        tr.add(k, &s, s.mean < 1.0);
        if k == frames / 2 && p.keep {
            store_pair(p, scene, k, &test, &plain).await?;
        }
        set_hud(&format!("taa-tests: phase3 {scene} k{k}/{frames}"));
        gpu_idle(&t.renderer).await;
    }
    t.destroy();
    rs.destroy();
    hs.destroy();
    r.emit(
        &format!("phase3-hud:{scene}"),
        tr.frames > 0 && tr.failing.is_empty(),
        &format!(
            "HUD pixels ({hidden:?}, up to {px} px) TAA vs the TAA-off render of the same frame: {} (threshold mean < 1, every frame)",
            tr.show()
        ),
    );
    Ok(())
}

async fn store_pair(p: &Params, scene: &str, k: u32, a: &Image, b: &Image) -> Result<()> {
    crate::store::put(&p.store, &format!("phase3/{scene}-k{k:03}-taa.png"), a).await?;
    crate::store::put(&p.store, &format!("phase3/{scene}-k{k:03}-noaa.png"), b).await
}
