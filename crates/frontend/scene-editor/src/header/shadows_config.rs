//! Modal panel for the renderer-wide `ShadowsConfig`. Mirrors the
//! on-disk `EditorProject::shadows` block — every change here writes
//! straight into `app_state().scene.shadows`, which the renderer
//! bridge observes and pushes into the renderer.
//!
//! Resource-shaped fields (`atlas_size`, `point_shadow_resolution`,
//! `max_point_shadows`, `evsm_atlas_size`) are honoured at renderer
//! construction time; runtime tweaks of those during a single editor
//! session don't resize the live textures (the change still persists
//! to project.json and applies on the next session). The other fields
//! apply live.

use awsm_scene_schema::ShadowsConfig;
use awsm_web_shared::atoms::modal::Modal;
use futures_signals::signal::Mutable;
use std::sync::LazyLock;
use web_sys::HtmlSelectElement;

use crate::prelude::*;
use crate::state::app_state;

/// Public entry — wired into the Environment-tab toolbar.
pub fn open_modal() {
    Modal::open(|| render_modal_body());
}

fn render_modal_body() -> Dom {
    let scene = app_state().scene.clone();
    let cfg = scene.shadows.clone();

    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.65rem")
        .style("color", ColorText::SidebarHeader.value())
        .style("min-width", "560px")
        .style("max-height", "70vh")
        .style("overflow-y", "auto")
        .child(html!("h2", { .style("margin", "0").text("Shadows — global config") }))
        .child(html!("p", {
            .style("margin", "0")
            .style("font-size", "0.85rem")
            .style("line-height", "1.4")
            .style("color", ColorText::Byline.value())
            .text(
                "Renderer-wide shadow settings. Resource-shaped fields \
                (atlas / cube / EVSM sizes, max point shadows) take effect on \
                the next renderer rebuild — they're persisted to project.json \
                immediately so a reload picks them up. The remaining fields \
                apply live."
            )
        }))
        .child(section(
            "Screen-space contact shadows",
            vec![
                bool_row("SSCS enabled", cfg.clone(), |c| c.sscs_enabled, |c, v| c.sscs_enabled = v),
                u32_row("Step count", cfg.clone(), 1, 64, |c| c.sscs_step_count, |c, v| c.sscs_step_count = v),
            ],
        ))
        .child(section(
            "2D atlas / cascades",
            vec![
                pow2_select_row("Atlas size", cfg.clone(),
                    &[1024, 2048, 4096, 8192],
                    |c| c.atlas_size,
                    |c, v| c.atlas_size = v,
                ),
                bool_row("Debug cascade colors", cfg.clone(),
                    |c| c.debug_cascade_colors, |c, v| c.debug_cascade_colors = v),
            ],
        ))
        .child(section(
            "EVSM (soft far-cascade shadows)",
            vec![
                pow2_select_row("EVSM atlas size", cfg.clone(),
                    &[512, 1024, 2048, 4096],
                    |c| c.evsm_atlas_size,
                    |c, v| c.evsm_atlas_size = v,
                ),
                // 18 is the renderer-side cap (see
                // `ShadowsConfig::EVSM_EXPONENT_MAX_FP16`) — anything
                // above saturates fp16 moments and the Chebyshev curve
                // collapses to a hard binary mask. Authoring panel
                // matches the runtime clamp so the value the user
                // types is the value the renderer uses.
                f32_row("EVSM exponent", cfg.clone(), 1.0, 18.0, 0.5,
                    |c| c.evsm_exponent, |c, v| c.evsm_exponent = v),
                u32_row("Blur radius (texels)", cfg.clone(), 0, 8,
                    |c| c.evsm_blur_radius, |c, v| c.evsm_blur_radius = v),
            ],
        ))
        .child(section(
            "Point-light cube shadows",
            vec![
                u32_select_row("Max point shadows", cfg.clone(),
                    &[0, 2, 4, 8, 16],
                    |c| c.max_point_shadows,
                    |c, v| c.max_point_shadows = v,
                ),
                pow2_select_row("Point shadow resolution", cfg.clone(),
                    &[256, 512, 1024, 2048],
                    |c| c.point_shadow_resolution,
                    |c, v| c.point_shadow_resolution = v,
                ),
            ],
        ))
        .child(html!("div", {
            .style("display", "flex")
            .style("justify-content", "flex-end")
            .style("gap", "0.5rem")
            .style("padding-top", "0.5rem")
            .child(Button::new()
                .with_text("Reset to defaults")
                .with_style(ButtonStyle::Outline)
                .with_size(ButtonSize::Sm)
                .with_on_click(clone!(cfg => move || {
                    commit(&cfg, |c| *c = ShadowsConfig::default());
                }))
                .render())
            .child(Button::new()
                .with_text("Close")
                .with_size(ButtonSize::Sm)
                .with_on_click(Modal::close)
                .render())
        }))
    })
}

