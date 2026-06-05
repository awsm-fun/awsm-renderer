//! Per-row rendering of the tree. A row is: `[indent] [chevron] [icon] [name]`.
//!
//! Click / shift-click / ctrl-click modifier handling lives here, as does
//! the pointer-drag detection that turns into a reparent on drop.

use crate::config::{TREE_DRAG_THRESHOLD_PX, TREE_INDENT_PX, TREE_ROW_HEIGHT_PX};
use crate::prelude::*;
use crate::scene::{mutate, AssetStatus, Node, NodeId};
use crate::state::app_state;
use crate::tree::{
    context_menu,
    drag::{self, DropZone},
    icons,
};
use wasm_bindgen::JsCast;
use web_sys::Element;

const ROW_DATA_ATTR: &str = "data-ge-node-id";
const LOCK_DATA_ATTR: &str = "data-ge-lock";
const EYE_DATA_ATTR: &str = "data-ge-eye";
const CHEVRON_DATA_ATTR: &str = "data-ge-chevron";

/// Render a node plus (if expanded) its children, recursively.
pub fn render_subtree(node: Arc<Node>, depth: usize) -> Dom {
    let children_node = node.clone();
    html!("div", {
        .child(render_row(node.clone(), depth))
        .child(html!("div", {
            .style_signal("display", children_node.expanded.signal().map(|exp| {
                if exp { "block" } else { "none" }
            }))
            .children_signal_vec(children_node.children.signal_vec_cloned().map(move |child| {
                render_subtree(child, depth + 1)
            }))
        }))
    })
}

fn render_row(node: Arc<Node>, depth: usize) -> Dom {
    static ROW_CLASS: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("display", "flex")
            .style("align-items", "center")
            .style("gap", "0.35rem")
            .style("padding", "0 0.35rem")
            .style("box-sizing", "border-box")
            .style("position", "relative")
            .style("cursor", "pointer")
            .style("font-size", "0.85rem")
            .pseudo!(":hover", {
                .style("background", ColorBackground::SidebarSelected.value())
            })
        }
    });

    let state = app_state();
    let node_id = node.id;
    let selected_signal = state.selected.signal_ref(move |set| set.contains(&node_id));
    let zone_signal = state.tree_drop_zone_signal(node_id);

    let node_for_chevron = node.clone();
    let node_for_name = node.clone();
    let node_for_icon = node.clone();

    html!("div", {
        .class(&*ROW_CLASS)
        .attr(ROW_DATA_ATTR, &node.id.to_string())
        .style("height", &format!("{TREE_ROW_HEIGHT_PX}px"))
        .style_signal("background-color", selected_signal.map(|sel| {
            if sel {
                ColorBackground::SidebarSelected.value()
            } else {
                "transparent"
            }
        }))
        // Outliner filter: hide leaf rows whose name doesn't match. Group
        // rows (children > 0) always stay so the hierarchy holds.
        .style_signal("display", map_ref! {
            let filter = app_state().tree_filter.signal_cloned(),
            let name = node.name.signal_cloned(),
            let child_len = node.children.signal_vec_cloned().len() => {
                if filter.is_empty()
                    || *child_len > 0
                    || name.to_ascii_lowercase().contains(&filter.to_ascii_lowercase())
                {
                    "flex"
                } else {
                    "none"
                }
            }
        })
        .child(html!("div", {
            .style("flex", "0 0 auto")
            .style("width", &format!("{}px", depth as f64 * TREE_INDENT_PX))
        }))
        .child(render_chevron(node_for_chevron))
        .child(icons::for_kind(&node_for_icon.kind.get_cloned()))
        .child(render_prefab_badge(node.clone()))
        .child(html!("span", {
            .style("flex", "1 1 auto")
            .style("white-space", "nowrap")
            .style("overflow", "hidden")
            .style("text-overflow", "ellipsis")
            .style_signal("color", node_for_name.asset_status.signal_cloned().map(|s| {
                match s {
                    AssetStatus::Failed(_) => ColorRaw::Red.value(),
                    _ => ColorText::SidebarHeader.value(),
                }
            }))
            .text_signal(node_for_name.name.signal_cloned())
        }))
        .child(render_asset_status_badge(node.clone()))
        .child(render_eye_toggle(node.clone()))
        .child(render_lock_toggle(node.clone()))
        .child(drop_indicator(zone_signal))
        .with_node!(row_elem => {
            .event(clone!(node, row_elem => move |event: events::PointerDown| {
                if event.button() == events::MouseButton::Right { return; }
                if event_hits_lock(&event) { return; }
                if event_hits_eye(&event) { return; }
                // Same trick as the lock toggle: skip pointer capture when
                // the press starts on the chevron, otherwise the click
                // gets retargeted to the row and the expand/collapse
                // never fires.
                if event_hits_chevron(&event) { return; }
                if node.locked.get() { return; }
                let _ = row_elem.set_pointer_capture(event.pointer_id());
                on_pointer_down(&node, event);
            }))
            .event(clone!(node => move |event: events::PointerMove| {
                on_pointer_move(&node, event);
            }))
            .event(clone!(node => move |event: events::PointerUp| {
                on_pointer_up(&node, event);
            }))
            .event(clone!(node => move |_: events::PointerCancel| {
                on_pointer_cancel(&node);
            }))
            .event(clone!(node => move |event: events::ContextMenu| {
                event.prevent_default();
                context_menu::open_for(node.id, event.x(), event.y());
            }))
        })
    })
}

