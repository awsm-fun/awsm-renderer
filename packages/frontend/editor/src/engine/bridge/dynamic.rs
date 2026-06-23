//! Real GPU registration for custom WGSL materials. Converts a
//! [`CustomMaterial`] (the Studio's reactive model) into the renderer's
//! [`MaterialRegistration`] and registers it, so an assigned mesh can resolve
//! the material by name to a registered `MaterialShaderId` and render the WGSL.
//!
//! Adapted from the archived `renderer_bridge/dynamic_material_bridge.rs`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use awsm_materials::dynamic::{DynamicMaterial, DynamicTextureBinding};
use awsm_materials::dynamic_layout::{
    BufferSlotRuntime, FieldType, MaterialLayout, TextureSlotRuntime, UniformFieldRuntime,
    UniformValue,
};
use awsm_materials::{FragmentInputs, MaterialAlphaMode, MaterialShaderId, ShaderIncludes};
use awsm_renderer::dynamic_materials::MaterialRegistration;
use awsm_renderer::materials::Material;
use awsm_renderer::AwsmRenderer;

use awsm_editor_protocol::dynamic_material::{MaterialInstance, UniformValue as SceneUniformValue};

use crate::controller::{AlphaMode, CustomMaterial};
use crate::engine::context::renderer_handle;
use crate::engine::scene::AssetId;

thread_local! {
    /// Serializes the WHOLE of [`register`] / [`unregister`] so their thread-local
    /// bookkeeping (REGISTRY / LAST_HASH / DELETED) and renderer mutations can't
    /// interleave across the multiple `await`s each holds (renderer lock,
    /// `finalize_gpu_textures`, …). The renderer's own `AsyncMutex` only serializes
    /// the renderer ops, not the bookkeeping around them — e.g. `register` inserts
    /// into REGISTRY only AFTER `finalize_gpu_textures().await`, so a delete that
    /// runs during that await found no REGISTRY entry to unregister and left a
    /// permanent orphan (the sub-second-churn "aw snap" tail + "duplicate name"
    /// errors). Holding this guard for the entire op closes every such window.
    /// Always acquired BEFORE the renderer lock — consistent ordering, no deadlock.
    static BRIDGE_OP_LOCK: std::sync::Arc<xutex::AsyncMutex<()>> =
        std::sync::Arc::new(xutex::AsyncMutex::new(()));

    /// `material id → registered shader id`, populated by [`register`]. Keyed by
    /// the material's **stable id** (not its display name) so renaming a material
    /// never orphans an assigned mesh's resolution.
    static REGISTRY: RefCell<HashMap<AssetId, MaterialShaderId>> = RefCell::new(HashMap::new());

    /// `material id → (layout_hash, wgsl_hash)` of its last *successful*
    /// registration. Lets [`register`] no-op when re-registering byte-identical
    /// content — so the ~400ms debounced auto-register doesn't churn the
    /// shader_id (drop + re-add → a fresh id) out from under an in-flight
    /// compile-status poll, which would otherwise race the diagnostics.
    static LAST_HASH: RefCell<HashMap<AssetId, (u64, u64)>> = RefCell::new(HashMap::new());

    /// Tombstones for DELETED material ids. The auto-register is debounced
    /// (~400ms), so a create→edit→delete faster than the debounce fires the
    /// pending `register` AFTER `unregister` — re-registering a material the
    /// editor already deleted. That orphan never gets unregistered again, so its
    /// shader_id stays a live bucket forever (its opaque/edge pipelines recompile
    /// on every later edit → unbounded GPU growth, the sub-second-churn tail of
    /// the "aw snap" leak). [`register`] consults this set and bails for a
    /// tombstoned id. Asset ids are unique (never reused), so tombstones never
    /// need clearing.
    static DELETED: RefCell<std::collections::HashSet<AssetId>> =
        RefCell::new(std::collections::HashSet::new());
}

