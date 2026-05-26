//! Recompile orchestration — debounced edits → `MaterialRegistration`
//! → `AwsmRenderer::register_material`.
//!
//! Phase 9 fills this in. Phase 8 leaves it as a documented stub.
//!
//! The flow:
//!   1. Editor state's `definition` + `wgsl_source` mutate as the user
//!      types.
//!   2. A 500ms debounce coalesces rapid edits into one recompile.
//!   3. Build a `MaterialRegistration` from the state.
//!   4. Call `renderer.unregister_material(prev_id)` then
//!      `renderer.register_material(reg)`.
//!   5. On `Err(AwsmDynamicMaterialError::WgslCompile)`, append a
//!      `CompileError` to the errors mutable + leave the prev_id
//!      assigned so the preview keeps drawing the last-good shader.
