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
//! Scope: uniforms (defaults + per-mesh `uniform_overrides`), per-mesh
//! `texture_overrides` (the bake emits each as `assets/<id>.png`), and per-mesh
//! `buffer_overrides` (the bake emits each as `assets/<asset>.bin` of
//! little-endian u32 words) — all bound here like the editor's `build_custom`.
//! Texture/buffer slots with no per-mesh override stay unbound (correct: an
//! unbound texture samples transparent-black; there is no "default texture"
//! concept — only uniforms have authored defaults).

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use awsm_renderer::dynamic_materials::MaterialRegistration;
use awsm_renderer::materials::Material;
use awsm_renderer::AwsmRenderer;
use awsm_renderer_core::texture::mipmap::MipmapTextureKind;
use awsm_renderer_materials::dynamic::{DynamicMaterial, DynamicTextureBinding};
use awsm_renderer_materials::dynamic_layout::{
    BufferSlotRuntime, FieldType, MaterialLayout, TextureSlotRuntime, UniformFieldRuntime,
    UniformValue,
};
use awsm_renderer_materials::{
    FragmentInputs, MaterialAlphaMode as RAlphaMode, MaterialShaderId, ShaderIncludes,
};
use awsm_renderer_scene::{
    AssetId, FieldType as SFieldType, MaterialAlphaMode as SAlphaMode, MaterialDefinition,
    MaterialInstance, Scene, UniformValue as SUniformValue, ASSETS_DIR,
};

use crate::assets::SceneAssets;

/// Register every custom-WGSL material the scene declares, returning a map from
/// the material's `AssetId` (what a node's [`MaterialInstance::asset`] carries)
/// to its registered [`MaterialShaderId`]. A custom material whose folder files
/// are missing or malformed is skipped (its assigned nodes fall back to the
/// magenta placeholder). Compilation is render-driven — the caller's
/// `wait_for_pipelines_ready` (Phase 4) drives it.
pub async fn register_custom_materials(
    renderer: &mut AwsmRenderer,
    scene: &Scene,
    assets: &impl SceneAssets,
) -> HashMap<AssetId, MaterialShaderId> {
    let mut out = HashMap::new();
    for cm in &scene.custom_materials {
        let folder = cm.folder.to_string_lossy();
        // Every failure below is LOUD: a declared custom material that doesn't
        // register means its meshes silently render through their (meaningless
        // for a custom assignment) inline defs instead of the authored shader.
        // Not fatal only because pre-fix bundles listed BUILT-IN materials here
        // with folders that were never written (those legitimately fail the
        // fetch and correctly fall through to the builtin/inline path).
        let json = match assets.fetch(&format!("{folder}/material.json")).await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(
                    "custom material `{}` ({}): fetch {folder}/material.json failed ({e}) — \
                     meshes assigned to it will render via their inline def",
                    cm.name,
                    cm.id
                );
                continue;
            }
        };
        let wgsl = match assets.fetch(&format!("{folder}/material.wgsl")).await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(
                    "custom material `{}` ({}): fetch {folder}/material.wgsl failed ({e})",
                    cm.name,
                    cm.id
                );
                continue;
            }
        };
        let def = match serde_json::from_slice::<MaterialDefinition>(&json) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(
                    "custom material `{}` ({}): {folder}/material.json didn't parse ({e}) — \
                     is the server returning an HTML fallback for missing files?",
                    cm.name,
                    cm.id
                );
                continue;
            }
        };
        let wgsl = String::from_utf8_lossy(&wgsl).into_owned();
        // Optional 2nd alpha-only WGSL window (masked cutouts). Absent for
        // opaque/blend materials and older bundles → no cutout (back-compat).
        let alpha_wgsl = assets
            .fetch(&format!("{folder}/material.alpha.wgsl"))
            .await
            .ok()
            .map(|b| String::from_utf8_lossy(&b).into_owned());
        let reg = registration_from_definition(&cm.id, &def, wgsl, alpha_wgsl);
        match renderer.register_material(reg) {
            Ok(shader_id) => {
                out.insert(cm.id, shader_id);
            }
            Err(e) => {
                tracing::warn!(
                    "custom material `{}` ({}): register/compile failed ({e})",
                    cm.name,
                    cm.id
                );
            }
        }
    }
    out
}