/// Whether `mat_id` has been deleted (see [`DELETED`]). The debounced
/// auto-register call site checks this to skip a register that lost the race
/// with a delete.
pub fn is_deleted(mat_id: AssetId) -> bool {
    DELETED.with(|d| d.borrow().contains(&mat_id))
}

fn registered_shader_id(id: AssetId) -> Option<MaterialShaderId> {
    REGISTRY.with(|r| r.borrow().get(&id).copied())
}

/// The registered `MaterialShaderId` for a custom-material asset id, if it's been
/// registered with the renderer. Public seam for the animation bridge, which
/// resolves a `Uniform` track's material asset → shader id → live `MaterialKey`.
/// `None` while the material hasn't been registered yet (PENDING).
pub fn shader_id_for_asset(id: AssetId) -> Option<MaterialShaderId> {
    registered_shader_id(id)
}

thread_local! {
    /// Session-scoped data for per-mesh buffer-slot overrides: a synthetic
    /// `session://buffer/<uuid>` path → the loaded `.bin`'s little-endian u32
    /// words. (Persistence writes the `.bin` next to the project on save.)
    static BUFFER_DATA: RefCell<HashMap<String, Vec<u32>>> = RefCell::new(HashMap::new());
}

/// Store a loaded buffer's words and return the synthetic path that references
/// it (set as the `BufferRef::path` on a `MaterialInstance` override).
pub(crate) fn store_buffer_words(words: Vec<u32>) -> String {
    let path = format!("session://buffer/{}", AssetId::new().0);
    BUFFER_DATA.with(|m| m.borrow_mut().insert(path.clone(), words));
    path
}

pub(crate) fn buffer_words_for(path: &str) -> Option<Vec<u32>> {
    BUFFER_DATA.with(|m| m.borrow().get(path).cloned())
}

/// Register a custom material with the renderer (locks it, finalizes textures).
/// Returns the assigned shader id, or an error string on failure.
pub async fn register(mat: &CustomMaterial) -> Result<MaterialShaderId, String> {
    let mat_id = mat.id;
    // Serialize the whole op against any concurrent register/unregister (see
    // BRIDGE_OP_LOCK) — held until this fn returns.
    let op_lock = BRIDGE_OP_LOCK.with(|l| l.clone());
    let _op_guard = op_lock.lock().await;
    // Lost-the-race guard: a debounced register that fires after the material was
    // deleted must NOT re-register it (see DELETED). Without this the orphaned
    // registration leaks its GPU pipelines forever under sub-second churn.
    if is_deleted(mat_id) {
        return Err("custom material was deleted".to_string());
    }
    let reg = build_registration(mat);
    let hashes = (reg.layout_hash, reg.wgsl_hash);
    // No-op when the content is byte-identical to the last successful register
    // (same layout + wgsl) and we still hold its shader id. Avoids the debounced
    // auto-register dropping + re-adding the same material — which would mint a
    // new shader_id and invalidate an in-flight `await_dynamic_compile` poll.
    let existing = REGISTRY.with(|reg| reg.borrow().get(&mat_id).copied());
    if let Some(existing) = existing {
        if LAST_HASH.with(|h| h.borrow().get(&mat_id).copied()) == Some(hashes) {
            return Ok(existing);
        }
    }
    let handle = renderer_handle();
    let mut r = handle.lock().await;
    // (No second is_deleted re-check needed: BRIDGE_OP_LOCK is held for the whole
    // op, so no `unregister`/delete can run between the check above and here.)
    // Recompile: the renderer rejects re-registering a key whose content changed,
    // so drop this material's previous registration first (the editor's
    // edit→re-register cycle `unregister_material` expects). Keyed by id, so a
    // rename is just a display change — the registration key is unaffected.
    if let Some(old) = existing {
        let _ = r.unregister_material(old);
    }
    let shader_id = r.register_material(reg).map_err(|e| format!("{e}"))?;
    // Live material add: `register_material` only stages the bucket; the one
    // compile path (`commit_load`) finalizes textures + compiles the new
    // material's pipelines so it shades on the next frame.
    if let Err(e) = r
        .commit_load(crate::engine::activity::commit_phase_handler())
        .await
    {
        tracing::warn!("commit_load after register: {e}");
    }
    REGISTRY.with(|reg| reg.borrow_mut().insert(mat_id, shader_id));
    LAST_HASH.with(|h| h.borrow_mut().insert(mat_id, hashes));
    Ok(shader_id)
}

