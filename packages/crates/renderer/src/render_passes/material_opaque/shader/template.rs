//! Shader templates for the opaque material pass.

use askama::Template;
use awsm_renderer_materials::MaterialShaderId;

use crate::dynamic_materials::ShadingBase;
use crate::{
    render_passes::material_opaque::shader::cache_key::ShaderCacheKeyMaterialOpaque,
    shaders::{AwsmShaderError, Result},
};

/// Opaque material shader template components.
#[derive(Debug)]
pub struct ShaderTemplateMaterialOpaque {
    pub bind_groups: ShaderTemplateMaterialOpaqueBindGroups,
    pub compute: ShaderTemplateMaterialOpaqueCompute,
}

/// Bind group template for the opaque material pass.
#[derive(Template, Debug)]
#[template(
    path = "material_opaque_wgsl/bind_groups.wgsl",
    whitespace = "minimize"
)]
pub struct ShaderTemplateMaterialOpaqueBindGroups {
    pub texture_pool_arrays_len: u32,
    pub texture_pool_samplers_len: u32,
    pub debug: ShaderTemplateMaterialOpaqueDebug,
    pub mipmap: MipmapMode,
    pub multisampled_geometry: bool,
    pub msaa_sample_count: u32, // 0 if no MSAA
    /// Bind-group slot index the shadow declarations should occupy.
    /// Opaque pipelines use slot 3; the field exists so the same
    /// `shared_wgsl/shadow/bind_groups.wgsl` include can be reused by
    /// the transparent pipeline (slot 1).
    pub shadow_group_index: u32,
    /// Registry bucket list — drives the templated `ClassifyBuckets`
    /// struct emit, must match the classify-pass writer's struct
    /// byte-for-byte.
    pub bucket_entries: Vec<crate::dynamic_materials::BucketEntry>,
    /// Trailing alignment-pad u32 indices for `ClassifyBuckets`,
    /// mirrors the classify-pass template's `pad_words_iter`.
    pub pad_words_iter: Vec<u32>,
    /// Effective SSCS gate = pass-capability (the opaque pass has `depth_tex`
    /// bound) AND the global `ShadowsConfig::sscs_enabled`. When `false`,
    /// `apply_sscs` short-circuits to `return 1.0` at compile time (zero cost).
    pub sscs_available: bool,
    /// SSCS ray-march step count baked as the `apply_sscs` loop bound
    /// (compile-time constant). Only read when `sscs_available`.
    pub sscs_step_count: u32,
    /// Whether the ~50 KB shadow SAMPLING block in
    /// `shared_wgsl/shadow/bind_groups.wgsl` is emitted. Set from
    /// `inc.apply_lighting` (the only caller of `sample_shadow_*`), so
    /// materials that don't run first-party lighting drop it. The shadow
    /// bind group + structs are always emitted (ABI) regardless.
    pub needs_shadow_sampling: bool,
    /// Emit the cascade-debug overlay (`debug_cascade_tint` +
    /// `debug_picked_cascade`) in `shared_wgsl/shadow/bind_groups.wgsl`. Set
    /// from `inc.apply_lighting` (the only caller — the overlay is a
    /// shading-time colour op), NOT from `needs_shadow_sampling`: opaque
    /// compiles with the sampler block dropped (prep reads the shadow buffer)
    /// but must still carry the overlay, or `debug_cascade_colors` changes
    /// zero pixels. The overlay only reads the always-bound shadow uniforms.
    pub needs_cascade_debug: bool,
    /// Plan B: always `true` for the opaque pass (prep is unconditional);
    /// the shared transparent template sets it `false`. When true, the gated
    /// `prep_uv` / `prep_vcolor` / `prep_shadow_visibility` sampled
    /// `texture_2d_array<f32>` declarations are emitted in the MAIN bind
    /// group so the shared `texture_uv()` / `vertex_color()` / shadow
    /// helpers can `textureLoad` them (`cs_opaque` reads prep under any AA).
    pub prep_present: bool,
    /// Depth convention (003) — read by the shared SSCS body in
    /// `shared_wgsl/shadow/bind_groups.wgsl`.
    pub reverse_z: bool,
}

