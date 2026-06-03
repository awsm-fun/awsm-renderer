//! Reporting types for mega texture usage.

use serde::{Deserialize, Serialize};

use crate::texture::mega_texture::MegaTextureInfo;

/// Summary report for a mega texture.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MegaTextureReport<ID> {
    pub entries: Vec<Vec<Vec<MegaTextureReportEntry<ID>>>>,
    pub count: MegaTextureReportCount,
    pub size: MegaTextureReportSizes,
    pub mip_levels: Option<u32>,
}

/// Count statistics for a mega texture report.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MegaTextureReportCount {
    pub entries: u32,
    pub layers: u32,
    pub atlases: u32,
    pub max_atlases: u32,
    pub max_layers_per_atlas: u32,
    pub max_layers_total: u32,
}

/// Size statistics for a mega texture report.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MegaTextureReportSizes {
    pub total: MegaTextureReportArea,
    pub atlases: Vec<MegaTextureReportArea>,
    pub layers: Vec<Vec<MegaTextureReportArea>>,
    pub texture_size: MegaTextureReportSize,
}

/// Single entry record in a mega texture report.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MegaTextureReportEntry<ID> {
    pub pixel_offset: MegaTextureReportCoords,
    pub size: MegaTextureReportSize,
    pub id: ID,
    pub uv_scale: MegaTextureReportCoordsF32,
    pub uv_offset: MegaTextureReportCoordsF32,
    pub grad_scale: MegaTextureReportCoordsF32,
}

/// Integer coordinate pair for report output.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct MegaTextureReportCoords {
    pub x: u32,
    pub y: u32,
}
impl From<(u32, u32)> for MegaTextureReportCoords {
    fn from(coords: (u32, u32)) -> Self {
        let (x, y) = coords;
        Self { x, y }
    }
}

/// Float coordinate pair for report output.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct MegaTextureReportCoordsF32 {
    pub x: f32,
    pub y: f32,
}
impl From<(f32, f32)> for MegaTextureReportCoordsF32 {
    fn from(coords: (f32, f32)) -> Self {
        let (x, y) = coords;
        Self { x, y }
    }
}
/// Size and area record used in reports.
#[derive(Clone, Debug, Copy, Serialize, Deserialize)]
pub struct MegaTextureReportSize {
    pub width: u32,
    pub height: u32,
    pub area: f64,
}

impl MegaTextureReportSize {
    /// Empty size with zero area.
    pub const ZERO: Self = Self {
        width: 0,
        height: 0,
        area: 0.0,
    };

    /// Adds dimensions and updates area.
    pub fn add(&mut self, width: u32, height: u32) {
        self.width += width;
        self.height += height;
        self.area = (self.width as f64) * (self.height as f64);
    }
}
impl From<(u32, u32)> for MegaTextureReportSize {
    fn from(width_height: (u32, u32)) -> Self {
        let (width, height) = width_height;
        let area = width as f64 * height as f64;
        Self {
            width,
            height,
            area,
        }
    }
}

/// Area usage statistics for reports.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MegaTextureReportArea {
    pub perc_free: f64,
    pub perc_used: f64,
    pub max_size: MegaTextureReportSize,
    pub used_size: MegaTextureReportSize,
}

impl MegaTextureReportArea {
    /// Creates a report area from max and used sizes.
    pub fn new(max_size: MegaTextureReportSize, used_size: MegaTextureReportSize) -> Self {
        let perc_used = (used_size.area / max_size.area) * 100.0;
        let perc_free = (1.0 - perc_used / 100.0) * 100.0;

        Self {
            perc_free,
            perc_used,
            max_size,
            used_size,
        }
    }
}

