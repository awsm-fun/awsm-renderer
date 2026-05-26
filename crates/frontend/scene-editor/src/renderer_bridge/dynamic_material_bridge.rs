//! Bridge between `awsm-scene-schema`'s dynamic-material types and the
//! renderer's runtime registration API.
//!
//! `register_loaded_folder` is wired to the Materials pane's Import
//! button. `build_custom_instance` + the related per-mesh converters
//! (`convert_uniform_value`, `default_value_for`, `lookup`) are the
//! public surface the per-mesh property panel will consume once it
//! grows a "Custom" submenu in the material-type chooser. Allow the
//! dead-code lint on those — the methods exist intentionally and
//! removing them would lose the surface.
#![allow(dead_code)]

//!
//! On project load the bridge:
//! 1. Walks `EditorProject.custom_materials`.
//! 2. For each entry, loads the folder via
//!    `awsm_scene_schema::dynamic_material::load_material_folder`
//!    (when feature `fs-loader` is enabled — the browser path uses the
//!    [`register_loaded_folder`] direct entrypoint with bytes loaded
//!    via the File System Access API).
//! 3. Converts each [`LoadedMaterialFolder`] into a
//!    [`MaterialRegistration`] and calls
//!    [`AwsmRenderer::register_material`].
//! 4. Records `name → MaterialShaderId` in a [`CustomMaterialRegistryMap`]
//!    so per-mesh `CustomMaterialInstance` references can resolve to
//!    the renderer-side shader id.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use awsm_materials::dynamic::{DynamicMaterial, DynamicTextureBinding};
use awsm_materials::dynamic_layout::{
    BufferSlotRuntime, FieldType as RuntimeFieldType, MaterialLayout, TextureSlotRuntime,
    UniformFieldRuntime, UniformValue as RuntimeUniformValue,
};
use awsm_materials::MaterialAlphaMode as MaterialAlphaModeRuntime;
use awsm_materials::MaterialShaderId;
use awsm_renderer::dynamic_materials::MaterialRegistration;
use awsm_renderer::materials::Material;
use awsm_renderer::AwsmRenderer;
use awsm_renderer_core::keys::TextureKey;
use awsm_scene_schema::dynamic_material::{
    CustomMaterialInstance, FieldType as SchemaFieldType, LoadedMaterialFolder, MaterialDefinition,
    UniformValue as SchemaUniformValue,
};
use awsm_scene_schema::MaterialAlphaMode as MaterialAlphaModeSchema;

/// `name → MaterialShaderId` lookup populated as the bridge registers
/// custom materials. Stored on the scene's renderer-bridge so per-mesh
/// `CustomMaterialInstance::material` resolves to a registered id.
#[derive(Default, Debug)]
pub struct CustomMaterialRegistryMap {
    map: HashMap<String, MaterialShaderId>,
}

impl CustomMaterialRegistryMap {
    /// Empty map. Populated by repeated calls to [`Self::register`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Records that the material with the given on-disk name resolved
    /// to a renderer-side shader id.
    pub fn register(&mut self, name: impl Into<String>, id: MaterialShaderId) {
        self.map.insert(name.into(), id);
    }

    /// Returns the shader id for a previously-registered material name,
    /// if any.
    pub fn lookup(&self, name: &str) -> Option<MaterialShaderId> {
        self.map.get(name).copied()
    }
}

/// Convert a [`LoadedMaterialFolder`] (the schema-side resolved
/// material) into a [`MaterialRegistration`] (the renderer-side
/// payload) plus register it with the given renderer. Returns the
/// assigned [`MaterialShaderId`].
///
/// On success, the registry-map is updated so subsequent per-mesh
/// `CustomMaterialInstance` references can resolve by name.
pub fn register_loaded_folder(
    renderer: &mut AwsmRenderer,
    map: &mut CustomMaterialRegistryMap,
    folder: &LoadedMaterialFolder,
) -> Result<MaterialShaderId, awsm_renderer::dynamic_materials::AwsmDynamicMaterialError> {
    let definition = &folder.definition;
    let layout = convert_layout(definition);
    let layout_hash = layout_hash_of(definition);
    let wgsl_hash = wgsl_hash_of(&folder.wgsl_source);
    let alpha_mode = convert_alpha_mode(definition.alpha_mode.clone());
    // Build buffer-slot defaults from the LoadedMaterialFolder's
    // resolved buffer_data, in declaration order. Slots without a
    // default (`BufferSlot::default == None`) get an empty Vec.
    let buffer_defaults: Vec<Vec<u32>> = definition
        .buffers
        .iter()
        .map(|slot| {
            slot.default
                .as_ref()
                .and_then(|path| folder.buffer_data.get(path).cloned())
                .unwrap_or_default()
        })
        .collect();
    // Forward the authored uniform defaults so per-mesh
    // `Material::Custom` instances that don't override a slot still
    // pick up the registration-time value (e.g. `tint` defaults to
    // a brand colour) instead of falling back to type-zero.
    let uniform_defaults: Vec<RuntimeUniformValue> = definition
        .uniforms
        .iter()
        .map(|u| convert_uniform_value(&u.default))
        .collect();
    let reg = MaterialRegistration {
        name: definition.name.clone(),
        alpha_mode,
        double_sided: definition.double_sided,
        layout,
        layout_hash,
        wgsl_hash,
        wgsl_fragment: folder.wgsl_source.clone(),
        buffer_defaults,
        uniform_defaults,
    };
    let id = renderer.register_material(reg)?;
    map.register(&definition.name, id);
    Ok(id)
}

