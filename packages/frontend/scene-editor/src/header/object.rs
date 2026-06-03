//! Object action-row — selection-scoped actions (Duplicate / Split /
//! Deselect / Delete). Every button disables when the current
//! selection is empty.

use crate::{actions, prelude::*, state};

pub(super) fn render_object_row() -> Dom {
    let has_selection = state::app_state().has_selection.clone();
    let disabled = move || has_selection.signal().map(|has| !has);
    html!("div", {
        .style("display", "flex")
        .style("gap", "0.5rem")
        .style("align-items", "center")
        .child(Button::new()
            .with_text("Duplicate")
            .with_size(ButtonSize::Sm)
            .with_disabled_signal(disabled())
            .with_on_click(actions::object::duplicate)
            .render())
        // Split is only enabled when at least one selected node is a
        // Model whose underlying gltf has >1 mesh primitives — see
        // `AppState::can_split_signal` for the exact rule.
        .child(Button::new()
            .with_text("Split")
            .with_style(ButtonStyle::Outline)
            .with_size(ButtonSize::Sm)
            .with_disabled_signal(state::app_state().can_split_signal().map(|can| !can))
            .with_on_click(actions::object::split)
            .render())
        .child(Button::new()
            .with_text("Deselect")
            .with_style(ButtonStyle::Outline)
            .with_size(ButtonSize::Sm)
            .with_disabled_signal(disabled())
            .with_on_click(actions::object::deselect)
            .render())
        .child(Button::new()
            .with_text("Delete")
            .with_color(ButtonColor::Red)
            .with_size(ButtonSize::Sm)
            .with_disabled_signal(disabled())
            .with_on_click(actions::object::delete)
            .render())
    })
}
