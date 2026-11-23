//! The harness modes (`?mode=`). Each prints `TAA-TEST` lines and ends with
//! `TAA-TESTS COMPLETE: <pass>/<total>`.

use anyhow::{anyhow, Result};

use crate::metrics::{self, abs_error, Image};
#[cfg(feature = "taa-hooks")]
use crate::reference::{self, AccumSpace, ReferenceAccumulator};
use crate::report::{set_hud, Report};
use crate::session::{gpu_idle, Aa, Session, SessionOpts};
use crate::store;
use crate::Params;
#[cfg(feature = "taa-hooks")]
use glam::Vec2;

/// The scenes the TAA-off baseline covers (bundled ones; `aa-edges` too).
pub const BASELINE_SCENES: &[&str] = &[
    "aa-edges",
    "kitchen-sink",
    "transparent",
    "anim-skinned",
    "ssr",
    "bloom-post",
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

/// Play a fresh session and capture the frames listed (ascending).
async fn capture_run(
    p: &Params,
    scene: &str,
    aa: Aa,
    frames: &[u32],
) -> Result<(Vec<(u32, Image)>, u64)> {
    let mut s = Session::open(opts(p, scene, aa)).await?;
    let mut out = Vec::new();
    let last = *frames.iter().max().unwrap_or(&0);
    while s.next_k <= last {
        if frames.contains(&s.next_k) {
            out.push(s.render_next_captured().await?);
        } else {
            s.render_next()?;
            gpu_idle(&s.renderer).await;
        }
    }
    let fi = s.frame_index_at_k0;
    let morphs = s.morph_weights();
    if !morphs.is_empty() {
        crate::report::log(&format!(
            "TAA-INFO morph-weights:{scene}@k{}: {morphs:?}",
            s.next_k - 1
        ));
    }
    s.destroy();
    Ok((out, fi))
}

fn baseline_name(scene: &str, aa: Aa, k: u32, last: u32) -> String {
    if k == last {
        format!("{scene}-{}.png", aa.name())
    } else {
        format!("{scene}-{}-k{k:03}.png", aa.name())
    }
}

/// `?mode=determinism`: render the same scenario `runs` times from fresh
/// renderers and require bit-identical captures.
pub async fn determinism(p: &Params, r: &mut Report) {
    for scene in &p.scenes {
        for aa in &p.aas {
            let name = format!("determinism:{scene}:{}", aa.name());
            set_hud(&format!("taa-tests: {name}…"));
            let res: Result<(bool, String)> = async {
                let (first, fi0) = capture_run(p, scene, *aa, &p.frames).await?;
                let mut worst = metrics::ErrorStats::default();
                let mut details = Vec::new();
                for run in 1..p.runs {
                    let (other, fi) = capture_run(p, scene, *aa, &p.frames).await?;
                    if fi != fi0 {
                        details.push(format!("run{run} frame_index@k0 {fi} != {fi0}"));
                    }
                    for ((k, a), (_, b)) in first.iter().zip(&other) {
                        let s = abs_error(a, b, None);
                        if s.max > 0 {
                            details.push(format!("run{run} k{k}: {s}"));
                        }
                        if s.max > worst.max || (s.max == worst.max && s.mean > worst.mean) {
                            worst = s;
                        }
                    }
                }
                let ok = details.is_empty();
                Ok((
                    ok,
                    format!(
                        "{} runs × frames {:?} @ {}x{}: worst max {} mean {:.4} nonzero {} (frame_index@k0 {fi0}){}",
                        p.runs,
                        p.frames,
                        p.size.0,
                        p.size.1,
                        worst.max,
                        worst.mean,
                        worst.nonzero,
                        if ok { String::new() } else { format!(" — {}", details.join("; ")) }
                    ),
                ))
            }
            .await;
            r.emit_result(&name, res);
        }
    }
}

/// `?mode=baseline-record`: capture the baseline set into the store.
pub async fn baseline_record(p: &Params, r: &mut Report) {
    let last = *p.frames.iter().max().unwrap_or(&0);
    for scene in &p.scenes {
        for aa in &p.aas {
            let name = format!("record:{scene}:{}", aa.name());
            set_hud(&format!("taa-tests: {name}…"));
            let res: Result<(bool, String)> = async {
                let (caps, fi) = capture_run(p, scene, *aa, &p.frames).await?;
                let mut files = Vec::new();
                for (k, img) in &caps {
                    let file = baseline_name(scene, *aa, *k, last);
                    store::put(&p.store, &format!("{}/{file}", p.set), img).await?;
                    files.push(format!("{file} (mean {:.3})", mean_value(img)));
                }
                Ok((true, format!("{} (frame_index@k0 {fi})", files.join(", "))))
            }
            .await;
            r.emit_result(&name, res);
        }
    }
}

/// `?mode=baseline-check`: capture again and require bit-identical pixels to
/// the recorded set (the "TAA off = unchanged" pixel proof).
pub async fn baseline_check(p: &Params, r: &mut Report) {
    let last = *p.frames.iter().max().unwrap_or(&0);
    for scene in &p.scenes {
        for aa in &p.aas {
            set_hud(&format!("taa-tests: baseline {scene} {}…", aa.name()));
            let caps = match capture_run(p, scene, *aa, &p.frames).await {
                Ok((c, _)) => c,
                Err(e) => {
                    r.emit(
                        &format!("baseline:{scene}:{}", aa.name()),
                        false,
                        &format!("error: {e:#}"),
                    );
                    continue;
                }
            };
            for (k, img) in &caps {
                let file = baseline_name(scene, *aa, *k, last);
                let name = format!("baseline:{}", file.trim_end_matches(".png"));
                let res: Result<(bool, String)> = async {
                    let base = store::get(&p.store, &format!("{}/{file}", p.set))
                        .await?
                        .ok_or_else(|| anyhow!("no baseline {}/{file} in the store", p.set))?;
                    if !base.same_size(img) {
                        return Ok((
                            false,
                            format!(
                                "size {}x{} vs baseline {}x{}",
                                img.width, img.height, base.width, base.height
                            ),
                        ));
                    }
                    let s = abs_error(img, &base, None);
                    let ok = s.max <= p.tolerance;
                    if !ok || p.keep {
                        let check_set = format!("{}-check", p.set);
                        store::put(&p.store, &format!("{check_set}/{file}"), img).await?;
                        let diff = metrics::error_image(img, &base, 16.0);
                        store::put(&p.store, &format!("{check_set}/diff-{file}"), &diff).await?;
                    }
                    Ok((ok, format!("{s} (tolerance max ≤ {})", p.tolerance)))
                }
                .await;
                r.emit_result(&name, res);
            }
        }
    }
}

/// `?mode=reference&scenes=<scene>&k=60,120`: ground-truth frames (05 §3).
/// Stores `ref/<scene>-k<k>-<space>.png` for both accumulation spaces and the
/// plain (unjittered, TAA-off) frame `ref/<scene>-k<k>-aliased.png`.
pub async fn reference_mode(p: &Params, r: &mut Report) {
    for scene in &p.scenes {
        let name = format!("reference:{scene}");
        set_hud(&format!("taa-tests: {name}…"));
        let res: Result<(bool, String)> = async {
            let mut s = Session::open(opts(p, scene, Aa::None)).await?;
            let mut details = Vec::new();
            for &k in &p.ks {
                if k + 1 < s.next_k {
                    return Err(anyhow!("k list must ascend"));
                }
                if k > 0 {
                    s.play_to(k - 1).await?;
                }
                let (_, aliased) = s.render_next_captured().await?;
                let t0 = crate::now_ms();
                let (lin, kar) = build_reference(&mut s, p.phases, true).await?;
                let ms = crate::now_ms() - t0;
                let (lin, kar) = (&lin, kar.as_ref().expect("karis requested"));
                store::put(&p.store, &format!("ref/{scene}-k{k:03}-aliased.png"), &aliased).await?;
                store::put(&p.store, &format!("ref/{scene}-k{k:03}-display-linear.png"), lin).await?;
                store::put(&p.store, &format!("ref/{scene}-k{k:03}-karis.png"), kar).await?;
                let space_bias = abs_error(lin, kar, None);
                let alias_err = abs_error(&aliased, lin, None);
                details.push(format!(
                    "k{k}: {} phases in {:.0} ms; aliased-vs-ref {alias_err}; display-linear-vs-karis {space_bias}",
                    p.phases, ms
                ));
            }
            s.destroy();
            Ok((true, details.join(" | ")))
        }
        .await;
        r.emit_result(&name, res);
    }
}

/// Freeze the session at its last frame and accumulate `phases` jittered
/// renders: display-linear, plus the Karis-weighted one when `karis`.
/// The display-linear 64-phase reference of the session's current frame
/// (`next_k − 1`), from the store's `refcache/<name>/` when `?refcache=` is
/// set (built and stored on a miss). The session must be exactly at that
/// frame (a lockstep TAA-off session); on a hit it renders nothing.
pub async fn reference_cached(s: &mut Session, p: &Params) -> Result<Image> {
    let k = s
        .next_k
        .checked_sub(1)
        .ok_or_else(|| anyhow!("reference before any frame"))?;
    let path = p.refcache.as_ref().map(|name| {
        format!(
            "refcache/{name}/{}-{}x{}-k{k:03}-p{}.png",
            s.opts.scene.replace('@', "_"),
            s.opts.size.0,
            s.opts.size.1,
            p.phases
        )
    });
    if let Some(path) = &path {
        if let Some(img) = store::get(&p.store, path).await? {
            return Ok(img);
        }
    }
    let (img, _) = build_reference(s, p.phases, false).await?;
    if let Some(path) = &path {
        store::put(&p.store, path, &img).await?;
    }
    Ok(img)
}

pub async fn build_reference(
    s: &mut Session,
    phases: u32,
    karis: bool,
) -> Result<(Image, Option<Image>)> {
    #[cfg(not(feature = "taa-hooks"))]
    {
        let _ = (s, phases, karis);
        Err(anyhow!(
            "reference capture needs the taa-hooks build (jitter override)"
        ))
    }
    #[cfg(feature = "taa-hooks")]
    {
        let size = s.opts.size;
        let mut lin = ReferenceAccumulator::new(size, size, AccumSpace::DisplayLinear);
        let mut kar = karis.then(|| ReferenceAccumulator::new(size, size, AccumSpace::Karis));
        for i in 0..phases {
            let j = reference::jitter_offset(i, phases);
            let img = s.render_frozen_captured(Some(j)).await?;
            lin.add_frame(&img, j);
            if let Some(k) = kar.as_mut() {
                k.add_frame(&img, j);
            }
        }
        Ok((lin.finish(), kar.map(|k| k.finish())))
    }
}

/// `?mode=hooks`: the renderer hooks the reference relies on behave as the
/// harness assumes (TAA off).
pub async fn hooks_mode(p: &Params, r: &mut Report) {
    #[cfg(not(feature = "taa-hooks"))]
    {
        let _ = p;
        r.emit("hooks", false, "built without taa-hooks");
    }
    #[cfg(feature = "taa-hooks")]
    {
        match crate::hooks::mirrors_agree() {
            Ok(d) => r.emit("hooks-agree", true, &d),
            Err(e) => r.emit("hooks-agree", false, &e),
        }
        for scene in &p.scenes {
            let res = jitter_checks(p, scene, r).await;
            if let Err(e) = res {
                r.emit(&format!("jitter:{scene}"), false, &format!("error: {e:#}"));
            }
        }
    }
}

#[cfg(feature = "taa-hooks")]
async fn jitter_checks(p: &Params, scene: &str, r: &mut Report) -> Result<()> {
    // MSAA: anti-aliased edges move continuously with a sub-pixel jitter, so
    // the shift estimate is unbiased. On aliased (no-AA) edges each row moves
    // by 0 or 1 px and a few long straight edges can bias it by ~0.1 px.
    let mut s = Session::open(opts(p, scene, Aa::Msaa)).await?;
    let k = p.ks.first().copied().unwrap_or(30);
    if k > 0 {
        s.play_to(k - 1).await?;
    }
    let (_, base) = s.render_next_captured().await?;
    // A frozen re-render is bit-identical to the frame (time really froze).
    let again = s.render_frozen_captured(None).await?;
    let st = abs_error(&base, &again, None);
    r.emit(
        &format!("frozen-rerender:{scene}"),
        st.max == 0,
        &format!("{st}"),
    );
    // A zero override is the same image as no override.
    let zero = s.render_frozen_captured(Some(Vec2::ZERO)).await?;
    let st = abs_error(&base, &zero, None);
    r.emit(
        &format!("zero-jitter-neutral:{scene}"),
        st.max == 0,
        &format!("{st}"),
    );
    // Convention: with jitter j, content rasterises at s + j (x right, y down).
    let xa = s
        .render_frozen_captured(Some(Vec2::new(-0.25, 0.0)))
        .await?;
    let xb = s.render_frozen_captured(Some(Vec2::new(0.25, 0.0))).await?;
    let ya = s
        .render_frozen_captured(Some(Vec2::new(0.0, -0.25)))
        .await?;
    let yb = s.render_frozen_captured(Some(Vec2::new(0.0, 0.25))).await?;
    let sx = metrics::estimate_shift_x(&xa, &xb, 1.0, 0.01, 8.0);
    let sy = metrics::estimate_shift_y(&ya, &yb, 1.0, 0.01, 8.0);
    // Per-tile (4×3) shifts, to localise anything that does not follow the
    // jitter (content that ignores the jittered matrices).
    let (tw, th) = (p.size.0 / 4, p.size.1 / 3);
    let mut tiles = Vec::new();
    for ty in 0..3 {
        for tx in 0..4 {
            let c = |i: &Image| i.crop(tx * tw, ty * th, tw, th);
            let txs = metrics::estimate_shift_x(&c(&xa), &c(&xb), 1.0, 0.01, 8.0);
            let tys = metrics::estimate_shift_y(&c(&ya), &c(&yb), 1.0, 0.01, 8.0);
            let f = |v: f32| {
                if v.is_nan() {
                    "n/a".to_string()
                } else {
                    format!("{v:+.2}")
                }
            };
            tiles.push(format!("[{tx},{ty}] {}/{}", f(txs), f(tys)));
        }
    }
    r.info(&format!("jitter-tiles:{scene}"), &tiles.join(" "));
    if p.keep {
        for (n, img) in [
            ("x-neg", &xa),
            ("x-pos", &xb),
            ("y-neg", &ya),
            ("y-pos", &yb),
        ] {
            store::put(&p.store, &format!("hooks/{scene}-{n}.png"), img).await?;
        }
    }
    let ok = (sx - 0.5).abs() <= 0.05 && (sy - 0.5).abs() <= 0.05;
    r.emit(
        &format!("jitter-convention:{scene}"),
        ok,
        &format!(
            "Δj = +0.5 px moved content x {sx:+.3} px, y {sy:+.3} px (expect +0.500 ±0.05 each)"
        ),
    );
    s.destroy();
    Ok(())
}

fn mean_value(img: &Image) -> f64 {
    let sum: u64 = img
        .rgba
        .chunks_exact(4)
        .map(|p| p[0] as u64 + p[1] as u64 + p[2] as u64)
        .sum();
    sum as f64 / (img.pixel_count() as f64 * 3.0)
}

/// `?mode=frame-cost&scenes=…&aa=…&size=1920x1435`: GPU frame time from the
/// renderer's timestamp queries (`profiling::gpu_frame_timing`, the whole-frame
/// span: earliest pass begin → latest pass end), over `frames` frames after a
/// 60-frame warm-up, with the GPU drained between frames (no present / vsync
/// wait lands in the numbers). `runs` interleaved rounds over all
/// configurations; per configuration the mean and min/max over the runs.
/// `&repeat=1,4,8` (TAA, `taa-phase2`): the resolve dispatched N times per
/// frame; resolve cost = (T(N) − T(1)) / (N − 1) per run, and TAA − none −
/// resolve (velocity target + second vertex evaluation + the rest).
/// Informational (always PASS unless a measurement fails); compare two builds
/// (e.g. the 0.45 worktree and HEAD) at the same size.
pub async fn frame_cost(p: &Params, r: &mut Report) {
    // Configurations: scene × AA (× resolve repeat count for TAA). `runs`
    // rounds, each measuring every configuration once (interleaved), so slow
    // drift (thermals, other tabs) spreads over all configurations alike.
    let mut configs: Vec<(String, Aa, u32)> = Vec::new();
    for scene in &p.scenes {
        for aa in &p.aas {
            if *aa == Aa::Taa {
                for n in &p.repeats {
                    configs.push((scene.clone(), *aa, (*n).max(1)));
                }
            } else {
                configs.push((scene.clone(), *aa, 1));
            }
        }
    }
    let label = |(scene, aa, n): &(String, Aa, u32)| {
        if *aa == Aa::Taa && p.repeats.len() > 1 {
            format!("{scene}:{}x{n}", aa.name())
        } else {
            format!("{scene}:{}", aa.name())
        }
    };
    let mut results: Vec<Vec<f64>> = vec![Vec::new(); configs.len()];
    for run in 0..p.runs.max(1) {
        for (i, c) in configs.iter().enumerate() {
            let name = format!("frame-cost:{}@{}x{}:run{run}", label(c), p.size.0, p.size.1);
            set_hud(&format!("taa-tests: {name}…"));
            match frame_cost_one(p, &c.0, c.1, c.2).await {
                Ok((mean, detail)) => {
                    results[i].push(mean);
                    r.info(&name, &detail);
                }
                Err(e) => r.emit(&name, false, &format!("error: {e:#}")),
            }
        }
    }
    let spread = |v: &[f64]| -> (f64, f64, f64) {
        let mean = v.iter().sum::<f64>() / v.len().max(1) as f64;
        let lo = v.iter().cloned().fold(f64::INFINITY, f64::min);
        let hi = v.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        (mean, lo, hi)
    };
    let fmt = |v: &[f64]| {
        let (m, lo, hi) = spread(v);
        let runs: Vec<String> = v.iter().map(|x| format!("{x:.3}")).collect();
        format!(
            "{m:.3} ms (min {lo:.3}, max {hi:.3}; runs {})",
            runs.join(", ")
        )
    };
    for (i, c) in configs.iter().enumerate() {
        if results[i].is_empty() {
            continue;
        }
        r.emit(
            &format!("frame-cost:{}@{}x{}", label(c), p.size.0, p.size.1),
            true,
            &format!("GPU frame mean {}", fmt(&results[i])),
        );
    }
    // Derived per scene: resolve = (T(N) - T(1)) / (N - 1), per run; and the
    // rest of TAA's cost = T_taa(1) - T_none - resolve.
    for scene in &p.scenes {
        let find = |aa: Aa, n: u32| {
            configs
                .iter()
                .position(|c| &c.0 == scene && c.1 == aa && c.2 == n)
                .map(|i| results[i].clone())
        };
        let (Some(t1), Some(none)) = (find(Aa::Taa, 1), find(Aa::None, 1)) else {
            continue;
        };
        for n in p.repeats.iter().filter(|n| **n > 1) {
            let Some(tn) = find(Aa::Taa, *n) else {
                continue;
            };
            let runs = t1.len().min(tn.len()).min(none.len());
            let resolve: Vec<f64> = (0..runs)
                .map(|k| (tn[k] - t1[k]) / (*n as f64 - 1.0))
                .collect();
            let rest: Vec<f64> = (0..runs).map(|k| t1[k] - none[k] - resolve[k]).collect();
            let total: Vec<f64> = (0..runs).map(|k| t1[k] - none[k]).collect();
            r.info(
                &format!("frame-cost:{scene}:taa-resolve(N={n})@{}x{}", p.size.0, p.size.1),
                &format!(
                    "resolve {}; TAA - none {}; velocity target + 2nd vertex evaluation + rest = TAA - none - resolve {}",
                    fmt(&resolve),
                    fmt(&total),
                    fmt(&rest)
                ),
            );
        }
    }
}

/// One frame-cost measurement: (mean GPU frame ms, detail).
async fn frame_cost_one(p: &Params, scene: &str, aa: Aa, repeat: u32) -> Result<(f64, String)> {
    use awsm_renderer::debug::TimingTier;
    use awsm_renderer::profiling;
    let n = p.cost_frames;
    let mut s = Session::open(opts(p, scene, aa)).await?;
    #[cfg(feature = "taa-phase2")]
    if aa == Aa::Taa {
        s.renderer.set_taa_debug_repeat(repeat);
    }
    #[cfg(not(feature = "taa-phase2"))]
    if repeat > 1 {
        return Err(anyhow!("repeat needs the taa-phase2 feature"));
    }
    s.renderer.logging.gpu = TimingTier::Frame;
    // Warm-up: pipelines, the timestamp query set, caches.
    for _ in 0..60 {
        s.render_next()?;
        gpu_idle(&s.renderer).await;
    }
    let stat0 = profiling::gpu_frame_timing()
        .map(|t| (t.count, t.total_ms))
        .unwrap_or((0, 0.0));
    let passes0 = profiling::gpu_timing_stats();
    for _ in 0..n {
        s.render_next()?;
        gpu_idle(&s.renderer).await;
    }
    // The timestamp readback folds a few frames late: render a few more
    // frames so every measured frame has landed (they add a handful of
    // samples of the same work; the mean is unaffected).
    for _ in 0..6 {
        s.render_next()?;
        gpu_idle(&s.renderer).await;
        crate::session::sleep_ms(4).await;
    }
    let stat1 = profiling::gpu_frame_timing()
        .ok_or_else(|| anyhow!("no GPU frame timing (timestamp-query unavailable?)"))?;
    let count = stat1.count.saturating_sub(stat0.0);
    if count == 0 {
        return Err(anyhow!("GPU timing produced no samples"));
    }
    let mean = (stat1.total_ms - stat0.1) / count as f64;
    // Per-pass means over the same window (top 6 by time).
    let passes1 = profiling::gpu_timing_stats();
    let mut per_pass: Vec<(String, f64)> = passes1
        .iter()
        .filter(|(k, _)| *k != profiling::GPU_FRAME_SCOPE)
        .filter_map(|(k, v1)| {
            let (c0, t0) = passes0
                .iter()
                .find(|(k0, _)| k0 == k)
                .map(|(_, v)| (v.count, v.total_ms))
                .unwrap_or((0, 0.0));
            let c = v1.count.saturating_sub(c0);
            (c > 0).then(|| (k.to_string(), (v1.total_ms - t0) / c as f64))
        })
        .collect();
    per_pass.sort_by(|a, b| b.1.total_cmp(&a.1));
    let top: Vec<String> = per_pass
        .iter()
        .take(6)
        .map(|(k, v)| format!("{k} {v:.2}"))
        .collect();
    s.renderer.logging.gpu = TimingTier::Off;
    s.destroy();
    Ok((
        mean,
        format!(
            "GPU frame mean {mean:.3} ms over {count} frames; passes (ms, overlap on Apple GPUs): {}",
            top.join(", ")
        ),
    ))
}