fn render_lock_toggle(node: Arc<Node>) -> Dom {
    static WRAPPER: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("flex", "0 0 auto")
            .style("display", "inline-flex")
            .style("align-items", "center")
            .style("justify-content", "center")
            .style("width", "1.1rem")
            .style("height", "1.1rem")
            .style("cursor", "pointer")
            .style("color", ColorText::Byline.value())
            .pseudo!(":hover", {
                .style("color", ColorText::SidebarHeader.value())
            })
        }
    });

    let locked = node.locked.clone();
    html!("span", {
        .class(&*WRAPPER)
        .attr(LOCK_DATA_ATTR, "1")
        .style_signal("opacity", locked.signal().map(|l| if l { "1" } else { "0.35" }))
        .child_signal(locked.signal().map(|l| Some(lock_icon(l))))
        .event(clone!(locked => move |event: events::Click| {
            // The row's pointerdown handler already skipped this click by
            // walking up to `data-ge-lock`, so we just need to toggle.
            event.stop_propagation();
            locked.set(!locked.get());
        }))
    })
}

fn lock_icon(closed: bool) -> Dom {
    // Padlock: shackle (arc) + body (rect). When open, tilt the shackle.
    // `pointer-events: none` on the svg so the transparent interior of the
    // padlock body doesn't let clicks slip past to the row behind.
    let color = ColorText::SidebarHeader.value();
    let shackle_d = if closed {
        "M5.5 7.5 V5 a2.5 2.5 0 0 1 5 0 V7.5"
    } else {
        "M5.5 7.5 V5 a2.5 2.5 0 0 1 4.2 -1.8"
    };
    svg!("svg", {
        .attr("style", "pointer-events: none")
        .attr("viewBox", "0 0 16 16")
        .attr("width", "12")
        .attr("height", "12")
        .attr("fill", "none")
        .attr("stroke", color)
        .attr("stroke-width", "1.5")
        .attr("stroke-linecap", "round")
        .attr("stroke-linejoin", "round")
        .child(svg!("rect", {
            .attr("x", "3.5")
            .attr("y", "7.5")
            .attr("width", "9")
            .attr("height", "6.5")
            .attr("rx", "1.2")
        }))
        .child(svg!("path", { .attr("d", shackle_d) }))
    })
}

/// Eye toggle. Behaves identically to the lock toggle (same hit-test
/// trick to keep the row's pointer-capture out of the way) but flips
/// `node.visible` instead of `node.locked`. Open eye = visible, closed
/// (slashed) eye = hidden.
fn render_eye_toggle(node: Arc<Node>) -> Dom {
    static WRAPPER: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("flex", "0 0 auto")
            .style("display", "inline-flex")
            .style("align-items", "center")
            .style("justify-content", "center")
            .style("width", "1.1rem")
            .style("height", "1.1rem")
            .style("cursor", "pointer")
            .style("color", ColorText::Byline.value())
            .pseudo!(":hover", {
                .style("color", ColorText::SidebarHeader.value())
            })
        }
    });

    let visible = node.visible.clone();
    html!("span", {
        .class(&*WRAPPER)
        .attr(EYE_DATA_ATTR, "1")
        // Visible nodes get a faded eye like the lock; hidden nodes use
        // full opacity so the slashed-eye state is the visually loud one.
        .style_signal("opacity", visible.signal().map(|v| if v { "0.45" } else { "1" }))
        .child_signal(visible.signal().map(|v| Some(eye_icon(v))))
        .event(clone!(visible => move |event: events::Click| {
            event.stop_propagation();
            visible.set(!visible.get());
        }))
    })
}

