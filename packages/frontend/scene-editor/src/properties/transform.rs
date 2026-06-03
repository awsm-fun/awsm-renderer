//! TRS editor for the currently-selected node. Translation and scale are
//! shown as three number inputs each. Rotation can be toggled between
//! Euler XYZ degrees (default) and quaternion XYZW.

use crate::prelude::*;
use crate::scene::{Node, NodeKind, SceneSnapshot};
use crate::state::{app_state, RotationDisplay};
use glam::{EulerRot, Quat};
use web_sys::HtmlInputElement;

pub fn render(node: Arc<Node>) -> Dom {
    static SECTION: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("display", "flex")
            .style("flex-direction", "column")
            .style("gap", "0.5rem")
            .style("padding", "0.5rem 0")
        }
    });

    html!("div", {
        .class(&*SECTION)
        .child(render_header())
        .child(render_vec3_row("Translation", node.clone(), Axis::T))
        .child(render_rotation_row(node.clone()))
        // Scale row is hidden for Collider AND Camera nodes. For
        // Colliders, the runtime physics path
        // (`ColliderSpec::from_node`) reads only translation +
        // rotation + shape's intrinsic dimensions; node scale is
        // silently ignored. For Cameras, the projection matrix is
        // independent of node scale — non-identity scale would just
        // skew the frustum wireframe relative to what the runtime
        // camera actually sees. Edit-time prevention > Build-time
        // error > silent drift.
        //
        // Dedupe on the boolean so dragging translation / rotation
        // doesn't tear the Scale row down and rebuild it on every
        // value change.
        .child_signal(node.kind.signal_ref(|k| {
            matches!(k, NodeKind::Collider(_) | NodeKind::Camera(_))
        }).dedupe().map(clone!(node => move |hide_scale| {
            if hide_scale {
                None
            } else {
                Some(render_vec3_row("Scale", node.clone(), Axis::S))
            }
        })))
    })
}

fn render_header() -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("align-items", "center")
        .style("justify-content", "space-between")
        .style("font-size", "0.75rem")
        .style("font-weight", "600")
        .style("text-transform", "uppercase")
        .style("letter-spacing", "0.05em")
        .style("color", ColorText::Byline.value())
        .child(html!("span", { .text("Transform") }))
    })
}

#[derive(Clone, Copy)]
enum Axis {
    T,
    S,
}

fn render_vec3_row(label: &'static str, node: Arc<Node>, axis: Axis) -> Dom {
    html!("div", {
        .style("display", "grid")
        .style("grid-template-columns", "5rem 1fr 1fr 1fr")
        .style("gap", "0.35rem")
        .style("align-items", "center")
        .child(html!("span", {
            .style("font-size", "0.8rem")
            .style("color", ColorText::Byline.value())
            .text(label)
        }))
        .child(render_axis_input(node.clone(), axis, 0))
        .child(render_axis_input(node.clone(), axis, 1))
        .child(render_axis_input(node, axis, 2))
    })
}

fn render_axis_input(node: Arc<Node>, axis: Axis, component: usize) -> Dom {
    let transform = node.transform.clone();

    let value_signal = transform.signal().map(move |trs| match axis {
        Axis::T => trs.translation[component],
        Axis::S => trs.scale[component],
    });

    labeled_axis_input(component, value_signal, move |new_value| {
        let mut trs = transform.get();
        match axis {
            Axis::T => trs.translation[component] = new_value,
            Axis::S => trs.scale[component] = new_value,
        }
        transform.set(trs);
    })
}

fn render_rotation_row(node: Arc<Node>) -> Dom {
    let state = app_state();
    let display = state.rotation_display.clone();

    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.35rem")
        .child(html!("div", {
            .style("display", "flex")
            .style("align-items", "center")
            .style("justify-content", "space-between")
            .child(html!("span", {
                .style("font-size", "0.8rem")
                .style("color", ColorText::Byline.value())
                .text("Rotation")
            }))
            .child(render_rotation_mode_toggle(display.clone()))
        }))
        .child_signal(display.signal().map(clone!(node => move |mode| {
            Some(match mode {
                RotationDisplay::EulerDegrees => render_euler_inputs(node.clone()),
                RotationDisplay::Quaternion => render_quat_inputs(node.clone()),
            })
        })))
    })
}