/// Drop a custom material's renderer registration when it's DELETED. Without
/// this the renderer keeps the dynamic registration — and its compiled GPU
/// compute pipelines + shader modules — forever, so repeated create/delete
/// editing churns GPU memory until Chrome OOMs ("aw snap"). `unregister_material`
/// also evicts the material's pipelines from the shared caches (the
/// pipeline-leak fix). No-op if the material was never registered.
pub async fn unregister(mat_id: AssetId) {
    // Serialize against any concurrent register/unregister (see BRIDGE_OP_LOCK):
    // notably this blocks until an in-flight `register` for the SAME id finishes
    // inserting into REGISTRY, so the remove below actually finds + drops it
    // (otherwise the registration completes after us → permanent orphan).
    let op_lock = BRIDGE_OP_LOCK.with(|l| l.clone());
    let _op_guard = op_lock.lock().await;
    // Tombstone the id so a debounced `register` that fires after this delete
    // (create→edit→delete faster than the ~400ms debounce) can't re-register the
    // now-deleted material into a permanent orphan. See DELETED.
    DELETED.with(|d| {
        d.borrow_mut().insert(mat_id);
    });
    let shader_id = REGISTRY.with(|reg| reg.borrow_mut().remove(&mat_id));
    LAST_HASH.with(|h| {
        h.borrow_mut().remove(&mat_id);
    });
    if let Some(shader_id) = shader_id {
        let handle = renderer_handle();
        let mut r = handle.lock().await;
        let _ = r.unregister_material(shader_id);
    }
}

/// Build + insert a `Material::Custom` for an assigned custom material `name`,
/// returning its `MaterialKey`. `None` if `name` isn't registered (the caller
/// falls back to the mesh's inline material). Mirrors `material::insert_material`'s
/// disjoint-field borrow so it composes with the renderer lock.
pub fn insert_custom(
    renderer: &mut AwsmRenderer,
    inst: &MaterialInstance,
) -> Option<awsm_renderer::materials::MaterialKey> {
    let material = build_custom(renderer, inst)?;
    // Upload per-instance buffer-override words to the extras pool BEFORE insert
    // (insert packs `MaterialData.<slot>_offset` from `extras_pool.slice_for`).
    renderer.upload_dynamic_material_buffers(&material);
    Some(renderer.materials.insert(
        material,
        &renderer.textures,
        &renderer.dynamic_materials,
        &renderer.extras_pool,
    ))
}

