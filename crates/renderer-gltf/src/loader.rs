//! Loads glTF assets independently of the renderer.
//!
//! This is a web-specific adaptation of <https://github.com/gltf-rs/gltf/blob/master/src/import.rs>.
//! Main differences:
//! 1. Everything is async.
//! 2. Uses web APIs (via the internal `ImageData` helper).
//! 3. No image_data_reference feature (avoids base64/image crate dependencies).
//! 4. Some error checks are omitted because the web APIs enforce them (for example, mime type).

use awsm_renderer_core::image::{
    ColorSpaceConversion, ImageBitmapOptions, ImageData, PremultiplyAlpha,
};
use futures::future::try_join_all;
use gltf::{buffer, image, Document, Error as GltfError, Gltf};
use std::future::Future;
use std::sync::{Arc, Mutex};

use crate::error::AwsmGltfError;

/// Loaded glTF document plus buffer and image data.
pub struct GltfLoader {
    pub doc: Document,
    pub buffers: Vec<Vec<u8>>,
    pub images: Vec<ImageData>,
}

/// Supported glTF file types.
pub enum GltfFileType {
    Json,
    Glb,
    Draco, //TODO
}

/// Determines the glTF file type based on a filename extension.
pub fn get_type_from_filename(url: &str) -> Option<GltfFileType> {
    match url.rsplit('.').next() {
        Some("gltf") => Some(GltfFileType::Json),
        Some("glb") => Some(GltfFileType::Glb),
        Some("drc") => Some(GltfFileType::Draco),
        _ => None,
    }
}

impl GltfLoader {
    /// Loads a glTF asset from a URL.
    pub async fn load(url: &str, file_type: Option<GltfFileType>) -> anyhow::Result<Self> {
        let url = url.to_owned();
        let file_type = match file_type {
            Some(file_type) => file_type,
            None => get_type_from_filename(&url).unwrap_or(GltfFileType::Json),
        };

        let Gltf {
            document: doc,
            blob,
        } = match file_type {
            GltfFileType::Json => {
                let text = gloo_net::http::Request::get(&url)
                    .send()
                    .await?
                    .text()
                    .await?;

                let bytes: &[u8] = text.as_bytes();
                Gltf::from_slice(bytes)
            }
            GltfFileType::Glb => {
                let bytes = gloo_net::http::Request::get(&url)
                    .send()
                    .await?
                    .binary()
                    .await?;
                Gltf::from_slice(&bytes)
            }
            _ => return Err(AwsmGltfError::Load.into()),
        }?;

        let base_path = get_base_path(&url);
        let buffers = import_buffer_data(&doc, base_path, blob).await?;

        //info!("loaded {} buffers", buffer_data.len());

        let images = import_image_data(&doc, base_path, &buffers).await?;

        //info!("loaded {} images", image_data.len());

        Ok(Self {
            doc,
            buffers,
            images,
        })
    }

    /// Clones the loaded glTF data and buffers.
    pub fn heavy_clone(&self) -> Self {
        Self {
            doc: self.doc.clone(),
            buffers: self.buffers.clone(),
            images: self.images.clone(),
        }
    }
}

fn get_base_path(url: &str) -> &str {
    let idx1: i32 = url.rfind('/').map(|n| n as i32).unwrap_or(-1) + 1;
    let idx2: i32 = url.rfind('\\').map(|n| n as i32).unwrap_or(-1) + 1;

    if idx1 == 0 && idx2 == 0 {
        url
    } else {
        &url[0..(std::cmp::max(idx1, idx2) as usize)]
    }
}

async fn import_buffer_data<'a>(
    document: &'a Document,
    base: &'a str,
    blob: Option<Vec<u8>>,
) -> anyhow::Result<Vec<Vec<u8>>> {
    let futures = get_buffer_futures(document, base, blob);

    let datas: Vec<Vec<u8>> = try_join_all(futures).await?;

    let mut buffers = Vec::new();
    for (mut data, buffer) in datas.into_iter().zip(document.buffers()) {
        if data.len() < buffer.length() {
            return Err(GltfError::BufferLength {
                buffer: buffer.index(),
                expected: buffer.length(),
                actual: data.len(),
            }
            .into());
        }
        while data.len() % 4 != 0 {
            data.push(0);
        }
        buffers.push(data);
    }
    Ok(buffers)
}

fn get_buffer_futures<'a>(
    document: &'a Document,
    base: &str,
    blob: Option<Vec<u8>>,
) -> Vec<impl Future<Output = anyhow::Result<Vec<u8>>> + 'a> {
    //these need to be owned by each future simultaneously
    let blob = Arc::new(Mutex::new(blob));
    let base = Arc::new(base.to_owned());

    document
        .buffers()
        .map(|buffer| {
            let blob = blob.clone();
            let base = base.clone();

            async move {
                match buffer.source() {
                    buffer::Source::Uri(uri) => {
                        let url = get_url(base.as_ref(), uri)?;
                        let bytes = gloo_net::http::Request::get(&url)
                            .send()
                            .await?
                            .binary()
                            .await?;
                        Ok(bytes)
                    }
                    buffer::Source::Bin => {
                        // should this be cloned?
                        blob.lock()
                            .unwrap()
                            .take()
                            .ok_or(GltfError::MissingBlob.into())
                    }
                }
            }
        })
        .collect()
}

async fn import_image_data<'a>(
    document: &'a Document,
    base: &'a str,
    buffer_data: &'a [Vec<u8>],
) -> anyhow::Result<Vec<ImageData>> {
    let futures = get_image_futures(document, base, buffer_data);

    try_join_all(futures).await
}

fn get_image_futures<'a>(
    document: &'a Document,
    base: &str,
    buffer_data: &'a [Vec<u8>],
) -> Vec<impl Future<Output = anyhow::Result<ImageData>> + 'a> {
    //these need to be owned by each future simultaneously
    let base = Arc::new(base.to_owned());

    document
        .images()
        .map(|image| {
            let base = Arc::clone(&base);
            // We very intentionally set these. See notes on `ImageData::load_url` for why.
            let options = Some(
                ImageBitmapOptions::new()
                    .with_premultiply_alpha(PremultiplyAlpha::None)
                    .with_color_space_conversion(ColorSpaceConversion::Default),
            );
            async move {
                match image.source() {
                    image::Source::Uri { uri, mime_type: _ } => {
                        let url = get_url(base.as_ref(), uri)?;
                        Ok(ImageData::load_url(&url, options).await?)
                    }
                    image::Source::View { view, mime_type } => {
                        let parent_buffer_data = &buffer_data[view.buffer().index()];
                        let begin = view.offset();
                        let end = begin + view.length();
                        let encoded_image = &parent_buffer_data[begin..end];
                        let image = awsm_renderer_core::image::bitmap::load_u8(
                            &encoded_image,
                            mime_type,
                            options.clone(),
                        )
                        .await?;
                        Ok(ImageData::Bitmap { image, options })
                    }
                }
            }
        })
        .collect()
}

fn get_url(base: &str, uri: &str) -> anyhow::Result<String> {
    if uri.contains(":") {
        //absolute
        if uri.starts_with("data:") || uri.starts_with("http:") || uri.starts_with("https://") {
            Ok(uri.to_owned())
        } else {
            Err(GltfError::UnsupportedScheme.into())
        }
    } else {
        //relative
        Ok(format!("{base}{uri}"))
    }
}