fn render_rotation_mode_toggle(display: Mutable<RotationDisplay>) -> Dom {
    static SWITCH: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("display", "inline-flex")
            .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
            .style("border-radius", "0.3rem")
            .style("overflow", "hidden")
            .style("font-size", "0.7rem")
        }
    });

    let render_button = move |mode: RotationDisplay, label: &'static str| -> Dom {
        let display = display.clone();
        html!("button", {
            .style("border", "0")
            .style("padding", "0.2rem 0.5rem")
            .style("cursor", "pointer")
            .style("font-weight", "600")
            .style_signal("background-color", display.signal().map(move |d| {
                if d == mode {
                    ColorBackground::ButtonPrimary.value()
                } else {
                    "transparent"
                }
            }))
            .style_signal("color", display.signal().map(move |d| {
                if d == mode {
                    ColorText::ButtonPrimary.value()
                } else {
                    ColorText::SidebarHeader.value()
                }
            }))
            .text(label)
            .event(clone!(display => move |_: events::Click| {
                display.set(mode);
            }))
        })
    };

    html!("div", {
        .class(&*SWITCH)
        .child(render_button(RotationDisplay::EulerDegrees, "Euler"))
        .child(render_button(RotationDisplay::Quaternion, "Quat"))
    })
}

fn render_euler_inputs(node: Arc<Node>) -> Dom {
    html!("div", {
        .style("display", "grid")
        .style("grid-template-columns", "repeat(3, 1fr)")
        .style("gap", "0.35rem")
        .child(euler_input(node.clone(), 0))
        .child(euler_input(node.clone(), 1))
        .child(euler_input(node, 2))
    })
}

fn euler_input(node: Arc<Node>, component: usize) -> Dom {
    let transform = node.transform.clone();

    let value_signal = transform.signal().map(move |trs| {
        let euler = quat_to_euler_degrees(Quat::from_array(trs.rotation));
        euler[component]
    });

    labeled_axis_input(component, value_signal, move |new_deg| {
        let mut trs = transform.get();
        let mut euler = quat_to_euler_degrees(Quat::from_array(trs.rotation));
        euler[component] = new_deg;
        let quat = euler_degrees_to_quat(euler);
        trs.rotation = quat.to_array();
        transform.set(trs);
    })
}

fn render_quat_inputs(node: Arc<Node>) -> Dom {
    html!("div", {
        .style("display", "grid")
        .style("grid-template-columns", "repeat(4, 1fr)")
        .style("gap", "0.35rem")
        .child(quat_input(node.clone(), 0))
        .child(quat_input(node.clone(), 1))
        .child(quat_input(node.clone(), 2))
        .child(quat_input(node, 3))
    })
}

fn quat_input(node: Arc<Node>, component: usize) -> Dom {
    let transform = node.transform.clone();

    let value_signal = transform.signal().map(move |trs| trs.rotation[component]);

    labeled_axis_input(component, value_signal, move |new_value| {
        let mut trs = transform.get();
        trs.rotation[component] = new_value;
        // Renormalize so the stored quat stays a rotation.
        let q = Quat::from_array(trs.rotation).normalize();
        trs.rotation = q.to_array();
        transform.set(trs);
    })
}

/// Small colored X/Y/Z/W label followed by a `number_input`. Used by
/// every vector-valued field in the inspector so axes are unambiguous.
pub(super) fn labeled_axis_input(
    component: usize,
    value_signal: impl Signal<Item = f32> + 'static,
    on_commit: impl FnMut(f32) + 'static,
) -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("align-items", "stretch")
        .style("gap", "0.25rem")
        .style("min-width", "0")
        .child(html!("span", {
            .style("flex", "0 0 auto")
            .style("width", "1rem")
            .style("display", "inline-flex")
            .style("align-items", "center")
            .style("justify-content", "center")
            .style("color", axis_color(component))
            .style("font-weight", "700")
            .style("font-family", "monospace")
            .style("font-size", "0.75rem")
            .text(axis_label(component))
        }))
        .child(html!("div", {
            .style("flex", "1 1 0")
            .style("min-width", "0")
            .child(number_input(value_signal, on_commit))
        }))
    })
}

fn axis_label(component: usize) -> &'static str {
    match component {
        0 => "X",
        1 => "Y",
        2 => "Z",
        3 => "W",
        _ => "?",
    }
}

