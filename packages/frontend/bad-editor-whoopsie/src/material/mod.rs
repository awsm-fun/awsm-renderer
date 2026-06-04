//! Material mode — the custom-WGSL material workspace, folded in from the
//! former standalone `material-editor` crate.
//!
//! It shares the unified editor's single scene `AwsmRenderer`: edits are
//! debounced and registered via [`host::SceneRendererSink`], which makes the
//! material available to assign onto scene meshes and surfaces compile errors
//! into the Errors pane. (A second renderer for a dedicated preview ball is not
//! viable — see `host.rs`.) The workspace DOM is built once and kept mounted
//! (display-toggled by `main.rs`); the recompile loop spawns lazily the first
//! time the user enters Material mode.

pub mod app;
pub mod host;
pub mod panes;
pub mod recompile;
pub mod state;

use std::cell::OnceCell;
use std::rc::Rc;

use crate::prelude::*;
use crate::state::{app_state, EditorMode};
use futures_signals::signal::Mutable;
use host::SceneRendererSink;
use recompile::RecompileSink;
use state::EditState;

thread_local! {
    /// The single Material-mode edit state. Created on first workspace render
    /// so the panes can bind to it; persists for the app's life.
    static EDIT_STATE: OnceCell<EditState> = const { OnceCell::new() };
    /// Set once the recompile loop has been spawned.
    static RECOMPILE_SPAWNED: OnceCell<()> = const { OnceCell::new() };
}

fn ensure_state() -> EditState {
    EDIT_STATE.with(|cell| {
        cell.get_or_init(|| {
            let state = EditState::new_scanline();
            if let Some(folder) = panes::deep_link_banner::read_folder_query_param() {
                state.deep_link_folder.set(Some(folder));
            }
            state
        })
        .clone()
    })
}

/// Spawn the debounced recompile loop (registering into the shared scene
/// renderer). Guarded so it runs exactly once, on the first entry into
/// Material mode — by which point the scene renderer is already booted.
fn ensure_recompile(state: &EditState) {
    let first = RECOMPILE_SPAWNED.with(|cell| cell.set(()).is_ok());
    if !first {
        return;
    }

    let sink: Rc<Mutable<Box<dyn RecompileSink>>> =
        Rc::new(Mutable::new(Box::new(SceneRendererSink::new())));
    recompile::spawn(state.clone(), sink);

    // Kick the WGSL signal so the debounced loop registers the initial
    // scanline material now (Mutable::set re-fires even on an equal value).
    let current = state.wgsl_source.lock_ref().clone();
    state.wgsl_source.set(current);
}

/// The Material-mode workspace DOM. Built once and kept mounted; the recompile
/// loop spawns lazily the first time `mode` becomes `Material`.
pub fn render_workspace() -> Dom {
    let state = ensure_state();
    html!("div", {
        .style("display", "flex")
        .style("flex", "1 1 0")
        .style("min-height", "0")
        .style("min-width", "0")
        .future(clone!(state => async move {
            app_state().mode.signal().for_each(move |m| {
                if m == EditorMode::Material {
                    ensure_recompile(&state);
                }
                async {}
            }).await;
        }))
        .child(app::root_with_state(state.clone()))
    })
}