/// Build a per-instance `Material::Custom` from a registered
/// [`CustomMaterialInstance`] (per-mesh override layer).
///
/// Returns `None` if the instance's `material` name is unknown — the
/// caller falls back to the mesh's inline / shared material.
///
/// `instance_textures_resolver` translates each
/// `TextureRef` override into a renderer-side `TextureKey`. The
/// editor's existing texture cache supplies this.
pub fn build_custom_instance<F>(
    renderer: &AwsmRenderer,
    map: &CustomMaterialRegistryMap,
    instance: &CustomMaterialInstance,
    mut instance_textures_resolver: F,
) -> Option<Material>
where
    F: FnMut(&awsm_scene_schema::primitive::TextureRef) -> Option<TextureKey>,
{
    let shader_id = map.lookup(&instance.material)?;
    let reg = renderer.dynamic_material_registration(shader_id)?;

    // Build uniform values: prefer per-instance override; fall back
    // to the registration's authored default (when present and type-
    // matching); fall back further to a type-zero default if neither
    // exists. Without the middle tier, a CustomMaterialInstance that
    // doesn't override `tint` would render with the type-zero black
    // instead of the authored brand colour.
    let mut values: Vec<RuntimeUniformValue> = Vec::with_capacity(reg.layout.uniforms.len());
    for (i, uniform) in reg.layout.uniforms.iter().enumerate() {
        let value = instance
            .uniform_overrides
            .get(&uniform.name)
            .map(convert_uniform_value)
            .or_else(|| {
                reg.uniform_defaults
                    .get(i)
                    .cloned()
                    .filter(|v| v.field_type() == uniform.ty)
            })
            .unwrap_or_else(|| default_value_for(uniform.ty));
        values.push(value);
    }

    // Build texture bindings: per-instance override > unbound.
    let mut textures: Vec<Option<DynamicTextureBinding>> =
        Vec::with_capacity(reg.layout.textures.len());
    for tex_slot in &reg.layout.textures {
        let resolved = instance
            .texture_overrides
            .get(&tex_slot.name)
            .and_then(&mut instance_textures_resolver)
            .map(DynamicTextureBinding::Pooled);
        textures.push(resolved);
    }

    // Buffer slots — Phase 6's extras pool wires the actual data; for
    // Phase 5 we record None (the packer writes (0, 0)).
    let buffers: Vec<Option<Vec<u32>>> = vec![None; reg.layout.buffers.len()];

    Some(Material::Custom(Box::new(DynamicMaterial {
        shader_id,
        values,
        textures,
        buffers,
    })))
}

// ─────────────────────────────────────────────────────────────────────
// Converters
// ─────────────────────────────────────────────────────────────────────

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

fn convert_alpha_mode(a: MaterialAlphaModeSchema) -> MaterialAlphaModeRuntime {
    match a {
        MaterialAlphaModeSchema::Opaque => MaterialAlphaModeRuntime::Opaque,
        MaterialAlphaModeSchema::Mask { cutoff } => MaterialAlphaModeRuntime::Mask { cutoff },
        MaterialAlphaModeSchema::Blend => MaterialAlphaModeRuntime::Blend,
    }
}

fn default_value_for(ty: RuntimeFieldType) -> RuntimeUniformValue {
    match ty {
        RuntimeFieldType::F32 => RuntimeUniformValue::F32(0.0),
        RuntimeFieldType::Vec2 => RuntimeUniformValue::Vec2([0.0; 2]),
        RuntimeFieldType::Vec3 => RuntimeUniformValue::Vec3([0.0; 3]),
        RuntimeFieldType::Vec4 => RuntimeUniformValue::Vec4([0.0; 4]),
        RuntimeFieldType::U32 => RuntimeUniformValue::U32(0),
        RuntimeFieldType::IVec2 => RuntimeUniformValue::IVec2([0; 2]),
        RuntimeFieldType::IVec3 => RuntimeUniformValue::IVec3([0; 3]),
        RuntimeFieldType::IVec4 => RuntimeUniformValue::IVec4([0; 4]),
        RuntimeFieldType::Mat3 => RuntimeUniformValue::Mat3([0.0; 9]),
        RuntimeFieldType::Mat4 => RuntimeUniformValue::Mat4([0.0; 16]),
        RuntimeFieldType::Color3 => RuntimeUniformValue::Color3([0.0; 3]),
        RuntimeFieldType::Color4 => RuntimeUniformValue::Color4([0.0; 4]),
        RuntimeFieldType::Bool => RuntimeUniformValue::Bool(false),
    }
}

fn layout_hash_of(def: &MaterialDefinition) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    def.name.hash(&mut h);
    def.version.hash(&mut h);
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

fn wgsl_hash_of(src: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    src.hash(&mut h);
    h.finish()
}
