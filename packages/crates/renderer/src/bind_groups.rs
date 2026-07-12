//! Bind group recreation coordination.

use std::collections::HashSet;

use awsm_renderer_core::renderer::AwsmRendererWebGpu;
use strum::{EnumIter, IntoEnumIterator};
use thiserror::Error;

use crate::{
    anti_alias::AntiAliasing, bind_group_layout::BindGroupLayouts, camera::CameraBuffer,
    environment::Environment, features::RendererFeatures, instances::Instances, lights::Lights,
    materials::Materials, meshes::Meshes, picker::Picker, render_passes::RenderPasses,
    render_textures::RenderTextureViews, shadows::Shadows, textures::Textures,
    transforms::Transforms,
};

// There are no cache keys for bind groups, they are created on demand
// Since changes to storages, uniforms, and textures are the reason to recreate bind groups,
// and these may be shared across multiple bind groups, we use a "create list" to track which bind groups need to be recreated
//
// Specifically, typical causes of change are:
// 1. A change in raw buffer size which causes a reallocation
// 2. A change in texture view size which causes new textures to be created
//
// That conscpicuously does not include changes to material textures
// since those are looked up via the material key and do not require a bind group recreation
/// Inputs required to rebuild bind groups.
pub struct BindGroupRecreateContext<'a> {
    pub gpu: &'a AwsmRendererWebGpu,
    pub render_texture_views: &'a RenderTextureViews,
    pub textures: &'a Textures,
    pub materials: &'a Materials,
    pub bind_group_layouts: &'a BindGroupLayouts,
    pub meshes: &'a Meshes,
    pub camera: &'a CameraBuffer,
    /// Frame-globals uniform — bound alongside camera in every pass
    /// that needs renderer-wide per-frame state (`time`, `delta_time`,
    /// `frame_count`, `resolution`). Lifetimes are identical to
    /// camera's so the binding rides on the same group.
    pub frame_globals: &'a crate::frame_globals::FrameGlobals,
    pub environment: &'a Environment,
    pub lights: &'a Lights,
    pub transforms: &'a Transforms,
    pub instances: &'a Instances,
    pub anti_aliasing: &'a AntiAliasing,
    pub shadows: &'a Shadows,
    /// Classify-pass output buffer. Bound read-write by the classify
    /// pass; bound read-only as the per-`shader_id` tile bucket source
    /// on the opaque main bind group; consumed as indirect-args by
    /// the opaque dispatch.
    pub material_classify_buffers:
        &'a crate::render_passes::material_classify::buffers::ClassifyBuffers,
    /// `shader_id → bucket_index` LUT (§4a), bound read-only into classify
    /// as the O(1) per-pixel/per-sample bucket map.
    pub material_bucket_lut:
        &'a crate::render_passes::material_classify::bucket_lut::MaterialBucketLut,
    /// GPU light-culling froxel buffers (params uniform + per-froxel
    /// counts + flat indices + overflow counter). Bound RW on the cull
    /// pass; bound read-only by the transparent + opaque-oversized
    /// shaders that consume the per-froxel light slice.
    pub light_culling_buffers: &'a crate::render_passes::light_culling::LightCullingBuffers,
    /// Priority-3 MSAA edge-resolve composite buffer. `Some` only when
    /// MSAA is on (no edges to resolve under single-sample). Bound
    /// read-write to the classify pass (binding 4) and the per-shader
    /// edge_resolve / skybox_edge_resolve / final_blend pipelines.
    pub material_edge_buffers:
        Option<&'a crate::render_passes::material_opaque::edge_buffers::MaterialEdgeBuffers>,
    /// `EdgeBufferLayout` uniform companion. Same `Some` discipline as
    /// `material_edge_buffers`.
    pub material_edge_layout_uniform: Option<&'a web_sys::GpuBuffer>,
    /// Renderer-wide variable-length per-material data pool. Bound on
    /// the opaque + transparent main bind groups so custom-material
    /// WGSL fragments can resolve `<slot>_offset` /
    /// `<slot>_length` via `extras_load_*` helpers.
    pub extras_pool: &'a crate::dynamic_materials::extras_pool::ExtrasPool,
    /// Projection-decal subsystem. Holds the per-decal GPU buffer the
    /// `material_decal` compute pass reads at shading time. `None`
    /// when `features.decals == false` — the decal pass's bind groups
    /// are skipped in that mode.
    pub decals: Option<&'a crate::decals::Decals>,
    /// Occlusion-cull instance + visibility buffers. `None` when
    /// `features.gpu_culling == false`.
    pub occlusion_buffers: Option<&'a crate::render_passes::occlusion::buffers::OcclusionBuffers>,
    /// Full-chain HZB view used by the cull pass to sample at
    /// per-instance mip levels. `None` when
    /// `features.gpu_culling == false`.
    pub hzb_full_view: Option<web_sys::GpuTextureView>,
    /// Per-tile decal classify buckets. `None` when
    /// `features.decals == false`.
    pub decal_classify_buffers:
        Option<&'a crate::render_passes::material_decal::classify::buffers::DecalClassifyBuffers>,
    /// GPU compaction `IndirectDrawArgs` buffer. `None` when
    /// `features.gpu_culling == false`.
    pub compaction_buffers:
        Option<&'a crate::render_passes::occlusion::compaction::CompactionBuffers>,
    /// GPU mesh-pixel-coverage producer buffers. `None` when
    /// `features.coverage_lod == false`.
    pub coverage_buffers: Option<&'a crate::render_passes::coverage::buffers::CoverageBuffers>,
    /// Active feature gates — the dispatcher uses these to skip
    /// recreating bind groups for passes whose feature is disabled.
    pub features: &'a RendererFeatures,
    /// Plan B (stage 5b-shadow): the prep pass's compact per-edge-sample shadow
    /// texture (sampled view), bound at opaque group(0) binding 27 so `cs_edge`
    /// (EDGE mode) reads it. `Some` only under prep + MSAA (the prep pass owns
    /// the texture; cloned here so the recreate borrow doesn't conflict with the
    /// `&mut render_passes` the dispatcher also takes). `None` otherwise → the
    /// opaque main layout omits binding 27.
    pub prep_edge_shadow_view: Option<web_sys::GpuTextureView>,
}