/// Compute shader template for the opaque material pass.
#[derive(Template, Debug)]
#[template(path = "material_opaque_wgsl/compute.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateMaterialOpaqueCompute {
    pub texture_pool_arrays_len: u32,
    pub texture_pool_samplers_len: u32,
    pub debug: ShaderTemplateMaterialOpaqueDebug,
    pub mipmap: MipmapMode,
    pub multisampled_geometry: bool,
    pub msaa_sample_count: u32, // 0 if no MSAA
    /// Whether to wire shadow sampling into `apply_lighting`. Opaque
    /// is always `true`; the empty / transparent templates leave it
    /// `false` until they pull in the shadow bind-group declarations
    /// themselves.
    pub shadows_enabled: bool,
    /// Switch the punctual-light walk to the per-mesh slice fed by
    /// `mesh_light_slices` + `mesh_light_indices`. Opaque is true;
    /// transparent stays false.
    /// When `true`, the shared `lights.wgsl` emits the
    /// `apply_lighting_per_froxel*` helpers. Opaque sets this `true` —
    /// all opaque shading reads the per-pixel froxel light list produced
    /// by the GPU cull pass (see `compute.wgsl`).
    pub use_froxel_lights: bool,
    /// Number of view-space Z slices in the cull grid. Constant-
    /// folded into the per-pixel froxel-index calc that the gated
    /// `apply_lighting_per_froxel*` helpers contain.
    pub froxel_slice_count: u32,
    /// Concatenated `wgsl_fragment()` of every enabled material — see
    /// `awsm_renderer_materials::registry::build_materials_wgsl`.
    pub materials_wgsl: String,
    /// Generated `const SHADER_ID_X: u32 = N;` lines — see
    /// `awsm_renderer_materials::registry::build_shader_id_consts`.
    pub shader_id_consts: String,
    /// Which material shader_id this specialized pipeline handles.
    /// The compute.wgsl template renders only the matching material's
    /// shading code (PBR / Unlit / Toon / FlipBook / a registered
    /// dynamic material), with a per-pixel guard early-returning on
    /// mismatch so a full-screen dispatch is correct even before
    /// classify+indirect lands.
    pub shader_id: MaterialShaderId,
    /// Which built-in shading family's body this pipeline emits
    /// (`{% if base == ShadingBase::Pbr %}` etc.). Decoupled from
    /// `shader_id` so a per-feature-set PBR variant whose id is in the
    /// dynamic range still emits the PBR path. The per-pixel guard uses
    /// the numeric `shader_id` regardless of `base`.
    pub base: crate::dynamic_materials::ShadingBase,
    /// Which optional shared modules this pipeline's material declares (skinny
    /// materials). The host gates heavy
    /// PBR-only includes (brdf / apply_lighting) behind these so non-PBR
    /// pipelines don't compile them.
    pub inc: crate::dynamic_materials::ShaderIncludeFlags,
    /// Whether this pipeline owns the skybox write (only the dedicated SKYBOX
    /// bucket; see [`ShaderCacheKeyMaterialOpaque::owns_skybox`]).
    pub owns_skybox: bool,
    /// PBR feature set this specialized pipeline is compiled for. The
    /// compute template + `material_color_calc.wgsl` gate per-feature code
    /// behind `{% if pbr_features.<x> %}`, so an unused feature (no
    /// clearcoat in the scene, etc.) emits no code.
    /// The empty set for non-PBR ids and the SKYBOX bucket — inert for the
    /// former (their body doesn't read it) and the minimal skybox-only shader
    /// for the latter. Never the full "uber" set.
    pub pbr_features: awsm_renderer_materials::pbr::PbrFeatures,
    /// M2a: emit the per-base reflectance computation + the
    /// `reflection_descriptor_tex` stores. Gated so an SSR-off kernel computes
    /// and stores nothing. See
    /// [`ShaderCacheKeyMaterialOpaque::write_ssr_descriptor`].
    pub write_ssr_descriptor: bool,
    /// For dynamic shader ids: the auto-generated `struct
    /// MaterialData { ... }` declaration emitted above the author's
    /// WGSL fragment. Empty string for first-party ids.
    pub dynamic_struct_decl: String,
    /// For dynamic shader ids: the auto-generated
    /// `fn material_data_load(byte_offset: u32) -> MaterialData`
    /// accessor. Empty string for first-party ids.
    pub dynamic_loader_decl: String,
    /// For dynamic shader ids: the author's WGSL fragment (wrapped at
    /// template render time into
    /// `fn custom_shade_<id>(...) -> OpaqueShadingOutput { <body> }`).
    /// Empty string for first-party ids.
    pub dynamic_wgsl_fragment: String,
    /// Registry bucket list — used by the per-shader-id
    /// `bucket_offset` lookup chain so dynamic shader_ids can resolve
    /// their `classify_buckets.<name>_offset` field. Mirrors the
    /// classify-pass template's `bucket_entries`.
    pub bucket_entries: Vec<crate::dynamic_materials::BucketEntry>,
    /// Bucket index this shader_id occupies inside `bucket_entries`
    /// (§ Part B — the unified module). Hard-coded into the merged
    /// `cs_edge` entry point's `{{ bucket_index }}u` slot-match in the
    /// slot_map scan + the per-shader entry-count read — same as the
    /// former standalone `edge_resolve.wgsl`. Resolved from the
    /// shader_id's position in `bucket_entries` at template-render time
    /// (0 for the SKYBOX bucket, which renders `skybox_primary` and so
    /// never emits `cs_edge`).
    pub bucket_index: u32,
    /// Edge `edge_slot_map` packing width (8 or 16), §5 — derived from
    /// the live bucket count. Gates the `cs_edge` slot_map read between
    /// one u32/edge (8-bit, ≤254 buckets) and two u32/edge (16-bit, for over
    /// 254 buckets). Only consumed inside the `{% if multisampled_geometry %}`
    /// `cs_edge` block; inert on the singlesampled module.
    pub edge_slot_bits: u32,
    /// Plan B: always `true` for opaque (prep is unconditional); transparent
    /// sets it `false`. When true, the shared `PrepReadContext` (`g_prep_ctx`)
    /// is emitted, each entry point sets its mode (cs_opaque → PRIMARY,
    /// cs_edge → RECOMPUTE), and the shared `texture_uv()` / `vertex_color()` /
    /// shadow helpers branch on `g_prep_ctx.mode` to read the prep-materialized
    /// array textures (PRIMARY) instead of recomputing (`cs_opaque` reads prep
    /// under any AA). The recompute body stays available because
    /// cs_edge=RECOMPUTE falls through to it.
    pub prep_present: bool,
    /// Plan B: `msaa none` (prep is always on for opaque). When true, the
    /// standalone `_texture_uv_per_vertex` / `_vertex_color_per_vertex`
    /// recompute helpers are NOT emitted (the 2b size win) — there is no
    /// `cs_edge` in the no-MSAA module, so nothing recomputes. Under MSAA
    /// these helpers STAY (cs_edge=RECOMPUTE needs them). (The gradient-mips /
    /// custom exceptions in the helper templates still apply.)
    pub prep_drops_recompute: bool,
    /// `MAX_PREP_UV_SETS` — clamp cap for the prep UV array layer index
    /// in `texture_uv()` when `prep_read`. Inert otherwise.
    pub max_prep_uv_sets: u32,
    /// `MAX_PREP_COLOR_SETS` — clamp cap for the prep vcolor array layer
    /// index in `vertex_color()` when `prep_read`. Inert otherwise.
    pub max_prep_color_sets: u32,
    /// Plan B (stage 5a): when true, the shared `sample_shadow_*` inline
    /// shadow-sampling block compiles (the legacy path). Mirrors the
    /// bind-group template's field of the same name so `apply_lighting.wgsl`
    /// (in the compute include) can gate its inline `else` arm — under
    /// MSAA+prep cs_edge=RECOMPUTE needs the inline sampler, so it stays;
    /// under no-MSAA+prep cs_opaque=PRIMARY only, so it drops. Derived
    /// `inc.apply_lighting && !prep_drops_recompute`.
    pub needs_shadow_sampling: bool,
    /// `K` — the per-pixel shadow-caster cap (`PrepPassConfig::clamped_k`),
    /// matching the prep buffer's layer/slot count. Consumed by the
    /// `{% if shadow_from_buffer %}` read path's `prep_shadow_read` bounds
    /// check. Inert otherwise.
    pub max_shadow_casters: u32,
    /// Plan B (stage 5b-shadow): fixed width of the compact per-edge-sample
    /// shadow texture. cs_edge maps `edge_pixel_id * MAX_SAMPLES + sample` to
    /// `(idx % W, idx / W)`; MUST match material_prep's `EDGE_SHADOW_TEX_WIDTH`.
    /// Only used inside the `{% if multisampled_geometry %}` EDGE-mode read.
    pub edge_shadow_tex_width: u32,
}

impl ShaderTemplateMaterialOpaqueCompute {
    /// Returns true if the shader includes IBL lighting.
    pub fn has_lighting_ibl(&self) -> bool {
        match self.debug.lighting {
            ShaderTemplateMaterialOpaqueDebugLighting::None => true,
            ShaderTemplateMaterialOpaqueDebugLighting::IblOnly => true,
            ShaderTemplateMaterialOpaqueDebugLighting::PunctualOnly => false,
        }
    }

    /// Returns true if the shader includes punctual lighting.
    pub fn has_lighting_punctual(&self) -> bool {
        match self.debug.lighting {
            ShaderTemplateMaterialOpaqueDebugLighting::None => true,
            ShaderTemplateMaterialOpaqueDebugLighting::IblOnly => false,
            ShaderTemplateMaterialOpaqueDebugLighting::PunctualOnly => true,
        }
    }
}

/// The dedicated skybox-writer kernel for the canonical skybox bucket — renders
/// `skybox_primary.wgsl` instead of the material `compute.wgsl`. It shares the
/// exact same fields (the shared `opaque_kernel_includes.wgsl` preamble reads
/// them), so it's built by moving them out of the material-compute template via
/// `From` (see [`ShaderTemplateMaterialOpaque::into_source`]).
#[derive(Template, Debug)]
#[template(
    path = "material_opaque_wgsl/skybox_primary.wgsl",
    whitespace = "minimize"
)]
pub struct ShaderTemplateMaterialOpaqueSkyboxPrimary {
    pub texture_pool_arrays_len: u32,
    pub texture_pool_samplers_len: u32,
    pub debug: ShaderTemplateMaterialOpaqueDebug,
    pub mipmap: MipmapMode,
    pub multisampled_geometry: bool,
    pub msaa_sample_count: u32,
    pub shadows_enabled: bool,
    pub use_froxel_lights: bool,
    pub froxel_slice_count: u32,
    pub materials_wgsl: String,
    pub shader_id_consts: String,
    pub shader_id: MaterialShaderId,
    pub base: crate::dynamic_materials::ShadingBase,
    pub inc: crate::dynamic_materials::ShaderIncludeFlags,
    pub owns_skybox: bool,
    pub pbr_features: awsm_renderer_materials::pbr::PbrFeatures,
    /// The skybox writer never writes the reflection descriptor itself, but
    /// the shared `brdf_pbr.wgsl` include gates the SSR IBL-specular
    /// suppression (`ssr-spread-gate`) on this field, so it must exist for
    /// askama. Carried from the compute template (matches the cache key).
    pub write_ssr_descriptor: bool,
    pub dynamic_struct_decl: String,
    pub dynamic_loader_decl: String,
    pub dynamic_wgsl_fragment: String,
    pub bucket_entries: Vec<crate::dynamic_materials::BucketEntry>,
    /// Always `false` for the skybox writer (the SKYBOX bucket never
    /// reads prep attributes), but the shared `opaque_kernel_includes`
    /// preamble references `prep_present` / `prep_drops_recompute`, so the
    /// fields must exist for askama.
    pub prep_present: bool,
    pub prep_drops_recompute: bool,
    /// Inert on the skybox writer (`prep_present` is false) but referenced by
    /// the shared helper includes, so the fields must exist.
    pub max_prep_uv_sets: u32,
    pub max_prep_color_sets: u32,
    /// Always `false` for the skybox writer (it never lights / samples
    /// shadows), but `apply_lighting.wgsl` references `needs_shadow_sampling`,
    /// so the field must exist for askama.
    pub needs_shadow_sampling: bool,
    /// Inert on the skybox writer; referenced by `apply_lighting.wgsl`.
    pub max_shadow_casters: u32,
    /// §5 edge slot-map width (8/16) — gates the skybox `cs_shade` arm's
    /// slot_map scan + the widened skybox sentinel (0xFE → 0xFFFE). Derived
    /// from the live bucket count.
    pub edge_slot_bits: u32,
}

