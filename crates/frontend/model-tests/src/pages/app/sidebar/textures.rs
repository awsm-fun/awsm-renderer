use std::cell::Cell;

use awsm_renderer::{core::texture::texture_pool::report::TexturePoolReport, textures::TextureKey};
use awsm_web::file::save::save_file;

use crate::{pages::app::context::AppContext, prelude::*};

use super::render_dropdown_label;

pub struct SidebarTextures {
    ctx: AppContext,
    phase: Mutable<Phase>,
    report: Mutex<Option<TexturePoolReport<TextureKey>>>,
    to_export: Mutable<Option<(usize, usize, u32)>>,
    mipmap_level: Cell<Option<u32>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Initializing,
    Ready,
    Exporting,
}

impl SidebarTextures {
    pub fn new(ctx: AppContext) -> Arc<Self> {
        Arc::new(Self {
            ctx,
            phase: Mutable::new(Phase::Initializing),
            report: Mutex::new(None),
            to_export: Mutable::new(None),
            mipmap_level: Cell::new(None),
        })
    }

    pub fn render(self: &Arc<Self>) -> Dom {
        let state = self;
        static CONTAINER: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("display", "flex")
                .style("flex-direction", "column")
                .style("align-items", "center")
            }
        });

        html!("div", {
            .class([&*CONTAINER])
            .future(clone!(state => async move {
                state.initialize().await;
            }))
            .child_signal(state.phase.signal().map(clone!(state => move |phase| {
                Some(match phase {
                    Phase::Initializing => html!("div", {
                        .class([FontSize::Lg.class(), ColorText::Byline.class()])
                        .text("Initializing...")
                    }),
                    Phase::Ready => state.render_inner(),
                    Phase::Exporting => state.render_exporting(),
                })
            })))
        })
    }

    async fn initialize(self: &Arc<Self>) {
        let state = self.clone();

        let renderer = state.ctx.scene.lock_ref();
        let renderer = renderer.as_ref().unwrap().renderer.lock().await;

        let size_report = renderer
            .textures
            .pool
            .generate_report(&renderer.gpu.device.limits());

        *state.report.lock().unwrap() = Some(size_report);

        state.phase.set(Phase::Ready);
    }

    fn render_exporting(self: &Arc<Self>) -> Dom {
        let state = self;

        let finished = Mutable::new(false);

        let (array_index, layer_index, _mip_levels) = state.to_export.lock_ref().unwrap();
        let mipmap_level = state.mipmap_level.get();

        html!("div", {
            .future(clone!(state, finished => async move {
                let (width, height) = {
                    let report = state.report.lock().unwrap();
                    let report = report.as_ref().unwrap();
                    (report.arrays[array_index].width, report.arrays[array_index].height)
                };

                let (gpu, texture_array, format) = {
                    let renderer = state.ctx.scene.lock_ref();
                    let renderer = renderer.as_ref().unwrap().renderer.lock().await;
                    let gpu = renderer.gpu.clone();
                    let texture_array = renderer.textures.pool.textures().nth(array_index).unwrap().clone();
                    let format = renderer.textures.pool.array_by_index(array_index).unwrap().format;
                    (gpu, texture_array, format)
                };

                let png_data = gpu.export_texture_as_png(
                    &texture_array,
                    width,
                    height,
                    layer_index as u32,
                    format,
                    mipmap_level,
                    true,
                    Some(true)
                ).await;

                match png_data {
                    Ok(png_data) => {
                        let filename = match mipmap_level {
                            Some(level) => format!("texture_pool_{}_layer_{}_mip_{}.png", array_index + 1, layer_index + 1, level),
                            None => format!("texture_pool_{}_layer_{}.png", array_index + 1, layer_index + 1),
                        };
                        tracing::info!("Exported PNG data for Texture Pool {} - Layer {}: {:?}", array_index + 1, layer_index + 1, png_data.len());
                        match save_file(&png_data, &filename, Some("image/png")) {
                            Ok(_) => {
                            }
                            Err(err) => {
                                Modal::open(move || {
                                    html!("div", {
                                        .class([FontSize::Lg.class(), ColorText::Error.class()])
                                        .text(&format!("Failed to save PNG data for Texture Pool {} - Layer {}: {:?}", array_index + 1, layer_index + 1, err))
                                    })
                                });
                            }
                        }
                    }
                    Err(err) => {
                        Modal::open(move || {
                            html!("div", {
                                .class([FontSize::Lg.class(), ColorText::Error.class()])
                                .text(&format!("Failed to export PNG data for Texture Pool {} - Layer {}: {:?}", array_index + 1, layer_index + 1, err))
                            })
                        });
                    }
                }

                finished.set(true);

            }))
            .child_signal(finished.signal().map(clone!(state => move |finished| {
                if finished {
                    state.phase.set(Phase::Ready);
                    None
                } else {
                    Some(html!("div", {
                        .class([FontSize::Lg.class(), ColorText::Byline.class()])
                        .text(&format!("Exporting Texture Pool {} - Layer {}", array_index + 1, layer_index + 1))
                    }))
                }
            })))
        })
    }

    fn render_inner(self: &Arc<Self>) -> Dom {
        let state = self;

        static CONTAINER: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("display", "flex")
                .style("flex-direction", "column")
                .style("align-items", "center")
                .style("gap", "3rem")
            }
        });

        static INNER_CONTAINER: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("display", "flex")
                .style("flex-direction", "column")
                .style("align-items", "center")
            }
        });

        html!("div", {
            .class(&*CONTAINER)
            .child(html!("div", {
                .class(&*INNER_CONTAINER)
                .child(state.render_export_selector())
                .child_signal(state.to_export.signal().map(clone!(state => move |to_export| {
                    match to_export {
                        None => None,
                        Some((_, _, mipmap_levels)) => {
                            if mipmap_levels > 1 {
                                Some(state.render_mipmap_selector(mipmap_levels))
                            } else {
                                None
                            }
                        }
                    }
                })))
                .child(state.render_export_button())
            }))
            .child(state.render_report_button())
        })
    }

    fn render_export_selector(self: &Arc<Self>) -> Dom {
        let state = self;

        let options: Vec<(String, (usize, usize, u32))> = state
            .report
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .arrays
            .iter()
            .enumerate()
            .flat_map(|(array_index, array)| {
                array
                    .entries
                    .iter()
                    .enumerate()
                    .map(move |(layer_index, _layer)| {
                        let label = format!(
                            "Texture Pool {} - Layer {}",
                            array_index + 1,
                            layer_index + 1
                        );
                        (label, (array_index, layer_index, array.mip_levels))
                    })
            })
            .collect();

        render_dropdown_label(
            "Layer to export",
            Dropdown::new()
                .with_intial_selected(None)
                .with_bg_color(ColorBackground::Dropdown)
                .with_on_change(
                    clone!(state => move |(atlas_index, layer_index, mip_levels)| {
                        state.to_export.set_neq(Some((*atlas_index, *layer_index, *mip_levels)));
                    }),
                )
                .with_options(options)
                .render(),
        )
    }

    fn render_mipmap_selector(self: &Arc<Self>, mipmap_levels: u32) -> Dom {
        let state = self;

        let options: Vec<(String, u32)> = (0..mipmap_levels)
            .map(|level| {
                let label = format!("Level - {level}");
                (label, level)
            })
            .collect();

        render_dropdown_label(
            "Mipmap Level",
            Dropdown::new()
                .with_intial_selected(None)
                .with_bg_color(ColorBackground::Dropdown)
                .with_on_change(clone!(state => move |level| {
                    state.mipmap_level.set(Some(*level));
                }))
                .with_options(options)
                .render(),
        )
    }

    fn render_export_button(self: &Arc<Self>) -> Dom {
        let state = self;

        Button::new()
            .with_style(ButtonStyle::Outline)
            .with_text("Export")
            .with_on_click(clone!(state => move || {
                if state.to_export.lock_ref().is_some() {
                    state.phase.set(Phase::Exporting);
                }
            }))
            .render()
    }

    fn render_report_button(self: &Arc<Self>) -> Dom {
        let state = self;

        Button::new()
            .with_style(ButtonStyle::Solid)
            .with_text("Show Report")
            .with_on_click(clone!(state => move || {
                let size_report = state.report.lock().unwrap().clone().unwrap();
                Modal::open(move || {
                    html!("pre", {
                        .class([FontSize::Md.class(), ColorText::Paragraph.class()])
                        .text(&format!("Mega Texture Size Report:\n\n{:#?}", size_report))
                    })
                });
            }))
            .render()
    }
}
