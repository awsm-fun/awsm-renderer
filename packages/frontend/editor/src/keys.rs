//! Document-level keyboard shortcuts. Attached once on startup from
//! `main.rs`. Key bindings live in `config::keys`.

use crate::config::keys;
use crate::scene::{mutate, NodeId};
use crate::state::app_state;
use crate::tree::context_menu;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::KeyboardEvent;

pub fn install() {
    let Some(window) = web_sys::window() else {
        return;
    };

    let closure = Closure::wrap(Box::new(move |event: KeyboardEvent| {
        // ⌘K / Ctrl+K toggles the command palette — handled before the
        // text-input guard so it works even while a field is focused.
        if event.key().eq_ignore_ascii_case("k") && (event.ctrl_key() || event.meta_key()) {
            event.prevent_default();
            let open = &app_state().cmdk_open;
            open.set_neq(!open.get());
            return;
        }

        if is_in_text_input(&event) {
            return;
        }
        let key = event.key();

        // Delete / Backspace → delete selection. Asset selection takes
        // priority over node selection: while at least one asset is
        // selected the key deletes that batch and leaves the node
        // selection alone.
        if keys::DELETE.iter().any(|k| *k == key) {
            event.prevent_default();
            let assets: Vec<crate::scene::AssetId> = app_state()
                .selected_assets
                .get_cloned()
                .into_iter()
                .collect();
            if !assets.is_empty() {
                crate::actions::project::delete_asset_entries(&assets);
            } else {
                crate::actions::object::delete();
            }
            return;
        }

        if key == keys::ESCAPE {
            // Close the tree context menu first if it's open; otherwise
            // clear the active selection. Asset selection takes priority
            // over node selection for the same reason Delete does.
            context_menu::close();
            let has_assets = !app_state().selected_assets.lock_ref().is_empty();
            if has_assets {
                app_state().selected_assets.set(indexmap::IndexSet::new());
            } else {
                crate::actions::object::deselect();
            }
            return;
        }

        if key == keys::ARROW_UP {
            event.prevent_default();
            move_selection(Direction::Up, event.shift_key());
            return;
        }
        if key == keys::ARROW_DOWN {
            event.prevent_default();
            move_selection(Direction::Down, event.shift_key());
            return;
        }

        // Duplicate via Cmd/Ctrl+D
        if key.eq_ignore_ascii_case(keys::DUPLICATE_KEY) && (event.ctrl_key() || event.meta_key()) {
            event.prevent_default();
            crate::actions::object::duplicate();
            return;
        }

        // Save via Cmd/Ctrl+S (only fires when there are unsaved changes).
        if key.eq_ignore_ascii_case(keys::SAVE_KEY) && (event.ctrl_key() || event.meta_key()) {
            event.prevent_default();
            if app_state().dirty.get() {
                crate::actions::project::save();
            }
        }
    }) as Box<dyn FnMut(_)>);

    if window
        .add_event_listener_with_callback("keydown", closure.as_ref().unchecked_ref())
        .is_ok()
    {
        // Leak intentionally — the listener lives for the life of the page.
        closure.forget();
    }
}

fn is_in_text_input(event: &KeyboardEvent) -> bool {
    let Some(target) = event.target() else {
        return false;
    };
    let Some(element) = target.dyn_ref::<web_sys::Element>() else {
        return false;
    };
    let tag = element.tag_name().to_ascii_uppercase();
    matches!(tag.as_str(), "INPUT" | "TEXTAREA" | "SELECT")
}

#[derive(Clone, Copy)]
enum Direction {
    Up,
    Down,
}

fn move_selection(direction: Direction, shift: bool) {
    let state = app_state();
    let order = mutate::flatten_visible_order(&state.scene);
    if order.is_empty() {
        return;
    }

    // Pick a pivot: the anchor if set, else the first selected, else the first row.
    let current = state
        .selection_anchor
        .get_cloned()
        .or_else(|| state.selected.lock_ref().iter().next().copied())
        .unwrap_or(order[0]);

    let Some(current_idx) = order.iter().position(|&id| id == current) else {
        state.select_only(order[0]);
        return;
    };

    let next_idx = match direction {
        Direction::Up => current_idx.saturating_sub(1),
        Direction::Down => (current_idx + 1).min(order.len() - 1),
    };
    if next_idx == current_idx {
        return;
    }
    let next_id = order[next_idx];

    if shift {
        let anchor = state.selection_anchor.get_cloned().unwrap_or(next_id);
        let Some(a) = order.iter().position(|&id| id == anchor) else {
            state.select_only(next_id);
            return;
        };
        let (lo, hi) = if a <= next_idx {
            (a, next_idx)
        } else {
            (next_idx, a)
        };
        let range: Vec<NodeId> = order[lo..=hi].to_vec();
        state.set_selection(range, Some(anchor));
    } else {
        state.select_only(next_id);
    }
}
