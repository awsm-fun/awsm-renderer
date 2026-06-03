//! Reporting types for texture pool usage.

use serde::{Deserialize, Serialize};

use crate::texture::mipmap::calculate_mipmap_levels;

/// Summary report for a texture pool.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TexturePoolReport<ID> {
    pub arrays: Vec<TexturePoolArrayReport<ID>>,
    pub arrays_free: usize,
}

/// Report for a single texture array in the pool.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TexturePoolArrayReport<ID> {
    pub format: String,
    pub width: u32,
    pub height: u32,
    pub mipmap: bool,
    pub mip_levels: u32,
    pub layers_free: usize,
    pub width_remaining: u32,
    pub height_remaining: u32,
    pub entries: Vec<TexturePoolEntryReport<ID>>,
}

/// Report for a single texture entry in a pool array.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TexturePoolEntryReport<ID> {
    pub id: ID,
    pub mipmap_kind: String,
    pub srgb_to_linear: bool,
    pub premultiplied_alpha: Option<bool>,
}

impl<ID> super::TexturePool<ID> {
    /// Generates a report using the provided GPU limits.
    pub fn generate_report(&self, limits: &web_sys::GpuSupportedLimits) -> TexturePoolReport<ID>
    where
        ID: Clone,
    {
        let mut arrays_report = Vec::new();

        for (array_key, array) in &self.arrays {
            let mut entries_report = Vec::new();

            for (id, _, color_info) in &array.images {
                entries_report.push(TexturePoolEntryReport {
                    id: id.clone(),
                    mipmap_kind: format!("{:?}", color_info.mipmap_kind),
                    srgb_to_linear: color_info.srgb_to_linear,
                    premultiplied_alpha: color_info.premultiplied_alpha,
                });
            }

            arrays_report.push(TexturePoolArrayReport {
                format: format!("{:?}", array.format),
                width: array_key.width,
                height: array_key.height,
                mip_levels: calculate_mipmap_levels(array_key.width, array_key.height),
                width_remaining: limits.max_texture_dimension_2d() - array_key.width,
                height_remaining: limits.max_texture_dimension_2d() - array_key.height,
                mipmap: array.mipmap,
                layers_free: limits.max_texture_array_layers() as usize - array.images.len(),
                entries: entries_report,
            });
        }

        let arrays_free =
            limits.max_sampled_textures_per_shader_stage() as usize - self.arrays.len();

        TexturePoolReport {
            arrays: arrays_report,
            arrays_free,
        }
    }
}