impl<ID> MegaTextureInfo<ID>
where
    ID: Clone,
{
    /// Converts info into a report structure.
    pub fn into_report(self) -> MegaTextureReport<ID> {
        let Self {
            entries,
            texture_size,
            max_depth,
            max_bindings_per_group,
            max_bind_groups,
            mip_levels,
        } = self;

        let entries: Vec<Vec<Vec<MegaTextureReportEntry<ID>>>> = entries
            .into_iter()
            .map(|atlas| {
                atlas
                    .into_iter()
                    .map(|layer| {
                        layer
                            .into_iter()
                            .map(|entry| MegaTextureReportEntry {
                                pixel_offset: MegaTextureReportCoords::from((
                                    entry.pixel_offset[0],
                                    entry.pixel_offset[1],
                                )),
                                size: MegaTextureReportSize::from((entry.size[0], entry.size[1])),
                                id: entry.id.clone(),
                                uv_scale: MegaTextureReportCoordsF32::from((
                                    entry.uv_scale[0],
                                    entry.uv_scale[1],
                                )),
                                uv_offset: MegaTextureReportCoordsF32::from((
                                    entry.uv_offset[0],
                                    entry.uv_offset[1],
                                )),
                                grad_scale: MegaTextureReportCoordsF32::from((
                                    entry.grad_scale[0],
                                    entry.grad_scale[1],
                                )),
                            })
                            .collect()
                    })
                    .collect()
            })
            .collect();

        let mut total_entries_len: u32 = 0;
        let mut total_layers_len: u32 = 0;
        let mut total_size = MegaTextureReportSize::ZERO;
        let mut atlas_sizes: Vec<MegaTextureReportSize> = Vec::new();
        let mut atlas_layer_sizes: Vec<Vec<MegaTextureReportSize>> = Vec::new();

        for atlas in &entries {
            let mut atlas_size = MegaTextureReportSize::ZERO;
            let mut layer_sizes: Vec<MegaTextureReportSize> = Vec::new();
            for layer in atlas {
                total_layers_len += 1;
                let mut layer_size = MegaTextureReportSize::ZERO;
                for entry in layer {
                    total_entries_len += 1;
                    total_size.add(entry.size.width, entry.size.height);
                    layer_size.add(entry.size.width, entry.size.height);
                    atlas_size.add(entry.size.width, entry.size.height);
                }
                layer_sizes.push(layer_size);
            }
            atlas_layer_sizes.push(layer_sizes);
            atlas_sizes.push(atlas_size);
        }

        let count = MegaTextureReportCount {
            entries: total_entries_len,
            layers: total_layers_len,
            atlases: atlas_sizes.len() as u32,
            max_atlases: max_bind_groups * max_bindings_per_group,
            max_layers_per_atlas: max_depth,
            max_layers_total: max_bindings_per_group * max_bind_groups * max_depth,
        };

        let max_total_size = MegaTextureReportSize::from((
            texture_size * count.max_layers_total,
            texture_size * count.max_layers_total,
        ));
        let max_size_per_atlas = MegaTextureReportSize::from((
            texture_size * count.max_layers_per_atlas,
            texture_size * count.max_layers_per_atlas,
        ));
        let max_size_per_layer = MegaTextureReportSize::from((texture_size, texture_size));

        let size = MegaTextureReportSizes {
            total: MegaTextureReportArea::new(max_total_size, total_size),
            atlases: atlas_sizes
                .into_iter()
                .map(|size| MegaTextureReportArea::new(max_size_per_atlas, size))
                .collect(),
            layers: atlas_layer_sizes
                .into_iter()
                .map(|layer_sizes| {
                    layer_sizes
                        .into_iter()
                        .map(|size| MegaTextureReportArea::new(max_size_per_layer, size))
                        .collect()
                })
                .collect(),
            texture_size: (texture_size, texture_size).into(),
        };

        MegaTextureReport {
            entries,
            count,
            size,
            mip_levels,
        }
    }
}

impl<ID> MegaTextureReport<ID>
where
    ID: Serialize,
{
    /// Logs the report to the browser console.
    pub fn console_log(&self) {
        let js_value = serde_wasm_bindgen::to_value(self).unwrap();
        web_sys::console::log_1(&js_value);
    }
}