impl ShaderTemplateMaterialOpaqueSkyboxPrimary {
    /// True if the shader includes IBL lighting (used by the shared preamble's
    /// lighting/shadow includes). Mirrors the material-compute template.
    pub fn has_lighting_ibl(&self) -> bool {
        !matches!(
            self.debug.lighting,
            ShaderTemplateMaterialOpaqueDebugLighting::PunctualOnly
        )
    }

    /// True if the shader includes punctual lighting. Mirrors the compute template.
    pub fn has_lighting_punctual(&self) -> bool {
        !matches!(
            self.debug.lighting,
            ShaderTemplateMaterialOpaqueDebugLighting::IblOnly
        )
    }
}

impl From<ShaderTemplateMaterialOpaqueCompute> for ShaderTemplateMaterialOpaqueSkyboxPrimary {
    fn from(c: ShaderTemplateMaterialOpaqueCompute) -> Self {
        Self {
            texture_pool_arrays_len: c.texture_pool_arrays_len,
            texture_pool_samplers_len: c.texture_pool_samplers_len,
            debug: c.debug,
            mipmap: c.mipmap,
            multisampled_geometry: c.multisampled_geometry,
            msaa_sample_count: c.msaa_sample_count,
            shadows_enabled: c.shadows_enabled,
            use_froxel_lights: c.use_froxel_lights,
            froxel_slice_count: c.froxel_slice_count,
            materials_wgsl: c.materials_wgsl,
            shader_id_consts: c.shader_id_consts,
            shader_id: c.shader_id,
            base: c.base,
            inc: c.inc,
            owns_skybox: c.owns_skybox,
            pbr_features: c.pbr_features,
            write_ssr_descriptor: c.write_ssr_descriptor,
            dynamic_struct_decl: c.dynamic_struct_decl,
            dynamic_loader_decl: c.dynamic_loader_decl,
            dynamic_wgsl_fragment: c.dynamic_wgsl_fragment,
            bucket_entries: c.bucket_entries,
            // The skybox writer never reads prep attributes; carry the
            // caps through inertly so the shared includes resolve.
            prep_present: c.prep_present,
            prep_drops_recompute: c.prep_drops_recompute,
            max_prep_uv_sets: c.max_prep_uv_sets,
            max_prep_color_sets: c.max_prep_color_sets,
            // The skybox writer never lights; carry the inline-sampling gate
            // through inertly so apply_lighting's gate resolves.
            needs_shadow_sampling: false,
            max_shadow_casters: c.max_shadow_casters,
            edge_slot_bits: c.edge_slot_bits,
        }
    }
}

impl TryFrom<&ShaderCacheKeyMaterialOpaque> for ShaderTemplateMaterialOpaque {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeyMaterialOpaque) -> Result<Self> {
        let texture_pool_arrays_len = value.texture_pool_arrays_len;
        let texture_pool_samplers_len = value.texture_pool_samplers_len;
        let mipmap = if value.mipmaps {
            MipmapMode::Gradient
        } else {
            MipmapMode::None
        };
        let multisampled_geometry = value.msaa_sample_count.is_some();
        let msaa_sample_count = value.msaa_sample_count.unwrap_or_default();
        let debug = ShaderTemplateMaterialOpaqueDebug::new();
        // Plan B: the shared prep pass is UNCONDITIONAL — the opaque deferred
        // path is prep-only, so `prep_present` is always true. The two derived
        // conditions remain because they still vary the SAME shared helpers
        // across cs_opaque (PRIMARY) + cs_edge (RECOMPUTE):
        //
        //  1. `prep_present` = ALWAYS true. Emits the PrepReadContext + the
        //     PRIMARY read branches + binds the prep textures to opaque.
        //  2. `prep_drops_recompute` = msaa off. Drops the standalone recompute
        //     helpers (no cs_edge there). Under MSAA the helpers STAY
        //     (cs_edge=RECOMPUTE uses them).
        let prep_present = true;
        let prep_drops_recompute = value.msaa_sample_count.is_none();
        let max_prep_uv_sets = crate::render_passes::material_prep::MAX_PREP_UV_SETS;
        let max_prep_color_sets = crate::render_passes::material_prep::MAX_PREP_COLOR_SETS;
        let max_shadow_casters = value.max_shadow_casters;

