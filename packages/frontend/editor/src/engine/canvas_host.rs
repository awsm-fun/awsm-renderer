//! Single owner of which DOM slot hosts the one live WebGPU canvas.
//!
//! Each mode's viewport registers its slot via [`register_slot`]; one mode
//! watcher — started on the first registration — moves the canvas into the
//! active mode's slot on every switch. This replaces per-viewport
//! `mode.signal()` watchers: a viewport's `after_inserted` can fire repeatedly
//! as the DOM rebuilds, and spawning a fresh forever-living watcher each time
//! leaked tasks and raced reparents into stale, detached slots. Registration is
//! idempotent — the latest slot for a mode wins, so a rebuilt viewport simply
//! replaces its entry and the lone watcher keeps using the current one.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use crate::controller::{controller, EditorMode};
use crate::engine::context::{sync_canvas_size, with_canvas};
use crate::prelude::*;

thread_local! {
    static SLOTS: RefCell<HashMap<EditorMode, web_sys::Element>> = RefCell::new(HashMap::new());
    static WATCHING: Cell<bool> = const { Cell::new(false) };
}

/// Register `slot` as the canvas host for `mode`. Safe to call repeatedly (a DOM
/// rebuild replaces the entry). Reparents immediately when `mode` is the active
/// mode, and starts the single mode watcher on the first call.
pub fn register_slot(mode: EditorMode, slot: web_sys::Element) {
    SLOTS.with(|s| {
        s.borrow_mut().insert(mode, slot.clone());
    });
    if controller().mode.get() == mode {
        reparent_into(&slot);
    }
    if !WATCHING.with(|w| w.replace(true)) {
        spawn_local(async move {
            controller()
                .mode
                .signal()
                .for_each(|m| {
                    if let Some(slot) = SLOTS.with(|s| s.borrow().get(&m).cloned()) {
                        reparent_into(&slot);
                    }
                    async {}
                })
                .await;
        });
    }
}

/// Move the single live canvas into `slot`, surfacing a mount failure (otherwise
/// a blank viewport with no diagnostics), then size the surface to it.
fn reparent_into(slot: &web_sys::Element) {
    with_canvas(|c| {
        if let Err(err) = slot.append_child(c) {
            Modal::error(format!("Failed to mount viewport canvas: {err:?}"));
        }
    });
    sync_canvas_size();
}
