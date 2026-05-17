#![allow(dead_code)]
use std::sync::{Arc, LazyLock, Mutex};

use awsm_renderer::{
    anti_alias::AntiAliasing, materials::pbr::PbrMaterialDebug, post_process::PostProcessing,
};

use crate::{
    pages::app::context::{IblId, SkyboxId},
    route::{AppRoute, Route},
};

#[derive(Debug, Clone)]
pub struct Config {
    pub debug: ConfigDebug,
    pub root_base_uri_path: String,
    pub media_base_url_gltf_samples: String,
    pub media_base_url_additional_assets: String,
    pub generate_mipmaps: bool,
    pub post_processing_enabled: bool,
    pub initial_ibl: IblId,
    pub initial_skybox: SkyboxId,
    pub cache_buster: bool,
    pub initial_material_debug: PbrMaterialDebug,
    pub initial_show_grid: bool,
    pub initial_show_gizmo_translation: bool,
    pub initial_show_gizmo_rotation: bool,
    pub initial_show_gizmo_scale: bool,
    pub initial_punctual_lights: bool,
    pub initial_anti_alias: AntiAliasing,
    pub initial_post_processing: PostProcessing,
    pub initial_camera_aperture: f32,
    pub initial_camera_focus_distance: f32,
    pub repo_url: &'static str,
}

#[allow(clippy::option_env_unwrap)]
#[allow(clippy::if_same_then_else)]
pub static CONFIG: LazyLock<Config> = LazyLock::new(|| {
    Config {
        media_base_url_gltf_samples: option_env!("MEDIA_BASE_URL_GLTF_SAMPLES")
            .expect("MEDIA_BASE_URL_GLTF_SAMPLES must be set")
            .to_string(),
        media_base_url_additional_assets: option_env!("MEDIA_BASE_URL_ADDITIONAL_ASSETS")
            .expect("MEDIA_BASE_URL_ADDITIONAL_ASSETS must be set")
            .to_string(),
        root_base_uri_path: option_env!("ROOT_BASE_URI_PATH")
            .expect("ROOT_BASE_URI_PATH must be set")
            .to_string(),
        debug: if cfg!(debug_assertions) {
            //ConfigDebug::release_mode()
            ConfigDebug::dev_mode()
        } else {
            ConfigDebug::release_mode()
        },

        generate_mipmaps: true,
        post_processing_enabled: true,
        initial_ibl: if cfg!(debug_assertions) {
            IblId::PhotoStudio
        } else {
            IblId::PhotoStudio
        },
        initial_skybox: if cfg!(debug_assertions) {
            SkyboxId::SameAsIbl
        } else {
            SkyboxId::SpecificIbl(IblId::SimpleSky)
        },
        cache_buster: cfg!(debug_assertions),
        initial_show_grid: false,
        initial_show_gizmo_translation: false,
        initial_show_gizmo_rotation: false,
        initial_show_gizmo_scale: false,
        initial_punctual_lights: true,
        initial_material_debug: PbrMaterialDebug::None,
        initial_anti_alias: AntiAliasing::default(),
        initial_post_processing: PostProcessing::default(),
        initial_camera_aperture: 5.6,
        initial_camera_focus_distance: 10.0,
        repo_url: "https://github.com/dakom/awsm-renderer",
    }
});

#[derive(Debug, Clone)]
pub struct ConfigDebug {
    pub start_route: Arc<Mutex<Option<Route>>>,
}

impl ConfigDebug {
    fn dev_mode() -> Self {
        Self {
            start_route: Arc::new(Mutex::new(Some(Route::App(AppRoute::Init)))),
        }
    }

    fn release_mode() -> Self {
        Self {
            start_route: Arc::new(Mutex::new(None)),
        }
    }
}
