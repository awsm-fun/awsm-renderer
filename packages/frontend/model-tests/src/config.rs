#![allow(dead_code)]
use std::sync::{Arc, LazyLock, Mutex};

use awsm_renderer::{
    anti_alias::AntiAliasing,
    materials::pbr::PbrMaterialDebug,
    post_process::PostProcessing,
    taa::{TaaConfig, TaaPreset},
};

use crate::{
    pages::app::context::{IblId, PunctualLightsMode, SkyboxId},
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
    pub initial_punctual_lights: PunctualLightsMode,
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
        // Default to "Auto": authored-lit assets (PlaysetLightTest,
        // PointLightIntensityTest, LightsPunctualLamp) use just their own
        // lights, while unlit assets still get the four-directional fill.
        // The other four modes are explicit overrides exposed in the
        // Lighting sidebar.
        initial_punctual_lights: PunctualLightsMode::Auto,
        initial_material_debug: PbrMaterialDebug::None,
        // `?aa=` overrides the default (see `AntiAliasMode::from_url`).
        initial_anti_alias: AntiAliasMode::from_url()
            .map(AntiAliasMode::anti_aliasing)
            .unwrap_or_default(),
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

/// The anti-aliasing modes the Processing sidebar offers. They are mutually
/// exclusive: the renderer rejects TAA combined with MSAA or SMAA.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AntiAliasMode {
    None,
    Msaa4,
    Smaa,
    Taa(TaaPreset),
}

impl AntiAliasMode {
    /// Every mode, in the order the selector lists them.
    pub const ALL: [AntiAliasMode; 6] = [
        AntiAliasMode::None,
        AntiAliasMode::Msaa4,
        AntiAliasMode::Smaa,
        AntiAliasMode::Taa(TaaPreset::Native),
        AntiAliasMode::Taa(TaaPreset::Quality),
        AntiAliasMode::Taa(TaaPreset::Performance),
    ];

    pub fn label(self) -> &'static str {
        match self {
            AntiAliasMode::None => "None",
            AntiAliasMode::Msaa4 => "MSAA 4x",
            AntiAliasMode::Smaa => "SMAA",
            AntiAliasMode::Taa(TaaPreset::Native) => "TAA (native)",
            AntiAliasMode::Taa(TaaPreset::Quality) => "TAA Quality (0.67)",
            AntiAliasMode::Taa(TaaPreset::Balanced) => "TAA Balanced (0.58)",
            AntiAliasMode::Taa(TaaPreset::Performance) => "TAA Performance (0.5)",
        }
    }

    pub fn anti_aliasing(self) -> AntiAliasing {
        match self {
            AntiAliasMode::None => AntiAliasing::none(),
            AntiAliasMode::Msaa4 => AntiAliasing::msaa4(),
            AntiAliasMode::Smaa => AntiAliasing::smaa(),
            AntiAliasMode::Taa(preset) => AntiAliasing::taa(TaaConfig::preset(preset)),
        }
    }

    /// The mode an `AntiAliasing` corresponds to. TAA maps to the preset its
    /// render scale matches; `None` for a combination the selector does not
    /// offer (e.g. MSAA + SMAA).
    pub fn from_anti_aliasing(aa: &AntiAliasing) -> Option<Self> {
        match (aa.msaa_sample_count, aa.smaa, &aa.taa) {
            (None, false, None) => Some(AntiAliasMode::None),
            (Some(4), false, None) => Some(AntiAliasMode::Msaa4),
            (None, true, None) => Some(AntiAliasMode::Smaa),
            (None, false, Some(taa)) => taa.matching_preset().map(AntiAliasMode::Taa),
            _ => None,
        }
    }

    /// `?aa=none|msaa|smaa|taa|taa-quality|taa-balanced|taa-performance`
    /// (`taa67` / `taa58` / `taa50` are accepted too). `None` when absent or
    /// unknown, so the default applies.
    pub fn from_url() -> Option<Self> {
        let search = web_sys::window()?.location().search().ok()?;
        let value = search
            .trim_start_matches('?')
            .split('&')
            .find_map(|pair| pair.strip_prefix("aa="))?;
        match value {
            "none" | "off" => Some(AntiAliasMode::None),
            "msaa" | "msaa4" => Some(AntiAliasMode::Msaa4),
            "smaa" => Some(AntiAliasMode::Smaa),
            "taa" | "taa-native" => Some(AntiAliasMode::Taa(TaaPreset::Native)),
            "taa-quality" | "taa67" => Some(AntiAliasMode::Taa(TaaPreset::Quality)),
            "taa-balanced" | "taa58" => Some(AntiAliasMode::Taa(TaaPreset::Balanced)),
            "taa-performance" | "taa50" => Some(AntiAliasMode::Taa(TaaPreset::Performance)),
            other => {
                tracing::warn!("unknown ?aa={other}, using the default anti-aliasing");
                None
            }
        }
    }
}