/// Build a per-mesh `Material::Custom` for an assigned custom-material
/// **instance**: starts from the registration's authored defaults, then applies
/// this mesh's per-instance `uniform_overrides` (#4.2) — matched by slot name
/// and type-checked against the layout. `None` if the material isn't registered.
fn build_custom(renderer: &mut AwsmRenderer, inst: &MaterialInstance) -> Option<Material> {
    let mut material = build_custom_for_shader(renderer, registered_shader_id(inst.asset)?)?;
    if let Material::Custom(dm) = &mut material {
        // Per-mesh uniform overrides (matched by slot name, type-checked).
        if !inst.uniform_overrides.is_empty() {
            // Snapshot the layout so we don't hold a renderer borrow while
            // resolving textures (which needs `&mut renderer`).
            let uniforms: Vec<(String, FieldType)> = renderer
                .dynamic_material_registration(dm.shader_id)
                .map(|reg| {
                    reg.layout
                        .uniforms
                        .iter()
                        .map(|u| (u.name.clone(), u.ty))
                        .collect()
                })
                .unwrap_or_default();
            for (i, (name, ty)) in uniforms.iter().enumerate() {
                if let Some(v) = inst.uniform_overrides.get(name) {
                    let rv = scene_to_renderer(v);
                    if rv.field_type() == *ty {
                        if let Some(slot) = dm.values.get_mut(i) {
                            *slot = rv;
                        }
                    }
                }
            }
        }
        // Per-mesh buffer-slot overrides (#4.2): bind a loaded `.bin`'s words.
        if !inst.buffer_overrides.is_empty() {
            let buf_slots: Vec<String> = renderer
                .dynamic_material_registration(dm.shader_id)
                .map(|reg| reg.layout.buffers.iter().map(|b| b.name.clone()).collect())
                .unwrap_or_default();
            for (i, name) in buf_slots.iter().enumerate() {
                if let Some(bref) = inst.buffer_overrides.get(name) {
                    if let Some(words) = buffer_words_for(&bref.path.to_string_lossy()) {
                        if let Some(slot) = dm.buffers.get_mut(i) {
                            *slot = Some(words);
                        }
                    }
                }
            }
        }
        // Per-mesh texture-slot overrides (#4.2): resolve each TextureRef to a
        // pooled renderer texture and bind it into the slot.
        if !inst.texture_overrides.is_empty() {
            let tex_slots: Vec<String> = renderer
                .dynamic_material_registration(dm.shader_id)
                .map(|reg| reg.layout.textures.iter().map(|t| t.name.clone()).collect())
                .unwrap_or_default();
            for (i, name) in tex_slots.iter().enumerate() {
                if let Some(tref) = inst.texture_overrides.get(name) {
                    if let Some((key, sampler)) =
                        super::material::resolve_texture_binding(renderer, tref)
                    {
                        if let Some(slot) = dm.textures.get_mut(i) {
                            *slot = Some(DynamicTextureBinding::Pooled {
                                texture: key,
                                sampler,
                            });
                        }
                    }
                }
            }
        }
    }
    Some(material)
}

/// The default value a declared uniform [`Slot`] parses to (its authored WGSL
/// type + default-value string), as the **schema** `UniformValue` the inspector
/// stores in `MaterialInstance::uniform_overrides` (#4.2).
pub fn slot_default_value(slot: &crate::controller::Slot) -> SceneUniformValue {
    renderer_to_scene(&parse_uniform_value(parse_field_type(&slot.ty), &slot.val))
}

/// Live preview (D3): push `value_str` into uniform slot `name` of every running
/// `Material::Custom` built from `asset`'s registered shader — the SAME per-mesh
/// `dm.values[slot]` write a uniform animation track does each frame
/// (`animations.rs apply_to_target` Uniform). So a manual `SetMaterialUniform`
/// previews IMMEDIATELY instead of only after a re-register. The scene-side
/// authored default is updated separately by the command handler (persisted +
/// used at the next full register / project reload). No-op until the material is
/// registered + a mesh using it is materialized.
pub fn set_uniform_live(r: &mut AwsmRenderer, asset: AssetId, name: &str, value_str: &str) {
    let Some(shader_id) = registered_shader_id(asset) else {
        return;
    };
    let Some((idx, ty)) = r.dynamic_material_registration(shader_id).and_then(|reg| {
        reg.layout
            .uniforms
            .iter()
            .enumerate()
            .find(|(_, u)| u.name == name)
            .map(|(i, u)| (i, u.ty))
    }) else {
        return;
    };
    let value = parse_uniform_value(ty, value_str);
    // Collect first (immutable borrow) so the per-key `update_material` (mutable)
    // doesn't overlap the iterator.
    let keys: Vec<awsm_renderer::materials::MaterialKey> = r
        .materials
        .iter()
        .filter_map(|(k, m)| match m {
            Material::Custom(dm) if dm.shader_id == shader_id => Some(k),
            _ => None,
        })
        .collect();
    for k in keys {
        let v = value.clone();
        r.update_material(k, move |m| {
            if let Material::Custom(dm) = m {
                if let Some(slot) = dm.values.get_mut(idx) {
                    *slot = v.clone();
                }
            }
        });
    }
}

