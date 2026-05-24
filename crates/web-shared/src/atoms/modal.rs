use std::fmt::Display;

use crate::{atoms::dynamic_svg::close_x::CloseX, prelude::*};

thread_local! {
    static MODAL:ModalInstance = ModalInstance::new()
}

struct ModalInstance {
    content: Mutable<Option<Box<dyn Fn() -> Dom>>>,
    /// When true, clicking the overlay or X button will not close the modal.
    locked: Mutable<bool>,
    /// Width preset for the container — `Standard` is the default
    /// for simple forms / errors; `Wide` is for content with
    /// side-by-side panels (e.g. the auth modal's email + SSO
    /// columns). Reset to `Standard` on close so the next caller
    /// doesn't inherit a stale width.
    size: Mutable<ModalSize>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ModalSize {
    Standard,
    Wide,
}

impl ModalSize {
    fn css_width(self) -> &'static str {
        match self {
            ModalSize::Standard => "min(42rem, calc(100vw - 2rem))",
            ModalSize::Wide => "min(56rem, calc(100vw - 2rem))",
        }
    }
}

/// Global modal namespace. All state lives in the thread-local
/// `MODAL`; this is a zero-sized handle so `Modal::open` /
/// `Modal::close` / etc. read like methods.
pub struct Modal;

impl Modal {
    pub fn error(msg: impl Display + 'static) {
        tracing::error!("{}", msg);

        Self::open(move || {
            html!("div", {
                .child(html!("div", {
                    .style("padding", "0.1rem 1.25rem 0.75rem")
                    .style("border-bottom", &format!("1px solid {}", ColorBorder::ErrorSubtle.value()))
                    .style("margin-bottom", "1rem")
                    .child(html!("h2", {
                        .style("margin", "0")
                        .style("font-size", "1.1rem")
                        .style("color", ColorText::Error.value())
                        .text("Error")
                    }))
                }))
                .child(html!("p", {
                    .style("margin", "0 1.25rem 0.25rem")
                    .style("color", ColorText::ErrorMuted.value())
                    .style("font-size", "0.95rem")
                    .style("line-height", "1.5")
                    .text(&format!("{}", msg))
                }))
            })
        });
    }

    pub fn iframe(url: &str) {
        let url = url.to_string();

        Self::open(move || {
            html!("iframe", {
                .attr("src", &url)
                .style("width", "calc(100% + 40px)")
                .style("height", "calc(100vh - 6rem)")
                .style("margin", "-20px")
                .style("border", "0")
                .style("display", "block")
                .style("background", "white")
                .style("border-radius", "0 0 0.95rem 0.95rem")
            })
        });
    }

    pub fn open(content: impl Fn() -> Dom + 'static) {
        MODAL.with(|modal| {
            modal.open(ModalSize::Standard, content);
        });
    }

    /// Open the modal with a specific size preset. Use [`ModalSize::Wide`]
    /// for content with side-by-side panels (e.g. the auth modal). The
    /// width resets to `Standard` on close.
    pub fn open_sized(size: ModalSize, content: impl Fn() -> Dom + 'static) {
        MODAL.with(|modal| {
            modal.open(size, content);
        });
    }

    pub fn close() {
        MODAL.with(|modal| {
            modal.close();
        });
    }

    /// Prevent the modal from being dismissed via overlay click or X button.
    /// Call `unlock()` to re-allow dismissal.
    pub fn lock() {
        MODAL.with(|modal| {
            modal.locked.set(true);
        });
    }

    /// Re-allow dismissal via overlay click or X button.
    pub fn unlock() {
        MODAL.with(|modal| {
            modal.locked.set(false);
        });
    }

    pub fn render() -> Dom {
        MODAL.with(|modal| modal.render())
    }

    /// Registers a global panic hook that displays any Rust panic as a modal error.
    ///
    /// Call once during app initialisation (before rendering). The hook closure
    /// captures nothing, so it satisfies the `Send + Sync` bounds required by
    /// `std::panic::set_hook` even in a WASM context.
    pub fn init_panic_hook() {
        std::panic::set_hook(Box::new(|info| {
            Modal::error(info.to_string());
        }));
    }
}

impl ModalInstance {
    pub fn new() -> Self {
        Self {
            content: Mutable::new(None),
            locked: Mutable::new(false),
            size: Mutable::new(ModalSize::Standard),
        }
    }

    pub fn open(&self, size: ModalSize, content: impl Fn() -> Dom + 'static) {
        self.locked.set(false);
        self.size.set(size);
        self.content.set(Some(Box::new(content)));
    }

    pub fn close(&self) {
        self.locked.set(false);
        self.size.set(ModalSize::Standard);
        self.content.set(None);
    }

    pub fn render(&self) -> Dom {
        static BG: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("position", "fixed")
                .style("top", "0")
                .style("left", "0")
                .style("width", "100vw")
                .style("height", "100vh")
                .style("background", ColorBackground::ModalOverlay.value())
                .style("backdrop-filter", "blur(4px)")
                .style("z-index", Zindex::Modal.value())
            }
        });

        // `width` is applied dynamically via `style_signal` below so
        // callers can pick a size preset; everything else is static.
        static CONTAINER: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("position", "fixed")
                .style("top", "50%")
                .style("left", "50%")
                .style("transform", "translate(-50%, -50%)")
                .style("background-color", ColorBackground::ModalContent.value())
                .style("color", ColorRaw::Whiteish.value())
                .style("border-width", "1px")
                .style("border-style", "solid")
                .style("border-color", ColorBorder::ModalContainer.value())
                .style("border-radius", "0.95rem")
                .style("max-height", "calc(100vh - 2rem)")
                .style("overflow-y", "auto")
                .style("box-shadow", "0 34px 90px rgba(1, 7, 15, 0.46)")
                .style("z-index", "1001")
                // Modal text is selectable even when the host app sets
                // `body { user-select: none }`. Error messages, ids,
                // and other surfaced text often need to be copied
                // (e.g. into bug reports). Buttons inside still get
                // the default no-select treatment from their own
                // styling.
                .style(["-moz-user-select", "user-select", "-webkit-user-select"], "text")
            }
        });

        static CLOSE_BUTTON: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("position", "absolute")
                .style("top", ".5rem")
                .style("right", ".5rem")
                .style("z-index", "1002")
            }
        });

        let m_content = self.content.clone();
        let m_locked = self.locked.clone();
        let m_size = self.size.clone();

        html!("div", {
            .child_signal(m_content.signal_ref(clone!(m_content, m_locked, m_size => move |content| {
                content.as_ref().map(|content| {
                    html!("div", {
                        .child(html!("div", {
                            .class(&*BG)
                            .event(clone!(m_content, m_locked => move |_: events::Click| {
                                if !m_locked.get() {
                                    m_content.set(None);
                                }
                            }))
                        }))
                        .child(html!("div", {
                            .class(&*CONTAINER)
                            .style_signal("width", m_size.signal().map(|s| s.css_width()))
                            .child(html!("div", {
                                .class(&*CLOSE_BUTTON)
                                .child(CloseX::render(ButtonSize::Lg, clone!(m_content, m_locked => move || {
                                    if !m_locked.get() {
                                        m_content.set(None);
                                    }
                                })))
                            }))
                            .child(html!("div", {
                                .child(html!("div", {
                                    .style("padding", "20px")
                                    .child(content())
                                }))
                            }))
                        }))
                    })
                })
            })))
        })
    }
}