// ─────────────────────────────────────────────────────────────────────
// Row helpers — all share the same modify-via-clone-set commit path
// because `Mutable::set` requires owned `T`. `commit` also bumps the
// scene revision + pushes a history entry so undo works.
// ─────────────────────────────────────────────────────────────────────

fn commit(cfg: &Mutable<ShadowsConfig>, f: impl FnOnce(&mut ShadowsConfig)) {
    let mut c = cfg.get_cloned();
    f(&mut c);
    let state = app_state();
    let previous = state.snapshot_scene();
    cfg.set(c);
    state.scene.bump_revision();
    state.commit_history(previous);
}

fn section(label: &'static str, rows: Vec<Dom>) -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.4rem")
        .style("padding", "0.55rem 0")
        .style("border-top", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .child(html!("div", {
            .style("font-size", "0.78rem")
            .style("font-weight", "700")
            .style("letter-spacing", "0.04em")
            .style("text-transform", "uppercase")
            .style("color", ColorText::Byline.value())
            .text(label)
        }))
        .children(rows)
    })
}

fn field_row(label: &'static str, control: Dom) -> Dom {
    html!("div", {
        .style("display", "grid")
        .style("grid-template-columns", "13rem 1fr")
        .style("gap", "0.5rem")
        .style("align-items", "center")
        .child(html!("span", {
            .style("font-size", "0.8rem")
            .style("color", ColorText::Byline.value())
            .text(label)
        }))
        .child(control)
    })
}

fn bool_row(
    label: &'static str,
    cfg: Mutable<ShadowsConfig>,
    get: impl Fn(&ShadowsConfig) -> bool + Clone + 'static,
    set: impl Fn(&mut ShadowsConfig, bool) + 'static,
) -> Dom {
    let checked = cfg.signal_cloned().map(clone!(get => move |c| get(&c)));
    field_row(label, html!("input" => web_sys::HtmlInputElement, {
        .attr("type", "checkbox")
        .style("width", "1rem")
        .style("height", "1rem")
        .style("cursor", "pointer")
        .with_node!(input => {
            .future(clone!(input => {
                checked.for_each(move |b| {
                    if input.checked() != b { input.set_checked(b); }
                    async {}
                })
            }))
            .event(clone!(cfg, input => move |_: events::Change| {
                let v = input.checked();
                commit(&cfg, |c| set(c, v));
            }))
        })
    }))
}

fn u32_row(
    label: &'static str,
    cfg: Mutable<ShadowsConfig>,
    min: u32,
    max: u32,
    get: impl Fn(&ShadowsConfig) -> u32 + Clone + 'static,
    set: impl Fn(&mut ShadowsConfig, u32) + 'static,
) -> Dom {
    static INPUT: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("width", "100%")
            .style("padding", "0.25rem 0.4rem")
            .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
            .style("border-radius", "0.25rem")
            .style("background-color", ColorRaw::Darkest.value())
            .style("color", ColorText::SidebarHeader.value())
            .style("font-size", "0.8rem")
            .style("font-family", "monospace")
        }
    });
    let value_signal = cfg.signal_cloned().map(clone!(get => move |c| get(&c).to_string()));
    field_row(label, html!("input" => web_sys::HtmlInputElement, {
        .class(&*INPUT)
        .attr("type", "number")
        .attr("min", &min.to_string())
        .attr("max", &max.to_string())
        .attr("step", "1")
        .with_node!(input => {
            .future(clone!(input => {
                value_signal.for_each(move |s| {
                    if input.value() != s { input.set_value(&s); }
                    async {}
                })
            }))
            .event(clone!(cfg, input => move |_: events::Change| {
                if let Ok(parsed) = input.value().parse::<u32>() {
                    let v = parsed.clamp(min, max);
                    commit(&cfg, |c| set(c, v));
                }
            }))
        })
    }))
}