/// Convert the serializable schema `UniformValue` (stored on a
/// `MaterialInstance`) into the renderer's value type. The two enums have
/// identical variants — this is the (deliberately exhaustive) bridge so adding a
/// variant to one forces updating the other.
fn scene_to_renderer(v: &SceneUniformValue) -> UniformValue {
    match *v {
        SceneUniformValue::F32(x) => UniformValue::F32(x),
        SceneUniformValue::Vec2(x) => UniformValue::Vec2(x),
        SceneUniformValue::Vec3(x) => UniformValue::Vec3(x),
        SceneUniformValue::Vec4(x) => UniformValue::Vec4(x),
        SceneUniformValue::U32(x) => UniformValue::U32(x),
        SceneUniformValue::IVec2(x) => UniformValue::IVec2(x),
        SceneUniformValue::IVec3(x) => UniformValue::IVec3(x),
        SceneUniformValue::IVec4(x) => UniformValue::IVec4(x),
        SceneUniformValue::Mat3(x) => UniformValue::Mat3(x),
        SceneUniformValue::Mat4(x) => UniformValue::Mat4(x),
        SceneUniformValue::Color3(x) => UniformValue::Color3(x),
        SceneUniformValue::Color4(x) => UniformValue::Color4(x),
        SceneUniformValue::Bool(x) => UniformValue::Bool(x),
    }
}

/// Inverse of [`scene_to_renderer`].
fn renderer_to_scene(v: &UniformValue) -> SceneUniformValue {
    match *v {
        UniformValue::F32(x) => SceneUniformValue::F32(x),
        UniformValue::Vec2(x) => SceneUniformValue::Vec2(x),
        UniformValue::Vec3(x) => SceneUniformValue::Vec3(x),
        UniformValue::Vec4(x) => SceneUniformValue::Vec4(x),
        UniformValue::U32(x) => SceneUniformValue::U32(x),
        UniformValue::IVec2(x) => SceneUniformValue::IVec2(x),
        UniformValue::IVec3(x) => SceneUniformValue::IVec3(x),
        UniformValue::IVec4(x) => SceneUniformValue::IVec4(x),
        UniformValue::Mat3(x) => SceneUniformValue::Mat3(x),
        UniformValue::Mat4(x) => SceneUniformValue::Mat4(x),
        UniformValue::Color3(x) => SceneUniformValue::Color3(x),
        UniformValue::Color4(x) => SceneUniformValue::Color4(x),
        UniformValue::Bool(x) => SceneUniformValue::Bool(x),
    }
}

/// Like [`build_custom`] but for an explicit `shader_id` (used by the 2nd-renderer
/// preview, whose ids live in its own registry, not the main thread-local one).
pub fn build_custom_for_shader(
    renderer: &AwsmRenderer,
    shader_id: MaterialShaderId,
) -> Option<Material> {
    let reg = renderer.dynamic_material_registration(shader_id)?;
    let values: Vec<UniformValue> = reg
        .layout
        .uniforms
        .iter()
        .enumerate()
        .map(|(i, u)| {
            reg.uniform_defaults
                .get(i)
                .cloned()
                .filter(|v| v.field_type() == u.ty)
                .unwrap_or_else(|| default_value_for(u.ty))
        })
        .collect();
    Some(Material::Custom(Box::new(DynamicMaterial {
        shader_id,
        alpha_mode: reg.alpha_mode,
        double_sided: reg.double_sided,
        values,
        textures: vec![None; reg.layout.textures.len()],
        buffers: vec![None; reg.layout.buffers.len()],
    })))
}

