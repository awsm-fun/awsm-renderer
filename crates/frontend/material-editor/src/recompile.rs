//! Recompile orchestration — debounced edits → `MaterialRegistration` →
//! `AwsmRenderer::register_material`.
//!
//! The flow:
//!   1. Editor state's `definition` + `wgsl_source` mutate as the user
//!      types.
//!   2. A 500 ms debounce coalesces rapid edits into one recompile.
//!   3. Build a `MaterialRegistration` from the state.
//!   4. Call `renderer.unregister_material(prev_id)` then
//!      `renderer.register_material(reg)` (or simply re-register if
//!      the id hasn't been allocated yet).
//!   5. On `Err(AwsmDynamicMaterialError::WgslCompile)`, append a
//!      `CompileError` to the errors mutable + keep the previous
//!      registration active so the preview keeps drawing the
//!      last-good shader.

use std::hash::{Hash, Hasher};
use std::rc::Rc;

use awsm_materials::dynamic_layout::{
    BufferSlotRuntime, FieldType as RuntimeFieldType, MaterialLayout, TextureSlotRuntime,
    UniformFieldRuntime, UniformValue as RuntimeUniformValue,
};
use awsm_materials::MaterialAlphaMode as MaterialAlphaModeRuntime;
use awsm_renderer::dynamic_materials::MaterialRegistration;
use awsm_scene_schema::dynamic_material::{
    FieldType as SchemaFieldType, MaterialDefinition, UniformValue as SchemaUniformValue,
};
use awsm_scene_schema::material::MaterialAlphaMode as MaterialAlphaModeSchema;
use futures_signals::signal::SignalExt;
use gloo_timers::future::TimeoutFuture;
use wasm_bindgen_futures::spawn_local;

use crate::state::{CompileError, EditState};

/// Debounce window for edits → recompile. Mirrors the plan's
/// "~500 ms" guidance.
const DEBOUNCE_MS: u32 = 500;

/// Build a [`MaterialRegistration`] from the current edit state. This
/// is the single source of truth for the schema-→-renderer conversion
/// inside the material-editor (the scene-editor's bridge uses
/// `dynamic_material_bridge.rs`'s equivalent).
pub fn build_registration(state: &EditState) -> MaterialRegistration {
    let def = state.definition.lock_ref().clone();
    let wgsl = state.wgsl_source.lock_ref().clone();
    let layout = convert_layout(&def);
    let alpha_mode = convert_alpha_mode(def.alpha_mode.clone());

    let mut h = std::collections::hash_map::DefaultHasher::new();
    def.name.hash(&mut h);
    def.version.hash(&mut h);
    // alpha_mode + double_sided are part of the registration's
    // identity — toggling them changes which pipeline (opaque vs
    // transparent) the renderer routes the material through, so
    // they must contribute to layout_hash. The recompile sink's
    // idempotency check uses (layout_hash, wgsl_hash) to decide
    // whether to skip re-registration; without alpha_mode in the
    // hash, switching Opaque → Blend would silently no-op.
    std::mem::discriminant(&def.alpha_mode).hash(&mut h);
    if let awsm_scene_schema::material::MaterialAlphaMode::Mask { cutoff } = def.alpha_mode {
        cutoff.to_bits().hash(&mut h);
    }
    def.double_sided.hash(&mut h);
    for u in &def.uniforms {
        u.name.hash(&mut h);
        std::mem::discriminant(&u.ty).hash(&mut h);
    }
    for t in &def.textures {
        t.name.hash(&mut h);
    }
    for b in &def.buffers {
        b.name.hash(&mut h);
    }
    let layout_hash = h.finish();
    let mut h2 = std::collections::hash_map::DefaultHasher::new();
    wgsl.hash(&mut h2);
    let wgsl_hash = h2.finish();

    // Per-uniform schema-declared defaults. The editor's
    // `MaterialDefinition` carries an authored default (`scan_freq=80`,
    // `tint=(0.6, 0.9, 0.6)`, etc.) for every uniform; without
    // forwarding them through, the host's `apply_quad` would seed a
    // fresh `DynamicMaterial` with zero values for everything, which
    // for the scanline material means `sin(uv.y * 0) == 0` → no
    // visible animation. Same order as `layout.uniforms`.
    let uniform_defaults: Vec<RuntimeUniformValue> = def
        .uniforms
        .iter()
        .map(|u| convert_uniform_value(&u.default))
        .collect();

    MaterialRegistration {
        name: def.name.clone(),
        alpha_mode,
        double_sided: def.double_sided,
        layout,
        layout_hash,
        wgsl_hash,
        wgsl_fragment: wgsl,
        buffer_defaults: Vec::new(),
        uniform_defaults,
    }
}

