use awsm_renderer::core::cubemap::images::{CubemapBitmapColors, CubemapSkyGradient};
use awsm_renderer::core::cubemap::CubemapImage;

use crate::prelude::*;

pub async fn load_from_path(path: &str) -> Result<CubemapImage> {
    let filename = if CONFIG.cache_buster {
        format!("skybox.ktx2?cb={}", js_sys::Date::now())
    } else {
        "skybox.ktx2".to_string()
    };

    let skybox_cubemap = CubemapImage::load_url_ktx(&format!(
        "{}/{}/{}",
        CONFIG.media_base_url_additional_assets, path, filename
    ))
    .await?;

    Ok(skybox_cubemap)
}

pub async fn load_from_colors(colors: CubemapBitmapColors) -> Result<CubemapImage> {
    Ok(CubemapImage::new_colors(colors, 1024, 1024).await?)
}

pub async fn load_simple_sky() -> Result<CubemapImage> {
    Ok(CubemapImage::new_sky_gradient(CubemapSkyGradient::default(), 1024, 1024).await?)
}