// ── registration construction ─────────────────────────────────────────────────

pub fn build_registration(mat: &CustomMaterial) -> MaterialRegistration {
    let uniforms = mat.uniforms.get_cloned();
    let textures = mat.textures.get_cloned();
    let buffers = mat.buffers.get_cloned();

    let layout = MaterialLayout {
        uniforms: uniforms
            .iter()
            .map(|u| UniformFieldRuntime {
                name: u.name.clone(),
                ty: parse_field_type(&u.ty),
            })
            .collect(),
        textures: textures
            .iter()
            .map(|t| TextureSlotRuntime {
                name: t.name.clone(),
            })
            .collect(),
        buffers: buffers
            .iter()
            .map(|b| BufferSlotRuntime {
                name: b.name.clone(),
            })
            .collect(),
    };

    let uniform_defaults: Vec<UniformValue> = uniforms
        .iter()
        .map(|u| parse_uniform_value(parse_field_type(&u.ty), &u.val))
        .collect();
    let buffer_defaults: Vec<Vec<u32>> = buffers.iter().map(|_| Vec::new()).collect();

    let main_wgsl = mat.wgsl.get_cloned();
    let alpha_body = mat.alpha_wgsl.get_cloned();
    let alpha_mode = convert_alpha(mat.alpha.get(), mat.cutoff.get() as f32);
    // The 2nd ("alpha-only") WGSL window — only meaningful for MASK materials.
    // Empty body → `None` (no masked variant built).
    let alpha_wgsl = if matches!(mat.alpha.get(), AlphaMode::Mask) && !alpha_body.trim().is_empty()
    {
        Some(alpha_body.clone())
    } else {
        None
    };
    // CRITICAL: the bridge's register no-op + the registry's idempotency are keyed
    // on `wgsl_hash`, so it MUST cover everything that changes the compiled output
    // — not just the main WGSL. Fold in the alpha mode/cutoff and the alpha-only
    // WGSL, else editing only the cutout (or toggling Mask) is treated as a no-op
    // and the masked variant never (re)builds.
    let wgsl_hash = hash_str(&format!(
        "{main_wgsl}\u{0}alpha_mode={alpha_mode:?}\u{0}alpha_wgsl={alpha_body}"
    ));

    MaterialRegistration {
        // The renderer-internal registration key is the material's stable id
        // (the display name is UI-only); keeps the registry rename-proof and
        // free of duplicate-display-name collisions.
        name: mat.id.to_string(),
        alpha_mode,
        double_sided: mat.double_sided.get(),
        layout,
        layout_hash: layout_hash(mat, &uniforms, &textures, &buffers),
        wgsl_hash,
        wgsl_fragment: main_wgsl,
        buffer_defaults,
        uniform_defaults,
        shader_includes: includes_from_keys(&mat.shader_includes.get_cloned()),
        fragment_inputs: inputs_from_keys(&mat.fragment_inputs.get_cloned()),
        alpha_wgsl,
        // The 3rd ("vertex") WGSL window — wired to the editor's custom-vertex
        // toggle + window in CV3. Until then no material declares one; when it
        // does, fold the body into `wgsl_hash` above so an edit recompiles.
        wgsl_vertex: None,
    }
}

