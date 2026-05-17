//! Camera action-row — Reset View, projection-mode dropdown, and the
//! authored-camera picker (`Free Fly` plus every `NodeKind::Camera`
//! in the scene). The authored-camera list rebuilds on every
//! `scene.revision` tick so new Camera nodes show up live.

use crate::{actions, prelude::*, state};

pub(super) fn render_camera_row() -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("gap", "0.75rem")
        .style("align-items", "center")
        .child(Button::new()
            .with_text("Reset View")
            .with_style(ButtonStyle::Outline)
            .with_size(ButtonSize::Sm)
            .with_on_click(actions::camera::reset_view)
            .render())
        .child(render_projection_mode_dropdown())
        .child(render_authored_camera_dropdown())
    })
}

/// Authored-camera picker. Lists every `NodeKind::Camera` in the scene
/// plus a `Free Fly` option (the default). Picking an authored camera
/// makes the viewport render from that node's `CameraBehavior` —
/// `editor_camera_target` is the source of truth; the render loop swaps
/// in driven matrices when it's Some.
///
/// The option list is rebuilt on every `scene.revision` tick so new
/// Camera nodes show up without reloading the editor.
fn render_authored_camera_dropdown() -> Dom {
    use awsm_web_shared::prelude::SignalExt;
    let revision = state::app_state().scene.revision.clone();
    html!("div", {
        .style("display", "flex")
        .style("align-items", "center")
        .style("gap", "0.4rem")
        .child(html!("span", {
            .style("font-size", "0.8rem")
            .style("color", ColorText::Byline.value())
            .text("View")
        }))
        .child_signal(revision.signal().map(|_rev| {
            Some(render_authored_camera_dropdown_inner())
        }))
    })
}

fn render_authored_camera_dropdown_inner() -> Dom {
    use awsm_web_shared::prelude::SignalExt;
    let cameras = crate::renderer_bridge::camera_driver::list_authored_cameras();
    let editor_camera_target = state::app_state().editor_camera_target.clone();
    html!("select" => web_sys::HtmlSelectElement, {
        .style("padding", "0.3rem 0.5rem")
        .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .style("border-radius", "0.3rem")
        .style("background-color", ColorRaw::Darkest.value())
        .style("color", ColorText::SidebarHeader.value())
        .style("font-size", "0.8rem")
        .style("cursor", "pointer")
        .child(html!("option", {
            .attr("value", "")
            .text("Free Fly")
        }))
        .children(cameras.iter().map(|(id, name)| {
            html!("option", {
                .attr("value", &id.0.to_string())
                .text(if name.is_empty() {
                    format!("Camera {}", &id.0.to_string()[..8])
                } else {
                    name.clone()
                }.as_str())
            })
        }))
        .with_node!(select => {
            .future(clone!(editor_camera_target, select => async move {
                editor_camera_target.signal().for_each(move |t| {
                    let want = t.map(|id| id.0.to_string()).unwrap_or_default();
                    if select.value() != want {
                        select.set_value(&want);
                    }
                    async {}
                }).await;
            }))
            .event(clone!(editor_camera_target, select => move |_: events::Change| {
                let value = select.value();
                let new_target = if value.is_empty() {
                    None
                } else {
                    uuid::Uuid::parse_str(&value)
                        .ok()
                        .map(crate::scene::NodeId)
                };
                editor_camera_target.set(new_target);
            }))
        })
    })
}

/// Projection-mode dropdown. The `select`'s value is driven by the
/// `projection_mode` signal on `AppState`, and `change` events route
/// through `actions::camera::set_projection_mode` so the AppState
/// `Mutable` and the renderer's live `Camera` stay in sync.
fn render_projection_mode_dropdown() -> Dom {
    use awsm_web_shared::util::free_camera::ProjectionMode;
    use wasm_bindgen::JsCast;

    let projection_mode = state::app_state().projection_mode.clone();

    html!("div", {
        .style("display", "flex")
        .style("align-items", "center")
        .style("gap", "0.4rem")
        .child(html!("span", {
            .style("font-size", "0.8rem")
            .style("color", ColorText::Byline.value())
            .text("Projection")
        }))
        .child(html!("select" => web_sys::HtmlSelectElement, {
            .style("padding", "0.3rem 0.5rem")
            .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
            .style("border-radius", "0.3rem")
            .style("background-color", ColorRaw::Darkest.value())
            .style("color", ColorText::SidebarHeader.value())
            .style("font-size", "0.8rem")
            .style("cursor", "pointer")
            .children(ProjectionMode::ALL.iter().map(|m| {
                html!("option", {
                    .attr("value", m.id())
                    .text(m.label())
                })
            }))
            .with_node!(select => {
                // Reflect the `Mutable` into the DOM `value` so external
                // changes (e.g. Reset View if we ever surface mode reset
                // there) keep the dropdown in sync.
                .future(clone!(projection_mode, select => async move {
                    use futures_signals::signal::SignalExt;
                    projection_mode.signal().for_each(move |m| {
                        select.set_value(m.id());
                        async {}
                    }).await;
                }))
                .event(clone!(select => move |_: events::Change| {
                    let value = select.value();
                    if let Some(mode) = ProjectionMode::from_id(&value) {
                        actions::camera::set_projection_mode(mode);
                    } else if let Some(opt) = select.dyn_ref::<web_sys::HtmlSelectElement>() {
                        // Out-of-range value: snap the DOM back to whatever
                        // AppState says is active.
                        opt.set_value(state::app_state().projection_mode.get().id());
                    }
                }))
            })
        }))
    })
}
