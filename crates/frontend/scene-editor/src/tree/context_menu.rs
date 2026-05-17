//! Right-click context menu on tree rows. A single popup exists at most
//! once at a time; its position + visibility live on the module-local
//! state below so clicks outside can close it.

use crate::prelude::*;
use crate::{actions, scene::NodeId, state::app_state};

#[derive(Clone, Copy)]
pub struct MenuPosition {
    pub x: f64,
    pub y: f64,
}

thread_local! {
    static MENU_STATE: Mutable<Option<MenuPosition>> = Mutable::new(None);
}

pub fn open_for(target: NodeId, x: f64, y: f64) {
    // Ensure the right-clicked node is in the selection — typical OS
    // behavior: right-clicking a non-selected row replaces the selection
    // with that node; right-clicking a selected row leaves the multi-set
    // intact so the action applies to the group.
    let state = app_state();
    let already_selected = state.selected.lock_ref().contains(&target);
    if !already_selected {
        state.select_only(target);
    }
    let _ = target; // selection is already mutated above; id not needed in the popup state
    with_menu(|m| m.set(Some(MenuPosition { x, y })));
}

pub fn close() {
    with_menu(|m| m.set(None));
}

pub fn render_overlay() -> Dom {
    let signal = with_menu(|m| m.signal_cloned());
    html!("div", {
        .child_signal(signal.map(|pos| pos.map(render_menu)))
    })
}

fn render_menu(pos: MenuPosition) -> Dom {
    static POPUP: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("position", "fixed")
            .style("min-width", "10rem")
            .style("background-color", ColorBackground::Sidebar.value())
            .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
            .style("border-radius", "0.4rem")
            .style("box-shadow", "0 6px 24px rgba(0, 0, 0, 0.35)")
            .style("padding", "0.35rem 0")
            .style("z-index", "100")
        }
    });

    html!("div", {
        .child(html!("div", {
            // Backdrop behind the menu — any click outside the popup closes it.
            .style("position", "fixed")
            .style("inset", "0")
            .style("z-index", "99")
            .style("background", "transparent")
            .event(move |event: events::PointerDown| {
                event.stop_propagation();
                close();
            })
        }))
        .child(html!("div", {
            .class(&*POPUP)
            .style("left", &format!("{}px", pos.x))
            .style("top", &format!("{}px", pos.y))
            .child(render_item("Duplicate", false, || {
                close();
                actions::object::duplicate();
            }))
            .child(render_item("Delete", true, || {
                close();
                actions::object::delete();
            }))
            .child(render_separator())
            .child(render_item("Deselect", false, || {
                close();
                actions::object::deselect();
            }))
        }))
    })
}

fn render_item(label: &'static str, destructive: bool, on_click: impl Fn() + 'static) -> Dom {
    static ITEM: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("display", "flex")
            .style("align-items", "center")
            .style("width", "100%")
            .style("padding", "0.45rem 0.8rem")
            .style("border", "0")
            .style("background", "transparent")
            .style("cursor", "pointer")
            .style("font-size", "0.9rem")
            .style("text-align", "left")
            .pseudo!(":hover", {
                .style("background", ColorBackground::SidebarSelected.value())
            })
        }
    });

    html!("button", {
        .class(&*ITEM)
        .style("color", if destructive { ColorRaw::Red.value() } else { ColorText::SidebarHeader.value() })
        .text(label)
        .event(move |_: events::Click| on_click())
    })
}

fn render_separator() -> Dom {
    html!("div", {
        .style("height", "1px")
        .style("margin", "0.3rem 0")
        .style("background-color", ColorBackground::UnderlineSecondary.value())
    })
}

fn with_menu<T>(f: impl FnOnce(&Mutable<Option<MenuPosition>>) -> T) -> T {
    MENU_STATE.with(|m| f(m))
}