/// Serialize a live `CustomMaterial` into the bundle's serde `MaterialDefinition`
/// (written as `material.json`) — the player rebuilds a `MaterialRegistration`
/// from this + `material.wgsl`. Texture/buffer slot DEFAULTS serialize as `None`
/// (the editor's `CustomMaterial` doesn't carry default bytes); per-mesh texture/
/// buffer overrides still apply at instance time.
pub fn material_definition(mat: &CustomMaterial) -> awsm_editor_protocol::MaterialDefinition {
    use awsm_editor_protocol::dynamic_material::{
        BufferSlot, FieldType as FT, MaterialDefinition, TextureSlot, UniformField,
        UniformValue as UV,
    };
    use awsm_editor_protocol::MaterialAlphaMode;

    let parse_ty = |s: &str| -> FT {
        match s {
            "u32" | "i32" => FT::U32,
            "vec2<f32>" => FT::Vec2,
            "vec3<f32>" => FT::Vec3,
            "vec4<f32>" => FT::Vec4,
            "vec2<i32>" => FT::IVec2,
            "vec3<i32>" => FT::IVec3,
            "vec4<i32>" => FT::IVec4,
            "mat3x3<f32>" => FT::Mat3,
            "mat4x4<f32>" => FT::Mat4,
            "color3" => FT::Color3,
            "color4" => FT::Color4,
            "bool" => FT::Bool,
            _ => FT::F32,
        }
    };
    let parse_val = |ty: FT, s: &str| -> UV {
        let fnums: Vec<f32> = s.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        let inums: Vec<i32> = s.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        let f = |i: usize| fnums.get(i).copied().unwrap_or(0.0);
        let n = |i: usize| inums.get(i).copied().unwrap_or(0);
        match ty {
            FT::F32 => UV::F32(f(0)),
            FT::U32 => UV::U32(s.trim().parse().unwrap_or(0)),
            FT::Vec2 => UV::Vec2([f(0), f(1)]),
            FT::Vec3 => UV::Vec3([f(0), f(1), f(2)]),
            FT::Vec4 => UV::Vec4([f(0), f(1), f(2), f(3)]),
            FT::IVec2 => UV::IVec2([n(0), n(1)]),
            FT::IVec3 => UV::IVec3([n(0), n(1), n(2)]),
            FT::IVec4 => UV::IVec4([n(0), n(1), n(2), n(3)]),
            FT::Mat3 => UV::Mat3(std::array::from_fn(f)),
            FT::Mat4 => UV::Mat4(std::array::from_fn(f)),
            FT::Color3 => UV::Color3([f(0), f(1), f(2)]),
            FT::Color4 => UV::Color4([f(0), f(1), f(2), f(3)]),
            FT::Bool => UV::Bool(matches!(s.trim(), "true" | "1")),
        }
    };
    let alpha_mode = match mat.alpha.get() {
        AlphaMode::Opaque => MaterialAlphaMode::Opaque,
        AlphaMode::Mask => MaterialAlphaMode::Mask {
            cutoff: mat.cutoff.get() as f32,
        },
        AlphaMode::Blend => MaterialAlphaMode::Blend,
    };
    MaterialDefinition {
        name: mat.name.get_cloned(),
        version: 1,
        alpha_mode,
        double_sided: mat.double_sided.get(),
        uniforms: mat
            .uniforms
            .get_cloned()
            .iter()
            .map(|u| {
                let ty = parse_ty(&u.ty);
                UniformField {
                    name: u.name.clone(),
                    ty,
                    default: parse_val(ty, &u.val),
                }
            })
            .collect(),
        textures: mat
            .textures
            .get_cloned()
            .iter()
            .map(|t| TextureSlot {
                name: t.name.clone(),
                default: None,
            })
            .collect(),
        buffers: mat
            .buffers
            .get_cloned()
            .iter()
            .map(|b| BufferSlot {
                name: b.name.clone(),
                default: None,
            })
            .collect(),
        shader_includes: mat.shader_includes.get_cloned(),
        fragment_inputs: mat.fragment_inputs.get_cloned(),
    }
}

fn parse_field_type(s: &str) -> FieldType {
    match s {
        "f32" => FieldType::F32,
        "u32" | "i32" => FieldType::U32,
        "vec2<f32>" => FieldType::Vec2,
        "vec3<f32>" => FieldType::Vec3,
        "vec4<f32>" => FieldType::Vec4,
        "mat3x3<f32>" => FieldType::Mat3,
        "mat4x4<f32>" => FieldType::Mat4,
        _ => FieldType::F32,
    }
}