/// Reasons to recreate bind groups.
#[derive(Hash, Debug, Clone, PartialEq, Eq, EnumIter)]
pub enum BindGroupCreate {
    CameraInitOnly,
    LightsResize,
    LightsInfoCreate,
    BrdfLutTextures,
    IblTextures,
    EnvironmentSkyboxCreate,
    TransformsResize,
    GeometryMorphTargetWeightsResize,
    GeometryMorphTargetValuesResize,
    MaterialMorphTargetWeightsResize,
    MaterialMorphTargetValuesResize,
    SkinJointMatricesResize,
    SkinJointIndexAndWeightsResize,
    GeometryMeshMetaResize,
    MaterialMeshMetaResize,
    /// Merged geometry pool was reallocated — opaque main must rebind
    /// the new buffer handle (transparent's vertex/index buffers are
    /// bound per-draw via setVertexBuffer / setIndexBuffer, so its
    /// main bind group is unaffected).
    MeshGeometryPoolResize,
    /// Occlusion-cull instance / visibility buffers were reallocated.
    /// Only the cull pass's bind group binds them.
    OcclusionBuffersResize,
    /// Decal classify buckets were reallocated (viewport tile-count
    /// grew). The classify pass + decal shading pass both rebind.
    DecalClassifyBuffersResize,
    /// Compaction `IndirectDrawArgs` buffer was reallocated. Only the
    /// compaction pass binds it.
    CompactionBuffersResize,
    /// GPU coverage `counts_buffer` was reallocated. Only the
    /// coverage pass binds it.
    CoverageBuffersResize,
    MaterialResize,
    TextureViewRecreate,
    TexturePool,
    TextureTransformsResize,
    AntiAliasingChange,
    /// Per-instance attribute storage buffer was reallocated; opaque + transparent
    /// shading bind groups must re-bind the new buffer.
    InstanceAttributesResize,
    /// Shadow atlas, EVSM atlas, cube array, or descriptors buffer was recreated;
    /// the opaque + transparent shading bind groups must re-bind the new resources.
    ShadowsResourcesChange,
    /// Classify output buffer was (re)allocated (first frame, or
    /// viewport resize bumped the tile count past current capacity).
    /// The classify pass's bind group and the opaque main bind group
    /// (which reads the classify output for the tile lookup) must
    /// re-bind the new buffer.
    MaterialClassifyBuffersResize,
    /// Decals GPU buffer was re-allocated (capacity grew). The decal
    /// pass's main bind group must re-bind the new buffer handle.
    /// In v1 the buffer is fixed-capacity so this event never fires;
    /// kept here so a future dynamic-resize path (when MAX_DECAL_COUNT
    /// becomes a per-frame value) doesn't need to add a new variant.
    DecalsResize,
    /// Dynamic-materials `extras_pool` GPU buffer was reallocated (the
    /// bump allocator overflowed and the pool grew to 2× capacity).
    /// Both the opaque and transparent main bind groups bind
    /// `extras_pool.buffer` and must re-bind the new handle.
    ExtrasPoolResize,
    /// GPU light-culling froxel buffers were (re)allocated — either
    /// because the viewport tile count grew, or because the auto-grow
    /// path bumped `max_per_froxel_capacity`. The cull pass rebinds
    /// the new buffer handles; the transparent + opaque main bind
    /// groups also rebind (Phase 1C / Phase 2 of the light-culling
    /// plan land the consumer-side bindings).
    LightCullingFroxelsResize,
}