        let bucket_entries = value.bucket_entries.clone();
        let pad_words_iter: Vec<u32> = (0
            ..crate::render_passes::material_classify::shader::template::pad_words_count(
                bucket_entries.len() as u32,
            ))
            .collect();
        // § Part B (the unified module): the merged `cs_edge` entry point
        // hard-codes this bucket's index for its slot-map scan + per-shader
        // entry-count read. Resolve it from the shader_id's position in
        // `bucket_entries`; default 0 if not found (the SKYBOX bucket
        // renders `skybox_primary` and never emits `cs_edge`, so its value
        // is inert).
        let bucket_index = bucket_entries
            .iter()
            .position(|e| e.shader_id == value.shader_id)
            .unwrap_or(0) as u32;
        let edge_slot_bits =
            crate::dynamic_materials::edge_slot_bits(bucket_entries.len() as u32) as u32;
        // Compute the include set once so both the bind-group template (shadow
        // sampling gate) and the compute template (everything else) agree.
        // `for_custom` forces the Tier-B PBR-internal flags OFF — a custom
        // material can never enable brdf/apply_lighting/material_color_calc.
        let inc = if value.owns_skybox {
            crate::dynamic_materials::ShaderIncludeFlags::skybox_only()
        } else if let Some(d) = value.dynamic_shader.as_ref() {
            crate::dynamic_materials::ShaderIncludeFlags::for_custom(d.shader_includes)
        } else {
            crate::dynamic_materials::ShaderIncludeFlags::for_base(value.base)
        };
        // Plan B (stage 5b-shadow): drop the inline `sample_shadow_*` block in
        // ANY-AA prep (was no-MSAA-only in 5a). Under no-MSAA+prep cs_opaque reads
        // the full-screen buffer (PRIMARY, stage 4). Under MSAA+prep cs_opaque
        // reads the full-screen buffer (PRIMARY) AND cs_edge reads the compact
        // per-edge-sample buffer (EDGE, stage 5b) — so NOTHING inline-samples
        // shadows in the MSAA opaque module, and the ~50 KB block drops (the MSAA
        // analog of stage 4's no-MSAA win). Non-prep keeps it (byte-identical to
        // today). Computed once so the bind-group template (the block emit) and
        // the compute template (apply_lighting's inline `else` arm gate) agree.
        // Prep is always on for opaque, so the inline shadow-sampling block is
        // never compiled for the no-MSAA primary; cs_opaque reads the prep
        // buffer. (Matches the former `inc.apply_lighting && !prep_enabled`,
        // which was always false once prep was on.)
        let needs_shadow_sampling = false;
        let _self = Self {
            bind_groups: ShaderTemplateMaterialOpaqueBindGroups {
                texture_pool_arrays_len,
                texture_pool_samplers_len,
                mipmap,
                multisampled_geometry,
                msaa_sample_count,
                debug,
                shadow_group_index: 3,
                // Opaque is SSCS-capable; effective gate is the global enable.
                // step_count clamped ≥1 (safe loop bound + f32(steps) divisor).
                sscs_available: value.sscs_enabled,
                sscs_step_count: value.sscs_step_count.max(1),
                // Plan B (stage 5a): drop the inline `sample_shadow_*` block
                // only in no-MSAA+prep (cs_opaque=PRIMARY reads the buffer).
                // Under MSAA+prep cs_edge=RECOMPUTE still inline-samples, so it
                // stays. (See `needs_shadow_sampling` derivation above.)
                needs_shadow_sampling,
                // The cascade-debug overlay rides `apply_lighting` (its only
                // caller), independent of the dropped sampler block.
                needs_cascade_debug: inc.apply_lighting,
                bucket_entries: bucket_entries.clone(),
                pad_words_iter,
                prep_present,
                reverse_z: value.reverse_z,
            },
            compute: ShaderTemplateMaterialOpaqueCompute {
                texture_pool_arrays_len,
                texture_pool_samplers_len,
                mipmap,
                multisampled_geometry,
                msaa_sample_count,
                debug,
                shadows_enabled: true,
                // All opaque shading reads the per-pixel froxel light
                // list from the GPU cull pass; `lights.wgsl` emits the
                // `apply_lighting_per_froxel*` helpers when this is `true`.
                use_froxel_lights: true,
                froxel_slice_count: crate::render_passes::light_culling::DEFAULT_SLICE_COUNT,
                // Skinny materials: emit only this pipeline's base material body
                // (the dispatch references only that base's fragment).
                //   - skybox-owner shades nothing (body gated out) → none.
                //   - Custom (dynamic) shades ONLY via `custom_shade_dynamic`
                //     (emitted in the dynamic-material wrapper); the first-party
                //     PBR/Unlit/Toon/Flipbook bodies are never referenced, so
                //     emit none of them. Previously `canonical_shader_id()` was
                //     `None` for Custom → `build_materials_wgsl_filtered(None)`
                //     emitted ALL four first-party bodies (~33 KB of dead WGSL)
                //     into every dynamic pipeline. (Phase 3 item 3 — bug #1.)
                //   - First-party bases emit exactly their own fragment.
                materials_wgsl: if value.owns_skybox
                    || value.base == crate::dynamic_materials::ShadingBase::Custom
                {
                    String::new()
                } else {
                    awsm_renderer_materials::registry::build_materials_wgsl_filtered(
                        value.base.canonical_shader_id(),
                    )
                },
                shader_id_consts: awsm_renderer_materials::registry::build_shader_id_consts(),
                shader_id: value.shader_id,
                base: value.base,
                // Custom (dynamic) materials carry their own author-declared
                // include set; first-party bases use the canonical set (computed
                // once as `inc` above and shared with the bind-group template).
                inc,
                owns_skybox: value.owns_skybox,
                pbr_features: awsm_renderer_materials::pbr::PbrFeatures::from_bits(
                    value.pbr_features,
                ),
                write_ssr_descriptor: value.write_ssr_descriptor,
                dynamic_struct_decl: value
                    .dynamic_shader
                    .as_ref()
                    .map(|d| d.struct_decl.clone())
                    .unwrap_or_default(),
                dynamic_loader_decl: value
                    .dynamic_shader
                    .as_ref()
                    .map(|d| d.loader_decl.clone())
                    .unwrap_or_default(),
                dynamic_wgsl_fragment: value
                    .dynamic_shader
                    .as_ref()
                    .map(|d| d.wgsl_fragment.clone())
                    .unwrap_or_default(),
                bucket_entries,
                bucket_index,
                edge_slot_bits,
                prep_present,
                prep_drops_recompute,
                max_prep_uv_sets,
                max_prep_color_sets,
                // Mirror of the bind-group gate: apply_lighting's inline `else`
                // arm (the legacy `sample_shadow_*` path) is emitted only when
                // the inline sampler is compiled. Under MSAA+prep cs_edge needs
                // it; under no-MSAA+prep cs_opaque=PRIMARY reads the buffer so
                // it's dropped.
                needs_shadow_sampling,
                max_shadow_casters,
                edge_shadow_tex_width:
                    crate::render_passes::material_prep::buffers::EDGE_SHADOW_TEX_WIDTH,
            },
        };

        Ok(_self)
    }
}

/// Mipmap sampling mode for the material opaque pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MipmapMode {
    None,
    Gradient,
}

impl MipmapMode {
    /// Returns the function name suffix for this mipmap mode
    pub fn suffix(&self) -> &'static str {
        match self {
            MipmapMode::Gradient => "_grad",
            MipmapMode::None => "_no_mips",
        }
    }

    /// Returns the texture sampling function name for this mode
    pub fn sample_fn(&self) -> &'static str {
        match self {
            MipmapMode::Gradient => "texture_pool_sample_grad",
            MipmapMode::None => "texture_pool_sample_no_mips",
        }
    }

    /// Returns true if this is gradient mode (for conditional template logic)
    pub fn is_gradient(&self) -> bool {
        matches!(self, MipmapMode::Gradient)
    }
}

/// Debug flags for the opaque material pass.
#[derive(Clone, Copy, Debug, Default)]
pub struct ShaderTemplateMaterialOpaqueDebug {
    mips: bool,
    n_dot_v: bool,
    normals: bool,
    base_color: bool,
    view_direction: bool,
    irradiance_sample: bool,
    msaa_detect_edges: bool,
    lighting: ShaderTemplateMaterialOpaqueDebugLighting,
    /// Gate for the runtime debug VIEW overlays (global unlit/flat view mode,
    /// wireframe overlay, froxel light-count heatmap). Driven purely by the
    /// `debug-views` cargo feature: `true` compiles the `cull_params`-driven
    /// branches into the shader (runtime-switchable via the renderer setters),
    /// `false` (the default game build) collapses every `{% if debug.views %}`
    /// gate so those branches never reach the WGSL. `pub` because the shared
    /// `apply_lighting.wgsl` reads it from the cross-module edge-resolve
    /// template too.
    pub views: bool,
}

impl ShaderTemplateMaterialOpaqueDebug {
    /// Creates a default debug configuration. The view-overlay gate follows the
    /// `debug-views` cargo feature (off for game builds, on for the editor).
    pub fn new() -> Self {
        Self {
            views: cfg!(feature = "debug-views"),
            ..Self::default()
        }
    }
    /// Returns true if any debug mode is enabled.
    pub fn any(&self) -> bool {
        self.mips
            || self.n_dot_v
            || self.normals
            || self.base_color
            || self.view_direction
            || self.irradiance_sample
            || self.msaa_detect_edges
            || !matches!(
                self.lighting,
                ShaderTemplateMaterialOpaqueDebugLighting::None
            )
    }
}

/// Lighting debug override for opaque materials.
#[derive(Clone, Copy, Debug, Default)]
pub enum ShaderTemplateMaterialOpaqueDebugLighting {
    #[default]
    None,
    IblOnly,
    PunctualOnly,
}

