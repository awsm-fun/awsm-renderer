//! Environment action-row — Skybox + IBL modal launchers. The modals
//! each accept KTX/KTX2 cubemaps (procedural defaults are available
//! via the `Apply Default Gradient` shortcut).

use crate::{actions, prelude::*};

pub(super) fn render_environment_row() -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("gap", "0.75rem")
        .style("align-items", "center")
        .child(Button::new()
            .with_text("Skybox…")
            .with_style(ButtonStyle::Outline)
            .with_size(ButtonSize::Sm)
            .with_on_click(open_skybox_modal)
            .render())
        .child(Button::new()
            .with_text("IBL…")
            .with_style(ButtonStyle::Outline)
            .with_size(ButtonSize::Sm)
            .with_on_click(open_ibl_modal)
            .render())
    })
}

fn open_skybox_modal() {
    Modal::open(|| {
        let picked: Mutable<Option<web_sys::File>> = Mutable::new(None);
        let file_input: Mutable<Option<web_sys::HtmlInputElement>> = Mutable::new(None);

        html!("div", {
            .style("display", "flex")
            .style("flex-direction", "column")
            .style("gap", "0.85rem")
            .style("color", ColorText::SidebarHeader.value())
            .style("min-width", "420px")
            .child(html!("h2", { .style("margin", "0") .text("Skybox") }))
            .child(html!("p", {
                .style("margin", "0")
                .style("font-size", "0.9rem")
                .style("line-height", "1.4")
                .text("Choose a KTX/KTX2 cubemap, or apply the built-in sky gradient.")
            }))
            .child(html!("div", {
                .style("display", "flex")
                .style("gap", "0.625rem")
                .style("flex-wrap", "wrap")
                .child(Button::new()
                    .with_text("Apply Default Gradient")
                    .with_on_click(|| {
                        Modal::close();
                        actions::view::apply_default_skybox();
                    })
                    .render())
                .child(Button::new()
                    .with_text("Choose KTX…")
                    .with_style(ButtonStyle::Outline)
                    .with_on_click(clone!(file_input => move || {
                        if let Some(input) = file_input.get_cloned() {
                            input.click();
                        }
                    }))
                    .render())
                .child(Button::new()
                    .with_text("Cancel")
                    .with_style(ButtonStyle::Outline)
                    .with_on_click(Modal::close)
                    .render())
            }))
            .child(html!("div", {
                .style("font-size", "0.8rem")
                .style("color", ColorText::Byline.value())
                .text_signal(picked.signal_cloned().map(|p| {
                    p.map(|f| format!("Selected: {}", f.name()))
                        .unwrap_or_else(|| "No file selected".to_string())
                }))
            }))
            .child(html!("input" => web_sys::HtmlInputElement, {
                .attr("type", "file")
                .attr("accept", ".ktx,.ktx2")
                .style("display", "none")
                .with_node!(input => {
                    .after_inserted(clone!(file_input, input => move |_| {
                        file_input.set(Some(input));
                    }))
                    .after_removed(clone!(file_input => move |_| {
                        file_input.set(None);
                    }))
                    .event(clone!(picked, input => move |_: events::Change| {
                        if let Some(file) = input.files().and_then(|fs| fs.get(0)) {
                            picked.set(Some(file.clone()));
                            input.set_value("");
                            Modal::close();
                            actions::view::apply_skybox_ktx_file(file);
                        }
                    }))
                })
            }))
        })
    });
}

fn open_ibl_modal() {
    Modal::open(|| {
        let prefiltered: Mutable<Option<web_sys::File>> = Mutable::new(None);
        let irradiance: Mutable<Option<web_sys::File>> = Mutable::new(None);
        let prefiltered_input: Mutable<Option<web_sys::HtmlInputElement>> = Mutable::new(None);
        let irradiance_input: Mutable<Option<web_sys::HtmlInputElement>> = Mutable::new(None);

        let file_row = |label: &'static str,
                        file: Mutable<Option<web_sys::File>>,
                        input_ref: Mutable<Option<web_sys::HtmlInputElement>>|
         -> Dom {
            html!("div", {
                .style("display", "flex")
                .style("align-items", "center")
                .style("gap", "0.6rem")
                .child(html!("span", {
                    .style("flex", "0 0 10rem")
                    .style("font-size", "0.85rem")
                    .style("color", ColorText::Byline.value())
                    .text(label)
                }))
                .child(Button::new()
                    .with_text("Choose KTX…")
                    .with_style(ButtonStyle::Outline)
                    .with_size(ButtonSize::Sm)
                    .with_on_click(clone!(input_ref => move || {
                        if let Some(i) = input_ref.get_cloned() {
                            i.click();
                        }
                    }))
                    .render())
                .child(html!("span", {
                    .style("font-size", "0.8rem")
                    .style("color", ColorText::Byline.value())
                    .text_signal(file.signal_cloned().map(|f| {
                        f.map(|f| f.name()).unwrap_or_else(|| "<none>".to_string())
                    }))
                }))
                .child(html!("input" => web_sys::HtmlInputElement, {
                    .attr("type", "file")
                    .attr("accept", ".ktx,.ktx2")
                    .style("display", "none")
                    .with_node!(input => {
                        .after_inserted(clone!(input_ref, input => move |_| {
                            input_ref.set(Some(input));
                        }))
                        .after_removed(clone!(input_ref => move |_| {
                            input_ref.set(None);
                        }))
                        .event(clone!(file, input => move |_: events::Change| {
                            if let Some(f) = input.files().and_then(|fs| fs.get(0)) {
                                file.set(Some(f));
                                input.set_value("");
                            }
                        }))
                    })
                }))
            })
        };

        html!("div", {
            .style("display", "flex")
            .style("flex-direction", "column")
            .style("gap", "0.85rem")
            .style("color", ColorText::SidebarHeader.value())
            .style("min-width", "520px")
            .child(html!("h2", { .style("margin", "0") .text("Image-Based Lighting") }))
            .child(html!("p", {
                .style("margin", "0")
                .style("font-size", "0.9rem")
                .style("line-height", "1.4")
                .text("Pick the two KTX/KTX2 cubemaps (prefiltered environment + irradiance), or apply the built-in sky gradient.")
            }))
            .child(file_row("Prefiltered env", prefiltered.clone(), prefiltered_input))
            .child(file_row("Irradiance", irradiance.clone(), irradiance_input))
            .child(html!("div", {
                .style("display", "flex")
                .style("gap", "0.625rem")
                .style("flex-wrap", "wrap")
                .style("padding-top", "0.25rem")
                .child(Button::new()
                    .with_text("Apply Default Gradient")
                    .with_on_click(|| {
                        Modal::close();
                        actions::view::apply_default_ibl();
                    })
                    .render())
                .child(Button::new()
                    .with_text("Apply Chosen Files")
                    .with_disabled_signal(map_ref! {
                        let p = prefiltered.signal_cloned(),
                        let i = irradiance.signal_cloned() => !(p.is_some() && i.is_some())
                    })
                    .with_on_click(clone!(prefiltered, irradiance => move || {
                        let Some(p) = prefiltered.get_cloned() else { return };
                        let Some(i) = irradiance.get_cloned() else { return };
                        Modal::close();
                        actions::view::apply_ibl_ktx_files(p, i);
                    }))
                    .render())
                .child(Button::new()
                    .with_text("Cancel")
                    .with_style(ButtonStyle::Outline)
                    .with_on_click(Modal::close)
                    .render())
            }))
        })
    });
}
