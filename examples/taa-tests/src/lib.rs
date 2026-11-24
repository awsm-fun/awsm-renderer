//! TAA test harness (docs/plans/taa-upscaling/05-verification.md).
//!
//! Loads test-scene bundles through the real player loader, plays each
//! scene's scripted scenario with a fixed 1/60 s timestep on a fixed-size
//! canvas, reads back the presented frames bit-exactly, and prints one
//! machine-readable line per check:
//!
//! ```text
//! TAA-TEST <name>: PASS — <numbers>
//! TAA-TEST <name>: FAIL — <numbers>
//! TAA-TESTS COMPLETE: <pass>/<total>
//! ```
//!
//! mirrored to the page's `#log` (every line) and `#hud` (aggregate). See the
//! crate README for the modes and URL parameters.

pub mod metrics;
pub mod reference;
pub mod scenario;

#[cfg(feature = "taa-phase1")]
mod disocc;
#[cfg(feature = "taa-phase1")]
mod evidence;
#[cfg(feature = "taa-hooks")]
mod gates;
#[cfg(feature = "taa-hooks")]
mod hooks;
mod modes;
#[cfg(feature = "taa-phase1")]
mod oracle;
#[cfg(feature = "taa-fb-overlay")]
mod overlay;
#[cfg(feature = "taa-phase1")]
mod phase3;
#[cfg(feature = "taa-phase4")]
mod phase4;
#[cfg(feature = "taa-phase1")]
mod reactive;
mod report;
mod session;
mod store;
#[cfg(feature = "taa-tuning-v2")]
mod sweep;
#[cfg(feature = "taa-tuning-v2")]
mod sweep2;
#[cfg(feature = "taa-phase1")]
mod velocity;

use wasm_bindgen::prelude::*;

/// Parsed URL parameters.
#[derive(Clone, Debug)]
pub struct Params {
    pub mode: String,
    pub scenes: Vec<String>,
    pub aas: Vec<session::Aa>,
    pub size: (u32, u32),
    pub bundles: String,
    pub store: String,
    pub set: String,
    /// Scenario frames to capture (baseline / determinism).
    pub frames: Vec<u32>,
    /// Frames to build references at (reference / hooks modes).
    pub ks: Vec<u32>,
    pub runs: u32,
    pub warmup: u32,
    pub phases: u32,
    /// Baseline-check tolerance on the max per-pixel error (default 0).
    pub tolerance: u8,
    /// Store the current captures + diffs even when the check passes.
    pub keep: bool,
    /// Measured frames for `frame-cost`.
    pub cost_frames: u32,
    /// `?mode=gate&gates=…`.
    pub gates: Vec<String>,
    /// Evaluate every n-th frame in the per-frame gates (trail, pan).
    pub stride: u32,
    /// Threshold multiplier (05 §4.5: 1.5 at 0.67, 2 at 0.5).
    pub relax: f64,
    /// Whether `?aa=` / `?scenes=` were given (gates have their own defaults).
    pub aa_given: bool,
    pub scenes_given: bool,
    pub ks_given: bool,
    pub frames_given: bool,
    /// `history-burst` density case: transforms inserted on the event frame.
    pub burst_n: u32,
    /// Per-object vector-scale sweep in every scene (`?sweep`).
    pub sweep: bool,
    /// `frame-cost`: TAA resolve repeat counts (`?repeat=1,4,8`; needs
    /// `taa-phase2`), each measured as its own configuration.
    pub repeats: Vec<u32>,
    /// `?tune=name:value,…`: TaaTuning overrides for every TAA session.
    pub tune: Vec<(String, f32)>,
    /// `?refcache=<name>`: cache the 64-phase references in the store under
    /// `refcache/<name>/` and reuse them (they do not depend on TAA tuning).
    pub refcache: Option<String>,
    /// `?ref=karis|linear`: the reference's accumulation space (Karis by
    /// default: the space the resolve blends in, 05 §3 "the same weights the
    /// resolve uses"; display-linear is the stricter external yardstick).
    pub karis: bool,
    /// `?scale=`: TAA render scale (phase 4; 1 = native).
    pub scale: f32,
    /// `?combos=a-b`: the slice of the tune-sweep grid this page runs.
    pub combos: Option<(usize, usize)>,
}