impl ShaderTemplateMaterialOpaque {
    /// Renders the opaque material shader into WGSL.
    pub fn into_source(self) -> Result<String> {
        let bind_groups_source = self.bind_groups.render()?;
        // The canonical skybox bucket renders the dedicated skybox writer
        // (skybox_primary.wgsl) instead of the material kernel — same bind
        // groups + bucket dispatch, but skybox-only. See skybox_primary.wgsl.
        let compute_source = if self.compute.owns_skybox {
            ShaderTemplateMaterialOpaqueSkyboxPrimary::from(self.compute).render()?
        } else {
            self.compute.render()?
        };

        let source = format!("{}\n{}", bind_groups_source, compute_source);
        // print_shader_source(&source, true);

        //debug_unique_string(1, &source, || print_shader_source(&source, false));

        Ok(source)
    }

    /// Returns an optional debug label for shader compilation.
    pub fn debug_label(&self) -> Option<&str> {
        if self.compute.owns_skybox {
            Some("Material Opaque Skybox")
        } else {
            Some("Material Opaque")
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// Empty-registry guarantee tests
// ─────────────────────────────────────────────────────────────────────
//
// The dynamic-materials plan promises that when no dynamic materials
// are registered, first-party pipelines' compiled WGSL is
// bit-identical to the pre-feature build. The strict cross-branch
// hash diff requires checking out main and re-running the template;
// the structural guarantees below are what we can verify in-tree:
//
//  1. dispatch_hash is 0 when the registry is empty
//  2. bucket_entries collapses to the first-party list
//  3. The templated WGSL emits no `custom_shade_dynamic` wrapper
//     (the {% if shader_id.is_dynamic() %} block evaluates to empty)
//  4. The templated `ClassifyBuckets` struct has the same field
//     names as the hand-rolled version (args_pbr / args_unlit /
//     args_toon / args_flipbook + their _offset siblings).

#[cfg(test)]
mod empty_registry_tests {
    use super::*;
    use crate::render_passes::material_opaque::shader::cache_key::ShaderCacheKeyMaterialOpaque;
    use awsm_renderer_materials::MaterialShaderId;

    // Custom author accessors must exist in the opaque-compute kernel includes.
    // Since the opaque-shade + edge-resolve kernels were unified into one shader
    // module (the `cs_opaque` + `cs_edge` entry points both include
    // `opaque_kernel_includes.wgsl`), a single source covers both contexts — an
    // accessor missing here fails pipeline compile for both. This asserts against
    // the source WGSL directly (include_str!) so the guard can't drift from the
    // rendered templates. (Whether a non-zero set visually differs is a separate
    // GPU state-2 confirm — needs a multi-UV asset the repo lacks.)
    const OPAQUE_KERNEL_WGSL: &str =
        include_str!("material_opaque_wgsl/opaque_kernel_includes.wgsl");

    #[test]
    fn custom_attribute_accessors_exist_in_both_opaque_kernels() {
        // One shared opaque-kernel include file (the legacy `edge_resolve` kernel
        // it used to also cover was removed with the cs_edge split).
        let (name, src) = ("opaque_kernel_includes", OPAQUE_KERNEL_WGSL);
        {
            assert!(
                src.contains("fn material_uv(input: OpaqueShadingInput"),
                "{name}.wgsl missing material_uv(input, set) accessor (dual-context invariant)"
            );
            assert!(
                src.contains("fn material_vertex_color(input: OpaqueShadingInput"),
                "{name}.wgsl missing material_vertex_color(input, set) accessor"
            );
            // material_uv reads `input.uv_sets_index`, so the struct must carry it.
            assert!(
                src.contains("uv_sets_index"),
                "{name}.wgsl OpaqueShadingInput missing uv_sets_index — material_uv can't reach set N"
            );
            // Out-of-range clamp: the struct must carry the per-mesh set COUNTS and
            // the accessors must guard against them, so sampling a set the mesh
            // lacks returns a benign default instead of reading an adjacent
            // vertex's floats from the shared attribute pool (no auto bounds guard
            // on the index-driven `visibility_data` fetch).
            assert!(
                src.contains("uv_set_count") && src.contains("color_set_count"),
                "{name}.wgsl OpaqueShadingInput missing uv_set_count/color_set_count for the OOB clamp"
            );
            assert!(
                src.contains("set_index >= input.uv_set_count"),
                "{name}.wgsl material_uv missing the out-of-range clamp"
            );
            assert!(
                src.contains("set_index >= input.color_set_count"),
                "{name}.wgsl material_vertex_color missing the out-of-range clamp"
            );
        }
    }

    fn render_first_party_wgsl(shader_id: MaterialShaderId, msaa: Option<u32>) -> String {
        let key = ShaderCacheKeyMaterialOpaque {
            write_ssr_descriptor: false,
            reverse_z: false,
            texture_pool_arrays_len: 1,
            texture_pool_samplers_len: 1,
            msaa_sample_count: msaa,
            mipmaps: true,
            max_shadow_casters: 4,
            sscs_enabled: false,
            sscs_step_count: 16,
            shader_id,
            base: crate::dynamic_materials::ShadingBase::for_shader_id(shader_id),
            owns_skybox: shader_id == MaterialShaderId::SKYBOX,
            // Canonical first-party buckets carry the empty feature-set
            // (the minimal shader, never the uber `all()`).
            pbr_features: awsm_renderer_materials::pbr::PbrFeatures::default().bits(),
            dispatch_hash: 0,
            dynamic_shader: None,
            bucket_entries: crate::dynamic_materials::first_party_bucket_entries(),
        };
        let template = ShaderTemplateMaterialOpaque::try_from(&key).expect("template");
        template.into_source().expect("render")
    }

    #[test]
    fn empty_registry_emits_no_dynamic_wrapper() {
        // The `{% if shader_id.is_dynamic() %}` block in compute.wgsl
        // must evaluate to empty for every first-party shader_id, so
        // the emitted WGSL contains none of the dynamic-wrapper
        // marker tokens.
        for shader_id in [
            MaterialShaderId::PBR,
            MaterialShaderId::UNLIT,
            MaterialShaderId::TOON,
            MaterialShaderId::FLIPBOOK,
        ] {
            let wgsl = render_first_party_wgsl(shader_id, None);
            assert!(
                !wgsl.contains("custom_shade_dynamic"),
                "first-party {shader_id:?} pipeline accidentally emits `custom_shade_dynamic`"
            );
            assert!(
                !wgsl.contains("dynamic-material wrapper"),
                "first-party {shader_id:?} pipeline emits the dynamic-material wrapper block"
            );
            assert!(
                !wgsl.contains("material_data_load"),
                "first-party {shader_id:?} pipeline emits the dynamic-material loader"
            );
        }
    }

    #[test]
    fn classify_buckets_uses_data_driven_array_layout() {
        // O(N²) fix: ClassifyBuckets is now `args`/`offsets` ARRAYS (indexed by
        // bucket index), NOT 2N per-bucket named fields. The struct text is O(1)
        // regardless of bucket count. Assert the arrays are present and the old
        // per-name fields are GONE (so the struct stops growing with N).
        let wgsl = render_first_party_wgsl(MaterialShaderId::PBR, None);
        assert!(
            wgsl.contains("args: array<vec4<u32>,") && wgsl.contains("offsets: array<u32,"),
            "ClassifyBuckets should declare `args`/`offsets` arrays"
        );
        for gone in ["args_pbr", "pbr_offset", "unlit_offset", "flipbook_offset"] {
            assert!(
                !wgsl.contains(gone),
                "ClassifyBuckets still emits per-bucket named field `{gone}` (O(N) regression)"
            );
        }
    }

    #[test]
    fn empty_registry_bucket_offset_resolves() {
        // A past bug was `let bucket_offset =;` when the lookup chain had no
        // match. With the data-driven layout the offset is read by bucket index
        // from the `offsets` array; verify it resolves (non-empty) for every
        // first-party shader_id.
        for shader_id in [
            MaterialShaderId::PBR,
            MaterialShaderId::UNLIT,
            MaterialShaderId::TOON,
            MaterialShaderId::FLIPBOOK,
        ] {
            let wgsl = render_first_party_wgsl(shader_id, None);
            assert!(
                wgsl.contains("let bucket_offset = classify_buckets.offsets["),
                "first-party {shader_id:?} pipeline's bucket_offset doesn't read offsets[bucket_index]"
            );
            assert!(
                !wgsl.contains("let bucket_offset =;"),
                "first-party {shader_id:?} pipeline emits empty bucket_offset assignment"
            );
        }
    }

    #[test]
    fn debug_view_branches_follow_feature_gate() {
        // The runtime debug-VIEW overlays (unlit/flat view mode, wireframe,
        // light heatmap) must be compiled out of game builds. The wireframe
        // overlay lives in compute.wgsl's color-write path, present for any
        // non-skybox opaque pipeline, so it's the clean signal to assert on.
        //
        // Invariant 1: the `CullParams` struct field is ALWAYS declared (the
        // 64-byte uniform layout the Rust writer fills must not shift between
        // game and editor builds).
        //
        // Invariant 2: the branch that READS it (`cull_params.debug_wireframe`)
        // is emitted iff the `debug-views` feature is on — i.e. game builds pay
        // no per-fragment branch. This assertion is symmetric, so it holds
        // whether the test runs with the feature on (`--all-features`) or off
        // (a bare `cargo test -p awsm-renderer`).
        let wgsl = render_first_party_wgsl(MaterialShaderId::TOON, None);
        assert!(
            wgsl.contains("debug_wireframe: u32"),
            "CullParams must always declare debug_wireframe (stable uniform layout)"
        );
        assert_eq!(
            wgsl.contains("cull_params.debug_wireframe"),
            cfg!(feature = "debug-views"),
            "wireframe branch presence ({}) must match the debug-views feature ({})",
            wgsl.contains("cull_params.debug_wireframe"),
            cfg!(feature = "debug-views"),
        );
    }

    #[test]
    fn dispatch_hash_is_zero_on_empty_registry() {
        // The dispatch_hash field on the cache key is what triggers
        // pipeline cache invalidation when registrations change.
        // For the empty registry it must be a stable 0, otherwise
        // the first-party-only build doesn't get cache hits across
        // session restarts.
        let registry = crate::dynamic_materials::DynamicMaterials::new();
        assert_eq!(registry.dispatch_hash(), 0);
    }
}

// ─────────────────────────────────────────────────────────────────────
// Transparent dynamic-shader compile verification (lives next to the
// opaque tests for convenience — both walk the dynamic-substitution
// path with different cache keys).
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod transparent_dynamic_tests {
    use crate::render_passes::material_opaque::shader::cache_key::DynamicShaderInfo;
    use crate::render_passes::material_transparent::shader::cache_key::ShaderCacheKeyMaterialTransparent;
    use crate::render_passes::material_transparent::shader::template::ShaderTemplateMaterialTransparent;
    use crate::render_passes::shared::material::cache_key::ShaderMaterialVertexAttributes;
    use awsm_renderer_materials::dynamic_layout::{
        FieldType, MaterialLayout, TextureSlotRuntime, UniformFieldRuntime,
    };
    use awsm_renderer_materials::MaterialShaderId;

    /// Verifies the transparent fragment template + the dynamic-
    /// material wrapper compose into valid WGSL. Uses a soft-glass-
    /// style author fragment that references the TransparentShadingInput
    /// contract fields (no opaque `input.coords` / `input.screen_dims`
    /// — those are opaque-only).
    #[test]
    fn transparent_dynamic_template_renders_valid_wgsl() {
        let layout = MaterialLayout {
            uniforms: vec![
                UniformFieldRuntime {
                    name: "tint".into(),
                    ty: FieldType::Color3,
                },
                UniformFieldRuntime {
                    name: "refraction_strength".into(),
                    ty: FieldType::F32,
                },
            ],
            textures: vec![TextureSlotRuntime {
                name: "bg".into(),
                srgb: true,
                mipmap_kind: awsm_renderer_core::texture::mipmap::MipmapTextureKind::Albedo,
            }],
            buffers: Vec::new(),
        };
        let struct_decl =
            awsm_renderer_materials::dynamic_layout::generate_wgsl_struct("MaterialData", &layout);
        let loader_decl = awsm_renderer_materials::dynamic_layout::generate_wgsl_loader(
            "MaterialData",
            "material_data_load",
            &layout,
        );

        // Soft-glass-style fragment — references TransparentShadingInput's
        // actual fields + input.material.<field>, AND the per-vertex attribute
        // accessors (material_uv / material_vertex_color) so we assert they
        // resolve on the transparent path too (multiplied by 0 to keep the
        // visual unchanged while still forcing the references into the module).
        let wgsl_fragment = r#"
let cos_theta = clamp(dot(input.world_normal, input.surface_to_camera), 0.0, 1.0);
let alpha = mix(0.85, 0.25, cos_theta);
let uv1 = material_uv(input, 1u);
let c1 = material_vertex_color(input, 1u);
let color = input.material.tint * (1.0 - input.material.refraction_strength)
    + vec3<f32>(uv1, 0.0) * 0.0 + c1.rgb * 0.0;
return TransparentShadingOutput(vec4<f32>(color, alpha));
"#
        .to_string();

        let dyn_info = DynamicShaderInfo {
            shader_includes: awsm_renderer_materials::ShaderIncludes::all(),
            struct_decl,
            loader_decl,
            wgsl_fragment,
        };
        let dyn_id = MaterialShaderId::from_dynamic_raw(MaterialShaderId::DYNAMIC_START);

        let key = ShaderCacheKeyMaterialTransparent {
            instancing_transforms: false,
            // 2 UV + 2 COLOR sets so the templated accessors emit real branches.
            attributes: ShaderMaterialVertexAttributes {
                color_sets: Some(2),
                uv_sets: Some(2),
                ..ShaderMaterialVertexAttributes::default()
            },
            texture_pool_arrays_len: 1,
            texture_pool_samplers_len: 1,
            msaa_sample_count: None,
            mipmaps: true,
            reverse_z: false,
            base: crate::dynamic_materials::ShadingBase::Custom,
            pbr_features: awsm_renderer_materials::pbr::PbrFeatures::default().bits(),
            dispatch_hash: 0,
            dynamic_shader_id: Some(dyn_id),
            dynamic_shader: Some(dyn_info),
            dynamic_vertex_shader: None,
            froxel_slice_count: crate::render_passes::light_culling::DEFAULT_SLICE_COUNT,
        };

        let template = ShaderTemplateMaterialTransparent::try_from(&key)
            .expect("transparent template must build from a valid cache key");
        let source = template
            .into_source()
            .expect("transparent template must render without errors");

        // Structural assertions on the emitted source. The specialize-only
        // transparent fragment selects the body at COMPILE time on
        // `base == Custom` (no runtime `shader_id ==` dispatch arm), so the
        // wrapper + its call must be present.
        assert!(
            source.contains("fn custom_shade_transparent_dynamic"),
            "transparent template missing custom_shade_transparent_dynamic wrapper"
        );
        assert!(
            source.contains("material_data_load"),
            "transparent template missing material_data_load accessor"
        );
        assert!(
            source.contains("struct MaterialData"),
            "transparent template missing auto-generated MaterialData struct"
        );
        // Per-vertex attribute accessors must exist on the transparent path too
        // (parity with the opaque kernels) so the same custom fragment compiles
        // whether the material is opaque or transparent.
        assert!(
            source.contains("fn material_uv(input: TransparentShadingInput"),
            "transparent template missing material_uv accessor"
        );
        assert!(
            source.contains("fn material_vertex_color(input: TransparentShadingInput"),
            "transparent template missing material_vertex_color accessor"
        );
        let _ = dyn_id;
    }

    #[test]
    fn transparent_empty_dynamic_path_collapses() {
        // When no dynamic transparent material is registered
        // (dynamic_shader_id = None), the {% if shader_id_dynamic != 0 %}
        // block must emit nothing — no wrapper, no extra dispatch arm,
        // no MaterialData decl that doesn't belong on this pipeline.
        let key = ShaderCacheKeyMaterialTransparent {
            instancing_transforms: false,
            attributes: ShaderMaterialVertexAttributes::default(),
            texture_pool_arrays_len: 1,
            texture_pool_samplers_len: 1,
            msaa_sample_count: None,
            mipmaps: true,
            reverse_z: false,
            base: crate::dynamic_materials::ShadingBase::Pbr,
            pbr_features: awsm_renderer_materials::pbr::PbrFeatures::default().bits(),
            dispatch_hash: 0,
            dynamic_shader_id: None,
            dynamic_shader: None,
            dynamic_vertex_shader: None,
            froxel_slice_count: crate::render_passes::light_culling::DEFAULT_SLICE_COUNT,
        };

        let template = ShaderTemplateMaterialTransparent::try_from(&key)
            .expect("transparent template must build");
        let source = template.into_source().expect("transparent must render");

        assert!(
            !source.contains("custom_shade_transparent_dynamic"),
            "first-party transparent pipeline accidentally emits the dynamic wrapper"
        );
        assert!(
            !source.contains("material_data_load"),
            "first-party transparent pipeline accidentally emits the dynamic loader"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────
// Per-material shader SIZE regression guard.
//
// Renders the opaque kernel for a representative Custom (dynamic) material and
// asserts upper bounds on the emitted WGSL. Upper bounds are the right shape:
// the optimization phases only SHRINK these shaders, so a passing bound stays
// valid as sizes drop — the test fails only if a change REGROWS a shader (e.g.
// re-introduces the PBR stack into a lean material). Tighten the CEILINGs as
// Phases 3–5 land; the eprintln prints the live sizes for each run.
//
// Why Custom: the whole effort targets dynamic materials. A Custom material that
// declares no includes is the leanest case; `all()` is the conservative case.
#[cfg(test)]
mod size_regression {
    use super::*;
    use crate::render_passes::material_opaque::shader::cache_key::{
        DynamicShaderInfo, ShaderCacheKeyMaterialOpaque,
    };
    use awsm_renderer_materials::MaterialShaderId;

    fn render_custom(
        includes: awsm_renderer_materials::ShaderIncludes,
        msaa: Option<u32>,
        mipmaps: bool,
    ) -> String {
        let dyn_id = MaterialShaderId::from_dynamic_raw(MaterialShaderId::DYNAMIC_START);
        let mut bucket_entries = crate::dynamic_materials::first_party_bucket_entries();
        bucket_entries.push(crate::dynamic_materials::BucketEntry {
            shader_id: dyn_id,
            base: crate::dynamic_materials::ShadingBase::Custom,
            pbr_features: awsm_renderer_materials::pbr::PbrFeatures::default().bits(),
            name: "noise".to_string(),
        });
        let key = ShaderCacheKeyMaterialOpaque {
            write_ssr_descriptor: false,
            reverse_z: false,
            texture_pool_arrays_len: 1,
            texture_pool_samplers_len: 1,
            msaa_sample_count: msaa,
            mipmaps,
            max_shadow_casters: 4,
            sscs_enabled: false,
            sscs_step_count: 16,
            shader_id: dyn_id,
            base: crate::dynamic_materials::ShadingBase::Custom,
            owns_skybox: false,
            pbr_features: awsm_renderer_materials::pbr::PbrFeatures::default().bits(),
            dispatch_hash: 1,
            dynamic_shader: Some(DynamicShaderInfo {
                shader_includes: includes,
                struct_decl: "struct MaterialData { _pad: u32, };".to_string(),
                loader_decl:
                    "fn material_data_load(byte_offset: u32) -> MaterialData { return MaterialData(0u); }"
                        .to_string(),
                wgsl_fragment:
                    "return OpaqueShadingOutput(input.world_normal * 0.5 + 0.5, 1.0);".to_string(),
            }),
            bucket_entries,
        };
        ShaderTemplateMaterialOpaque::try_from(&key)
            .expect("template builds")
            .into_source()
            .expect("renders")
    }

    /// Remove WGSL `//` line comments and `/* */` block comments so token scans
    /// match real code, not prose. Non-nesting block handling is fine for our
    /// generated shaders (the include fences are simple `/* ... */`).
    fn strip_wgsl_comments(src: &str) -> String {
        // Drop block comments first.
        let mut out = String::with_capacity(src.len());
        let bytes = src.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
                // skip to closing */
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
            } else {
                out.push(bytes[i] as char);
                i += 1;
            }
        }
        // Then drop line comments.
        out.lines()
            .map(|l| match l.find("//") {
                Some(p) => &l[..p],
                None => l,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    // Ceilings (bytes). Worst realistic config = MSAA4 + mips. Set with headroom
    // above the current measured sizes; TIGHTEN as further work shrinks the shaders.
    // History (msaa4+mips empty / all): Phase 2 ~196 / ~262 KB → Phase 3 ~162.6 KB
    // both → Phase 4 gating → shadow-sampling gate (apply_lighting only): the ~50 KB
    // PCSS/EVSM/cascade/cube block now drops from every non-lighting Custom shader,
    // landing at **83.3 KB empty / 113.6 KB all**. Unified-edge removal: the merged
    // `cs_shade` entry point is now UNCONDITIONAL under MSAA (it was the default-on
    // production path), so the measured MSAA4 sizes included it: ~90.7 KB / ~126.7 KB.
    // **A2 (compile invariant):** the MSAA module no longer carries the dead
    // `cs_opaque` entry (non-MSAA dispatches it; MSAA dispatches only `cs_shade`),
    // so the MSAA4 module shrank to **~82.0 KB empty / ~118.0 KB all**.
    // **Prep-only (prep flag removed):** the opaque path is now UNCONDITIONALLY
    // prep — the measured Custom variants are the prep-ON build, which under MSAA4
    // carries the PrepReadContext + prep texture reads on top of the still-present
    // recompute helpers (cs_edge=RECOMPUTE keeps them; 5b-attrs deferred). Measured
    // **85.7 KB empty / 122.8 KB all** — ceilings raised to fit the prep-on sizes
    // (these were previously measured against the now-removed prep-OFF variant).
    // **D1-normalmap (tangent-frame ABI):** `OpaqueShadingInput` now always carries
    // `world_tangent`/`world_bitangent` (2 vec3 fields + 2 constructor args at each
    // of the 3 dynamic-shade sites), so EVERY Custom shader grew ~0.6 KB — a
    // permanent, intended ABI addition (normal mapping without re-deriving a TBN),
    // not a regression. Ceilings bumped to ~87.6 KB empty / ~125.6 KB all (measured)
    // plus headroom. (`normal_map` itself is opt-in, so the *helpers* aren't in all.)
    // **Uniformity-safe dynamic sampling (blend-custom fix):** the texture
    // helper surface gained `texture_pool_sample_nu` — a thin alias here (the
    // compute kernels have no uniform-control-flow restriction), the real
    // sample-all-select body lives in the transparent fragment. A permanent,
    // intended ~0.3 KB addition so dynamic-material helpers compile in BOTH
    // contexts. Measured 88.4 KB empty / 127.0 KB all; ALL ceiling bumped.
    // **Per-sample SSR descriptor resolve (MSAA fix):** shade_sample returns
    // a struct carrying each sample's reflection descriptor and the edge arm
    // accumulates it into the widened (8-word) accumulator slots — a
    // permanent ~2 KB addition so final_blend can resolve a coverage-correct
    // per-pixel descriptor (single-sample descriptors made SSR visibly undo
    // MSAA along silhouettes). Measured 91.1 KB empty; both ceilings bumped.
    const CEIL_EMPTY_MSAA4_MIPS: usize = 94_000;
    const CEIL_ALL_MSAA4_MIPS: usize = 134_000;

    #[test]
    fn custom_shader_sizes_within_ceiling() {
        let empty = render_custom(
            awsm_renderer_materials::ShaderIncludes::empty(),
            Some(4),
            true,
        );
        let all = render_custom(
            awsm_renderer_materials::ShaderIncludes::all(),
            Some(4),
            true,
        );
        eprintln!(
            "[size_regression] Custom msaa4+mips — empty: {} B, all: {} B (delta {})",
            empty.len(),
            all.len(),
            all.len() - empty.len()
        );
        assert!(
            empty.len() < CEIL_EMPTY_MSAA4_MIPS,
            "empty-includes Custom shader {} B exceeded ceiling {} B — a lean material regrew",
            empty.len(),
            CEIL_EMPTY_MSAA4_MIPS
        );
        assert!(
            all.len() < CEIL_ALL_MSAA4_MIPS,
            "all-includes Custom shader {} B exceeded ceiling {} B",
            all.len(),
            CEIL_ALL_MSAA4_MIPS
        );
        // Declaring fewer includes must never produce a LARGER shader.
        assert!(
            empty.len() <= all.len(),
            "empty-includes ({} B) should not exceed all-includes ({} B)",
            empty.len(),
            all.len()
        );
    }

    // Phase 3 item 4 — Custom-path validation. Render the Custom kernel across
    // {empty, all (Tier A), an explicit-Tier-B declaration} × {mips,no-mips} ×
    // {msaa,no-msaa} and assert:
    //   (a) every combo renders (template builds + into_source Ok), and
    //   (b) NO first-party shading body or PBR type leaks into a Custom shader —
    //       the dead-code kill (item 3) + Tier-B masking (item 2) hold even when
    //       the registration tries to declare Tier B (S::all() is Tier-A-only,
    //       but we also throw an explicit S::BRDF | MATERIAL_COLOR_CALC at it).
    //
    // These string assertions are the in-tree proxy for WGSL validation; the
    // phase-end browser run GPU-compiles the empty-includes Custom shader.
    #[test]
    fn custom_path_never_leaks_first_party_shading() {
        use awsm_renderer_materials::ShaderIncludes as S;
        // Tier-B forced declaration — must still be stripped on the Custom path.
        let tier_b = S::BRDF
            .union(S::APPLY_LIGHTING)
            .union(S::MATERIAL_COLOR_CALC);
        let include_sets = [S::empty(), S::all(), tier_b, S::all().union(tier_b)];
        // Markers that must NEVER appear in a Custom shader: first-party shading
        // bodies (materials_wgsl) + the PBR types they/the Tier-B modules use.
        let forbidden = [
            "fn pbr_get_material(",
            "fn compute_unlit_material_color(",
            "fn compute_toon_lit_color(",
            "fn flipbook_finalize_color(",
            "fn brdf_direct(",
            "fn apply_lighting_per_froxel(",
            "PbrMaterialColor",
            "PbrMaterial",
        ];
        for inc in include_sets {
            for msaa in [None, Some(4)] {
                for mips in [false, true] {
                    let raw = render_custom(inc, msaa, mips);
                    // Strip comments before scanning — a comment mentioning
                    // `PbrMaterial` is harmless; only TYPE refs in real code can
                    // break compilation. (WGSL `//` line + `/* */` block comments;
                    // our generated shaders never nest block comments.)
                    let src = strip_wgsl_comments(&raw);
                    // (a) renders (render_custom already unwraps build + source).
                    assert!(
                        src.contains("fn custom_shade_dynamic("),
                        "Custom shader must emit the dynamic wrapper (inc={:?} msaa={:?} mips={})",
                        inc.bits(),
                        msaa,
                        mips
                    );
                    // (b) no first-party / PBR leakage.
                    for marker in forbidden {
                        assert!(
                            !src.contains(marker),
                            "Custom shader leaked first-party/PBR token `{marker}` \
                             (inc={:?} msaa={:?} mips={}) — Tier-B masking or the dead-code \
                             kill regressed",
                            inc.bits(),
                            msaa,
                            mips
                        );
                    }
                }
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// brdf.wgsl PBR feature-gating tests. Render the shared lobe template
// standalone (it only references `pbr_features`) and assert the
// specialize-only contract: feature presence is purely compile-time, and
// NO feature runtime guards (`if (color.x > 0)`) survive in any
// configuration.
// Call-site-unique tokens (cc_fresnel / sheen_scaling / iri_f0) are used
// because the lobe *functions* are defined in brdf.wgsl and would always
// match by name.
#[cfg(test)]
mod brdf_gate_tests {
    use askama::Template;
    use awsm_renderer_materials::pbr::PbrFeatures;

    #[derive(Template)]
    #[template(path = "shared_wgsl/lighting/brdf.wgsl")]
    struct BrdfGateTest {
        pbr_features: PbrFeatures,
        /// Referenced by `brdf_pbr.wgsl`'s SSR IBL-specular suppression gate
        /// (`ssr-spread-gate`); rendered `false` here — these tests exercise
        /// the PBR feature gating, not the SSR axis.
        write_ssr_descriptor: bool,
    }

    /// Rendered brdf with `//` line comments removed (so marker tokens
    /// that appear in comments outside the gates don't pollute matches)
    /// and all whitespace stripped (spacing-robust token matching).
    fn render_nows(pbr_features: PbrFeatures) -> String {
        let raw = BrdfGateTest {
            pbr_features,
            write_ssr_descriptor: false,
        }
        .render()
        .expect("brdf.wgsl renders");
        raw.lines()
            .map(|l| match l.find("//") {
                Some(i) => &l[..i],
                None => l,
            })
            .collect::<Vec<_>>()
            .join("\n")
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect()
    }

    #[test]
    fn no_feature_runtime_guards_anywhere() {
        // Even the all-features shader must have NO per-feature runtime
        // guards — the specialize-only design moved all feature branching
        // to compile time. (Lighting-geometry guards like n_dot_l_back
        // remain; those aren't feature guards.)
        let w = render_nows(PbrFeatures::all());
        assert!(
            !w.contains("color.clearcoat>0"),
            "no clearcoat runtime guard"
        );
        assert!(
            !w.contains("color.iridescence>0"),
            "no iridescence runtime guard"
        );
        assert!(
            !w.contains("color.anisotropy_strength!=0"),
            "no anisotropy runtime guard"
        );
        assert!(
            !w.contains("color.diffuse_transmission>0"),
            "no diffuse-transmission runtime guard"
        );
        // ...but every lobe is still present at all().
        assert!(w.contains("cc_fresnel") && w.contains("sheen_scaling") && w.contains("iri_f0"));
    }

    #[test]
    fn specialized_strips_absent_lobes() {
        let mut f = PbrFeatures::all();
        f.clearcoat = false;
        f.sheen = false;
        let w = render_nows(f);
        assert!(
            !w.contains("cc_fresnel"),
            "absent clearcoat is compile-time stripped"
        );
        assert!(
            !w.contains("sheen_scaling"),
            "absent sheen is compile-time stripped"
        );
        assert!(w.contains("iri_f0"), "present iridescence is kept");
    }

    #[test]
    fn specialized_keeps_only_present_lobes() {
        let f = PbrFeatures {
            clearcoat: true,
            iridescence: true,
            ..PbrFeatures::default()
        };
        let w = render_nows(f);
        assert!(w.contains("cc_fresnel"), "present clearcoat emitted");
        assert!(w.contains("iri_f0"), "present iridescence emitted");
        assert!(!w.contains("sheen_scaling"), "absent sheen stripped");
    }
}
