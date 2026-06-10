//! Player-side custom-WGSL material support.
//!
//! The editor authors custom materials (WGSL + a uniform/texture/buffer layout);
//! the bundle exports each as `<folder>/material.json` (a serde
//! [`MaterialDefinition`]) + `<folder>/material.wgsl`. This module rebuilds a
//! renderer [`MaterialRegistration`] from those, registers it, and builds a
//! per-mesh `Material::Custom` — mirroring the editor's `build_registration` /
//! `build_custom`, but from the serialized definition instead of the live
//! `CustomMaterial`.
//!
//! Scope: uniforms (defaults + per-mesh `uniform_overrides`). Texture/buffer
//! slot DEFAULTS and per-mesh texture/buffer OVERRIDES are a follow-on — the
//! editor's CustomMaterial doesn't carry default bytes, so the bundle can't
//! export them yet; slots stay unbound (the renderer falls back at upload time).

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use awsm_materials::dynamic::DynamicMaterial;
use awsm_materials::dynamic_layout::{
    BufferSlotRuntime, FieldType, MaterialLayout, TextureSlotRuntime, UniformFieldRuntime,
    UniformValue,
};
use awsm_materials::{
    FragmentInputs, MaterialAlphaMode as RAlphaMode, MaterialShaderId, ShaderIncludes,
};
use awsm_renderer::dynamic_materials::MaterialRegistration;
use awsm_renderer::materials::Material;
use awsm_renderer::AwsmRenderer;
use awsm_scene::{
    AssetId, FieldType as SFieldType, MaterialAlphaMode as SAlphaMode, MaterialDefinition,
    MaterialInstance, Scene, UniformValue as SUniformValue,
};

/// Register every custom-WGSL material the scene declares, returning a map from
/// the material's `AssetId` (what a node's [`MaterialInstance::asset`] carries)
/// to its registered [`MaterialShaderId`]. A custom material whose folder files
/// are missing or malformed is skipped (its assigned nodes fall back to the
/// magenta placeholder). Compilation is render-driven — the caller's
/// `wait_for_pipelines_ready` (Phase 4) drives it.
pub fn register_custom_materials(
    renderer: &mut AwsmRenderer,
    scene: &Scene,
    assets: &HashMap<String, Vec<u8>>,
) -> HashMap<AssetId, MaterialShaderId> {
    let mut out = HashMap::new();
    for cm in &scene.custom_materials {
        let folder = cm.folder.to_string_lossy();
        let Some(json) = assets.get(&format!("{folder}/material.json")) else {
            continue;
        };
        let Some(wgsl) = assets.get(&format!("{folder}/material.wgsl")) else {
            continue;
        };
        let Ok(def) = serde_json::from_slice::<MaterialDefinition>(json) else {
            continue;
        };
        let wgsl = String::from_utf8_lossy(wgsl).into_owned();
        let reg = registration_from_definition(&cm.id, &def, wgsl);
        if let Ok(shader_id) = renderer.register_material(reg) {
            out.insert(cm.id, shader_id);
        }
    }
    out
}

/// Build a per-mesh `Material::Custom` for a registered custom material, applying
/// the instance's `uniform_overrides` (matched by slot name + type) on top of the
/// registration defaults. Returns `None` if the shader id isn't registered.
/// (Texture/buffer overrides are a follow-on — slots stay unbound.)
pub fn build_custom_material(
    renderer: &AwsmRenderer,
    shader_id: MaterialShaderId,
    inst: &MaterialInstance,
) -> Option<Material> {
    let reg = renderer.dynamic_material_registration(shader_id)?;
    // Snapshot what we need so we don't hold the renderer borrow.
    let alpha_mode = reg.alpha_mode;
    let double_sided = reg.double_sided;
    let texture_count = reg.layout.textures.len();
    let buffer_count = reg.layout.buffers.len();
    let uniforms: Vec<(String, FieldType)> = reg
        .layout
        .uniforms
        .iter()
        .map(|u| (u.name.clone(), u.ty))
        .collect();
    let mut values: Vec<UniformValue> = reg
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

    // Per-mesh uniform overrides (matched by name, type-checked).
    for (i, (name, ty)) in uniforms.iter().enumerate() {
        if let Some(v) = inst.uniform_overrides.get(name) {
            let rv = uniform_value(v);
            if rv.field_type() == *ty {
                values[i] = rv;
            }
        }
    }

    Some(Material::Custom(Box::new(DynamicMaterial {
        shader_id,
        alpha_mode,
        double_sided,
        values,
        textures: vec![None; texture_count],
        buffers: vec![None; buffer_count],
    })))
}

