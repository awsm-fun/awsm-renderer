use crate::prelude::*;
use dominator::svg;

pub struct EyeIcon {}

impl EyeIcon {
    pub fn render(show_signal: impl Signal<Item = bool> + 'static) -> Dom {
        static CLASS: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("width", "1.6rem")
                .style("height", "1.6rem")
            }
        });
        svg!("svg", {
            .class(&*CLASS)
            .attr_signal("fill", show_signal.map(|show| {
                if !show {
                    ColorRaw::MidGrey.value()
                } else {
                    ColorRaw::Accent.value()
                }
            }))
            .attrs!{
                "viewBox": "0 0 120 120",
            }
            .children([
                svg!("path", {
                    .attr("d", "M60,19.089C22.382,19.089,0.053,60,0.053,60S22.382,100.91,60,100.91S119.947,60,119.947,60S97.618,19.089,60,19.089z
            M59.999,84.409C46.54,84.409,35.59,73.459,35.59,60c0-13.459,10.95-24.409,24.409-24.409c13.459,0,24.409,10.95,24.409,24.409
            C84.408,73.459,73.458,84.409,59.999,84.409z")

                }),
                svg!("circle", {
                    .attrs!{
                        "cx": "60",
                        "cy": "60.583",
                        "r": "14.409",
                    }
                }),
            ])
        })
    }
}
