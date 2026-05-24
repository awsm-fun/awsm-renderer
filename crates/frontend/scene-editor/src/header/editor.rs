//! Editor action-row — viewport toggles (Show Grid / Show Gizmo /
//! MSAA Anti-Aliasing). Each routes through `actions::view::*` so the
//! renderer and the `AppState` mirror stay in sync.

use crate::{actions, prelude::*, state};

pub(super) fn render_editor_row() -> Dom {
    let grid_enabled = state::app_state().grid_enabled.clone();
    let gizmo_enabled = state::app_state().gizmo_enabled.clone();
    let anti_aliasing = state::app_state().anti_aliasing.clone();
    html!("div", {
        .style("display", "flex")
        .style("gap", "1rem")
        .style("align-items", "center")
        .child(Checkbox::new(CheckboxStyle::Dark)
            .with_selected_signal(grid_enabled.signal())
            .with_content_after(html!("span", {
                .text("Show Grid")
            }))
            .with_on_click(clone!(grid_enabled => move || {
                actions::view::set_grid_enabled(!grid_enabled.get());
            }))
            .render())
        .child(Checkbox::new(CheckboxStyle::Dark)
            .with_selected_signal(gizmo_enabled.signal())
            .with_content_after(html!("span", {
                .text("Show Gizmo")
            }))
            .with_on_click(clone!(gizmo_enabled => move || {
                actions::view::set_gizmo_enabled(!gizmo_enabled.get());
            }))
            .render())
        .child(Checkbox::new(CheckboxStyle::Dark)
            .with_selected_signal(anti_aliasing.signal_ref(|aa| aa.msaa_sample_count.is_some()))
            .with_content_after(html!("span", {
                .text("MSAA Anti-Aliasing")
            }))
            .with_on_click(|| {
                actions::view::toggle_msaa();
            })
            .render())
    })
}
