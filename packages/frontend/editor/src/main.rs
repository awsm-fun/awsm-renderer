//! awsm-editor — v2 blank-slate rebuild bootstrap.
//!
//! M0 only mounts the design-system stylesheet + an empty themed app shell so
//! the build can be verified in real Chrome on `:9085` before any panel is
//! written. The renderer/worker bootstrap (the multi-second cold-start window,
//! pipeline warm, gizmo load) returns in M3 once the `EditorController` +
//! scene renderer land.

mod app;

use awsm_web_shared::{logger, prelude::*, theme};
use dominator::stylesheet;

pub fn main() {
    logger::init_logger();
    Modal::init_panic_hook();
    theme::stylesheet::init();

    stylesheet!("html, body", {
        .style("width", "100%")
        .style("height", "100%")
    });
    // Disable stray text selection across editor chrome; inputs opt back in so
    // typing / copying still works normally.
    stylesheet!("body", {
        .style(["-moz-user-select", "user-select", "-webkit-user-select"], "none")
    });
    stylesheet!("input, textarea, [contenteditable='true']", {
        .style(["-moz-user-select", "user-select", "-webkit-user-select"], "text")
    });

    awsm_web_shared::util::window::remove_boot_loader();

    dominator::append_dom(&dominator::body(), app::render());
}