/// Parse a comma-separated default value string against the field type.
fn parse_uniform_value(ty: FieldType, val: &str) -> UniformValue {
    let nums: Vec<f32> = val
        .split(',')
        .filter_map(|s| s.trim().parse::<f32>().ok())
        .collect();
    let f = |i: usize| nums.get(i).copied().unwrap_or(0.0);
    match ty {
        FieldType::F32 => UniformValue::F32(f(0)),
        FieldType::U32 => UniformValue::U32(val.trim().parse::<u32>().unwrap_or(0)),
        FieldType::Vec2 => UniformValue::Vec2([f(0), f(1)]),
        FieldType::Vec3 => UniformValue::Vec3([f(0), f(1), f(2)]),
        FieldType::Vec4 => UniformValue::Vec4([f(0), f(1), f(2), f(3)]),
        _ => default_value_for(ty),
    }
}

fn default_value_for(ty: FieldType) -> UniformValue {
    match ty {
        FieldType::F32 => UniformValue::F32(0.0),
        FieldType::Vec2 => UniformValue::Vec2([0.0; 2]),
        FieldType::Vec3 => UniformValue::Vec3([0.0; 3]),
        FieldType::Vec4 => UniformValue::Vec4([0.0; 4]),
        FieldType::U32 => UniformValue::U32(0),
        FieldType::IVec2 => UniformValue::IVec2([0; 2]),
        FieldType::IVec3 => UniformValue::IVec3([0; 3]),
        FieldType::IVec4 => UniformValue::IVec4([0; 4]),
        FieldType::Mat3 => UniformValue::Mat3([0.0; 9]),
        FieldType::Mat4 => UniformValue::Mat4([0.0; 16]),
        FieldType::Color3 => UniformValue::Color3([0.0; 3]),
        FieldType::Color4 => UniformValue::Color4([0.0; 4]),
        FieldType::Bool => UniformValue::Bool(false),
    }
}

fn convert_alpha(a: AlphaMode, cutoff: f32) -> MaterialAlphaMode {
    match a {
        AlphaMode::Opaque => MaterialAlphaMode::Opaque,
        AlphaMode::Mask => MaterialAlphaMode::Mask { cutoff },
        AlphaMode::Blend => MaterialAlphaMode::Blend,
    }
}

fn includes_from_keys(keys: &[String]) -> ShaderIncludes {
    let mut s = ShaderIncludes::empty();
    for k in keys {
        // Single source of truth: awsm_materials::ShaderIncludes::KEY_TABLE.
        // Unknown keys dropped; Tier-B keys parse for back-compat but are masked
        // off for custom materials by ShaderIncludeFlags::for_custom.
        s = s.union(ShaderIncludes::from_key(k).unwrap_or_else(ShaderIncludes::empty));
    }
    s
}

fn inputs_from_keys(keys: &[String]) -> FragmentInputs {
    let mut s = FragmentInputs::empty();
    for k in keys {
        let flag = match k.as_str() {
            "normals" => FragmentInputs::NORMALS,
            "tangents" => FragmentInputs::TANGENTS,
            "uv" => FragmentInputs::UV,
            "lights" => FragmentInputs::LIGHTS,
            "view_dir" => FragmentInputs::VIEW_DIR,
            "vertex_color" => FragmentInputs::VERTEX_COLOR,
            _ => FragmentInputs::empty(),
        };
        s = s.union(flag);
    }
    s
}

fn layout_hash(
    mat: &CustomMaterial,
    uniforms: &[crate::controller::Slot],
    textures: &[crate::controller::Slot],
    buffers: &[crate::controller::Slot],
) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    mat.name.get_cloned().hash(&mut h);
    for u in uniforms {
        u.name.hash(&mut h);
        u.ty.hash(&mut h);
    }
    for t in textures {
        t.name.hash(&mut h);
    }
    for b in buffers {
        b.name.hash(&mut h);
    }
    h.finish()
}

fn hash_str(s: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}