fn eye_icon(visible: bool) -> Dom {
    let color = ColorText::SidebarHeader.value();
    svg!("svg", {
        .attr("style", "pointer-events: none")
        .attr("viewBox", "0 0 16 16")
        .attr("width", "13")
        .attr("height", "13")
        .attr("fill", "none")
        .attr("stroke", color)
        .attr("stroke-width", "1.4")
        .attr("stroke-linecap", "round")
        .attr("stroke-linejoin", "round")
        // Almond outline + iris pupil — same shape regardless of state so
        // the icon's footprint stays stable. Toggle adds a slash overlay
        // when hidden.
        .child(svg!("path", {
            .attr("d", "M1.5 8 C3.5 4 6 3 8 3 C10 3 12.5 4 14.5 8 C12.5 12 10 13 8 13 C6 13 3.5 12 1.5 8 Z")
        }))
        .child(svg!("circle", {
            .attr("cx", "8")
            .attr("cy", "8")
            .attr("r", "1.8")
            .attr("fill", color)
            .attr("stroke", "none")
        }))
        .apply_if(!visible, |dom| {
            dom.child(svg!("line", {
                .attr("x1", "2")
                .attr("y1", "14")
                .attr("x2", "14")
                .attr("y2", "2")
                .attr("stroke-width", "1.6")
            }))
        })
    })
}

fn render_chevron(node: Arc<Node>) -> Dom {
    static CHEVRON: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("flex", "0 0 auto")
            .style("width", "1rem")
            .style("height", "1rem")
            .style("display", "inline-flex")
            .style("align-items", "center")
            .style("justify-content", "center")
            .style("color", ColorText::Byline.value())
            .style("font-size", "0.7rem")
            .style("line-height", "1")
        }
    });

    let expanded = node.expanded.clone();
    let children = node.children.clone();

    html!("span", {
        .class(&*CHEVRON)
        .attr(CHEVRON_DATA_ATTR, "1")
        .style_signal("visibility", children.signal_vec_cloned().len().map(|len| {
            if len == 0 { "hidden" } else { "visible" }
        }))
        .style_signal("transform", expanded.signal().map(|e| {
            if e { "rotate(90deg)" } else { "rotate(0deg)" }
        }))
        .text("▶")
        .event(clone!(expanded => move |event: events::Click| {
            event.stop_propagation();
            expanded.set(!expanded.get());
        }))
    })
}

/// Tiny "P" pill that shows next to the kind icon when this node is a
/// prefab root. The badge is purely informational — the Static/Prefab
/// dropdown lives in the properties panel. Hidden (zero-width) when the
/// node is Static so the row layout stays unchanged for the common case.
fn render_prefab_badge(node: Arc<Node>) -> Dom {
    static BADGE: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("flex", "0 0 auto")
            .style("display", "inline-flex")
            .style("align-items", "center")
            .style("justify-content", "center")
            .style("min-width", "0.95rem")
            .style("height", "0.95rem")
            .style("padding", "0 0.25rem")
            .style("border-radius", "0.25rem")
            .style("background", ColorRaw::Accent.value())
            .style("color", ColorRaw::Darkest.value())
            .style("font-size", "0.6rem")
            .style("font-weight", "700")
            .style("letter-spacing", "0.05em")
            .style("pointer-events", "none")
            .style(["-moz-user-select", "user-select", "-webkit-user-select"], "none")
        }
    });

    html!("span", {
        .child_signal(node.prefab.signal().map(|is_prefab| {
            if is_prefab {
                Some(html!("span", {
                    .class(&*BADGE)
                    .attr("title", "Prefab root")
                    .text("P")
                }))
            } else {
                None
            }
        }))
    })
}

fn render_asset_status_badge(node: Arc<Node>) -> Dom {
    html!("span", {
        .style("flex", "0 0 auto")
        .style("display", "inline-flex")
        .style("align-items", "center")
        .style("gap", "0.25rem")
        .child_signal(node.asset_status.signal_cloned().map(|s| match s {
            AssetStatus::Loading => Some(render_spinner()),
            AssetStatus::Failed(err) => Some(render_error_icon(&err)),
            _ => None,
        }))
    })
}

