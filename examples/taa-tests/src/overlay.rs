//! The 32 B/sample fallback's deforming-mesh velocity overlay (round 2,
//! docs/TAA.md §3.10), on the forced fallback:
//!
//! - `?mode=overlay-check`: the overlay's debug check every frame. For each
//!   mesh the overlay drew: the pixels it wrote (FAIL when a drawn mesh wrote
//!   none: the silent failure mode of an identity test), and of those, how
//!   many a depth-EQUAL test would have lost and how many the production
//!   depth compare loses (must be 0). Per scene also the frames with and
//!   without overlay draws (a `@cadence` scene draws only on stepping
//!   frames). `&fallback-storage=N` exercises the limit tiers.
//! - `?mode=overlay-cost&size=1920x1435`: the overlay pass by repetition,
//!   `(T(N) − T(1)) / (N − 1)` over a busy window (`repeat=1,8`).
//! - `?mode=level-switch` (round 3, `taa-disocclusion`): live changes of the
//!   disocclusion level on one renderer (with `&fallback`, on the forced
//!   fallback, where only the id level copies the previous visibility
//!   target). Each switch must set and commit, keep the velocity path, and
//!   converge (time frozen) to the same image as a fresh session set to that
//!   level once.

use anyhow::{anyhow, Result};

use crate::report::{set_hud, Report};
use crate::session::{gpu_idle, Aa, Session, SessionOpts};
use crate::Params;