/// Tracks pending bind group recreations.
pub struct BindGroups {
    create_list: HashSet<BindGroupCreate>,
}

impl BindGroups {
    /// Creates a bind group tracker with all groups marked dirty.
    /// Variants belonging to passes gated off by [`RendererFeatures`]
    /// are filtered out — they would otherwise fire on the first
    /// frame and try to bind resources that were never allocated.
    pub fn new(features: &RendererFeatures) -> Self {
        let create_list = BindGroupCreate::iter()
            .filter(|v| match v {
                BindGroupCreate::OcclusionBuffersResize
                | BindGroupCreate::CompactionBuffersResize => features.gpu_culling,
                BindGroupCreate::DecalsResize | BindGroupCreate::DecalClassifyBuffersResize => {
                    features.decals
                }
                _ => true,
            })
            .collect::<HashSet<_>>();
        Self { create_list }
    }

    /// Marks a bind group recreation reason.
    pub fn mark_create(&mut self, create: BindGroupCreate) {
        self.create_list.insert(create);
    }

    /// Recreates bind groups affected by pending changes.
    pub fn recreate(
        &mut self,
        ctx: BindGroupRecreateContext<'_>,
        render_passes: &mut RenderPasses,
        picker: Option<&mut Picker>,
    ) -> crate::error::Result<()> {
        if self.create_list.is_empty() {
            return Ok(());
        }

        #[derive(Hash, Debug, Clone, Copy, PartialEq, Eq)]
        enum FunctionToCall {
            GeometryCamera,
            GeometryTransformMaterials,
            GeometryMeta,
            GeometryAnimation,
            GeometryMasked,
            ShadowMasked,
            Hzb,
            Occlusion,
            OcclusionCompaction,
            Coverage,
            MaterialClassify,
            MaterialPrep,
            MaterialDecalMain,
            MaterialDecalComposite,
            MaterialDecalClassify,
            MaterialDecalTextures,
            OpaqueMain,
            OpaqueLights,
            OpaqueTextures,
            OpaqueShadows,
            TransparentMain,
            TransparentMeshMaterial,
            TransparentLights,
            TransparentTextures,
            TransparentShadows,
            LightCulling,
            Bloom,
            Ssr,
            Effects,
            Display,
            Picker,
        }

        let mut functions_to_call = HashSet::new();

        for create in self.create_list.drain() {
            match create {
                BindGroupCreate::CameraInitOnly => {
                    functions_to_call.insert(FunctionToCall::GeometryCamera);
                    functions_to_call.insert(FunctionToCall::GeometryMasked);
                    functions_to_call.insert(FunctionToCall::ShadowMasked);
                    functions_to_call.insert(FunctionToCall::TransparentMain);
                }
                BindGroupCreate::LightsInfoCreate => {
                    functions_to_call.insert(FunctionToCall::OpaqueLights);
                    functions_to_call.insert(FunctionToCall::TransparentLights);
                    // Prep's group(1) binds lights_info / lights (Stage 3b).
                    functions_to_call.insert(FunctionToCall::MaterialPrep);
                }
                BindGroupCreate::LightsResize => {
                    functions_to_call.insert(FunctionToCall::OpaqueLights);
                    functions_to_call.insert(FunctionToCall::TransparentLights);
                    // Prep's group(1) binds the punctual-light buffer (Stage 3b).
                    functions_to_call.insert(FunctionToCall::MaterialPrep);
                }
                BindGroupCreate::TransformsResize => {
                    functions_to_call.insert(FunctionToCall::GeometryTransformMaterials);
                    functions_to_call.insert(FunctionToCall::TransparentMain);
                    functions_to_call.insert(FunctionToCall::TransparentMeshMaterial);
                }
                BindGroupCreate::MaterialResize => {
                    functions_to_call.insert(FunctionToCall::GeometryTransformMaterials);
                    functions_to_call.insert(FunctionToCall::GeometryMasked);
                    functions_to_call.insert(FunctionToCall::ShadowMasked);
                    functions_to_call.insert(FunctionToCall::OpaqueMain);
                    functions_to_call.insert(FunctionToCall::TransparentMeshMaterial);
                    // The deferred material-classify pass binds the material
                    // data buffer (`materials.gpu_buffer`, slot 2) to read each
                    // material's shader_id when bucketing tiles. Materials
                    // register asynchronously during a scene load, so this
                    // buffer reallocates mid-load; without rebinding, classify
                    // reads stale material data, misclassifies, and emits zero
                    // workgroups for the live buckets — the indirect opaque
                    // shading dispatch then covers no tiles and the geometry
                    // renders black until an unrelated event (e.g. a viewport
                    // resize) rebuilds the classify bind group.
                    functions_to_call.insert(FunctionToCall::MaterialClassify);
                }
                BindGroupCreate::GeometryMeshMetaResize => {
                    functions_to_call.insert(FunctionToCall::GeometryMeta);
                    functions_to_call.insert(FunctionToCall::TransparentMeshMaterial);
                }
                BindGroupCreate::GeometryMorphTargetWeightsResize
                | BindGroupCreate::GeometryMorphTargetValuesResize
                | BindGroupCreate::SkinJointMatricesResize
                | BindGroupCreate::SkinJointIndexAndWeightsResize => {
                    functions_to_call.insert(FunctionToCall::GeometryAnimation);
                    functions_to_call.insert(FunctionToCall::TransparentMain);
                }
                BindGroupCreate::TextureViewRecreate => {
                    functions_to_call.insert(FunctionToCall::Hzb);
                    functions_to_call.insert(FunctionToCall::Occlusion);
                    functions_to_call.insert(FunctionToCall::LightCulling);
                    functions_to_call.insert(FunctionToCall::MaterialClassify);
                    // Prep binds the visibility + barycentric views and its own
                    // output storage textures (all recreated on resize). No-op
                    // when prep is off (the dispatch arm skips when `None`).
                    functions_to_call.insert(FunctionToCall::MaterialPrep);
                    functions_to_call.insert(FunctionToCall::MaterialDecalMain);
                    functions_to_call.insert(FunctionToCall::MaterialDecalComposite);
                    // The decal classify bind group binds the HZB view
                    // when `gpu_culling && decals`; rebuild on every
                    // HZB view recreate. No-op when the HZB is off
                    // (the dispatcher filter below short-circuits).
                    functions_to_call.insert(FunctionToCall::MaterialDecalClassify);
                    functions_to_call.insert(FunctionToCall::Display);
                    functions_to_call.insert(FunctionToCall::Effects);
                    // Bloom pyramid pass binds composite (read) + the full-res
                    // bloom texture (write) + its per-mip pyramid views — all
                    // recreated on resize.
                    functions_to_call.insert(FunctionToCall::Bloom);
                    // SSR binds depth + normal + transparent (read) + the ssr
                    // target (write); all recreated on resize.
                    functions_to_call.insert(FunctionToCall::Ssr);
                    functions_to_call.insert(FunctionToCall::OpaqueMain);
                    functions_to_call.insert(FunctionToCall::TransparentMain);
                    functions_to_call.insert(FunctionToCall::Picker);
                    // Coverage pass binds `visibility_data`; rebuild
                    // on view recreate.
                    functions_to_call.insert(FunctionToCall::Coverage);
                }
                BindGroupCreate::TexturePool => {
                    functions_to_call.insert(FunctionToCall::OpaqueTextures);
                    functions_to_call.insert(FunctionToCall::TransparentTextures);
                    functions_to_call.insert(FunctionToCall::MaterialDecalTextures);
                    // Masked group-0 carries the texture pool too. NOTE: when the
                    // pool *grows* the masked layout changes — the finalize flow
                    // relayouts the bind group + recompiles the pipelines first,
                    // then marks TexturePool so this rebinds against the new layout.
                    functions_to_call.insert(FunctionToCall::GeometryMasked);
                    functions_to_call.insert(FunctionToCall::ShadowMasked);
                }
                BindGroupCreate::TextureTransformsResize => {
                    functions_to_call.insert(FunctionToCall::OpaqueTextures);
                    functions_to_call.insert(FunctionToCall::TransparentTextures);
                    functions_to_call.insert(FunctionToCall::GeometryMasked);
                    functions_to_call.insert(FunctionToCall::ShadowMasked);
                }
                BindGroupCreate::BrdfLutTextures => {
                    functions_to_call.insert(FunctionToCall::OpaqueMain);
                    functions_to_call.insert(FunctionToCall::TransparentLights);
                }
                BindGroupCreate::IblTextures => {
                    functions_to_call.insert(FunctionToCall::OpaqueMain);
                    functions_to_call.insert(FunctionToCall::TransparentLights);
                    // The SSR trace binds the prefiltered specular env as its
                    // miss-path fallback; a specular-slot swap must rebind
                    // the trace group or it samples a stale view.
                    functions_to_call.insert(FunctionToCall::Ssr);
                }
                BindGroupCreate::EnvironmentSkyboxCreate => {
                    functions_to_call.insert(FunctionToCall::OpaqueMain);
                    functions_to_call.insert(FunctionToCall::TransparentLights);
                    // The SSR trace binds the skybox cubemap + sampler as its
                    // miss-path environment fallback; a skybox swap must
                    // rebind the trace group or it samples a stale view.
                    functions_to_call.insert(FunctionToCall::Ssr);
                }
                BindGroupCreate::MaterialMorphTargetWeightsResize => {
                    functions_to_call.insert(FunctionToCall::OpaqueMain);
                    functions_to_call.insert(FunctionToCall::TransparentMain);
                }
                BindGroupCreate::MaterialMorphTargetValuesResize => {
                    functions_to_call.insert(FunctionToCall::OpaqueMain);
                    functions_to_call.insert(FunctionToCall::TransparentMain);
                }
                BindGroupCreate::MaterialMeshMetaResize => {
                    functions_to_call.insert(FunctionToCall::OpaqueMain);
                    functions_to_call.insert(FunctionToCall::GeometryMasked);
                    functions_to_call.insert(FunctionToCall::ShadowMasked);
                    functions_to_call.insert(FunctionToCall::TransparentMain);
                    functions_to_call.insert(FunctionToCall::Picker);
                    // The deferred material-classify pass also binds the
                    // per-mesh material-meta buffer (`meta.material_gpu_buffer`,
                    // slot 1) to map each visibility sample → shader_id when
                    // bucketing tiles. When the meta buffer is reallocated on
                    // grow, classify must rebind it too — otherwise it reads a
                    // stale buffer, misclassifies (often emitting zero
                    // workgroups for the live buckets), and the indirect opaque
                    // shading dispatch covers no tiles, so freshly-loaded
                    // geometry renders black until some unrelated event (e.g. a
                    // viewport resize) rebuilds the classify bind group.
                    functions_to_call.insert(FunctionToCall::MaterialClassify);
                    // Prep binds the per-mesh material-meta buffer (slot 3).
                    functions_to_call.insert(FunctionToCall::MaterialPrep);
                }
                BindGroupCreate::MeshGeometryPoolResize => {
                    functions_to_call.insert(FunctionToCall::OpaqueMain);
                    functions_to_call.insert(FunctionToCall::GeometryMasked);
                    functions_to_call.insert(FunctionToCall::ShadowMasked);
                    // Prep binds the merged geometry pool (storage slot 2).
                    functions_to_call.insert(FunctionToCall::MaterialPrep);
                }
                BindGroupCreate::AntiAliasingChange => {
                    functions_to_call.insert(FunctionToCall::OpaqueMain);
                    functions_to_call.insert(FunctionToCall::TransparentMain);
                    // Coverage pass binds a multisampled vs
                    // single-sample visibility-data view depending
                    // on the active MSAA setting.
                    functions_to_call.insert(FunctionToCall::Coverage);
                }
                BindGroupCreate::InstanceAttributesResize => {
                    // Per-instance attribute storage buffer is bound on the
                    // opaque + transparent main bind groups for shading lookup.
                    functions_to_call.insert(FunctionToCall::OpaqueMain);
                    functions_to_call.insert(FunctionToCall::TransparentMain);
                }
                BindGroupCreate::ShadowsResourcesChange => {
                    functions_to_call.insert(FunctionToCall::OpaqueShadows);
                    functions_to_call.insert(FunctionToCall::TransparentShadows);
                    // Prep's group(2) binds the shadow atlas / cube / cascade /
                    // EVSM views + globals (Stage 3b — prep samples shadows).
                    functions_to_call.insert(FunctionToCall::MaterialPrep);
                }
                BindGroupCreate::MaterialClassifyBuffersResize => {
                    // Classify rebuilds its own bind group; opaque
                    // main re-binds the buckets read-only.
                    functions_to_call.insert(FunctionToCall::MaterialClassify);
                    functions_to_call.insert(FunctionToCall::OpaqueMain);
                }
                BindGroupCreate::DecalsResize => {
                    // Decals buffer is bound on the decal pass's
                    // main bind group.
                    functions_to_call.insert(FunctionToCall::MaterialDecalMain);
                }
                BindGroupCreate::OcclusionBuffersResize => {
                    functions_to_call.insert(FunctionToCall::Occlusion);
                    functions_to_call.insert(FunctionToCall::OcclusionCompaction);
                }
                BindGroupCreate::CompactionBuffersResize => {
                    functions_to_call.insert(FunctionToCall::OcclusionCompaction);
                }
                BindGroupCreate::CoverageBuffersResize => {
                    functions_to_call.insert(FunctionToCall::Coverage);
                }
                BindGroupCreate::DecalClassifyBuffersResize => {
                    functions_to_call.insert(FunctionToCall::MaterialDecalClassify);
                    functions_to_call.insert(FunctionToCall::MaterialDecalMain);
                }
                BindGroupCreate::ExtrasPoolResize => {
                    // `extras_pool.buffer` is bound on the opaque
                    // + transparent main bind groups (see the
                    // `recreate_main` paths in each pass). A pool
                    // resize re-allocates the GPU buffer, so both
                    // groups must re-bind the new handle.
                    functions_to_call.insert(FunctionToCall::OpaqueMain);
                    functions_to_call.insert(FunctionToCall::TransparentMain);
                }
                BindGroupCreate::LightCullingFroxelsResize => {
                    // Cull pass owns the froxel buffers; on resize it
                    // must re-bind them. The merged `storage_buffer` +
                    // `params_buffer` are also bound on the opaque /
                    // transparent **lights** bind groups (via
                    // `recreate_lights`), NOT the main bind groups —
                    // `recreate_main` binds only frame_globals +
                    // extras_pool. So the consumer fan-out must target
                    // OpaqueLights / TransparentLights, otherwise the
                    // lights bind groups keep a stale buffer handle
                    // after every viewport / mesh-indices / per-froxel
                    // capacity resize.
                    functions_to_call.insert(FunctionToCall::LightCulling);
                    functions_to_call.insert(FunctionToCall::OpaqueLights);
                    functions_to_call.insert(FunctionToCall::TransparentLights);
                    // Prep's group(1) binds lights_storage + cull_params, both
                    // reallocated on a froxel-buffer resize (Stage 3b).
                    functions_to_call.insert(FunctionToCall::MaterialPrep);
                }
            }
        }

        // Gate the function calls for passes whose feature is off.
        // The dispatcher receives events from unrelated resources
        // (e.g. `TextureViewRecreate` fires for every pass that owns
        // a texture-view-dependent bind group, including the HZB /
        // occlusion / decal passes); without this filter, the
        // recreators would try to bind buffers / texture views that
        // were never allocated.
        let features = ctx.features;
        let mut picker = picker;
        let picker_present = picker.is_some();
        let allow_function = |f: FunctionToCall| match f {
            FunctionToCall::Hzb
            | FunctionToCall::Occlusion
            | FunctionToCall::OcclusionCompaction => features.gpu_culling,
            FunctionToCall::MaterialDecalMain
            | FunctionToCall::MaterialDecalComposite
            | FunctionToCall::MaterialDecalClassify
            | FunctionToCall::MaterialDecalTextures => features.decals,
            FunctionToCall::Picker => picker_present,
            _ => true,
        };

        for f in functions_to_call.into_iter().filter(|f| allow_function(*f)) {
            match f {
                FunctionToCall::GeometryCamera => {
                    render_passes.geometry.bind_groups.camera.recreate(&ctx)?;
                }
                FunctionToCall::GeometryTransformMaterials => {
                    render_passes
                        .geometry
                        .bind_groups
                        .transforms
                        .recreate(&ctx)?;
                }
                FunctionToCall::GeometryMeta => {
                    render_passes.geometry.bind_groups.meta.recreate(&ctx)?;
                }
                FunctionToCall::GeometryAnimation => {
                    render_passes
                        .geometry
                        .bind_groups
                        .animation
                        .recreate(&ctx)?;
                }
                FunctionToCall::GeometryMasked => {
                    // Masked group-0 binds camera/frame_globals + materials,
                    // material_mesh_metas, the merged geometry pool,
                    // texture_transforms and the texture pool — so it recreates
                    // on the union of those buffers' resize events (see the
                    // fan-out above).
                    render_passes.geometry.masked_bind_group.recreate(&ctx)?;
                }
                FunctionToCall::ShadowMasked => {
                    // Masked-shadow group-0 binds shadow_view (from
                    // `ctx.shadows`) + materials, material_mesh_metas, the merged
                    // geometry pool, texture_transforms and the texture pool —
                    // the same resize-event union as the geometry masked group.
                    render_passes.shadow_masked.bind_group.recreate(&ctx)?;
                }
                FunctionToCall::OpaqueMain => {
                    render_passes
                        .material_opaque
                        .bind_groups
                        .recreate_main(&ctx)?;
                }
                FunctionToCall::OpaqueLights => {
                    render_passes
                        .material_opaque
                        .bind_groups
                        .recreate_lights(&ctx)?;
                }
                FunctionToCall::OpaqueTextures => {
                    render_passes
                        .material_opaque
                        .bind_groups
                        .recreate_texture_pool(&ctx)?;
                }
                FunctionToCall::OpaqueShadows => {
                    render_passes
                        .material_opaque
                        .bind_groups
                        .recreate_shadows(&ctx)?;
                }
                FunctionToCall::TransparentShadows => {
                    render_passes
                        .material_transparent
                        .bind_groups
                        .recreate_shadows(&ctx)?;
                }
                FunctionToCall::TransparentMain => {
                    render_passes
                        .material_transparent
                        .bind_groups
                        .recreate_main(&ctx)?;
                }
                FunctionToCall::TransparentMeshMaterial => {
                    render_passes
                        .material_transparent
                        .bind_groups
                        .recreate_mesh_material(&ctx)?;
                }
                FunctionToCall::TransparentLights => {
                    // 16.B folded `lights` into `main` on the transparent
                    // pipeline; the upstream signal still fires, but it
                    // now routes through `recreate_main` so the merged
                    // group picks up the new IBL / light buffer views.
                    render_passes
                        .material_transparent
                        .bind_groups
                        .recreate_main(&ctx)?;
                }
                FunctionToCall::TransparentTextures => {
                    render_passes
                        .material_transparent
                        .bind_groups
                        .recreate_texture_pool(&ctx)?;
                }
                FunctionToCall::LightCulling => {
                    render_passes.light_culling.bind_groups.recreate(&ctx)?;
                }
                FunctionToCall::Hzb => {
                    // `allow_function` already gated this on
                    // `features.gpu_culling`; the unwrap is sound.
                    let hzb = render_passes
                        .hzb
                        .as_mut()
                        .expect("HZB pass missing despite gpu_culling feature on");
                    hzb.bind_groups.recreate(&ctx, &hzb.texture)?;
                }
                FunctionToCall::Occlusion => {
                    render_passes
                        .occlusion
                        .as_mut()
                        .expect("Occlusion pass missing despite gpu_culling feature on")
                        .bind_groups
                        .recreate(&ctx)?;
                }
                FunctionToCall::OcclusionCompaction => {
                    render_passes
                        .occlusion_compaction
                        .as_mut()
                        .expect("Compaction pass missing despite gpu_culling feature on")
                        .bind_groups
                        .recreate(&ctx)?;
                }
                FunctionToCall::MaterialClassify => {
                    render_passes.material_classify.bind_groups.recreate(&ctx)?;
                }
                FunctionToCall::MaterialPrep => {
                    // Skip when prep is disabled (`None`) — the output storage
                    // textures don't exist, so there's nothing to (re)bind.
                    if let Some(prep) = render_passes.material_prep.as_mut() {
                        prep.bind_groups.recreate(&ctx)?;
                    }
                }
                FunctionToCall::Coverage => {
                    // Only rebuild the bind group that matches the
                    // current MSAA setting. Building both would bind
                    // a multisampled visibility_data view through a
                    // single-sample layout (or vice versa) →
                    // validation error. The render-time `render()`
                    // path picks the matching variant; the inactive
                    // variant's bind group stays `None` and is
                    // ignored.
                    if let Some(coverage) = render_passes.coverage.as_mut() {
                        if ctx.anti_aliasing.msaa_sample_count.is_some() {
                            coverage.bind_groups_multisampled.recreate(&ctx)?;
                        } else {
                            coverage.bind_groups_singlesampled.recreate(&ctx)?;
                        }
                    }
                }
                FunctionToCall::MaterialDecalMain => {
                    render_passes
                        .material_decal
                        .as_mut()
                        .expect("Decal pass missing despite decals feature on")
                        .bind_groups
                        .recreate_main(&ctx)?;
                }
                FunctionToCall::MaterialDecalComposite => {
                    // Deferred-boot: composite may not be compiled yet — its
                    // eventual `new()` builds bind groups against the
                    // then-current views, so skipping here loses nothing.
                    if let Some(composite) = render_passes
                        .material_decal
                        .as_mut()
                        .expect("Decal pass missing despite decals feature on")
                        .composite
                        .as_mut()
                    {
                        composite.recreate(&ctx)?;
                    }
                }
                FunctionToCall::MaterialDecalClassify => {
                    render_passes
                        .material_decal
                        .as_mut()
                        .expect("Decal pass missing despite decals feature on")
                        .classify_pass
                        .bind_groups
                        .recreate(&ctx)?;
                }
                FunctionToCall::MaterialDecalTextures => {
                    render_passes
                        .material_decal
                        .as_mut()
                        .expect("Decal pass missing despite decals feature on")
                        .bind_groups
                        .recreate_texture_pool(&ctx)?;
                }
                FunctionToCall::Bloom => {
                    // Lazy pass: `None` until bloom is first enabled — its
                    // eventual construction marks `TextureViewRecreate`, so
                    // this arm runs against the live views right after.
                    if let Some(bloom) = render_passes.bloom.as_mut() {
                        // Split-borrow: bind_groups (mut) vs texture/params (shared).
                        let crate::render_passes::bloom::render_pass::BloomRenderPass {
                            bind_groups,
                            texture,
                            params,
                            ..
                        } = bloom;
                        bind_groups.recreate(&ctx, texture, &params.gpu_buffer)?;
                    }
                }
                FunctionToCall::Ssr => {
                    // Lazy pass: `None` until SSR is first enabled (same flow
                    // as bloom above).
                    if let Some(ssr) = render_passes.ssr.as_mut() {
                        let crate::render_passes::ssr::render_pass::SsrRenderPass {
                            bind_groups,
                            params,
                            composite,
                            ..
                        } = ssr;
                        bind_groups.recreate(&ctx, &params.gpu_buffer)?;
                        composite.recreate(&ctx)?;
                    }
                }
                FunctionToCall::Effects => {
                    render_passes.effects.bind_groups.recreate(&ctx)?;
                }
                FunctionToCall::Display => {
                    render_passes.display.bind_groups.recreate(&ctx)?;
                }
                FunctionToCall::Picker => {
                    // Guarded above by `allow_function`'s
                    // `picker_present` check, but we still need to
                    // dereference the Option for the call site.
                    if let Some(p) = picker.as_mut() {
                        p.recreate_bind_group(&ctx)?;
                    }
                }
            }
        }

        Ok(())
    }
}

/// Bind group errors.
#[derive(Error, Debug)]
pub enum AwsmBindGroupError {
    #[error("[bind group] bind group not found for {0}")]
    NotFound(String),

    #[error("[bind group] texture pool placeholder {0} missing (neutrals not uploaded yet)")]
    TexturePoolPlaceholderMissing(&'static str),
}