/// Build a [`MaterialRegistration`] from a serialized [`MaterialDefinition`] +
/// its WGSL. Mirrors the editor's `build_registration`, but the def is already
/// typed (no string parsing). The registration `name` is the material's id so
/// the registry keys it stably; the hashes only need to be deterministic per
/// distinct material within this session.
fn registration_from_definition(
    id: &AssetId,
    def: &MaterialDefinition,
    wgsl: String,
) -> MaterialRegistration {
    let layout = MaterialLayout {
        uniforms: def
            .uniforms
            .iter()
            .map(|u| UniformFieldRuntime {
                name: u.name.clone(),
                ty: field_type(u.ty),
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
    };
    let uniform_defaults: Vec<UniformValue> = def
        .uniforms
        .iter()
        .map(|u| uniform_value(&u.default))
        .collect();
    // Buffer defaults aren't exported yet (see module docs); empty per slot.
    let buffer_defaults: Vec<Vec<u32>> = def.buffers.iter().map(|_| Vec::new()).collect();

    MaterialRegistration {
        name: id.0.to_string(),
        alpha_mode: alpha_mode(def.alpha_mode.clone()),
        double_sided: def.double_sided,
        layout_hash: layout_hash(def),
        wgsl_hash: hash_str(&wgsl),
        layout,
        wgsl_fragment: wgsl,
        buffer_defaults,
        uniform_defaults,
        shader_includes: includes_from_keys(&def.shader_includes),
        fragment_inputs: inputs_from_keys(&def.fragment_inputs),
    }
}

fn field_type(t: SFieldType) -> FieldType {
    match t {
        SFieldType::F32 => FieldType::F32,
        SFieldType::Vec2 => FieldType::Vec2,
        SFieldType::Vec3 => FieldType::Vec3,
        SFieldType::Vec4 => FieldType::Vec4,
        SFieldType::U32 => FieldType::U32,
        SFieldType::IVec2 => FieldType::IVec2,
        SFieldType::IVec3 => FieldType::IVec3,
        SFieldType::IVec4 => FieldType::IVec4,
        SFieldType::Mat3 => FieldType::Mat3,
        SFieldType::Mat4 => FieldType::Mat4,
        SFieldType::Color3 => FieldType::Color3,
        SFieldType::Color4 => FieldType::Color4,
        SFieldType::Bool => FieldType::Bool,
    }
}

fn uniform_value(v: &SUniformValue) -> UniformValue {
    match *v {
        SUniformValue::F32(x) => UniformValue::F32(x),
        SUniformValue::Vec2(x) => UniformValue::Vec2(x),
        SUniformValue::Vec3(x) => UniformValue::Vec3(x),
        SUniformValue::Vec4(x) => UniformValue::Vec4(x),
        SUniformValue::U32(x) => UniformValue::U32(x),
        SUniformValue::IVec2(x) => UniformValue::IVec2(x),
        SUniformValue::IVec3(x) => UniformValue::IVec3(x),
        SUniformValue::IVec4(x) => UniformValue::IVec4(x),
        SUniformValue::Mat3(x) => UniformValue::Mat3(x),
        SUniformValue::Mat4(x) => UniformValue::Mat4(x),
        SUniformValue::Color3(x) => UniformValue::Color3(x),
        SUniformValue::Color4(x) => UniformValue::Color4(x),
        SUniformValue::Bool(x) => UniformValue::Bool(x),
    }
}

fn alpha_mode(a: SAlphaMode) -> RAlphaMode {
    match a {
        SAlphaMode::Opaque => RAlphaMode::Opaque,
        SAlphaMode::Mask { cutoff } => RAlphaMode::Mask { cutoff },
        SAlphaMode::Blend => RAlphaMode::Blend,
    }
}

/// A zero/identity value for each field type (the no-default fallback).
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

fn includes_from_keys(keys: &[String]) -> ShaderIncludes {
    let mut s = ShaderIncludes::empty();
    for k in keys {
        s = s.union(match k.as_str() {
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
        });
    }
    s
}

fn inputs_from_keys(keys: &[String]) -> FragmentInputs {
    let mut s = FragmentInputs::empty();
    for k in keys {
        s = s.union(match k.as_str() {
            "normals" => FragmentInputs::NORMALS,
            "tangents" => FragmentInputs::TANGENTS,
            "uv" => FragmentInputs::UV,
            "lights" => FragmentInputs::LIGHTS,
            "view_dir" => FragmentInputs::VIEW_DIR,
            "vertex_color" => FragmentInputs::VERTEX_COLOR,
            _ => FragmentInputs::empty(),
        });
    }
    s
}

fn layout_hash(def: &MaterialDefinition) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    def.name.hash(&mut h);
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
    h.finish()
}

fn hash_str(s: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}
