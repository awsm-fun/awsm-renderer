//! Lightweight global **activity indicator**: a list of in-progress background
//! operations — model import / GPU upload, material + render-pipeline
//! compilation, etc. Issue #7 ("no indication of when it's creating new
//! materials/pipelines"): the editor does real async work on import and on
//! material registration, and the user needs to see it.
//!
//! Each [`begin_activity`] pushes a label and returns an RAII [`Activity`]
//! guard that removes it on drop — so the indicator always reflects what's
//! actually running, even if a task errors out early. The app shell renders the
//! live list as a floating pill (see `app.rs::activity_indicator`).

use std::cell::Cell;

use awsm_web_shared::prelude::Mutable;
use wasm_bindgen_futures::spawn_local;

/// How long an activity lingers after its work finishes. A fast op (cached
/// import, quick compile) otherwise pushes then pops within a single executor
/// burst — dominator never gets a turn to render the pill, so it never shows.
/// Lingering both guarantees the pill renders and reads as a deliberate
/// "finished" beat rather than a sub-frame flash.
const LINGER_MS: u32 = 450;

thread_local! {
    static ACTIVITIES: Mutable<Vec<(u64, String)>> = Mutable::new(Vec::new());
    static NEXT_ID: Cell<u64> = const { Cell::new(0) };
}

/// The shared list of `(id, label)` for in-progress activities. The app shell
/// observes this to render the indicator.
pub fn activities() -> Mutable<Vec<(u64, String)>> {
    ACTIVITIES.with(|a| a.clone())
}

/// A running activity. Dropping it removes the label from the indicator — so
/// hold the guard for the lifetime of the work (e.g. `let _a = begin_activity(…)`
/// across an `.await`).
#[must_use = "the activity is shown until this guard is dropped"]
pub struct Activity {
    id: u64,
}

/// Start showing `label` in the activity indicator until the returned guard drops.
pub fn begin_activity(label: impl Into<String>) -> Activity {
    let id = NEXT_ID.with(|n| {
        let id = n.get();
        n.set(id.wrapping_add(1));
        id
    });
    ACTIVITIES.with(|a| a.lock_mut().push((id, label.into())));
    Activity { id }
}

impl Drop for Activity {
    fn drop(&mut self) {
        let id = self.id;
        // Remove after a short linger (see LINGER_MS) rather than immediately,
        // so the pill is actually rendered for fast operations.
        spawn_local(async move {
            gloo_timers::future::TimeoutFuture::new(LINGER_MS).await;
            ACTIVITIES.with(|a| a.lock_mut().retain(|(i, _)| *i != id));
        });
    }
}
