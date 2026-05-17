use crate::prelude::*;

pub struct Header {}

impl Header {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {})
    }

    pub fn render(self: &Arc<Self>) -> Dom {
        static CONTENT: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("display", "flex")
                .style("flex-direction", "row")
                .style("align-items", "center")
                .style("margin-top", "1rem")
                .style("margin-left", "1rem")
            }
        });
        html!("div", {
            .class(&*CONTENT)
            .child(Button::new()
                .with_style(ButtonStyle::Outline)
                .with_text("Home")
                .with_link(Route::App(AppRoute::Init).link())
                .render()
            )
        })
    }
}
