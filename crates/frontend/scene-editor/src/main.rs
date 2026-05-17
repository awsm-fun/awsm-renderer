mod actions;
mod canvas;
mod collider_wireframe;
mod config;
mod content_hash;
mod context;
mod error;
mod fs;
mod header;
mod keys;
mod loading_modal;
mod prelude;
mod properties;
mod renderer_bridge;
mod scene;
mod sidebar;
mod state;
mod tree;

use awsm_web_shared::{logger, prelude::*, theme};
use dominator::stylesheet;
use wasm_bindgen_futures::spawn_local;

use crate::{
    canvas::render_canvas,
    context::{create_context, with_canvas},
    header::Header,
    sidebar::{SidebarLeft, SidebarRight},
};

pub fn main() {
    awsm_web_shared::util::window::remove_boot_loader();
    logger::init_logger();
    Modal::init_panic_hook();
    theme::stylesheet::init();
    state::init();
    keys::install();

    stylesheet!("html, body", {
        .style("width", "100%")
        .style("height", "100%")
    });
    // Disable text selection across the editor so stray drags inside the
    // tree / header / properties panel don't leave accidental highlights.
    // Inputs + textareas + any explicit contenteditable surface opt back
    // in below so typing / copying still works normally.
    stylesheet!("body", {
        .style("user-select", "none")
        .style("-webkit-user-select", "none")
    });
    stylesheet!("input, textarea, [contenteditable='true']", {
        .style("user-select", "text")
        .style("-webkit-user-select", "text")
    });

    let ctx_ready = Mutable::new(false);

    dominator::append_dom(
        &dominator::body(),
        dominator::html!("div", {
            .style("width", "100%")
            .style("height", "100%")
            // Suppress the browser's native right-click menu everywhere in
            // the editor. Individual rows / surfaces can still listen for
            // `events::ContextMenu` and open their own popups.
            // `preventable: true` is required — dominator's default
            // `EventOptions` attach listeners passively, which makes
            // `prevent_default()` a no-op.
            .event_with_options(&dominator::EventOptions::preventable(), |event: events::ContextMenu| {
                event.prevent_default();
            })
            .child(Modal::render())
            .child(Toast::render())
            .child(render_canvas(clone!(ctx_ready => move |canvas| {
                spawn_local(async move {
                    match create_context(canvas).await {
                        Ok(_) => {
                            renderer_bridge::init();
                            ctx_ready.set(true);
                        }
                        Err(err) => {
                            Modal::error(format!("Failed to initialize AppContext: {err}"));
                        }
                    }
                });
            })))
            .child_signal(ctx_ready.signal().map(|ctx_ready| {
                if ctx_ready {
                    Some(render_initialized())
                } else {
                    None
                }
            }))
        }),
    );
}

fn render_initialized() -> Dom {
    static PAGE_LAYOUT_CLASS: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("display", "flex")
            .style("flex-direction", "column")
            .style("width", "100%")
            .style("height", "100%")
        }
    });

    static BODY_ROW_CLASS: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("flex", "1 1 0")
            .style("display", "flex")
            .style("flex-direction", "row")
            .style("min-height", "0")
            .style("min-width", "0")
            .style("overflow", "hidden")
        }
    });

    static CANVAS_SLOT_CLASS: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("flex", "1 1 0")
            .style("min-height", "0")
            .style("min-width", "0")
            .style("overflow", "hidden")
            .style("position", "relative")
        }
    });

    dominator::html!("div", {
        .class(&*PAGE_LAYOUT_CLASS)
        .child(Header::new().render())
        .child(dominator::html!("div", {
            .class(&*BODY_ROW_CLASS)
            .child(SidebarLeft::new(tree::render).render())
            .child(dominator::html!("div", {
                .class(&*CANVAS_SLOT_CLASS)
                .after_inserted(|elem| {
                    with_canvas(|canvas| {
                        if let Err(err) = elem.append_child(canvas) {
                            Modal::error(format!("Failed to append canvas to main layout: {err:?}"));
                        } else {
                            tracing::info!("Reparented canvas!");
                        }
                    });
                })
            }))
            .child(SidebarRight::new(properties::render).render())
        }))
    })
}
