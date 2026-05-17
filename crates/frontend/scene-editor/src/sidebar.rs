//! Resizable left + right sidebars. The actual content of each sidebar
//! is supplied by the caller as a `Fn() -> Dom`, so this module knows
//! nothing about the tree or properties panel — it only owns the chrome
//! (panel + drag handle) and the per-sidebar width state.

use crate::prelude::*;

const DEFAULT_WIDTH_LEFT: f64 = 260.0;
const DEFAULT_WIDTH_RIGHT: f64 = 340.0;
const MIN_WIDTH: f64 = 180.0;
/// Keep at least this much horizontal room for the central canvas when
/// dragging a resizer outward, otherwise a sidebar could swallow the
/// viewport entirely.
const MAX_WIDTH_MARGIN: f64 = 240.0;

#[derive(Clone, Copy)]
enum Side {
    Left,
    Right,
}

#[derive(Clone, Copy)]
struct DragState {
    start_x: f64,
    start_width: f64,
}

struct Inner {
    side: Side,
    width: Mutable<f64>,
    drag_state: Mutable<Option<DragState>>,
    content: Box<dyn Fn() -> Dom>,
}

impl Inner {
    fn new(side: Side, content: Box<dyn Fn() -> Dom>) -> Self {
        let default_width = match side {
            Side::Left => DEFAULT_WIDTH_LEFT,
            Side::Right => DEFAULT_WIDTH_RIGHT,
        };
        Self {
            side,
            width: Mutable::new(default_width),
            drag_state: Mutable::new(None),
            content,
        }
    }

    fn render(&self) -> Dom {
        static CONTAINER: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("flex", "0 0 auto")
                .style("display", "flex")
                .style("flex-direction", "row")
                .style("align-items", "stretch")
                .style("min-height", "0")
            }
        });

        html!("div", {
            .class(&*CONTAINER)
            .apply(|builder| match self.side {
                Side::Left => builder
                    .child(self.render_panel())
                    .child(self.render_resizer()),
                Side::Right => builder
                    .child(self.render_resizer())
                    .child(self.render_panel()),
            })
        })
    }

    fn render_panel(&self) -> Dom {
        static PANEL: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("flex", "0 0 auto")
                .style("min-width", "0")
                .style("height", "100%")
                .style("box-sizing", "border-box")
                .style("overflow", "auto")
                .style("overscroll-behavior", "contain")
                .style("padding", "0.75rem")
            }
        });

        html!("div", {
            .class(&*PANEL)
            .class([ColorBackground::Sidebar.class(), ColorText::SidebarHeader.class()])
            .style_signal("width", self.width.signal().map(|width| format!("{}px", width)))
            .child((self.content)())
        })
    }

    fn render_resizer(&self) -> Dom {
        static RESIZER: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("flex", "0 0 6px")
                .style("height", "100%")
                .style("cursor", "col-resize")
                .style("touch-action", "none")
                .style("position", "relative")
                .style("background", ColorRaw::GreyAlt1.value())
                .style("transition", "background 120ms ease")
                .pseudo!(":hover", {
                    .style("background", ColorRaw::MidGrey.value())
                })
            }
        });

        static HANDLE: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("position", "absolute")
                .style("top", "50%")
                .style("left", "50%")
                .style("width", "2px")
                .style("height", "48px")
                .style("transform", "translate(-50%, -50%)")
                .style("background", ColorRaw::MidGrey.value())
                .style("border-radius", "2px")
                .style("opacity", "0.9")
            }
        });

        let drag_state = self.drag_state.clone();
        let width = self.width.clone();
        let side = self.side;

        html!("div", {
            .class([&*RESIZER, &*USER_SELECT_NONE])
            .with_node!(divider => {
                .event(clone!(drag_state, width => move |event: events::PointerDown| {
                    let _ = divider.set_pointer_capture(event.pointer_id());
                    drag_state.set(Some(DragState {
                        start_x: event.x(),
                        start_width: width.get(),
                    }));
                }))
                .event(clone!(drag_state, width => move |event: events::PointerMove| {
                    let Some(state) = drag_state.get() else {
                        return;
                    };
                    let raw_delta = event.x() - state.start_x;
                    let delta_x = match side {
                        Side::Left => raw_delta,
                        Side::Right => -raw_delta,
                    };
                    let window_width = web_sys::window()
                        .and_then(|window| window.inner_width().ok())
                        .and_then(|value| value.as_f64())
                        .unwrap_or(1200.0);
                    let max_width = (window_width - MAX_WIDTH_MARGIN).max(MIN_WIDTH);
                    let next_width = (state.start_width + delta_x).clamp(MIN_WIDTH, max_width);
                    width.set(next_width);
                }))
                .event(clone!(drag_state => move |_: events::PointerUp| {
                    drag_state.set(None);
                }))
                .event(clone!(drag_state => move |_: events::PointerCancel| {
                    drag_state.set(None);
                }))
            })
            .child(html!("div", {
                .class(&*HANDLE)
            }))
        })
    }
}

pub struct SidebarLeft(Inner);

impl SidebarLeft {
    pub fn new(content: impl Fn() -> Dom + 'static) -> Self {
        Self(Inner::new(Side::Left, Box::new(content)))
    }

    pub fn render(&self) -> Dom {
        self.0.render()
    }
}

pub struct SidebarRight(Inner);

impl SidebarRight {
    pub fn new(content: impl Fn() -> Dom + 'static) -> Self {
        Self(Inner::new(Side::Right, Box::new(content)))
    }

    pub fn render(&self) -> Dom {
        self.0.render()
    }
}