impl Params {
    fn from_url() -> Self {
        let list = |key: &str, default: &[&str]| -> Vec<String> {
            url_value(key)
                .map(|v| {
                    v.split(',')
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_else(|| default.iter().map(|s| s.to_string()).collect())
        };
        let nums = |key: &str, default: &[u32]| -> Vec<u32> {
            url_value(key)
                .map(|v| v.split(',').filter_map(|s| s.parse().ok()).collect())
                .unwrap_or_else(|| default.to_vec())
        };
        let mode = url_value("mode").unwrap_or_else(|| "determinism".into());
        let default_scenes: &[&str] = match mode.as_str() {
            "hooks" => &["aa-edges"],
            "reference" => &["aa-edges"],
            "determinism" => &["kitchen-sink", "ssr", "transparent", "anim-skinned"],
            _ => modes::BASELINE_SCENES,
        };
        let default_aa: &[&str] = match mode.as_str() {
            "determinism" => &["msaa", "none"],
            _ => &["msaa", "smaa", "none"],
        };
        let size = url_value("size")
            .and_then(|v| {
                let (w, h) = v.split_once('x')?;
                Some((w.parse().ok()?, h.parse().ok()?))
            })
            .unwrap_or((960, 720));
        Self {
            scenes: list("scenes", default_scenes),
            aas: list("aa", default_aa)
                .iter()
                .filter_map(|s| session::Aa::parse(s))
                .collect(),
            size,
            bundles: url_value("bundles")
                .unwrap_or_else(|| "http://localhost:9084".into())
                .trim_end_matches('/')
                .to_string(),
            store: url_value("store")
                .unwrap_or_else(|| "http://localhost:9095".into())
                .trim_end_matches('/')
                .to_string(),
            set: url_value("set").unwrap_or_else(|| "baseline-0.45".into()),
            frames: nums("frames", &[5, 60]),
            ks: nums("k", &[30]),
            runs: url_value("runs").and_then(|v| v.parse().ok()).unwrap_or(2),
            warmup: url_value("warmup")
                .and_then(|v| v.parse().ok())
                .unwrap_or(8),
            phases: url_value("phases")
                .and_then(|v| v.parse().ok())
                .unwrap_or(reference::REFERENCE_PHASES),
            tolerance: url_value("tolerance")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            keep: url_value("keep").is_some(),
            cost_frames: url_value("cost_frames")
                .and_then(|v| v.parse().ok())
                .unwrap_or(240),
            gates: list("gates", &["ghosting", "convergence"]),
            stride: url_value("stride")
                .and_then(|v| v.parse().ok())
                .unwrap_or(1)
                .max(1),
            relax: url_value("relax")
                .and_then(|v| v.parse().ok())
                .unwrap_or(1.0),
            aa_given: url_value("aa").is_some(),
            scenes_given: url_value("scenes").is_some(),
            ks_given: url_value("k").is_some(),
            frames_given: url_value("frames").is_some(),
            burst_n: url_value("burst_n")
                .and_then(|v| v.parse().ok())
                .unwrap_or(64),
            sweep: url_value("sweep").is_some(),
            tune: url_value("tune")
                .map(|v| {
                    v.split(',')
                        .filter_map(|kv| {
                            let (k, x) = kv.split_once(':')?;
                            Some((k.trim().to_string(), x.trim().parse().ok()?))
                        })
                        .collect()
                })
                .unwrap_or_default(),
            refcache: url_value("refcache").filter(|v| !v.is_empty()),
            karis: url_value("ref").as_deref() != Some("linear"),
            scale: url_value("scale")
                .and_then(|v| v.parse().ok())
                .unwrap_or(1.0),
            combos: url_value("combos").and_then(|v| {
                let (a, b) = v.split_once('-')?;
                Some((a.parse().ok()?, b.parse().ok()?))
            }),
            repeats: url_value("repeat")
                .map(|v| v.split(',').filter_map(|x| x.trim().parse().ok()).collect())
                .unwrap_or_else(|| vec![1]),
            mode,
        }
    }
}

#[wasm_bindgen(start)]
pub fn boot() -> Result<(), JsValue> {
    install_tracing();
    std::panic::set_hook(Box::new(|info| {
        report::log(&format!("TAA-TEST panic: FAIL — {info}"));
        report::log("TAA-TESTS COMPLETE: aborted (panic)");
        report::set_hud("taa-tests: COMPLETE aborted (panic)");
    }));
    awsm_renderer_codec_basis::configure(awsm_renderer_codec_basis::BasisWorkerConfig::player(
        "/workers/basis-worker.js".to_string(),
        "/vendor/basis/basis_transcoder.js".to_string(),
    ));
    wasm_bindgen_futures::spawn_local(async {
        let p = Params::from_url();
        let mut r = report::Report::default();
        r.info(
            "config",
            &format!(
                "mode {} scenes {:?} aa {:?} size {}x{} frames {:?} k {:?} warmup {} runs {} build {}",
                p.mode,
                p.scenes,
                p.aas.iter().map(|a| a.name()).collect::<Vec<_>>(),
                p.size.0,
                p.size.1,
                p.frames,
                p.ks,
                p.warmup,
                p.runs,
                if cfg!(feature = "taa-hooks") { "taa-hooks" } else { "0.45-compatible (no taa hooks)" },
            ),
        );
        #[cfg(feature = "taa-hooks")]
        if let Err(e) = hooks::set_tune(&p.tune) {
            r.emit("tune", false, &e);
        }
        #[cfg(feature = "taa-hooks")]
        {
            hooks::set_scale(p.scale);
            r.info(
                "tuning",
                &format!(
                    "{}; TAA render scale {}; reference space {}",
                    hooks::tune_label(),
                    p.scale,
                    if p.karis { "ref=karis" } else { "ref=linear" }
                ),
            );
        }
        match p.mode.as_str() {
            "determinism" => modes::determinism(&p, &mut r).await,
            "baseline-record" => modes::baseline_record(&p, &mut r).await,
            "baseline-check" => modes::baseline_check(&p, &mut r).await,
            "reference" => modes::reference_mode(&p, &mut r).await,
            "hooks" => modes::hooks_mode(&p, &mut r).await,
            "frame-cost" => modes::frame_cost(&p, &mut r).await,
            #[cfg(feature = "taa-hooks")]
            "gate" => gates::gate_mode(&p, &mut r).await,
            #[cfg(feature = "taa-phase1")]
            "vectors" => velocity::vectors_mode(&p, &mut r).await,
            #[cfg(feature = "taa-phase1")]
            "aa-toggle" => velocity::aa_toggle_mode(&p, &mut r).await,
            #[cfg(feature = "taa-phase1")]
            "motion-history" => velocity::motion_history_mode(&p, &mut r).await,
            #[cfg(feature = "taa-phase1")]
            "history-burst" => velocity::history_burst_mode(&p, &mut r).await,
            #[cfg(feature = "taa-phase1")]
            "trail-evidence" => evidence::trail_evidence_mode(&p, &mut r).await,
            #[cfg(feature = "taa-phase1")]
            "speeds" => evidence::speeds_mode(&p, &mut r).await,
            #[cfg(feature = "taa-phase1")]
            "reference-floor" => evidence::reference_floor_mode(&p, &mut r).await,
            #[cfg(feature = "taa-phase1")]
            "phase3" => phase3::phase3_mode(&p, &mut r).await,
            #[cfg(feature = "taa-phase1")]
            "reactive-sweep" => reactive::reactive_sweep_mode(&p, &mut r).await,
            #[cfg(feature = "taa-phase4")]
            "scale-change" => phase4::scale_change_mode(&p, &mut r).await,
            #[cfg(feature = "taa-phase4")]
            "mip-bias" => phase4::mip_bias_mode(&p, &mut r).await,
            #[cfg(feature = "taa-phase4")]
            "governor-hitch" => phase4::governor_hitch_mode(&p, &mut r).await,
            #[cfg(feature = "taa-phase4")]
            "sharpen-sweep" => phase4::sharpen_sweep_mode(&p, &mut r).await,
            #[cfg(feature = "taa-phase4")]
            "upscale-sweep" => phase4::upscale_sweep_mode(&p, &mut r).await,
            #[cfg(feature = "taa-tuning-v2")]
            "gamma-sweep" => phase4::gamma_sweep_mode(&p, &mut r).await,
            #[cfg(feature = "taa-phase1")]
            "disocclusion" => disocc::disocclusion_mode(&p, &mut r).await,
            #[cfg(feature = "taa-phase1")]
            "disocclusion-static" => disocc::disocclusion_static_mode(&p, &mut r).await,
            #[cfg(feature = "taa-phase1")]
            "static-rejection" => disocc::static_rejection_mode(&p, &mut r).await,
            #[cfg(feature = "taa-phase1")]
            "rejection-motion" => disocc::rejection_motion_mode(&p, &mut r).await,
            #[cfg(feature = "taa-tuning-v2")]
            "tune-sweep" => sweep::tune_sweep_mode(&p, &mut r).await,
            #[cfg(feature = "taa-tuning-v2")]
            "tune-sweep2" => sweep2::tune_sweep2_mode(&p, &mut r).await,
            #[cfg(feature = "taa-phase1")]
            "trail-oracle" => oracle::trail_oracle_mode(&p, &mut r).await,
            #[cfg(feature = "taa-fb-overlay")]
            "overlay-check" => overlay::overlay_check_mode(&p, &mut r).await,
            #[cfg(feature = "taa-fb-overlay")]
            "overlay-cost" => overlay::overlay_cost_mode(&p, &mut r).await,
            #[cfg(feature = "taa-fb-overlay")]
            "overlay-parity" => overlay::overlay_parity_mode(&p, &mut r).await,
            other => r.emit("mode", false, &format!("unknown mode `{other}`")),
        }
        r.complete();
    });
    Ok(())
}

/// `performance.now()` in ms (harness timing only; never fed to the renderer).
pub fn now_ms() -> f64 {
    web_sys::window()
        .and_then(|w| w.performance())
        .map(|p| p.now())
        .unwrap_or(0.0)
}

pub fn url_value(key: &str) -> Option<String> {
    let search = web_sys::window()?.location().search().ok()?;
    let prefix = format!("{key}=");
    search.trim_start_matches('?').split('&').find_map(|p| {
        if p == key {
            Some(String::new())
        } else {
            p.strip_prefix(&prefix).map(|v| v.to_string())
        }
    })
}

fn install_tracing() {
    use tracing_subscriber::prelude::*;
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .without_time()
        .with_writer(tracing_web::MakeWebConsoleWriter::new())
        .with_target(false);
    let filter = tracing_subscriber::filter::LevelFilter::WARN;
    let _ = tracing_subscriber::registry()
        .with(fmt_layer.with_filter(filter))
        .try_init();
}
