use awsm_renderer::{
    anti_alias::AntiAliasing, materials::pbr::PbrMaterialDebug, post_process::PostProcessing,
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
    pub punctual_lights: Mutable<bool>,
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
    pub ibl: std::result::Result<bool, String>,
    pub skybox: std::result::Result<bool, String>,
    pub gltf_net: std::result::Result<bool, String>,
    pub gltf_data: std::result::Result<bool, String>,
    pub populate: std::result::Result<bool, String>,
}

impl Default for LoadingStatus {
    fn default() -> Self {
        Self {
            renderer: Ok(false),
            ibl: Ok(false),
            skybox: Ok(false),
            gltf_net: Ok(false),
            gltf_data: Ok(false),
            populate: Ok(false),
        }
    }
}

impl LoadingStatus {
    pub fn is_loading(&self) -> bool {
        matches!(self.renderer, Ok(true))
            || matches!(self.ibl, Ok(true))
            || matches!(self.skybox, Ok(true))
            || matches!(self.gltf_net, Ok(true))
            || matches!(self.gltf_data, Ok(true))
            || matches!(self.populate, Ok(true))
    }

    pub fn ok_strings(&self) -> Vec<String> {
        let mut statuses = Vec::new();

        if let Ok(true) = &self.renderer {
            statuses.push("Initializing Renderer...".to_string());
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
            statuses.push("Loading GLTF data...".to_string());
        }
        if let Ok(true) = &self.populate {
            statuses.push("Populating scene...".to_string());
        }

        statuses
    }

    pub fn any_error(&self) -> bool {
        self.renderer.is_err()
            || self.ibl.is_err()
            || self.skybox.is_err()
            || self.gltf_net.is_err()
            || self.gltf_data.is_err()
            || self.populate.is_err()
    }

    pub fn err_strings(&self) -> Vec<String> {
        let mut errors = Vec::new();

        if let Err(err) = &self.renderer {
            errors.push(format!("Error initializing Renderer: {}", err));
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
            errors.push(format!("Error loading GLTF data: {}", err));
        }
        if let Err(err) = &self.populate {
            errors.push(format!("Error populating scene: {}", err));
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
