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
    context::{create_context, renderer_handle, with_canvas},
    header::Header,
    sidebar::{SidebarLeft, SidebarRight},
};

pub fn main() {
    // Phase 4.3a / 4.4: register every `WorkerJob` the editor wants
    // available — runs on *both* main thread and pool workers (the
    // worker side re-runs this same wasm `main` during its
    // `wbg.default(wasm_module)` init). Registration is idempotent
    // and cheap; keeping it before the worker-bail below means the
    // dispatcher's thread-local registry has the right impls
    // populated regardless of which side we're on.
    awsm_renderer::workers::register_job::<awsm_renderer_gltf::worker_job::GltfParseJob>();

    // Phase 4.3a / 4.4: the scene-editor's wasm bundle is also
    // loaded inside the WorkerPool's pool workers (the inline-JS
    // shim re-imports this glue + runs `wbg.default(wasm_module)`).
    // The worker side runs `awsm_worker_entry()` explicitly and
    // doesn't want the editor's DOM-side bootstrap. Bail before any
    // `document` / `window`-touching setup if there's no Window.
    if web_sys::window().is_none() {
        // We're in a worker context — `awsm_worker_entry` is invoked
        // separately by the bootstrap JS and installs its dispatch
        // listener. Nothing else to do here.
        return;
    }

    // Boot-loader stays visible through the multi-second cold-start
    // window — `create_context` compiles ~14 pipelines and loads the
    // gizmo asset before the editor UI is ready. We update its label
    // through the phases below and remove it once `ctx_ready` flips.
    awsm_web_shared::util::window::set_boot_loader_message("Initializing renderer");
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
        .style(["-moz-user-select", "user-select", "-webkit-user-select"], "none")
    });
    stylesheet!("input, textarea, [contenteditable='true']", {
        .style(["-moz-user-select", "user-select", "-webkit-user-select"], "text")
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
                            // Renderer is built; surface the discrete
                            // phases that actually run before the
                            // editor's first interactive frame so a
                            // multi-hundred-ms wait isn't an opaque
                            // "Loading". The boot-loader label gets
                            // updated each step; `remove_boot_loader`
                            // fires once `ctx_ready` flips.
                            awsm_web_shared::util::window::set_boot_loader_message(
                                "Compiling shaders",
                            );
                            // The renderer's pipelines are already
                            // built at `AwsmRendererBuilder::build()`
                            // time (see `AwsmRenderer::prewarm_pipelines`
                            // doc for the catalogue); calling this is
                            // a no-op today but holds the phase label
                            // through the cold compile window and
                            // gives the dynamic-materials sprint a
                            // clean hook to extend.
                            {
                                let handle = renderer_handle();
                                let mut r = handle.lock().await;
                                if let Err(err) = r.prewarm_pipelines().await {
                                    tracing::warn!("prewarm_pipelines: {err}");
                                }
                            }
                            awsm_web_shared::util::window::set_boot_loader_message(
                                "Loading editor assets",
                            );
                            renderer_bridge::init();
                            ctx_ready.set(true);
                            awsm_web_shared::util::window::remove_boot_loader();
                        }
                        Err(err) => {
                            awsm_web_shared::util::window::remove_boot_loader();
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