fn axis_color(component: usize) -> &'static str {
    match component {
        0 => ColorRaw::Red.value(),
        1 => ColorRaw::Green.value(),
        2 => ColorRaw::Accent.value(),
        _ => ColorRaw::MidGrey.value(),
    }
}

/// Vertical drag threshold before we stop treating a pointerdown as a
/// potential click and start actually scrubbing the value.
const DRAG_THRESHOLD_PX: f64 = 3.0;

/// Pixels → value ratio for vertical drag. Holding Shift divides by 10
/// (fine tuning), Ctrl/Cmd multiplies by 10 (coarse).
const DRAG_SENSITIVITY: f32 = 0.1;

struct NumberDrag {
    start_y: f64,
    start_value: f32,
    pointer_id: i32,
    dragged: bool,
    /// Scene state captured at drag start. One history entry spans the
    /// whole drag — we apply live during pointermove, then commit this
    /// single snapshot on pointerup so undo restores the pre-drag state.
    pre_drag: SceneSnapshot,
}

type CommitFn = Arc<Mutex<Box<dyn FnMut(f32)>>>;

pub fn number_input(
    value_signal: impl Signal<Item = f32> + 'static,
    on_commit: impl FnMut(f32) + 'static,
) -> Dom {
    static INPUT: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("width", "100%")
            .style("padding", "0.25rem 0.35rem")
            .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
            .style("border-radius", "0.25rem")
            .style("background-color", ColorRaw::Darkest.value())
            .style("color", ColorText::SidebarHeader.value())
            .style("font-family", "monospace")
            .style("font-size", "0.8rem")
            // Hint at drag-to-change when not focused. Once focused (see
            // the `:focus` rule below) we flip to `text` so the caret
            // behaves normally for typing.
            .style("cursor", "ns-resize")
            .pseudo!(":focus", {
                .style("cursor", "text")
            })
        }
    });

    let editing = Mutable::new(false);
    let drag: Arc<Mutex<Option<NumberDrag>>> = Arc::new(Mutex::new(None));
    // Pre-edit scene snapshot, captured at FocusIn so the whole edit
    // session (typing + spinner clicks + focused scroll-wheel) collapses
    // into one undoable history entry on FocusOut. Paired with a `dirty`
    // flag so we don't push no-op snapshots when the user focuses + tabs
    // away without changing anything.
    #[allow(clippy::arc_with_non_send_sync)]
    let pre_edit: Arc<Mutex<Option<SceneSnapshot>>> = Arc::new(Mutex::new(None));
    #[allow(clippy::arc_with_non_send_sync)]
    let dirty: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    #[allow(clippy::arc_with_non_send_sync)]
    let on_commit: CommitFn = Arc::new(Mutex::new(Box::new(on_commit)));

    html!("input" => HtmlInputElement, {
        .class(&*INPUT)
        .attr("type", "number")
        .attr("step", "0.01")
        .with_node!(input => {
            .future(clone!(editing, input => {
                value_signal.for_each(move |v| {
                    if !editing.get() {
                        input.set_value(&format_number(v));
                    }
                    async {}
                })
            }))
            .event(clone!(editing, pre_edit, dirty => move |_: events::FocusIn| {
                editing.set_neq(true);
                *pre_edit.lock().unwrap() = Some(app_state().snapshot_scene());
                *dirty.lock().unwrap() = false;
            }))
            // The native `input` event fires for: keystrokes, clicks on
            // the `<input type="number">` spinner buttons, and (while
            // focused) wheel-scroll. Push every change into the scene
            // live so the renderer reacts immediately — without this the
            // value visibly updates in the box but the scene stays put
            // until the user blurs the field, which feels frozen. The
            // history snapshot is captured at FocusIn, so this stays a
            // single undoable step.
            //
            // Programmatic `input.set_value(...)` (used by the drag
            // scrubber + the value-signal observer above) does NOT fire
            // `input`, so this handler doesn't fight either of those.
            .event(clone!(input, on_commit, dirty => move |_: events::Input| {
                if let Ok(parsed) = input.value().parse::<f32>() {
                    apply_live(parsed, &on_commit);
                    *dirty.lock().unwrap() = true;
                }
            }))
            .event(clone!(editing, pre_edit, dirty => move |_: events::FocusOut| {
                editing.set_neq(false);
                let snap = pre_edit.lock().unwrap().take();
                let was_dirty = std::mem::replace(&mut *dirty.lock().unwrap(), false);
                if was_dirty {
                    if let Some(snap) = snap {
                        app_state().commit_history(snap);
                    }
                }
            }))
            .event(clone!(drag, input => move |event: events::PointerDown| {
                if event.button() != events::MouseButton::Left { return; }
                // Skip drag-scrub while the input is already focused so
                // clicking inside a value you're editing keeps the normal
                // text-caret behavior.
                if document_active_element_is(&input) { return; }
                let start_value = input.value().parse::<f32>().unwrap_or(0.0);
                let pid = event.pointer_id();
                *drag.lock().unwrap() = Some(NumberDrag {
                    start_y: event.y(),
                    start_value,
                    pointer_id: pid,
                    dragged: false,
                    pre_drag: app_state().snapshot_scene(),
                });
                let _ = input.set_pointer_capture(pid);
                // Block the browser's auto-focus-on-pointerdown so we can
                // decide between "click to edit" vs "drag to scrub" at
                // pointerup time. Without this a tiny drag still leaves
                // the caret blinking in the field.
                event.prevent_default();
            }))
            .event(clone!(drag, input, on_commit => move |event: events::PointerMove| {
                let mut guard = drag.lock().unwrap();
                let Some(state) = guard.as_mut() else { return };
                // Invert so pulling up increases the number (Blender-style).
                let dy = state.start_y - event.y();
                if !state.dragged && dy.abs() < DRAG_THRESHOLD_PX { return; }
                state.dragged = true;
                let mut sensitivity = DRAG_SENSITIVITY;
                if event.shift_key() { sensitivity *= 0.1; }
                if event.ctrl_key() { sensitivity *= 10.0; }
                let new_value = state.start_value + (dy as f32) * sensitivity;
                input.set_value(&format_number(new_value));
                // Drop the lock before calling `on_commit` — the commit
                // callback eventually runs a bridge observer that may
                // re-enter via a signal write back into the input.
                drop(guard);
                apply_live(new_value, &on_commit);
            }))
            .event(clone!(drag, input, on_commit => move |_: events::PointerUp| {
                let Some(state) = drag.lock().unwrap().take() else { return };
                let _ = input.release_pointer_capture(state.pointer_id);
                if state.dragged {
                    if let Ok(parsed) = input.value().parse::<f32>() {
                        apply_live(parsed, &on_commit);
                    }
                    // Single history entry for the entire drag.
                    app_state().commit_history(state.pre_drag);
                } else {
                    // Plain click — we suppressed the default focus in
                    // pointerdown, so do it manually now.
                    let _ = input.focus();
                    input.select();
                }
            }))
            .event(clone!(drag => move |_: events::PointerCancel| {
                *drag.lock().unwrap() = None;
            }))
        })
    })
}