fn render_spinner() -> Dom {
    static SPIN: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("animation", "ge-spin 0.9s linear infinite")
        }
    });
    // Dominator's `stylesheet!` macro is for declaration blocks; it can't
    // express @keyframes. Inject the keyframes once as a raw <style> tag
    // into <head> via `LazyLock` so the first spinner on the page sets
    // things up and subsequent ones reuse it.
    static KEYFRAMES: LazyLock<()> = LazyLock::new(|| {
        if let Some(document) = web_sys::window().and_then(|w| w.document()) {
            if let Some(head) = document.head() {
                if let Ok(style_el) = document.create_element("style") {
                    style_el.set_text_content(Some(
                        "@keyframes ge-spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }",
                    ));
                    let _ = head.append_child(&style_el);
                }
            }
        }
    });
    LazyLock::force(&KEYFRAMES);

    svg!("svg", {
        .class(&*SPIN)
        .attr("viewBox", "0 0 16 16")
        .attr("width", "12")
        .attr("height", "12")
        .attr("fill", "none")
        .attr("stroke", ColorRaw::Accent.value())
        .attr("stroke-width", "2")
        .attr("stroke-linecap", "round")
        .child(svg!("path", {
            .attr("d", "M8 2 A6 6 0 0 1 14 8")
        }))
    })
}

fn render_error_icon(err: &str) -> Dom {
    // Warning triangle with a centered exclamation. Slightly oversized
    // compared to the row's kind-icon so missing assets jump out at you.
    svg!("svg", {
        .attr("viewBox", "0 0 16 16")
        .attr("width", "14")
        .attr("height", "14")
        .attr("fill", ColorRaw::Red.value())
        .attr("stroke", ColorRaw::Red.value())
        .attr("stroke-width", "1")
        .attr("stroke-linejoin", "round")
        .attr("aria-label", &format!("Asset missing: {err}"))
        .child(svg!("title", { .text(&format!("Asset missing: {err}")) }))
        .child(svg!("path", {
            .attr("d", "M8 2 L14.5 13.5 L1.5 13.5 Z")
            .attr("fill", ColorRaw::Red.value())
            .attr("fill-opacity", "0.2")
        }))
        .child(svg!("path", {
            .attr("d", "M8 6 L8 10")
            .attr("stroke", ColorRaw::Red.value())
            .attr("stroke-width", "1.6")
        }))
        .child(svg!("circle", {
            .attr("cx", "8")
            .attr("cy", "11.8")
            .attr("r", "0.7")
            .attr("fill", ColorRaw::Red.value())
            .attr("stroke", "none")
        }))
    })
}

fn drop_indicator(signal: impl Signal<Item = Option<DropZone>> + 'static) -> Dom {
    html!("div", {
        .style("position", "absolute")
        .style("inset", "0")
        .style("pointer-events", "none")
        .child_signal(signal.map(|zone| zone.map(render_zone)))
    })
}

fn render_zone(zone: DropZone) -> Dom {
    let color = ColorRaw::Accent.value();
    match zone {
        DropZone::Above => html!("div", {
            .style("position", "absolute")
            .style("left", "0")
            .style("right", "0")
            .style("top", "-1px")
            .style("height", "2px")
            .style("background", color)
        }),
        DropZone::Below => html!("div", {
            .style("position", "absolute")
            .style("left", "0")
            .style("right", "0")
            .style("bottom", "-1px")
            .style("height", "2px")
            .style("background", color)
        }),
        DropZone::Inside => html!("div", {
            .style("position", "absolute")
            .style("inset", "0")
            .style("outline", &format!("2px solid {color}"))
            .style("outline-offset", "-2px")
        }),
    }
}

// ---- pointer-driven select / drag state machine ----

thread_local! {
    static PENDING: Mutable<Option<Pending>> = Mutable::new(None);
}

#[derive(Clone, Copy)]
struct Pending {
    start_x: f64,
    start_y: f64,
    dragging: bool,
}

fn on_pointer_down(node: &Arc<Node>, event: events::PointerDown) {
    if event.shift_key() {
        apply_shift_click(node.id);
        PENDING.with(|p| p.set(None));
        return;
    }
    // `ctrl_key()` already OR's with `meta_key()` in this dominator fork,
    // so Cmd on macOS works as the toggle modifier out of the box.
    if event.ctrl_key() {
        app_state().toggle_selection(node.id);
        PENDING.with(|p| p.set(None));
        return;
    }

    PENDING.with(|p| {
        p.set(Some(Pending {
            start_x: event.x(),
            start_y: event.y(),
            dragging: false,
        }))
    });
}