fn convert_uniform_value(v: &SchemaUniformValue) -> RuntimeUniformValue {
    match v {
        SchemaUniformValue::F32(x) => RuntimeUniformValue::F32(*x),
        SchemaUniformValue::Vec2(x) => RuntimeUniformValue::Vec2(*x),
        SchemaUniformValue::Vec3(x) => RuntimeUniformValue::Vec3(*x),
        SchemaUniformValue::Vec4(x) => RuntimeUniformValue::Vec4(*x),
        SchemaUniformValue::U32(x) => RuntimeUniformValue::U32(*x),
        SchemaUniformValue::IVec2(x) => RuntimeUniformValue::IVec2(*x),
        SchemaUniformValue::IVec3(x) => RuntimeUniformValue::IVec3(*x),
        SchemaUniformValue::IVec4(x) => RuntimeUniformValue::IVec4(*x),
        SchemaUniformValue::Mat3(x) => RuntimeUniformValue::Mat3(*x),
        SchemaUniformValue::Mat4(x) => RuntimeUniformValue::Mat4(*x),
        SchemaUniformValue::Color3(x) => RuntimeUniformValue::Color3(*x),
        SchemaUniformValue::Color4(x) => RuntimeUniformValue::Color4(*x),
        SchemaUniformValue::Bool(x) => RuntimeUniformValue::Bool(*x),
    }
}

/// Sink for compile attempts. The material-editor's renderer host
/// implements this to actually call `AwsmRenderer::register_material`
/// and surface results back into the editor's `EditState.errors`
/// mutable. The orchestration below is host-agnostic.
pub trait RecompileSink: 'static {
    /// Apply a fresh registration. Returns an `Err` with a single
    /// formatted message string when the compile fails — the
    /// orchestrator wraps it into a [`CompileError`].
    fn try_apply(&mut self, reg: MaterialRegistration) -> Result<(), String>;
}

/// Spawn the debounced-recompile loop. Listens to changes on
/// `state.definition` and `state.wgsl_source`; coalesces rapid edits
/// into a single registration attempt; pushes any
/// `AwsmDynamicMaterialError::WgslCompile` message onto
/// `state.errors` (replacing the previous list).
///
/// Cancelable by dropping the spawned future (today the editor
/// keeps it alive for the lifetime of the page; if multi-document
/// support lands later, each per-document state owns its own).
pub fn spawn(state: EditState, sink: Rc<futures_signals::signal::Mutable<Box<dyn RecompileSink>>>) {
    // Treat any change to either signal as the trigger. The
    // `for_each` + `Timeout` combination gives us a "last-write wins
    // within DEBOUNCE_MS" semantic with no extra state.
    let trigger = Rc::new(futures_signals::signal::Mutable::new(0u64));
    {
        let trigger = trigger.clone();
        let def = state.definition.clone();
        spawn_local(async move {
            def.signal_cloned()
                .for_each(move |_| {
                    let trigger = trigger.clone();
                    async move {
                        let next = trigger.get() + 1;
                        trigger.set(next);
                    }
                })
                .await;
        });
    }
    {
        let trigger = trigger.clone();
        let wgsl = state.wgsl_source.clone();
        spawn_local(async move {
            wgsl.signal_cloned()
                .for_each(move |_| {
                    let trigger = trigger.clone();
                    async move {
                        let next = trigger.get() + 1;
                        trigger.set(next);
                    }
                })
                .await;
        });
    }

    let state_for_loop = state.clone();
    let trigger_for_loop = trigger.clone();
    spawn_local(async move {
        let last_seen = std::rc::Rc::new(std::cell::Cell::new(0u64));
        trigger_for_loop
            .signal()
            .for_each(move |seen| {
                let state = state_for_loop.clone();
                let sink = sink.clone();
                let trigger = trigger_for_loop.clone();
                let last_seen = last_seen.clone();
                async move {
                    if seen == last_seen.get() {
                        return;
                    }
                    last_seen.set(seen);
                    // Debounce: sleep the window, then check if a
                    // newer edit arrived. If so, the next iteration's
                    // wakeup will pick up the latest value — we bail
                    // here so we don't run a stale compile.
                    TimeoutFuture::new(DEBOUNCE_MS).await;
                    let now_seen = trigger.get();
                    if now_seen != seen {
                        return;
                    }
                    let reg = build_registration(&state);
                    let mut sink = sink.lock_mut();
                    match sink.try_apply(reg) {
                        Ok(()) => {
                            state.errors.set(Vec::new());
                        }
                        Err(message) => {
                            let (line, column) = parse_naga_line_column(&message);
                            state.errors.set(vec![CompileError {
                                message,
                                line,
                                column,
                            }]);
                        }
                    }
                }
            })
            .await;
    });
}

