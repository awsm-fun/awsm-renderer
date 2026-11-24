//! The renderer's TAA test hooks (`awsm_renderer::taa`), behind the
//! `taa-hooks` feature so the same harness also builds against the 0.45
//! release (which has none of them) for the TAA-off baseline.

use std::cell::RefCell;

use awsm_renderer::taa::{self, JitterOverride, TaaConfig, TaaTuning};
use awsm_renderer::AwsmRenderer;
use glam::Vec2;

use crate::reference;

thread_local! {
    /// `?scale=`: the TAA render scale of every TAA session (phase 4).
    static SCALE: std::cell::Cell<f32> = const { std::cell::Cell::new(1.0) };
    /// `TaaTuning` overrides (`?tune=name:value,…`, or set by a sweep) for
    /// every TAA session opened after they are set.
    static TUNE: RefCell<Vec<(String, f32)>> = const { RefCell::new(Vec::new()) };
}

/// The doc-hidden experimental knobs (`set_taa_experimental_tuning`):
/// `clip_alpha`, `reactive_gamma_scale`. Not TaaTuning fields.
const EXPERIMENTAL: [(&str, f32); 2] = [("clip_alpha", 0.5), ("reactive_gamma_scale", 0.5)];

/// Replace the tuning overrides. Unknown names are an error.
pub fn set_tune(tune: &[(String, f32)]) -> Result<(), String> {
    let mut t = TaaTuning::default();
    for (k, v) in tune {
        if EXPERIMENTAL.iter().any(|(n, _)| n == k) {
            if !cfg!(feature = "taa-experimental") {
                return Err(format!("`{k}` needs the taa-experimental build"));
            }
            continue;
        }
        apply_tune(&mut t, k, *v)?;
    }
    TUNE.with(|c| *c.borrow_mut() = tune.to_vec());
    Ok(())
}

/// The experimental knob values to set on a TAA session, `None` when the
/// tuning names none of them (shipped behaviour).
pub fn experimental() -> Option<[f32; 2]> {
    TUNE.with(|c| {
        let t = c.borrow();
        let get = |name: &str, d: f32| {
            t.iter()
                .rev()
                .find(|(k, _)| k == name)
                .map(|(_, v)| *v)
                .unwrap_or(d)
        };
        let any = t
            .iter()
            .any(|(k, _)| EXPERIMENTAL.iter().any(|(n, _)| n == k));
        any.then(|| {
            [
                get(EXPERIMENTAL[0].0, EXPERIMENTAL[0].1),
                get(EXPERIMENTAL[1].0, EXPERIMENTAL[1].1),
            ]
        })
    })
}

pub fn tune_label() -> String {
    TUNE.with(|c| {
        let t = c.borrow();
        if t.is_empty() {
            "default tuning".to_string()
        } else {
            t.iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(",")
        }
    })
}

fn apply_tune(t: &mut TaaTuning, name: &str, v: f32) -> Result<(), String> {
    match name {
        "gamma_still" => t.gamma_still = v,
        "gamma_moving" => t.gamma_moving = v,
        "disocclusion_threshold" => t.disocclusion_threshold = v,
        "max_history_native" => t.max_history_native = v,
        "max_history_upscaling" => t.max_history_upscaling = v,
        "motion_alpha" => t.motion_alpha = v,
        "motion_alpha_speed" => t.motion_alpha_speed = v,
        "reactive_transparent" => t.reactive_transparent = v,
        "reactive_material" => t.reactive_material = v,
        "mip_bias_extra" => t.mip_bias_extra = v,
        #[cfg(feature = "taa-tuning-v2")]
        "max_history_moving" => t.max_history_moving = v,
        #[cfg(feature = "taa-tuning-v2")]
        "max_history_moving_speed" => t.max_history_moving_speed = v,
        #[cfg(feature = "taa-tuning-v2")]
        "motion_edge_alpha" => t.motion_edge_alpha = v,
        #[cfg(feature = "taa-tuning-v2")]
        "motion_edge_speed" => t.motion_edge_speed = v,
        other => {
            return Err(format!(
                "unknown (or not in this build) TaaTuning field `{other}`"
            ))
        }
    }
    Ok(())
}

pub fn set_scale(scale: f32) {
    SCALE.with(|c| c.set(scale));
}

pub fn scale() -> f32 {
    SCALE.with(|c| c.get())
}

/// `AntiAliasing` for TAA (sharpen 0) at the `?scale=` render scale (native
/// by default), with the current tuning overrides.
pub fn taa_native() -> awsm_renderer::anti_alias::AntiAliasing {
    let mut cfg = TaaConfig::native();
    cfg.render_scale = scale();
    TUNE.with(|c| {
        for (k, v) in c.borrow().iter() {
            // Validated in `set_tune` (experimental knobs are applied on the
            // renderer by the session, see `experimental`).
            let _ = apply_tune(&mut cfg.tuning, k, *v);
        }
    });
    awsm_renderer::anti_alias::AntiAliasing::taa(cfg)
}

/// Force (or release) the camera jitter, in render pixels.
pub fn set_jitter(renderer: &mut AwsmRenderer, jitter: Option<Vec2>) {
    renderer.set_jitter_override(jitter.map(JitterOverride::Fixed));
}

/// The harness's mirrors of the jitter sequence, the phase count and the
/// reconstruction kernel agree with the renderer's (bit-exact for the
/// sequence, 1e-7 for the kernel). Returns a detail string or the first
/// disagreement.
pub fn mirrors_agree() -> Result<String, String> {
    for n in [1u32, 8, 18, 24, 32, 64] {
        for i in 0..(2 * n) {
            let a = taa::jitter_offset(i, n);
            let b = reference::jitter_offset(i, n);
            if a != b {
                return Err(format!("jitter_offset({i}, {n}): renderer {a} harness {b}"));
            }
        }
    }
    for (r, o) in [
        (1920u32, 1920u32),
        (1286, 1920),
        (1114, 1920),
        (960, 1920),
        (643, 960),
    ] {
        let a = taa::jitter_phase_count(r, o);
        let b = reference::jitter_phase_count(r, o);
        if a != b {
            return Err(format!(
                "jitter_phase_count({r}, {o}): renderer {a} harness {b}"
            ));
        }
    }
    for i in 0..100 {
        let d2 = i as f32 * 0.05;
        let a = taa::reconstruction_weight(d2);
        let b = reference::reconstruction_weight(d2);
        if (a - b).abs() > 1e-7 {
            return Err(format!(
                "reconstruction_weight({d2}): renderer {a} harness {b}"
            ));
        }
    }
    Ok("jitter_offset (6 cycle lengths), jitter_phase_count (5 ratios), reconstruction_weight (100 d²) identical".into())
}
