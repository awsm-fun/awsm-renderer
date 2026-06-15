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

/// Reserved id for the live pipeline-compile entry (driven each frame from the
/// renderer's `compile_progress()`, not by an RAII guard). Distinct from the
/// monotonic ids handed out by `begin_activity` so it can be upserted/removed
/// in place without colliding.
const COMPILE_ID: u64 = u64::MAX;

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

/// Reflect the renderer's live pipeline-compile state into the indicator.
/// Called every frame from the render loop with the scheduler's authoritative
/// counts: `materials` still pending and the granular `subcompiles` in flight.
/// Upserts a single "Compiling N render pipelines…" pill while work is in
/// flight and removes it the moment the scheduler goes idle — so first-start
/// editor-pipeline warmup and post-import shader/pipeline compiles both show,
/// without any caller having to hold a guard across the async GPU work.
pub fn set_compile_progress(materials: usize, subcompiles: u32) {
    let busy = materials > 0 || subcompiles > 0;
    ACTIVITIES.with(|a| {
        let mut list = a.lock_mut();
        let pos = list.iter().position(|(i, _)| *i == COMPILE_ID);
        if busy {
            // Prefer the granular sub-pipeline count when present (it's the real
            // "how much is left" number); fall back to the material count.
            let label = if subcompiles > 0 {
                format!(
                    "Compiling {subcompiles} render pipeline{}…",
                    if subcompiles == 1 { "" } else { "s" }
                )
            } else {
                format!(
                    "Compiling {materials} material{}…",
                    if materials == 1 { "" } else { "s" }
                )
            };
            match pos {
                Some(p) => {
                    if list[p].1 != label {
                        list[p].1 = label;
                    }
                }
                // Front-insert so the concrete "Compiling N…" count is the
                // primary visible label, rather than hiding behind a generic
                // "uploading to GPU…" guard that's still lingering.
                None => list.insert(0, (COMPILE_ID, label)),
            }
        } else if let Some(p) = pos {
            list.remove(p);
        }
    });
}

/// Reserved id for the live scene-load phase entry (driven by a loader's
/// `LoadPhase` callback, not an RAII guard). Distinct from `COMPILE_ID` and the
/// monotonic `begin_activity` ids so it upserts/clears in place.
const LOAD_PHASE_ID: u64 = u64::MAX - 1;

/// Upsert (or clear, with `None`) the scene-load phase pill — "Building
/// materials…" / "Uploading meshes…" etc. Driven by `populate_awsm_scene`'s
/// `LoadPhase` callback (via `LoadPhase::label()`). Because `ACTIVITIES` is a
/// reactive `Mutable` and the loader's awaits yield to the event loop, the pill
/// updates live even while the loader holds the renderer lock.
pub fn set_load_phase(label: Option<String>) {
    ACTIVITIES.with(|a| {
        let mut list = a.lock_mut();
        let pos = list.iter().position(|(i, _)| *i == LOAD_PHASE_ID);
        match (label, pos) {
            (Some(label), Some(p)) => {
                if list[p].1 != label {
                    list[p].1 = label;
                }
            }
            (Some(label), None) => list.insert(0, (LOAD_PHASE_ID, label)),
            (None, Some(p)) => {
                list.remove(p);
            }
            (None, None) => {}
        }
    });
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
