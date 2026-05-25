use awsm_renderer::{
    anti_alias::AntiAliasing, materials::pbr::PbrMaterialDebug, post_process::PostProcessing,
    RendererLoadingPhase,
};

use crate::prelude::*;

use super::scene::{camera::CameraId, AppScene};

#[derive(Clone)]
pub struct AppContext {
    pub camera_id: Mutable<CameraId>,
    pub scene: Mutable<Option<Arc<AppScene>>>,
    pub material_debug: Mutable<PbrMaterialDebug>,
    pub anti_alias: Mutable<AntiAliasing>,
    pub post_processing: Mutable<PostProcessing>,
    pub camera_aperture: Mutable<f32>,
    pub camera_focus_distance: Mutable<f32>,
    pub ibl_id: Mutable<IblId>,
    pub punctual_lights: Mutable<PunctualLightsMode>,
    pub skybox_id: Mutable<SkyboxId>,
    pub editor_grid_enabled: Mutable<bool>,
    pub editor_gizmo_translation_enabled: Mutable<bool>,
    pub editor_gizmo_rotation_enabled: Mutable<bool>,
    pub editor_gizmo_scale_enabled: Mutable<bool>,
    pub loading_status: Mutable<LoadingStatus>,
}

#[derive(Clone, Debug)]
pub struct LoadingStatus {
    pub renderer: std::result::Result<bool, String>,
    /// Active phase of `AwsmRendererBuilder::build()`, fed by the
    /// builder's `with_phase_handler` callback. `None` means the
    /// builder hasn't started or has reached `Ready`. Surfacing the
    /// active phase lets the UI render distinct messages over the
    /// long cold-cache window (~tens of seconds on a fresh Chrome
    /// profile) — "Browser is compiling shaders…" rather than a
    /// frozen "Initializing renderer…".
    pub renderer_phase: Option<RendererLoadingPhase>,
    /// Set true while `AwsmRenderer::prewarm_pipelines()` runs — the
    /// trailing edge of the cold-start shader-compile window that
    /// otherwise hides inside the (already slow) `renderer` phase.
    /// Surfaced separately so the user can see "Compiling shaders…"
    /// distinctly from "Initializing renderer…" — particularly on the
    /// first post-deploy load when the browser's PSO disk cache
    /// (see PERFORMANCE.md §5g) misses on the new shader hashes.
    pub shader_prewarm: std::result::Result<bool, String>,
    pub ibl: std::result::Result<bool, String>,
    pub skybox: std::result::Result<bool, String>,
    pub gltf_net: std::result::Result<bool, String>,
    pub gltf_data: std::result::Result<bool, String>,
    /// True while the main glTF's `populate_gltf` runs. Covers mesh
    /// resource allocation, per-mesh meta upload, AND
    /// `finalize_gpu_textures` (texture-array bind group rebuild +
    /// mipmap generation pass). On a cold first load this is where
    /// the multi-MB PBR textures incur their GPU upload cost AND
    /// where the WebGPU driver actually finalises any pipelines
    /// whose layout depended on the new bind groups — see
    /// `PERFORMANCE.md §5g` for the browser-PSO-cache mechanics.
    /// Naming reflects the work the user can actually see ("uploading
    /// to GPU") rather than the renderer-internal call name
    /// ("populate_gltf").
    pub populate_gpu_upload: std::result::Result<bool, String>,
    /// True for the trailing-edge work after `populate_gltf` resolves
    /// — gizmo populate, IBL set, skybox bind, light/material/anti-
    /// alias state resets. Quick on warm cache; the slowness on
    /// cold first load is in `populate_gpu_upload` above, not here.
    /// Surfaced separately so users see the bar move past the
    /// heavy phase instead of staying on "Populating scene" all
    /// the way through.
    pub populate_finalize: std::result::Result<bool, String>,
}

impl Default for LoadingStatus {
    fn default() -> Self {
        Self {
            renderer: Ok(false),
            renderer_phase: None,
            shader_prewarm: Ok(false),
            ibl: Ok(false),
            skybox: Ok(false),
            gltf_net: Ok(false),
            gltf_data: Ok(false),
            populate_gpu_upload: Ok(false),
            populate_finalize: Ok(false),
        }
    }
}

impl LoadingStatus {
    pub fn is_loading(&self) -> bool {
        matches!(self.renderer, Ok(true))
            || matches!(self.shader_prewarm, Ok(true))
            || matches!(self.ibl, Ok(true))
            || matches!(self.skybox, Ok(true))
            || matches!(self.gltf_net, Ok(true))
            || matches!(self.gltf_data, Ok(true))
            || matches!(self.populate_gpu_upload, Ok(true))
            || matches!(self.populate_finalize, Ok(true))
    }