thread_local! {
    static DEBUG_CHECK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Sessions opened while set build the overlay's debug check variant.
pub fn debug_check() -> bool {
    DEBUG_CHECK.with(|c| c.get())
}

const DEFAULT_SCENES: [&str; 6] = [
    "taa-skin",
    "taa-skin@cadence",
    "taa-vertex-hook",
    "prefab-skinned-morph",
    "instancing-stress",
    "kitchen-sink",
];

fn opts(p: &Params, scene: &str) -> SessionOpts {
    SessionOpts {
        scene: scene.to_string(),
        aa: Aa::Taa,
        size: p.size,
        origin: p.bundles.clone(),
        warmup: p.warmup,
    }
}

fn scenes(p: &Params) -> Vec<String> {
    if p.scenes_given {
        p.scenes.clone()
    } else {
        DEFAULT_SCENES.iter().map(|s| s.to_string()).collect()
    }
}

pub async fn overlay_check_mode(p: &Params, r: &mut Report) {
    let frames = p
        .frames
        .iter()
        .copied()
        .max()
        .filter(|_| p.frames_given)
        .unwrap_or(90);
    crate::hooks::set_fallback(true);
    DEBUG_CHECK.with(|c| c.set(true));
    for scene in scenes(p) {
        set_hud(&format!("taa-tests: overlay-check {scene}…"));
        match overlay_check_scene(p, &scene, frames).await {
            Ok((ok, detail)) => r.emit(&format!("overlay-check:{scene}"), ok, &detail),
            Err(e) => r.emit(
                &format!("overlay-check:{scene}"),
                false,
                &format!("error: {e:#}"),
            ),
        }
    }
    DEBUG_CHECK.with(|c| c.set(false));
    crate::hooks::set_fallback(false);
}

async fn overlay_check_scene(p: &Params, scene: &str, frames: u32) -> Result<(bool, String)> {
    let mut s = Session::open(opts(p, scene)).await?;
    let tier = s.renderer.taa_fallback_overlay();
    // Pixels the reactive seed marks non-rigid (0.5) per frame: the meshes
    // the overlay does not cover, while they deform.
    let mut reactive_half = 0u64;
    if !tier.enabled() {
        // No overlay pass at all: the check has nothing to read; only the
        // draw set must stay empty and the seed must mark the deforming
        // meshes (as before the overlay).
        let mut draws = 0usize;
        for _ in 0..frames {
            s.render_next()?;
            draws += s.renderer.taa_overlay_draw_count();
            reactive_half += count_half(&mut s).await?;
        }
        let built = s.renderer.debug_read_taa_overlay_check().await.is_ok();
        s.destroy();
        return Ok((
            !built && draws == 0,
            format!(
                "tier {tier:?}: overlay pass built {built}, draw-set entries {draws}; \
                 reactive 0.5 px {:.0}/frame",
                reactive_half as f64 / frames as f64
            ),
        ));
    }
    let mut frames_drawn = 0u32;
    let mut draws = 0usize;
    let mut written = 0u64;
    let mut not_equal = 0u64;
    let mut compare_fails = 0u64;
    let mut zero_writes: Vec<String> = Vec::new();
    let mut drawn_pattern = String::new();
    for _ in 0..frames {
        let k = s.render_next()?;
        let fut = s.renderer.debug_read_taa_overlay_check();
        let check = fut
            .await
            .map_err(|e| anyhow!("debug_read_taa_overlay_check k{k}: {e}"))?;
        if !check.is_empty() {
            frames_drawn += 1;
        }
        if drawn_pattern.len() < 24 {
            drawn_pattern.push(if check.is_empty() { '.' } else { '#' });
        }
        draws += check.len();
        reactive_half += count_half(&mut s).await?;
        for m in &check {
            written += m.written as u64;
            not_equal += m.depth_not_equal as u64;
            compare_fails += m.depth_compare_fails as u64;
            if m.written == 0 && zero_writes.len() < 8 {
                zero_writes.push(format!("k{k} {:?}", m.mesh));
            }
        }
    }
    s.destroy();
    let ok = zero_writes.is_empty() && compare_fails == 0;
    Ok((
        ok,
        format!(
            "tier {tier:?}; {frames_drawn}/{frames} frames with overlay draws (first 24: {drawn_pattern}); \
             {draws} mesh draws, {written} px written, depth != stored {not_equal} px ({:.4}%), \
             production compare would lose {compare_fails} px; drawn meshes writing 0 px: {}; \
             reactive 0.5 px {:.0}/frame",
            if written > 0 {
                100.0 * not_equal as f64 / written as f64
            } else {
                0.0
            },
            if zero_writes.is_empty() {
                "none".to_string()
            } else {
                zero_writes.join(", ")
            },
            reactive_half as f64 / frames as f64
        ),
    ))
}

/// Reactive-mask pixels at the seed's non-rigid value (0.5 ± one r8 step).
async fn count_half(s: &mut Session) -> Result<u64> {
    let (_, _, data) = s
        .renderer
        .debug_read_reactive()
        .await
        .map_err(|e| anyhow!("debug_read_reactive: {e}"))?;
    Ok(data
        .iter()
        .filter(|&&v| (v - 0.5).abs() <= 1.0 / 255.0)
        .count() as u64)
}

/// `?mode=overlay-parity`: the fallback's vectors against the geometry
/// path's, pixel for pixel, in two lockstep TAA sessions (same jitter, so the
/// same rasterisation). `&move-instances` moves every instance of every
/// instanced mesh a little each frame in both sessions (no bundled scene
/// writes instance transforms). PASS when no pixel differs by more than
/// max(0.02 px, one rg16float ulp of the geometry path's vector).
pub async fn overlay_parity_mode(p: &Params, r: &mut Report) {
    let frames = p
        .frames
        .iter()
        .copied()
        .max()
        .filter(|_| p.frames_given)
        .unwrap_or(40);
    for scene in scenes(p) {
        set_hud(&format!("taa-tests: overlay-parity {scene}…"));
        match overlay_parity_scene(p, &scene, frames).await {
            Ok((ok, detail)) => r.emit(&format!("overlay-parity:{scene}"), ok, &detail),
            Err(e) => r.emit(
                &format!("overlay-parity:{scene}"),
                false,
                &format!("error: {e:#}"),
            ),
        }
    }
    crate::hooks::set_fallback(false);
}

fn move_instances(s: &mut Session, k: u32) {
    let keys: Vec<_> = s
        .renderer
        .meshes
        .iter()
        .filter(|(_, m)| m.instanced)
        .map(|(_, m)| m.transform_key)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    for key in keys {
        let Some(list) = s.renderer.instances.get_transforms(key) else {
            continue;
        };
        for (i, mut t) in list.into_iter().enumerate() {
            let phase = k as f32 * 0.15 + i as f32 * 0.7;
            t.translation += glam::Vec3::new(phase.sin(), phase.cos(), (phase * 0.5).sin()) * 0.02;
            t.rotation *= glam::Quat::from_rotation_y(0.03 * (i % 5) as f32);
            s.renderer.instances.transform_update(key, i, &t);
        }
    }
}

#[derive(Default)]
struct ParityStats {
    worst: f64,
    sum: f64,
    n: u64,
    over: u64,
    moving: u64,
}

/// Both sessions just rendered the same frame: accumulate the per-pixel
/// difference of their vectors (px).
async fn compare_velocity(
    g: &Session,
    f: &Session,
    (w, h): (u32, u32),
    st: &mut ParityStats,
) -> Result<()> {
    let (_, _, vg) = g
        .renderer
        .debug_read_velocity()
        .await
        .map_err(|e| anyhow!("geometry velocity: {e}"))?;
    let (_, _, vf) = f
        .renderer
        .debug_read_velocity()
        .await
        .map_err(|e| anyhow!("fallback velocity: {e}"))?;
    // One rg16float ulp of a component, in px.
    let ulp = |v: f32, size: u32| -> f64 {
        let e = (v.abs().max(6.1e-5) as f64).log2().floor();
        2f64.powf(e - 10.0) * size as f64
    };
    for (a, b) in vg.chunks_exact(2).zip(vf.chunks_exact(2)) {
        let dx = (a[0] - b[0]) as f64 * w as f64;
        let dy = (a[1] - b[1]) as f64 * h as f64;
        let d = (dx * dx + dy * dy).sqrt();
        let len_px = ((a[0] as f64 * w as f64).powi(2) + (a[1] as f64 * h as f64).powi(2)).sqrt();
        if len_px > 0.5 {
            st.moving += 1;
        }
        if d > 0.02f64.max(ulp(a[0], w).max(ulp(a[1], h))) {
            st.over += 1;
        }
        st.worst = st.worst.max(d);
        st.sum += d;
        st.n += 1;
    }
    Ok(())
}

async fn overlay_parity_scene(p: &Params, scene: &str, frames: u32) -> Result<(bool, String)> {
    // `&move-instances`: every frame; `&move-instances=2`: every 2nd frame
    // (the frames between are the first frames after the instances stop).
    let move_every: Option<u32> =
        crate::url_value("move-instances").map(|v| v.parse().unwrap_or(1).max(1));
    // `&hold`: after every frame, one more with time frozen in both sessions
    // (nothing deforms: the first frame after a deformation stops). The
    // vectors must still match; the overlay should draw only vertex hooks
    // there (reported).
    let hold = crate::url_value("hold").is_some();
    crate::hooks::set_fallback(false);
    let mut g = Session::open(opts(p, scene)).await?;
    crate::hooks::set_fallback(true);
    let mut f = Session::open(opts(p, scene)).await?;
    crate::hooks::set_fallback(false);
    let mut moving_frames = ParityStats::default();
    let mut held_frames = ParityStats::default();
    let mut draws = 0usize;
    let mut held_draws = 0usize;
    for _ in 0..frames {
        let k = g.next_k;
        if let Some(every) = move_every {
            if k > 0 && k % every == 0 {
                move_instances(&mut g, k);
                move_instances(&mut f, k);
            }
        }
        g.render_next()?;
        f.render_next()?;
        draws += f.renderer.taa_overlay_draw_count();
        compare_velocity(&g, &f, p.size, &mut moving_frames)
            .await
            .map_err(|e| anyhow!("k{k}: {e}"))?;
        if hold {
            g.render_frozen(None)?;
            f.render_frozen(None)?;
            held_draws += f.renderer.taa_overlay_draw_count();
            compare_velocity(&g, &f, p.size, &mut held_frames)
                .await
                .map_err(|e| anyhow!("k{k} held: {e}"))?;
        }
    }
    g.destroy();
    f.destroy();
    let line = |st: &ParityStats| {
        format!(
            "|v_fallback − v_geometry| mean {:.6} max {:.5} px over {} px ({} px with |v| > 0.5 px), {} px over max(0.02 px, 1 rg16float ulp)",
            st.sum / st.n.max(1) as f64,
            st.worst,
            st.n,
            st.moving,
            st.over
        )
    };
    let mut detail = format!(
        "{frames} frames{}: {}; overlay mesh draws {draws}",
        match move_every {
            Some(1) => " (instances moved every frame)".to_string(),
            Some(e) => format!(" (instances moved every {e}th frame)"),
            None => String::new(),
        },
        line(&moving_frames)
    );
    if hold {
        detail.push_str(&format!(
            " | held frames (time frozen after each): {}; overlay mesh draws {held_draws}",
            line(&held_frames)
        ));
    }
    Ok((moving_frames.over == 0 && held_frames.over == 0, detail))
}

pub async fn overlay_cost_mode(p: &Params, r: &mut Report) {
    use awsm_renderer::debug::TimingTier;
    use awsm_renderer::profiling;
    let repeats: Vec<u32> = crate::url_value("repeat")
        .map(|v| v.split(',').filter_map(|x| x.parse().ok()).collect())
        .filter(|v: &Vec<u32>| v.len() >= 2)
        .unwrap_or_else(|| vec![1, 32]);
    let n = p.cost_frames.max(60);
    crate::hooks::set_fallback(true);
    for scene in scenes(p) {
        set_hud(&format!("taa-tests: overlay-cost {scene}…"));
        let res: Result<String> =
            async {
                let mut s = Session::open(opts(p, &scene)).await?;
                s.renderer.logging.gpu = TimingTier::Frame;
                let mut per_repeat: Vec<Vec<(f64, f64)>> = vec![Vec::new(); repeats.len()];
                for run in 0..p.runs.max(1) {
                    // Alternate the order (ABBA) so a clock drift does not
                    // land on one repeat count.
                    let order: Vec<usize> = if run % 2 == 0 {
                        (0..repeats.len()).collect()
                    } else {
                        (0..repeats.len()).rev().collect()
                    };
                    for i in order {
                        let rep = repeats[i];
                        s.renderer.set_taa_debug_overlay_repeat(rep);
                        // Warm-up in the same busy pattern as the window
                        // (Apple GPUs clock down when drained every frame).
                        for f in 0..90 {
                            s.render_next()?;
                            if f % 4 == 3 {
                                gpu_idle(&s.renderer).await;
                            }
                        }
                        gpu_idle(&s.renderer).await;
                        for _ in 0..6 {
                            s.render_next()?;
                            gpu_idle(&s.renderer).await;
                            crate::session::sleep_ms(4).await;
                        }
                        let f0 = profiling::gpu_frame_timing()
                            .map(|t| (t.count, t.total_ms))
                            .unwrap_or((0, 0.0));
                        let pass0 = pass_stat(&profiling::gpu_timing_stats());
                        for f in 0..n {
                            s.render_next()?;
                            if f % 4 == 3 {
                                gpu_idle(&s.renderer).await;
                            }
                        }
                        gpu_idle(&s.renderer).await;
                        for _ in 0..6 {
                            s.render_next()?;
                            gpu_idle(&s.renderer).await;
                            crate::session::sleep_ms(4).await;
                        }
                        let f1 = profiling::gpu_frame_timing()
                            .ok_or_else(|| anyhow!("no GPU frame timing"))?;
                        let c = f1.count.saturating_sub(f0.0);
                        if c == 0 {
                            return Err(anyhow!("no samples"));
                        }
                        let frame = (f1.total_ms - f0.1) / c as f64;
                        let pass1 = pass_stat(&profiling::gpu_timing_stats());
                        let pass = if pass1.0 > pass0.0 {
                            (pass1.1 - pass0.1) / (pass1.0 - pass0.0) as f64
                        } else {
                            0.0
                        };
                        per_repeat[i].push((frame, pass));
                    }
                }
                s.renderer.set_taa_debug_overlay_repeat(1);
                s.renderer.logging.gpu = TimingTier::Off;
                s.destroy();
                let med = |v: &mut Vec<f64>| {
                    v.sort_by(f64::total_cmp);
                    v[v.len() / 2]
                };
                let mut lines = Vec::new();
                let mut frame_med = Vec::new();
                for (i, rep) in repeats.iter().enumerate() {
                    let mut frames: Vec<f64> = per_repeat[i].iter().map(|x| x.0).collect();
                    let mut passes: Vec<f64> = per_repeat[i].iter().map(|x| x.1).collect();
                    let fm = med(&mut frames);
                    frame_med.push(fm);
                    lines.push(format!(
                    "repeat {rep}: frame {fm:.3} ms (runs {}), 'TAA Overlay' timestamp {:.3} ms",
                    frames.iter().map(|x| format!("{x:.3}")).collect::<Vec<_>>().join("/"),
                    med(&mut passes)
                ));
                }
                let last = repeats.len() - 1;
                let slope = (frame_med[last] - frame_med[0]) / (repeats[last] - repeats[0]) as f64;
                Ok(format!(
                    "overlay pass by repetition {slope:.4} ms; {}",
                    lines.join("; ")
                ))
            }
            .await;
        match res {
            Ok(d) => r.emit(&format!("overlay-cost:{scene}"), true, &d),
            Err(e) => r.emit(
                &format!("overlay-cost:{scene}"),
                false,
                &format!("error: {e:#}"),
            ),
        }
    }
    crate::hooks::set_fallback(false);
}

/// (count, total ms) of the overlay pass's timestamp scope.
fn pass_stat(stats: &[(&'static str, awsm_renderer::profiling::TimingStat)]) -> (u64, f64) {
    stats
        .iter()
        .find(|(k, _)| *k == "TAA Overlay")
        .map(|(_, v)| (v.count, v.total_ms))
        .unwrap_or((0, 0.0))
}

/// The disocclusion levels a `level-switch` run steps through, starting from
/// the session's level (the default, `DepthMotionAndId`): every transition
/// between two levels, both ways.
#[cfg(feature = "taa-disocclusion")]
const LEVEL_SEQUENCE: [awsm_renderer::taa::TaaDisocclusion; 6] = {
    use awsm_renderer::taa::TaaDisocclusion::*;
    [
        Depth,
        DepthAndMotion,
        DepthMotionAndId,
        DepthAndMotion,
        Depth,
        DepthMotionAndId,
    ]
};

#[cfg(feature = "taa-disocclusion")]
pub async fn level_switch_mode(p: &Params, r: &mut Report) {
    let scenes = if p.scenes_given {
        p.scenes.clone()
    } else {
        vec!["taa-skin".to_string(), "kitchen-sink".to_string()]
    };
    for scene in scenes {
        set_hud(&format!("taa-tests: level-switch {scene}…"));
        match level_switch_scene(p, &scene).await {
            Ok(steps) => {
                for (name, ok, detail) in steps {
                    r.emit(&format!("level-switch:{scene}:{name}"), ok, &detail);
                }
            }
            Err(e) => r.emit(
                &format!("level-switch:{scene}"),
                false,
                &format!("error: {e:#}"),
            ),
        }
    }
}

/// Frames rendered moving after a switch, then frozen before the capture.
#[cfg(feature = "taa-disocclusion")]
const SWITCH_MOVING: u32 = 12;
#[cfg(feature = "taa-disocclusion")]
const SWITCH_FROZEN: u32 = 40;

#[cfg(feature = "taa-disocclusion")]
async fn set_level(s: &mut Session, level: awsm_renderer::taa::TaaDisocclusion) -> Result<()> {
    let mut aa = s.renderer.anti_aliasing.clone();
    aa.taa
        .as_mut()
        .ok_or_else(|| anyhow!("TAA is not on"))?
        .disocclusion = level;
    s.renderer
        .set_anti_aliasing(aa)
        .await
        .map_err(|e| anyhow!("set_anti_aliasing({level:?}): {e}"))?;
    s.renderer
        .commit_load(|_| {})
        .await
        .map_err(|e| anyhow!("commit_load({level:?}): {e}"))?;
    let now = s
        .renderer
        .anti_aliasing
        .taa
        .as_ref()
        .map(|t| t.disocclusion);
    if now != Some(level) {
        return Err(anyhow!(
            "level after the switch is {now:?}, asked {level:?}"
        ));
    }
    Ok(())
}

#[cfg(feature = "taa-disocclusion")]
async fn frozen_capture(s: &mut Session) -> Result<crate::metrics::Image> {
    for _ in 0..SWITCH_FROZEN {
        s.render_frozen(None)?;
        gpu_idle(&s.renderer).await;
    }
    s.render_frozen_captured(None).await
}

#[cfg(feature = "taa-disocclusion")]
async fn level_switch_scene(p: &Params, scene: &str) -> Result<Vec<(String, bool, String)>> {
    let mut steps = Vec::new();
    let mut s = Session::open(opts(p, scene)).await?;
    let path = s.renderer.taa_velocity_path();
    s.play_to(20).await?;
    for (i, &level) in LEVEL_SEQUENCE.iter().enumerate() {
        let name = format!("{i}-{level:?}");
        set_level(&mut s, level).await?;
        if s.renderer.taa_velocity_path() != path {
            return Err(anyhow!(
                "velocity path changed with the level: {path:?} -> {:?}",
                s.renderer.taa_velocity_path()
            ));
        }
        for _ in 0..SWITCH_MOVING {
            s.render_next()?;
            gpu_idle(&s.renderer).await;
        }
        let k = s.next_k.saturating_sub(1);
        let live = frozen_capture(&mut s).await?;
        // The reference: a fresh session set to this level once, at the
        // same scenario frame, converged the same way.
        let mut fresh = Session::open(opts(p, scene)).await?;
        set_level(&mut fresh, level).await?;
        fresh.play_to(k).await?;
        let reference = frozen_capture(&mut fresh).await?;
        fresh.destroy();
        let e = crate::metrics::abs_error(&live, &reference, None);
        // Same frozen frame, converged history: only the jitter phase at the
        // capture differs (the sessions rendered different frame counts).
        let ok = e.mean < 0.5 && e.p99 <= 4;
        steps.push((
            name,
            ok,
            format!(
                "{path:?}: set + commit ok, path kept; after {SWITCH_MOVING} moving + \
                 {SWITCH_FROZEN} frozen frames at k{k}, live vs fresh-at-this-level: {e} \
                 (bound mean < 0.5, p99 <= 4)"
            ),
        ));
    }
    s.destroy();
    Ok(steps)
}
