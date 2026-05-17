#![allow(dead_code)]
#![allow(clippy::arc_with_non_send_sync)]
#![allow(clippy::type_complexity)]
mod atoms;
mod config;
mod header;
mod logger;
mod models;
mod pages;
mod prelude;
mod route;
mod theme;
mod util;

use awsm_renderer::{core::compatibility::Compatibility, COMPATIBITLIY_REQUIREMENTS};
use prelude::*;

use crate::{
    header::Header,
    pages::{app::AppUi, not_found::NotFoundUi},
};

pub fn main() {
    wasm_bindgen_futures::spawn_local(async {
        init().await;
    });
}

async fn init() {
    logger::init_logger();
    theme::stylesheet::init();

    if let Some(init_url) = CONFIG.debug.start_route.lock().unwrap_throw().take() {
        init_url.go_to_url();
    }

    let compatibility = Mutable::new(None);

    dominator::append_dom(
        &dominator::body(),
        html!("div", {
            .future(clone!(compatibility => async move {
                compatibility.set(Some(Compatibility::check(Some(COMPATIBITLIY_REQUIREMENTS.clone())).await));
            }))
            .child_signal(compatibility.signal_cloned().map(|compatibility| {
                match compatibility {
                    None => None,
                    Some(Compatibility::Compatible) => Some(html!("div", {
                        .child_signal(TopLevelRoute::signal().map(|route| {
                            match route {
                                TopLevelRoute::App => None,
                                _ => Some(Header::new().render()),
                            }
                        }))
                        .child_signal(TopLevelRoute::signal().map(|route| {
                            Some(match route {
                                TopLevelRoute::App => AppUi::new().render(),
                                TopLevelRoute::NotFound => NotFoundUi::new().render()
                            })
                        }))
                        .fragment(&Modal::render())
                    })),
                    Some(error) => Some(render_incompatible(&error.main_text(), error.extra_text().as_deref())),
                }
            }))
        }),
    );
}

fn render_incompatible(main_text: &str, extra_text: Option<&str>) -> Dom {
    static GITHUB_LINK: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("display", "flex")
            .style("align-items", "center")
            .style("gap", "0.5rem")
            .style("color", ColorText::SidebarHeader.value())
            .style("text-decoration", "none")
            .style("opacity", "0.7")
            .style("transition", "opacity 0.3s")
            .style("font-size", FontSize::Md.value())
            .pseudo!(":hover", {
                .style("opacity", "1")
            })
        }
    });

    static GITHUB_SVG: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("width", "20px")
            .style("height", "20px")
            .style("fill", "currentColor")
        }
    });

    html!("div", {
        .style("width", "100vw")
        .style("height", "100vh")
        .class(ColorBackground::Sidebar.class())
        .child(html!("div", {
            .style("display", "flex")
            .style("flex-direction", "column")
            .style("justify-content", "center")
            .style("align-items", "center")
            .style("padding-top", "30vh")
            .style("gap", "1.5rem")
            .child(html!("div", {
                .style("display", "flex")
                .style("flex-direction", "column")
                .style("justify-content", "center")
                .style("align-items", "center")
                .style("gap", ".5rem")
                .child(html!("div", {
                    .class([FontSize::H3.class(), ColorText::Error.class()])
                    .text(main_text)
                }))
                .apply_if(extra_text.is_some(), |dom| {
                    dom.child(html!("div", {
                        .class([FontSize::Lg.class(), ColorText::Error.class()])
                        .text(extra_text.unwrap_or_default())
                    }))
                })
            }))
            .child(html!("div", {
                .child(html!("a", {
                    .class(&*GITHUB_LINK)
                    .attr("href", CONFIG.repo_url)
                    .attr("rel", "noopener noreferrer")
                    .child(svg!("svg", {
                        .class(&*GITHUB_SVG)
                        .attrs!{
                            "xmlns": "http://www.w3.org/2000/svg",
                            "viewBox": "0 0 24 24",
                        }
                        .child(svg!("path", {
                            .attr("d", "M12 0c-6.626 0-12 5.373-12 12 0 5.302 3.438 9.8 8.207 11.387.599.111.793-.261.793-.577v-2.234c-3.338.726-4.033-1.416-4.033-1.416-.546-1.387-1.333-1.756-1.333-1.756-1.089-.745.083-.729.083-.729 1.205.084 1.839 1.237 1.839 1.237 1.07 1.834 2.807 1.304 3.492.997.107-.775.418-1.305.762-1.604-2.665-.305-5.467-1.334-5.467-5.931 0-1.311.469-2.381 1.236-3.221-.124-.303-.535-1.524.117-3.176 0 0 1.008-.322 3.301 1.23.957-.266 1.983-.399 3.003-.404 1.02.005 2.047.138 3.006.404 2.291-1.552 3.297-1.23 3.297-1.23.653 1.653.242 2.874.118 3.176.77.84 1.235 1.911 1.235 3.221 0 4.609-2.807 5.624-5.479 5.921.43.372.823 1.102.823 2.222v3.293c0 .319.192.694.801.576 4.765-1.589 8.199-6.086 8.199-11.386 0-6.627-5.373-12-12-12z")
                        }))
                    }))
                    .child(html!("span", {
                        .class([FontSize::Lg.class(), ColorText::SidebarHeader.class()])
                        .text("You can still check out the repository :)")
                    }))
                }))
            }))
        }))
    })
}
