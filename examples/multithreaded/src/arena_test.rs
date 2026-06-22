//! M1 — browser proof of the shared arena + seqlock over **real** shared
//! linear memory.
//!
//! Topology (mirrors the eventual render/physics split):
//! - The **render** worker owns a
//!   [`awsm_renderer::buffer::shared_arena::SharedArena`], allocates N
//!   slots, and spawns the **physics** worker, handing it each slot's raw
//!   binding (value/version addresses + chunk) plus the dirty-bitmap
//!   address — once, at setup.
//! - The **physics** worker hammers every slot with a self-consistent
//!   value (all `stride` bytes equal a tick counter) as fast as it can,
//!   via [`awsm_renderer::buffer::shared_arena::foreign_write`] — **zero
//!   `postMessage`** on the write path.
//! - Each frame the render worker [`descend`](awsm_renderer::buffer::shared_arena::SharedArena::descend)s
//!   the dirty chunks and validates: (a) no torn value is ever *accepted*
//!   into the mirror (every accepted slot is internally consistent), and
//!   (b) every written slot is eventually observed dirty. It posts the
//!   verdict to the main thread, which exposes it at `globalThis.__mt_arena`
//!   for the gate.

use std::cell::RefCell;
use std::rc::Rc;

use awsm_renderer::buffer::shared_arena::{foreign_write, SharedArena, SlotBinding};
use wasm_bindgen::prelude::*;
use web_sys::js_sys;

const STRIDE: usize = 64; // a Mat4's worth of bytes
const CHUNK_SLOTS: usize = 256;
const MAX_CHUNKS: usize = 64;
const N_SLOTS: usize = 128; // all land in chunk 0
const CHECK_FRAMES: usize = 120;
const DESCEND_INTERVAL_MS: i32 = 16;
const PASSES_PER_TICK: usize = 50;

/// Main thread: spawn the render worker and expose its verdict.
pub fn start_main() -> Result<(), JsValue> {
    let on_msg = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(|e: web_sys::MessageEvent| {
        let data = e.data();
        let _ = js_sys::Reflect::set(&js_sys::global(), &JsValue::from_str("__mt_arena"), &data);
        let pass = js_sys::Reflect::get(&data, &JsValue::from_str("pass"))
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        tracing::info!("arena test verdict: pass={pass} (full result at globalThis.__mt_arena)");
    });
    crate::bootstrap::spawn_shared_worker(
        "arena-render",
        &JsValue::UNDEFINED,
        on_msg.as_ref().unchecked_ref(),
    )?;
    on_msg.forget();
    tracing::info!("arena test: spawned render (owner/reader) worker");
    Ok(())
}

/// Worker-side role dispatch.
pub fn worker_dispatch(role: &str, payload: JsValue) -> Result<(), JsValue> {
    match role {
        "arena-render" => render_main(),
        "arena-physics" => physics_main(payload),
        _ => Ok(()),
    }
}

/// Render worker (owner + reader). Creates the arena, binds the physics
/// worker to its slots, and runs the descend/validate loop.
fn render_main() -> Result<(), JsValue> {
    tracing::info!("arena render worker: creating shared arena ({N_SLOTS} slots)");
    let mut arena = SharedArena::new(STRIDE, CHUNK_SLOTS, MAX_CHUNKS);
    for _ in 0..N_SLOTS {
        arena.allocate();
    }

    // Build the payload: [count, stride, dirty_addr, (value, version, chunk)*N].
    let dirty_addr = arena.dirty_words_addr();
    let payload = js_sys::Array::new();
    payload.push(&JsValue::from_f64(N_SLOTS as f64));
    payload.push(&JsValue::from_f64(STRIDE as f64));
    payload.push(&JsValue::from_f64(dirty_addr as f64));
    for slot in 0..N_SLOTS {
        let b = arena.slot_binding(slot);
        payload.push(&JsValue::from_f64(b.value_addr as f64));
        payload.push(&JsValue::from_f64(b.version_addr as f64));
        payload.push(&JsValue::from_f64(b.chunk as f64));
    }

    // Spawn the physics worker and hand it the bindings.
    let noop = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(|_| {});
    crate::bootstrap::spawn_shared_worker(
        "arena-physics",
        &payload,
        noop.as_ref().unchecked_ref(),
    )?;
    noop.forget();

    // Descend/validate loop state.
    let state = Rc::new(RefCell::new(RenderState {
        arena,
        frame: 0,
        torn_detected: 0,
        torn_accepted: 0,
        total_updated: 0,
        seen: vec![false; N_SLOTS],
        max_value: 0,
        done: false,
    }));

    repeat_every(DESCEND_INTERVAL_MS, move || {
        let mut s = state.borrow_mut();
        if s.done {
            return;
        }
        let r = s.arena.descend();
        s.torn_detected += r.torn;
        s.total_updated += r.updated;

        // Validate every slot's mirror value is internally consistent (a
        // torn value accepted into the mirror would show mixed bytes).
        for slot in 0..N_SLOTS {
            let off = slot * STRIDE;
            let bytes = &s.arena.mirror()[off..off + STRIDE];
            let first = bytes[0];
            let consistent = bytes.iter().all(|&b| b == first);
            if !consistent {
                s.torn_accepted += 1;
            }
            if first != 0 {
                s.seen[slot] = true;
                if first > s.max_value {
                    s.max_value = first;
                }
            }
        }

        s.frame += 1;
        if s.frame >= CHECK_FRAMES {
            s.done = true;
            let distinct = s.seen.iter().filter(|&&x| x).count();
            let pass = s.torn_accepted == 0 && distinct == N_SLOTS && s.max_value > 0;
            tracing::info!(
                "arena render: frames={} torn_detected={} torn_accepted={} updated={} distinct={}/{} max_value={}",
                s.frame, s.torn_detected, s.torn_accepted, s.total_updated, distinct, N_SLOTS, s.max_value
            );
            let scope = js_sys::global().unchecked_into::<web_sys::DedicatedWorkerGlobalScope>();
            let result = js_sys::Object::new();
            set(&result, "pass", &JsValue::from_bool(pass));
            set(&result, "frames", &JsValue::from_f64(s.frame as f64));
            set(
                &result,
                "tornDetected",
                &JsValue::from_f64(s.torn_detected as f64),
            );
            set(
                &result,
                "tornAccepted",
                &JsValue::from_f64(s.torn_accepted as f64),
            );
            set(
                &result,
                "totalUpdated",
                &JsValue::from_f64(s.total_updated as f64),
            );
            set(
                &result,
                "distinctUpdated",
                &JsValue::from_f64(distinct as f64),
            );
            set(&result, "count", &JsValue::from_f64(N_SLOTS as f64));
            set(&result, "maxValue", &JsValue::from_f64(s.max_value as f64));
            let _ = scope.post_message(&result);
        }
    })?;

    Ok(())
}

