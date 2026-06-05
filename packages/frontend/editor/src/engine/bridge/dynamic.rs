//! Real GPU registration for custom WGSL materials (decision 3). Converts a
//! [`CustomMaterial`] (the Studio's reactive model) into the renderer's
//! [`MaterialRegistration`] and registers it, so an assigned mesh can resolve
//! the material by name to a registered `MaterialShaderId` and render the WGSL.
//!
//! Adapted from the archived `renderer_bridge/dynamic_material_bridge.rs`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use awsm_materials::dynamic::DynamicMaterial;
use awsm_materials::dynamic_layout::{
    BufferSlotRuntime, FieldType, MaterialLayout, TextureSlotRuntime, UniformFieldRuntime,
    UniformValue,
};
use awsm_materials::{FragmentInputs, MaterialAlphaMode, MaterialShaderId, ShaderIncludes};
use awsm_renderer::dynamic_materials::MaterialRegistration;
use awsm_renderer::materials::Material;
use awsm_renderer::AwsmRenderer;

use awsm_scene_schema::dynamic_material::{
    CustomMaterialInstance, UniformValue as SceneUniformValue,
};

use crate::controller::{AlphaMode, CustomMaterial};
use crate::engine::context::renderer_handle;
use crate::engine::scene::AssetId;

thread_local! {
    /// `material id → registered shader id`, populated by [`register`]. Keyed by
    /// the material's **stable id** (not its display name) so renaming a material
    /// never orphans an assigned mesh's resolution.
    static REGISTRY: RefCell<HashMap<AssetId, MaterialShaderId>> = RefCell::new(HashMap::new());
}

fn registered_shader_id(id: AssetId) -> Option<MaterialShaderId> {
    REGISTRY.with(|r| r.borrow().get(&id).copied())
}

/// Register a custom material with the renderer (locks it, finalizes textures).
/// Returns the assigned shader id, or an error string on failure.
pub async fn register(mat: &CustomMaterial) -> Result<MaterialShaderId, String> {
    let reg = build_registration(mat);
    let mat_id = mat.id;
    let handle = renderer_handle();
    let mut r = handle.lock().await;
    // Recompile: the renderer rejects re-registering a key whose content changed,
    // so drop this material's previous registration first (the editor's
    // edit→re-register cycle `unregister_material` expects). Keyed by id, so a
    // rename is just a display change — the registration key is unaffected.
    if let Some(old) = REGISTRY.with(|reg| reg.borrow().get(&mat_id).copied()) {
        let _ = r.unregister_material(old);
    }
    let shader_id = r.register_material(reg).map_err(|e| format!("{e}"))?;
    if let Err(e) = r.finalize_gpu_textures().await {
        tracing::warn!("finalize after register: {e}");
    }
    REGISTRY.with(|reg| reg.borrow_mut().insert(mat_id, shader_id));
    Ok(shader_id)
}

/// Build + insert a `Material::Custom` for an assigned custom material `name`,
/// returning its `MaterialKey`. `None` if `name` isn't registered (the caller
/// falls back to the mesh's inline material). Mirrors `material::insert_material`'s
/// disjoint-field borrow so it composes with the renderer lock.
pub fn insert_custom(
    renderer: &mut AwsmRenderer,
    inst: &CustomMaterialInstance,
) -> Option<awsm_renderer::materials::MaterialKey> {
    let material = build_custom(renderer, inst)?;
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
fn build_custom(renderer: &AwsmRenderer, inst: &CustomMaterialInstance) -> Option<Material> {
    let mut material = build_custom_for_shader(renderer, registered_shader_id(inst.material)?)?;
    if !inst.uniform_overrides.is_empty() {
        if let Material::Custom(dm) = &mut material {
            if let Some(reg) = renderer.dynamic_material_registration(dm.shader_id) {
                for (i, field) in reg.layout.uniforms.iter().enumerate() {
                    if let Some(v) = inst.uniform_overrides.get(&field.name) {
                        let rv = scene_to_renderer(v);
                        // Type-check so a stale override (material edited after
                        // assignment) can't poison the uniform buffer.
                        if rv.field_type() == field.ty {
                            if let Some(slot) = dm.values.get_mut(i) {
                                *slot = rv;
                            }
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
/// stores in `CustomMaterialInstance::uniform_overrides` (#4.2).
pub fn slot_default_value(slot: &crate::controller::Slot) -> SceneUniformValue {
    renderer_to_scene(&parse_uniform_value(parse_field_type(&slot.ty), &slot.val))
}

/// Convert the serializable schema `UniformValue` (stored on a
/// `CustomMaterialInstance`) into the renderer's value type. The two enums have
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

    MaterialRegistration {
        // The renderer-internal registration key is the material's stable id
        // (the display name is UI-only); keeps the registry rename-proof and
        // free of duplicate-display-name collisions.
        name: mat.id.to_string(),
        alpha_mode: convert_alpha(mat.alpha.get(), mat.cutoff.get() as f32),
        double_sided: mat.double_sided.get(),
        layout,
        layout_hash: layout_hash(mat, &uniforms, &textures, &buffers),
        wgsl_hash: hash_str(&mat.wgsl.get_cloned()),
        wgsl_fragment: mat.wgsl.get_cloned(),
        buffer_defaults,
        uniform_defaults,
        shader_includes: includes_from_keys(&mat.shader_includes.get_cloned()),
        fragment_inputs: inputs_from_keys(&mat.fragment_inputs.get_cloned()),
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
        let flag = match k.as_str() {
            "math" => ShaderIncludes::MATH,
            "camera" => ShaderIncludes::CAMERA,
            "color_space" => ShaderIncludes::COLOR_SPACE,
            "textures" => ShaderIncludes::TEXTURES,
            "vertex_color" => ShaderIncludes::VERTEX_COLOR,
            "light_access" => ShaderIncludes::LIGHT_ACCESS,
            "apply_lighting" => ShaderIncludes::APPLY_LIGHTING,
            "brdf" => ShaderIncludes::BRDF,
            "material_color_calc" => ShaderIncludes::MATERIAL_COLOR_CALC,
            "shadows" => ShaderIncludes::SHADOWS,
            "skybox" => ShaderIncludes::SKYBOX,
            "extras" => ShaderIncludes::EXTRAS,
            _ => ShaderIncludes::empty(),
        };
        s = s.union(flag);
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