/// Best-effort line / column parse from a naga diagnostic message.
///
/// Naga's error format varies but commonly includes
/// `┌─ wgsl:LINE:COL` markers and / or `at line N`-style snippets.
/// We grep for those patterns; anything not matching returns
/// `(None, None)` and the error pane shows the raw message.
pub fn parse_naga_line_column(message: &str) -> (Option<u32>, Option<u32>) {
    // Pattern 1: `wgsl:LINE:COLUMN`
    for token in message.split_whitespace() {
        if let Some(rest) = token.strip_prefix("wgsl:") {
            let mut parts = rest.splitn(2, ':');
            if let (Some(line), Some(col)) = (parts.next(), parts.next()) {
                let line = line.parse::<u32>().ok();
                let col = col
                    .trim_end_matches(|c: char| !c.is_ascii_digit())
                    .parse::<u32>()
                    .ok();
                if line.is_some() || col.is_some() {
                    return (line, col);
                }
            }
        }
    }
    // Pattern 2: a bare `:LINE:COLUMN` after the path-like prefix.
    for chunk in message.split('\n') {
        let trimmed = chunk.trim_start_matches(['─', ' ', '┌', '│'].as_ref());
        if let Some(idx) = trimmed.find(':') {
            let after = &trimmed[idx + 1..];
            let mut parts = after.splitn(2, ':');
            if let (Some(line), Some(col)) = (parts.next(), parts.next()) {
                if let (Ok(line), Ok(col)) = (line.parse::<u32>(), col.parse::<u32>()) {
                    return (Some(line), Some(col));
                }
            }
        }
    }
    (None, None)
}

fn convert_layout(def: &MaterialDefinition) -> MaterialLayout {
    MaterialLayout {
        uniforms: def
            .uniforms
            .iter()
            .map(|f| UniformFieldRuntime {
                name: f.name.clone(),
                ty: convert_field_type(f.ty),
            })
            .collect(),
        textures: def
            .textures
            .iter()
            .map(|t| TextureSlotRuntime {
                name: t.name.clone(),
            })
            .collect(),
        buffers: def
            .buffers
            .iter()
            .map(|b| BufferSlotRuntime {
                name: b.name.clone(),
            })
            .collect(),
    }
}

fn convert_field_type(ty: SchemaFieldType) -> RuntimeFieldType {
    match ty {
        SchemaFieldType::F32 => RuntimeFieldType::F32,
        SchemaFieldType::Vec2 => RuntimeFieldType::Vec2,
        SchemaFieldType::Vec3 => RuntimeFieldType::Vec3,
        SchemaFieldType::Vec4 => RuntimeFieldType::Vec4,
        SchemaFieldType::U32 => RuntimeFieldType::U32,
        SchemaFieldType::IVec2 => RuntimeFieldType::IVec2,
        SchemaFieldType::IVec3 => RuntimeFieldType::IVec3,
        SchemaFieldType::IVec4 => RuntimeFieldType::IVec4,
        SchemaFieldType::Mat3 => RuntimeFieldType::Mat3,
        SchemaFieldType::Mat4 => RuntimeFieldType::Mat4,
        SchemaFieldType::Color3 => RuntimeFieldType::Color3,
        SchemaFieldType::Color4 => RuntimeFieldType::Color4,
        SchemaFieldType::Bool => RuntimeFieldType::Bool,
    }
}

fn convert_alpha_mode(a: MaterialAlphaModeSchema) -> MaterialAlphaModeRuntime {
    match a {
        MaterialAlphaModeSchema::Opaque => MaterialAlphaModeRuntime::Opaque,
        MaterialAlphaModeSchema::Mask { cutoff } => MaterialAlphaModeRuntime::Mask { cutoff },
        MaterialAlphaModeSchema::Blend => MaterialAlphaModeRuntime::Blend,
    }
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    #[test]
    fn parse_naga_extracts_line_column_basic() {
        // wgsl:7:13 in a typical naga diagnostic
        let msg = "error: parse error\n  ┌─ wgsl:7:13\n  │";
        let (line, col) = parse_naga_line_column(msg);
        assert_eq!(line, Some(7));
        assert_eq!(col, Some(13));
    }

    #[test]
    fn parse_naga_returns_none_when_unrecognized() {
        let msg = "some random error string";
        let (line, col) = parse_naga_line_column(msg);
        assert!(line.is_none());
        assert!(col.is_none());
    }
}
