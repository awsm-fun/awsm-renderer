use crate::prelude::*;

#[derive(Debug, Clone, PartialEq)]
pub struct NotFoundUi {}

impl NotFoundUi {
    pub fn new() -> Self {
        Self {}
    }

    pub fn render(&self) -> Dom {
        html!("div", {
            .style("display", "flex")
            .style("flex-direction", "column")
            .style("min-height", "100%")
            .style("padding", "1.56rem 2.5rem")
            .child(html!("div", {
                .style("flex", "1")
                .child(html!("div", {
                    .class([FontSize::H1.class()])
                    .style("margin-top", "20px")
                    .style("text-align", "center")
                    .text("Not Found")
                }))
            }))
        })
    }
}