struct RenderState {
    arena: SharedArena,
    frame: usize,
    torn_detected: usize,
    torn_accepted: usize,
    total_updated: usize,
    seen: Vec<bool>,
    max_value: u8,
    done: bool,
}

/// Physics worker (foreign writer). Parses the bindings and hammers every
/// slot with a self-consistent ramp value as fast as it can.
fn physics_main(payload: JsValue) -> Result<(), JsValue> {
    let arr: js_sys::Array = payload.unchecked_into();
    let count = arr.get(0).as_f64().unwrap_or(0.0) as usize;
    let stride = arr.get(1).as_f64().unwrap_or(0.0) as usize;
    let dirty_addr = arr.get(2).as_f64().unwrap_or(0.0) as usize;
    let mut bindings = Vec::with_capacity(count);
    for slot in 0..count {
        let base = 3 + slot * 3;
        bindings.push(SlotBinding {
            value_addr: arr.get(base as u32).as_f64().unwrap_or(0.0) as usize,
            version_addr: arr.get((base + 1) as u32).as_f64().unwrap_or(0.0) as usize,
            chunk: arr.get((base + 2) as u32).as_f64().unwrap_or(0.0) as usize,
        });
    }
    tracing::info!("arena physics worker: {count} bindings, stride={stride}, writing ramp");

    let tick = Rc::new(RefCell::new(0u8));
    let mut buf = vec![0u8; stride];
    repeat_every(0, move || {
        for _ in 0..PASSES_PER_TICK {
            let v = {
                let mut t = tick.borrow_mut();
                *t = t.wrapping_add(1);
                *t
            };
            for b in buf.iter_mut() {
                *b = v;
            }
            for binding in &bindings {
                // SAFETY: addresses come from the owner arena (which
                // outlives this worker) and point into the shared memory
                // both workers attached to; `buf.len() == stride`.
                unsafe {
                    foreign_write(*binding, dirty_addr, &buf);
                }
            }
        }
    })?;
    Ok(())
}

/// Self-rescheduling `setTimeout` loop on the worker scope.
fn repeat_every<F: FnMut() + 'static>(ms: i32, mut f: F) -> Result<(), JsValue> {
    let scope = js_sys::global().unchecked_into::<web_sys::DedicatedWorkerGlobalScope>();
    let holder: Rc<RefCell<Option<Closure<dyn FnMut()>>>> = Rc::new(RefCell::new(None));
    let holder_run = holder.clone();
    let scope_run = scope.clone();
    *holder.borrow_mut() = Some(Closure::new(move || {
        f();
        if let Some(cb) = holder_run.borrow().as_ref() {
            let _ = scope_run.set_timeout_with_callback_and_timeout_and_arguments_0(
                cb.as_ref().unchecked_ref(),
                ms,
            );
        }
    }));
    if let Some(cb) = holder.borrow().as_ref() {
        scope.set_timeout_with_callback_and_timeout_and_arguments_0(
            cb.as_ref().unchecked_ref(),
            ms,
        )?;
    }
    std::mem::forget(holder);
    Ok(())
}

fn set(obj: &js_sys::Object, key: &str, value: &JsValue) {
    let _ = js_sys::Reflect::set(obj, &JsValue::from_str(key), value);
}