/// Build a per-mesh `Material::Custom` for a registered custom material, applying
/// the instance's `uniform_overrides` (by name + type), `texture_overrides` (each
/// `assets/<id>.png` decoded + staged), and `buffer_overrides` (each
/// `assets/<asset>.bin` of little-endian u32 words) — exactly like the
/// editor's `build_custom`. Returns `None` if the shader id isn't registered.
/// Slots without an override stay unbound (no "default texture" concept).
pub async fn build_custom_material(
    renderer: &mut AwsmRenderer,
    shader_id: MaterialShaderId,
    inst: &MaterialInstance,
    assets: &impl SceneAssets,
) -> Option<Material> {
    // Snapshot everything we need from the registration up front, then DROP the
    // borrow — binding textures below needs `&mut renderer`.
    let (alpha_mode, double_sided, texture_slots, buffer_slots, uniforms, mut values) = {
        let reg = renderer.dynamic_material_registration(shader_id)?;
        let uniforms: Vec<(String, FieldType)> = reg
            .layout
            .uniforms
            .iter()
            .map(|u| (u.name.clone(), u.ty))
            .collect();
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
        let texture_slots: Vec<String> =
            reg.layout.textures.iter().map(|t| t.name.clone()).collect();
        let buffer_slots: Vec<String> = reg.layout.buffers.iter().map(|b| b.name.clone()).collect();
        (
            reg.alpha_mode,
            reg.double_sided,
            texture_slots,
            buffer_slots,
            uniforms,
            values,
        )
    };

    // Per-mesh uniform overrides (matched by name, type-checked).
    for (i, (name, ty)) in uniforms.iter().enumerate() {
        if let Some(v) = inst.uniform_overrides.get(name) {
            let rv = uniform_value(v);
            if rv.field_type() == *ty {
                values[i] = rv;
            }
        }
    }

    // Per-mesh texture overrides → pooled bindings (slot order). A custom texture
    // is treated as color data (srgb + albedo mips), mirroring the editor's
    // `resolve_texture_binding`. An override whose texture isn't in the bundle (or
    // fails to decode) leaves the slot unbound.
    let mut textures: Vec<Option<DynamicTextureBinding>> = vec![None; texture_slots.len()];
    for (i, name) in texture_slots.iter().enumerate() {
        if let Some(tref) = inst.texture_overrides.get(name) {
            if let Some(mt) = crate::texture::load_texture(
                renderer,
                assets,
                tref,
                true,
                MipmapTextureKind::Albedo,
            )
            .await
            {
                if let Some(sampler) = mt.sampler_key {
                    textures[i] = Some(DynamicTextureBinding::Pooled {
                        texture: mt.key,
                        sampler,
                    });
                }
            }
        }
    }

    // Per-mesh buffer overrides (slot order): the bake emitted each as
    // `assets/<asset>.bin` (little-endian u32 words), keyed by the override's
    // asset id — exactly like a texture's `assets/<asset>.png`. Read it back into
    // the slot; an override whose `.bin` is absent leaves the slot unbound.
    let mut buffers: Vec<Option<Vec<u32>>> = vec![None; buffer_slots.len()];
    for (i, name) in buffer_slots.iter().enumerate() {
        if let Some(bref) = inst.buffer_overrides.get(name) {
            let path = format!("{ASSETS_DIR}/{}.bin", bref.asset);
            if let Ok(bytes) = assets.fetch(&path).await {
                buffers[i] = Some(
                    bytes
                        .chunks_exact(4)
                        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                        .collect(),
                );
            } else {
                tracing::warn!("scene-loader: bundle missing buffer `{path}` — slot left unbound");
            }
        }
    }

    Some(Material::Custom(Box::new(DynamicMaterial {
        shader_id,
        alpha_mode,
        double_sided,
        values,
        textures,
        buffers,
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
    alpha_wgsl: Option<String>,
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
        // 2nd alpha-only WGSL window for masked cutouts, loaded from the
        // `material.alpha.wgsl` sidecar (parallel to `material.wgsl`). Empty /
        // whitespace-only → None (no cutout); a non-empty window compiles into
        // the masked visibility-raster variant so the player rebuilds the
        // cutout just like the editor.
        alpha_wgsl: alpha_wgsl.filter(|s| !s.trim().is_empty()),
        // The player-side custom-vertex sidecar (a `material.vertex.wgsl`,
        // parallel to `material.wgsl`) is wired once the editor export emits it
        // (CV3). `None` until then → the material uses the shared fast pipeline.
        wgsl_vertex: None,
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
        // Single source of truth: awsm_renderer_materials::ShaderIncludes::KEY_TABLE.
        // Unknown keys are dropped; Tier-B keys still parse for back-compat but
        // are masked off for custom materials by ShaderIncludeFlags::for_custom.
        s = s.union(ShaderIncludes::from_key(k).unwrap_or_else(ShaderIncludes::empty));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_def() -> MaterialDefinition {
        MaterialDefinition {
            name: "m".into(),
            version: 1,
            alpha_mode: SAlphaMode::Mask { cutoff: 0.5 },
            double_sided: false,
            uniforms: vec![],
            textures: vec![],
            buffers: vec![],
            shader_includes: vec![],
            fragment_inputs: vec![],
        }
    }

    /// The `material.alpha.wgsl` sidecar threads into the rebuilt registration:
    /// a non-empty window survives (the player can rebuild the masked cutout),
    /// while absent / whitespace-only collapses to `None` (no cutout) — the
    /// round-trip the player previously dropped (TODO removed).
    #[test]
    fn alpha_wgsl_sidecar_threads_into_registration() {
        let id = AssetId::new();
        let wgsl = "fn frag() {}".to_string();

        let present = registration_from_definition(
            &id,
            &minimal_def(),
            wgsl.clone(),
            Some("fn custom_alpha() -> f32 { return 1.0; }".into()),
        );
        assert_eq!(
            present.alpha_wgsl.as_deref(),
            Some("fn custom_alpha() -> f32 { return 1.0; }"),
            "non-empty alpha sidecar must reach the registration"
        );

        let absent = registration_from_definition(&id, &minimal_def(), wgsl.clone(), None);
        assert!(absent.alpha_wgsl.is_none(), "no sidecar → no cutout");

        let blank = registration_from_definition(&id, &minimal_def(), wgsl, Some("  \n\t ".into()));
        assert!(
            blank.alpha_wgsl.is_none(),
            "whitespace-only sidecar collapses to None"
        );
    }

    // ── layout_hash: identity of the BINDING layout only ─────────────────────
    //
    // layout_hash is one dimension of the registry's
    // `(shader_id, name, layout_hash, wgsl_hash)` idempotency key. It captures
    // what the auto-generated `MaterialData` struct looks like — uniform
    // names+types, texture slot names, buffer slot names — NOT uniform values,
    // render-state (alpha_mode / double_sided), or the WGSL source (those live
    // in other key dimensions, chiefly wgsl_hash). These tests pin exactly that
    // boundary, so an authored uniform-value tweak stays a cheap no-recompile
    // update while a real layout change forces a distinct registration.

    fn uni(
        name: &str,
        ty: SFieldType,
        default: SUniformValue,
    ) -> awsm_renderer_scene::UniformField {
        awsm_renderer_scene::UniformField {
            name: name.into(),
            ty,
            default,
        }
    }

    #[test]
    fn layout_hash_is_deterministic() {
        assert_eq!(
            layout_hash(&minimal_def()),
            layout_hash(&minimal_def()),
            "same definition must hash identically"
        );
    }

    #[test]
    fn layout_hash_tracks_material_name() {
        let mut renamed = minimal_def();
        renamed.name = "other".into();
        assert_ne!(layout_hash(&minimal_def()), layout_hash(&renamed));
    }

    #[test]
    fn layout_hash_ignores_uniform_value_but_tracks_type() {
        let mut base = minimal_def();
        base.uniforms = vec![uni("k", SFieldType::F32, SUniformValue::F32(1.0))];

        // Same name+type, different default VALUE → layout unchanged (a uniform
        // edit is a no-recompile uniform update, not a pipeline rebuild).
        let mut val_changed = minimal_def();
        val_changed.uniforms = vec![uni("k", SFieldType::F32, SUniformValue::F32(9.0))];
        assert_eq!(
            layout_hash(&base),
            layout_hash(&val_changed),
            "uniform value edit must NOT change the layout hash"
        );

        // Same name, different TYPE → layout changed (recompile).
        let mut ty_changed = minimal_def();
        ty_changed.uniforms = vec![uni("k", SFieldType::Vec4, SUniformValue::Vec4([0.0; 4]))];
        assert_ne!(
            layout_hash(&base),
            layout_hash(&ty_changed),
            "uniform type change MUST change the layout hash"
        );
    }

    #[test]
    fn layout_hash_tracks_uniform_presence_and_name() {
        let mut added = minimal_def();
        added.uniforms = vec![uni("k", SFieldType::F32, SUniformValue::F32(0.0))];
        assert_ne!(
            layout_hash(&minimal_def()),
            layout_hash(&added),
            "adding a uniform changes the layout"
        );

        let mut renamed = minimal_def();
        renamed.uniforms = vec![uni("k2", SFieldType::F32, SUniformValue::F32(0.0))];
        assert_ne!(
            layout_hash(&added),
            layout_hash(&renamed),
            "renaming a uniform changes the layout"
        );
    }

    #[test]
    fn layout_hash_tracks_textures_and_buffers() {
        use awsm_renderer_scene::{BufferSlot, TextureSlot};

        let mut with_tex = minimal_def();
        with_tex.textures = vec![TextureSlot {
            name: "albedo".into(),
            default: None,
        }];
        assert_ne!(
            layout_hash(&minimal_def()),
            layout_hash(&with_tex),
            "adding a texture slot changes the layout"
        );

        let mut with_buf = minimal_def();
        with_buf.buffers = vec![BufferSlot {
            name: "data".into(),
            default: None,
        }];
        assert_ne!(
            layout_hash(&minimal_def()),
            layout_hash(&with_buf),
            "adding a buffer slot changes the layout"
        );
    }

    #[test]
    fn layout_hash_ignores_render_state_and_includes() {
        // alpha_mode / double_sided / shader_includes are not part of the
        // binding layout (they affect routing + WGSL, captured elsewhere).
        let mut changed = minimal_def();
        changed.alpha_mode = SAlphaMode::Blend;
        changed.double_sided = true;
        changed.shader_includes = vec!["brdf".into(), "shadows".into()];
        assert_eq!(
            layout_hash(&minimal_def()),
            layout_hash(&changed),
            "render-state / include changes do not alter the layout hash"
        );
    }

    // ── default_value_for: the zero/identity table ───────────────────────────

    #[test]
    fn default_value_for_is_zeroed_per_type() {
        assert_eq!(default_value_for(FieldType::F32), UniformValue::F32(0.0));
        assert_eq!(default_value_for(FieldType::U32), UniformValue::U32(0));
        assert_eq!(
            default_value_for(FieldType::Vec4),
            UniformValue::Vec4([0.0; 4])
        );
        assert_eq!(
            default_value_for(FieldType::Mat4),
            UniformValue::Mat4([0.0; 16])
        );
        assert_eq!(
            default_value_for(FieldType::Bool),
            UniformValue::Bool(false)
        );
    }
}