/// Push a value change into the scene without touching history. The
/// pointer-drag scrubber and the focused-typing flow both call this on
/// every intermediate value; history is captured once per edit session
/// (at FocusIn / PointerDown) and committed once at the end.
fn apply_live(new_value: f32, on_commit: &CommitFn) {
    (on_commit.lock().unwrap())(new_value);
    app_state().scene.bump_revision();
}

fn document_active_element_is(el: &HtmlInputElement) -> bool {
    web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.active_element())
        .map(|active| active.is_same_node(Some(el.as_ref())))
        .unwrap_or(false)
}

fn quat_to_euler_degrees(q: Quat) -> [f32; 3] {
    let (x, y, z) = q.to_euler(EulerRot::XYZ);
    [x.to_degrees(), y.to_degrees(), z.to_degrees()]
}

fn euler_degrees_to_quat(euler: [f32; 3]) -> Quat {
    Quat::from_euler(
        EulerRot::XYZ,
        euler[0].to_radians(),
        euler[1].to_radians(),
        euler[2].to_radians(),
    )
}

pub(super) fn format_number(v: f32) -> String {
    // Normalize -0.0 → 0.0 so rounding noise from quat→euler conversion
    // doesn't show up as "-0" in the UI.
    let v = if v == 0.0 { 0.0 } else { v };
    let s = format!("{v:.4}");
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}