fn on_pointer_move(source: &Arc<Node>, event: events::PointerMove) {
    let pending = PENDING.with(|p| p.get());
    let Some(mut pending) = pending else { return };

    let px = event.x();
    let py = event.y();

    if !pending.dragging {
        let dx = px - pending.start_x;
        let dy = py - pending.start_y;
        if dx.hypot(dy) < TREE_DRAG_THRESHOLD_PX {
            return;
        }
        let state = app_state();
        if !state.selected.lock_ref().contains(&source.id) {
            state.select_only(source.id);
        }
        let ids: Vec<NodeId> = state.selected.lock_ref().iter().copied().collect();
        let ids = mutate::ancestor_dedup(&state.scene, ids);
        state.begin_tree_drag(ids);
        pending.dragging = true;
        PENDING.with(|p| p.set(Some(pending)));
    }

    let state = app_state();
    // Pointer capture pins pointer events to the source row, so resolve
    // the actual row under the cursor via `document.elementFromPoint`.
    match find_row_under_pointer(px, py) {
        Some((target_id, offset_y, height)) => {
            let zone = drag::zone_from_offset(offset_y, height);
            let ids = state.dragged_node_ids();
            let into_descendant = ids
                .iter()
                .any(|&id| mutate::is_ancestor_of(&state.scene, id, target_id));
            if into_descendant {
                state.tree_drag_target.set(None);
            } else {
                state.tree_drag_target.set(Some((target_id, zone)));
            }
        }
        None => {
            state.tree_drag_target.set(None);
        }
    }
}

/// `dominator` attaches events in the capture phase by default, so the
/// row's `PointerDown` handler runs *before* the lock span's and has to
/// detect lock-originating events itself (otherwise the row would grab
/// pointer capture and swallow the click).
fn event_hits_lock(event: &events::PointerDown) -> bool {
    event_hits_descendant(event, LOCK_DATA_ATTR)
}

/// Same idea as `event_hits_lock` but for the eye toggle.
fn event_hits_eye(event: &events::PointerDown) -> bool {
    event_hits_descendant(event, EYE_DATA_ATTR)
}

/// Same idea as `event_hits_lock`: the chevron sits inside the row, and
/// without this check the row's pointer-capture would steal the click and
/// expand/collapse would never fire.
fn event_hits_chevron(event: &events::PointerDown) -> bool {
    event_hits_descendant(event, CHEVRON_DATA_ATTR)
}

fn event_hits_descendant(event: &events::PointerDown, attr: &str) -> bool {
    let Some(target) = event.target() else {
        return false;
    };
    let Ok(target) = target.dyn_into::<Element>() else {
        return false;
    };
    let selector = format!("[{attr}]");
    matches!(target.closest(&selector), Ok(Some(_)))
}

/// Walks up from the deepest element at `(x, y)` looking for a row with
/// our `data-ge-node-id` attribute. Returns `(id, offset_y_in_row, row_height)`.
fn find_row_under_pointer(x: f64, y: f64) -> Option<(NodeId, f64, f64)> {
    let document = web_sys::window()?.document()?;
    let mut el: Element = document.element_from_point(x as f32, y as f32)?;
    loop {
        if let Some(id_str) = el.get_attribute(ROW_DATA_ATTR) {
            if let Ok(uuid) = uuid::Uuid::parse_str(&id_str) {
                let id = NodeId(uuid);
                let rect = el.get_bounding_client_rect();
                return Some((id, y - rect.top(), rect.height()));
            }
        }
        match el.parent_element() {
            Some(parent) => el = parent.unchecked_into(),
            None => return None,
        }
    }
}

fn on_pointer_up(node: &Arc<Node>, event: events::PointerUp) {
    let pending = PENDING.with(|p| {
        let taken = p.get();
        p.set(None);
        taken
    });
    let Some(pending) = pending else { return };

    if pending.dragging {
        let state = app_state();
        let (dragged, target) = state.end_tree_drag();
        if let Some((target_id, zone)) = target {
            drag::apply_drop(target_id, zone, &dragged);
        }
    } else if !event.shift_key() && !event.ctrl_key() {
        app_state().select_only(node.id);
    }
}

fn on_pointer_cancel(_node: &Arc<Node>) {
    PENDING.with(|p| p.set(None));
    app_state().end_tree_drag();
}

fn apply_shift_click(to: NodeId) {
    let state = app_state();
    let anchor = state.selection_anchor.get_cloned().unwrap_or(to);
    let order = mutate::flatten_visible_order(&state.scene);
    let a = order.iter().position(|&id| id == anchor);
    let b = order.iter().position(|&id| id == to);
    let (Some(a), Some(b)) = (a, b) else {
        state.select_only(to);
        return;
    };
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    let range: Vec<NodeId> = order[lo..=hi].to_vec();
    state.set_selection(range, Some(anchor));
}
