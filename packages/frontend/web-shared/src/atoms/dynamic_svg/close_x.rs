use crate::prelude::*;

pub struct CloseX {}

impl CloseX {
    pub fn render(size: ButtonSize, on_click: impl Fn() + 'static) -> Dom {
        static CONTAINER_CLASS: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("cursor", "pointer")
                .style("border-radius", "50%")
                .style("transition", "box-shadow 170ms ease, transform 170ms ease, background-color 170ms ease")
            }
        });

        static SMALL_CLASS: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("width", "1rem")
                .style("height", "1rem")
            }
        });

        static LARGE_CLASS: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("width", "2rem")
                .style("height", "2rem")
            }
        });

        let hover = Mutable::new(false);

        html!("div", {
            .class(&*CONTAINER_CLASS)
            .class(match size {
                ButtonSize::Sm => &*SMALL_CLASS,
                _ => &*LARGE_CLASS,
            })
            // hover tracking and click live on the container so the full
            // circular hit-area responds, not just the SVG path
            .apply(set_on_hover(&hover))
            .event(move |_: events::Click| {
                on_click();
            })
            .style_signal("background-color", hover.signal().map(|hover| match hover {
                true => ColorRaw::Darkest.value(),
                false => ColorRaw::Darkish.value(),
            }))
            .style_signal("box-shadow", hover.signal().map(|hover| match hover {
                true => "0 0 14px rgba(114, 245, 255, 0.5), 0 0 5px rgba(114, 245, 255, 0.3)",
                false => "none",
            }))
            .style_signal("transform", hover.signal().map(|hover| match hover {
                true => "scale(1.1)",
                false => "scale(1.0)",
            }))
            .child(svg!("svg", {
                .attrs!{
                    "viewBox": "0 0 40 40",
                    "fill": "none",
                    "xmlns": "http://www.w3.org/2000/svg",
                }
                .child(
                    svg!("path", {
                        .attr("d", "M 10,10 L 30,30 M 30,10 L 10,30")
                        .attr("stroke-width", "4")
                        .attr("stroke-linecap", "butt")
                        // dims the X when idle, brightens on hover
                        .attr_signal("stroke", hover.signal().map(|hover| match hover {
                            true => ColorRaw::Whiteish.value(),
                            false => ColorRaw::MidGrey.value(),
                        }))
                    })
                )
            }))
        })
    }
}