fn f32_row(
    label: &'static str,
    cfg: Mutable<ShadowsConfig>,
    min: f32,
    max: f32,
    step: f32,
    get: impl Fn(&ShadowsConfig) -> f32 + Clone + 'static,
    set: impl Fn(&mut ShadowsConfig, f32) + 'static,
) -> Dom {
    static INPUT: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("width", "100%")
            .style("padding", "0.25rem 0.4rem")
            .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
            .style("border-radius", "0.25rem")
            .style("background-color", ColorRaw::Darkest.value())
            .style("color", ColorText::SidebarHeader.value())
            .style("font-size", "0.8rem")
            .style("font-family", "monospace")
        }
    });
    let value_signal = cfg.signal_cloned().map(clone!(get => move |c| format!("{:.2}", get(&c))));
    field_row(label, html!("input" => web_sys::HtmlInputElement, {
        .class(&*INPUT)
        .attr("type", "number")
        .attr("min", &min.to_string())
        .attr("max", &max.to_string())
        .attr("step", &step.to_string())
        .with_node!(input => {
            .future(clone!(input => {
                value_signal.for_each(move |s| {
                    if input.value() != s { input.set_value(&s); }
                    async {}
                })
            }))
            .event(clone!(cfg, input => move |_: events::Change| {
                if let Ok(parsed) = input.value().parse::<f32>() {
                    let v = parsed.clamp(min, max);
                    commit(&cfg, |c| set(c, v));
                }
            }))
        })
    }))
}

fn pow2_select_row(
    label: &'static str,
    cfg: Mutable<ShadowsConfig>,
    options: &'static [u32],
    get: impl Fn(&ShadowsConfig) -> u32 + Clone + 'static,
    set: impl Fn(&mut ShadowsConfig, u32) + 'static,
) -> Dom {
    u32_select_row(label, cfg, options, get, set)
}

fn u32_select_row(
    label: &'static str,
    cfg: Mutable<ShadowsConfig>,
    options: &'static [u32],
    get: impl Fn(&ShadowsConfig) -> u32 + Clone + 'static,
    set: impl Fn(&mut ShadowsConfig, u32) + 'static,
) -> Dom {
    static SELECT: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("width", "100%")
            .style("padding", "0.25rem 0.4rem")
            .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
            .style("border-radius", "0.25rem")
            .style("background-color", ColorRaw::Darkest.value())
            .style("color", ColorText::SidebarHeader.value())
            .style("font-size", "0.8rem")
            .style("font-family", "monospace")
            .style("cursor", "pointer")
        }
    });
    let value_signal = cfg.signal_cloned().map(clone!(get => move |c| get(&c).to_string()));
    let opts = options;
    field_row(label, html!("select" => HtmlSelectElement, {
        .class(&*SELECT)
        .children(opts.iter().map(|v| html!("option", {
            .attr("value", &v.to_string())
            .text(&v.to_string())
        })))
        .with_node!(select => {
            .future(clone!(select => {
                value_signal.for_each(move |s| {
                    if select.value() != s { select.set_value(&s); }
                    async {}
                })
            }))
            .event(clone!(cfg, select => move |_: events::Change| {
                if let Ok(parsed) = select.value().parse::<u32>() {
                    commit(&cfg, |c| set(c, parsed));
                }
            }))
        })
    }))
}