    pub fn ok_strings(&self) -> Vec<String> {
        let mut statuses = Vec::new();

        // Renderer-init phase, fed by `AwsmRendererBuilder::with_phase_handler`.
        // When the builder is active and has reported a phase, that
        // phase's user-facing label takes priority over the generic
        // boolean `renderer` flag.
        if let Some(phase) = self.renderer_phase {
            match phase {
                RendererLoadingPhase::Init => {
                    statuses.push("Initializing renderer...".to_string());
                }
                RendererLoadingPhase::CompilingShaders => {
                    statuses.push(
                        "Browser is compiling shaders... (first load may take a while)".to_string(),
                    );
                }
                RendererLoadingPhase::BuildingPipelines => {
                    statuses.push("Building render pipelines...".to_string());
                }
                RendererLoadingPhase::FinalizingScene => {
                    statuses.push("Finalising renderer setup...".to_string());
                }
                RendererLoadingPhase::Ready => {
                    // Builder reported Ready — no banner needed for
                    // this row; the `renderer` flag (set false by
                    // the caller) drives the rest.
                }
            }
        } else if let Ok(true) = &self.renderer {
            // Fallback when the phase handler hasn't fired yet (very
            // start of init) — keep showing a generic banner so the
            // UI isn't blank.
            statuses.push("Initializing renderer...".to_string());
        }

        if let Ok(true) = &self.shader_prewarm {
            statuses.push("Compiling scene shaders...".to_string());
        }

        if let Ok(true) = &self.ibl {
            statuses.push("Loading IBL...".to_string());
        }
        if let Ok(true) = &self.skybox {
            statuses.push("Loading Skybox...".to_string());
        }
        if let Ok(true) = &self.gltf_net {
            statuses.push("Loading GLTF from network...".to_string());
        }
        if let Ok(true) = &self.gltf_data {
            statuses.push("Decoding GLTF (textures + meshes)...".to_string());
        }
        if let Ok(true) = &self.populate_gpu_upload {
            statuses.push("Uploading meshes + textures to GPU...".to_string());
        }
        if let Ok(true) = &self.populate_finalize {
            statuses.push("Finalizing scene (IBL, skybox, lights)...".to_string());
        }

        statuses
    }

    pub fn any_error(&self) -> bool {
        self.renderer.is_err()
            || self.shader_prewarm.is_err()
            || self.ibl.is_err()
            || self.skybox.is_err()
            || self.gltf_net.is_err()
            || self.gltf_data.is_err()
            || self.populate_gpu_upload.is_err()
            || self.populate_finalize.is_err()
    }

    pub fn err_strings(&self) -> Vec<String> {
        let mut errors = Vec::new();

        if let Err(err) = &self.renderer {
            errors.push(format!("Error initializing Renderer: {}", err));
        }
        if let Err(err) = &self.shader_prewarm {
            errors.push(format!("Error compiling shaders: {}", err));
        }
        if let Err(err) = &self.ibl {
            errors.push(format!("Error loading IBL: {}", err));
        }
        if let Err(err) = &self.skybox {
            errors.push(format!("Error loading Skybox: {}", err));
        }
        if let Err(err) = &self.gltf_net {
            errors.push(format!("Error loading GLTF from network: {}", err));
        }
        if let Err(err) = &self.gltf_data {
            errors.push(format!("Error decoding GLTF: {}", err));
        }
        if let Err(err) = &self.populate_gpu_upload {
            errors.push(format!("Error uploading scene to GPU: {}", err));
        }
        if let Err(err) = &self.populate_finalize {
            errors.push(format!("Error finalizing scene: {}", err));
        }
        errors
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IblId {
    PhotoStudio,
    SimpleSky,
    AllWhite,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SkyboxId {
    SameAsIbl,
    SpecificIbl(IblId),
    // Not a real mode, just for debugging to use original default from renderer
    None,
}

/// Which set of punctual lights the model-tests scene should contribute.
///
/// "Model lights" are the `KHR_lights_punctual` lights the gltf populator
/// inserted from the loaded asset (e.g. lamps inside the PlaysetLightTest
/// scene). "Additional lights" is the four-directional fill the app sets
/// up so the default scene looks lit even when an asset doesn't carry
/// its own lighting.
///
/// Default is `Auto` — the previous app behavior: use the asset's lights
/// if it brings any, otherwise fall back to the additional fill. The
/// other four modes are explicit overrides for testing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PunctualLightsMode {
    /// No punctual lights at all (IBL still applies).
    Off,
    /// Use only the lights that came from the gltf asset. Falls back to
    /// no lights if the asset doesn't define any.
    ModelOnly,
    /// Use only the default four-directional fill. Strips any lights
    /// the gltf defined.
    AdditionalOnly,
    /// Both the model lights and the additional fill (tends to
    /// overexpose authored-lit assets, but useful for inspection).
    On,
    /// Smart default: use the asset's lights when present, otherwise
    /// fall back to the additional fill. Keeps light-test assets like
    /// `PlaysetLightTest` and `PointLightIntensityTest` reading right
    /// while still lighting up everything else.
    Auto,
}

impl Default for AppContext {
    fn default() -> Self {
        Self {
            camera_id: Mutable::new(CameraId::default()),
            scene: Mutable::new(None),
            material_debug: Mutable::new(CONFIG.initial_material_debug),
            ibl_id: Mutable::new(CONFIG.initial_ibl),
            skybox_id: Mutable::new(CONFIG.initial_skybox),
            editor_grid_enabled: Mutable::new(CONFIG.initial_show_grid),
            editor_gizmo_translation_enabled: Mutable::new(CONFIG.initial_show_gizmo_translation),
            editor_gizmo_rotation_enabled: Mutable::new(CONFIG.initial_show_gizmo_rotation),
            editor_gizmo_scale_enabled: Mutable::new(CONFIG.initial_show_gizmo_scale),
            loading_status: Mutable::new(LoadingStatus::default()),
            punctual_lights: Mutable::new(CONFIG.initial_punctual_lights),
            anti_alias: Mutable::new(CONFIG.initial_anti_alias.clone()),
            post_processing: Mutable::new(CONFIG.initial_post_processing.clone()),
            camera_aperture: Mutable::new(CONFIG.initial_camera_aperture),
            camera_focus_distance: Mutable::new(CONFIG.initial_camera_focus_distance),
        }
    }
}
