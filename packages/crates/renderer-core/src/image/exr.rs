//! EXR image loading utilities.

use std::{borrow::Cow, io::Cursor};

use crate::error::{AwsmCoreError, Result};
use exr::prelude::{ChannelDescription, ReadChannels, ReadLayers};

/// Loaded EXR image data.
#[derive(Clone, Debug)]
pub struct ExrImage {
    pub data: Vec<f32>,
    pub width: usize,
    pub height: usize,
    pub channel_info: (
        ChannelDescription,
        ChannelDescription,
        ChannelDescription,
        Option<ChannelDescription>,
    ),
}

impl ExrImage {
    /// Loads an EXR image from a URL.
    pub async fn load_url(url: &str) -> anyhow::Result<Self> {
        let bytes = gloo_net::http::Request::get(url)
            .send()
            .await?
            .binary()
            .await?;

        let cursor = Cursor::new(bytes);

        // https://github.com/johannesvollmer/exrs/blob/master/GUIDE.md
        let result = exr::image::read::read()
            .no_deep_data()
            .largest_resolution_level()
            .rgba_channels(
                |resolution, channel_info| Self {
                    data: vec![0.0; resolution.0 * resolution.1 * 4],
                    width: resolution.0,
                    height: resolution.1,
                    channel_info: channel_info.clone(),
                },
                |img, pos, (r, g, b, a): (f32, f32, f32, exr::prelude::f16)| {
                    //data: ImageData::new_with_sw(resolution.0 as u32, resolution.1 as u32).unwrap(),
                    // let width = img.data.width() as usize;
                    // let data = &mut img.data.data();

                    let x = pos.0;
                    let y = pos.1;
                    let offset = (y * img.width + x) * 4;

                    img.data[offset] = r;
                    img.data[offset + 1] = g;
                    img.data[offset + 2] = b;
                    img.data[offset + 3] = a.to_f32();
                },
            )
            .first_valid_layer()
            .all_attributes()
            .on_progress(|progress| {
                tracing::info!("progress: {:?}", progress);
            })
            .non_parallel()
            .from_buffered(cursor)?;

        Ok(result.layer_data.channel_data.pixels)
    }

    /// Returns a JS object for external image copies if supported.
    pub fn js_obj(&self) -> Result<Cow<'_, js_sys::Object>> {
        Err(AwsmCoreError::ExrImageToJsValue(
            "EXR to js value not implemented yet".to_string(),
        ))
    }
}
