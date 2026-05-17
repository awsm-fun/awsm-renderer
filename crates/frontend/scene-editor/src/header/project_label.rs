//! Center-strip display of the active project's name + dirty
//! indicator. Click the name to rename — the span swaps to an
//! `<input>` that commits on blur or Enter and reverts on Esc.
//! The `dirty` flag also drives `document.title` (see
//! `AppState::wire_document_title`); this surface mirrors it so
//! the user doesn't have to glance at the browser tab.

use crate::{actions, prelude::*, state};

pub(super) fn render() -> Dom {
    use futures_signals::signal::SignalExt;
    let project_name = state::app_state().project_name.clone();
    let dirty = state::app_state().dirty.clone();
    let editing = Mutable::new(false);

    html!("div", {
        .style("display", "flex")
        .style("align-items", "center")
        .style("gap", "0.35rem")
        .style("margin-left", "1rem")
        .style("color", ColorText::SidebarHeader.value())
        .style("font-size", "0.9rem")
        .style("font-weight", "500")
        // The dot is the dirty indicator — small, leading, only
        // present when there are unsaved changes.
        .child_signal(dirty.signal().map(|d| {
            if d {
                Some(html!("span", {
                    .style("font-size", "0.9rem")
                    .style("line-height", "1")
                    .style("color", ColorText::SidebarHeader.value())
                    .text("•")
                }))
            } else {
                None
            }
        }))
        .child_signal(editing.signal().map(clone!(project_name, editing => move |is_editing| {
            Some(if is_editing {
                render_input(project_name.clone(), editing.clone())
            } else {
                render_display(project_name.clone(), editing.clone())
            })
        })))
    })
}

fn render_display(project_name: Mutable<Option<String>>, editing: Mutable<bool>) -> Dom {
    use futures_signals::signal::SignalExt;
    static DISPLAY: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("cursor", "text")
            .style("padding", "0.1rem 0.25rem")
            .style("border-radius", "0.25rem")
            .pseudo!(":hover", {
                .style("background", ColorBackground::SidebarSelected.value())
            })
        }
    });
    html!("span", {
        .class(&*DISPLAY)
        .attr("title", "Click to rename")
        .text_signal(project_name.signal_cloned().map(|n| match n {
            Some(name) if !name.is_empty() => name,
            _ => "(unsaved project)".to_string(),
        }))
        .event(clone!(editing => move |_: events::Click| {
            editing.set(true);
        }))
    })
}

fn render_input(project_name: Mutable<Option<String>>, editing: Mutable<bool>) -> Dom {
    let initial = project_name.get_cloned().unwrap_or_default();
    html!("input" => web_sys::HtmlInputElement, {
        .attr("type", "text")
        .attr("value", &initial)
        .style("font-size", "0.9rem")
        .style("font-weight", "500")
        .style("padding", "0.1rem 0.3rem")
        .style("border-radius", "0.25rem")
        .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .style("background", ColorRaw::Darkest.value())
        .style("color", ColorText::SidebarHeader.value())
        .style("min-width", "12rem")
        .with_node!(input => {
            // Autofocus + select all so the user can just type the
            // new name without an extra click+drag.
            .after_inserted(clone!(input => move |_| {
                let _ = input.focus();
                input.select();
            }))
            .event(clone!(editing, input => move |_: events::Blur| {
                actions::project::rename(input.value());
                editing.set(false);
            }))
            .event(clone!(editing, input => move |e: events::KeyDown| {
                match e.key().as_str() {
                    "Enter" => {
                        actions::project::rename(input.value());
                        editing.set(false);
                    }
                    "Escape" => {
                        // Revert: just exit edit mode without committing.
                        editing.set(false);
                    }
                    _ => {}
                }
            }))
        })
    })
}
