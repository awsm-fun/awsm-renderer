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

/// glTF extensions this renderer handles — most via raw-JSON parsing in
/// `populate::material` / `populate::extensions`. The upstream `gltf` crate only
/// feature-gates a subset, so its `from_slice` validation rejects a model that
/// lists any of the others in `extensionsRequired` (even though we render them
/// fine). [`parse_gltf_lenient`] drops these from `extensionsRequired` before
/// re-validating, so they load; genuinely unsupported extensions (e.g.
/// `KHR_materials_variants`, `EXT_texture_webp`,
/// `KHR_draco_mesh_compression`) stay in the list and still fail validation
/// cleanly.
///
/// Keep this in sync with what `populate` actually consumes.
pub(crate) const RENDERER_SUPPORTED_EXTENSIONS: &[&str] = &[
    "KHR_lights_punctual",
    "KHR_texture_transform",
    "KHR_materials_emissive_strength",
    "KHR_materials_unlit",
    "KHR_materials_specular",
    "KHR_materials_ior",
    "KHR_materials_transmission",
    "KHR_materials_volume",
    "KHR_materials_clearcoat",
    "KHR_materials_sheen",
    "KHR_materials_anisotropy",
    "KHR_materials_iridescence",
    "KHR_materials_dispersion",
    "KHR_materials_diffuse_transmission",
    "EXT_mesh_gpu_instancing",
    // Compression trio (docs/plans/compression.md): meshopt-compressed
    // bufferViews are decoded by a pre-pass before accessors read them;
    // quantized accessor component types are dequantized in
    // `buffers/attributes.rs`; Basis KTX2 textures transcode via the Basis
    // worker.
    "EXT_meshopt_compression",
    "KHR_mesh_quantization",
    "KHR_texture_basisu",
];

/// Parse glTF/GLB bytes, tolerating `extensionsRequired` entries this renderer
/// supports but the `gltf` crate doesn't feature-gate (see
/// [`RENDERER_SUPPORTED_EXTENSIONS`]). Identical to `Gltf::from_slice` for
/// everything else — full structural validation is retained (we parse without
/// validation only to edit `extensionsRequired`, then re-validate via
/// `Document::from_json`).
pub(crate) fn parse_gltf_lenient(bytes: &[u8]) -> std::result::Result<Gltf, GltfError> {
    let gltf = Gltf::from_slice_without_validation(bytes)?;
    let blob = gltf.blob;
    let mut json = gltf.document.into_json();
    json.extensions_required
        .retain(|e| !RENDERER_SUPPORTED_EXTENSIONS.contains(&e.as_str()));

    // `KHR_texture_basisu` puts the image index in the extension and omits
    // the core `texture.source` (gltf-json's sentinel Index(u32::MAX)), which
    // fails `Document::from_json` validation as Missing. Lift the extension's
    // source into the core field: downstream, the texture then points at the
    // KTX2 image entry exactly like a PNG texture points at its PNG — the
    // image DECODE path branches on the KTX2 payload, not the texture.
    for texture in &mut json.textures {
        if texture.source.value() == u32::MAX as usize {
            let basisu_source = texture
                .extensions
                .as_ref()
                .and_then(|ext| ext.others.get("KHR_texture_basisu"))
                .and_then(|basisu| basisu.get("source"))
                .and_then(|source| source.as_u64());
            if let Some(source) = basisu_source {
                texture.source = gltf::json::Index::new(source as u32);
            }
        }
    }

    let document = Document::from_json(json)?;
    Ok(Gltf { document, blob })
}

/// Fixture-gated proof that the `gltf` crate accepts real gltfpack output
/// (meshopt + KHR_mesh_quantization) once [`parse_gltf_lenient`] strips the
/// supported extensions from `extensionsRequired` — i.e. no extra leniency is
/// needed for quantized accessor component types.
#[cfg(all(test, has_local_fixtures))]
mod fixture_tests {
    use super::*;

    const POLICE_GLB: &[u8] = include_bytes!("../../../../fixtures/local/police-meshopt.glb");

    #[test]
    fn gltf_crate_accepts_the_meshopt_quantized_robot() {
        let gltf = parse_gltf_lenient(POLICE_GLB)
            .expect("quantized+meshopt robot must parse after extension leniency");

        // gltfpack output for the robots: POSITION = VEC3 i16 NORMALIZED
        // (both the /32767 normalize AND the node/IBM transform apply).
        let mut quantized_positions = 0usize;
        let mut meshopt_views = 0usize;
        for mesh in gltf.document.meshes() {
            for prim in mesh.primitives() {
                let acc = prim.get(&gltf::Semantic::Positions).expect("POSITION");
                if acc.data_type() == gltf::accessor::DataType::I16 && acc.normalized() {
                    quantized_positions += 1;
                }
            }
        }
        // The ext lives in raw JSON (the gltf crate has no meshopt support);
        // count via the document's raw extension values.
        for view in gltf.document.views() {
            if view.extension_value("EXT_meshopt_compression").is_some() {
                meshopt_views += 1;
            }
        }
        assert!(
            quantized_positions > 0,
            "expected quantized POSITION accessors"
        );
        assert_eq!(
            meshopt_views, 82,
            "expected the robot's 82 meshopt bufferViews"
        );
    }
}

/// Loaded glTF document plus buffer and image data.
pub struct GltfLoader {
    pub doc: Document,
    pub buffers: Vec<Vec<u8>>,
    pub images: Vec<ImageData>,
    /// Encoded image bytes (PNG/JPEG) by glTF image index — retained so an importer
    /// can re-embed them into our-format. See [`EncodedImage`].
    pub encoded_images: Vec<EncodedImage>,
}

/// Supported glTF file types.
pub enum GltfFileType {
    Json,
    Glb,
}

/// Determines the glTF file type based on a filename extension.
pub fn get_type_from_filename(url: &str) -> Option<GltfFileType> {
    match url.rsplit('.').next() {
        Some("gltf") => Some(GltfFileType::Json),
        Some("glb") => Some(GltfFileType::Glb),
        _ => None,
    }
}

impl GltfLoader {
    /// Loads a glTF asset from a URL.
    /// `bypass_http_cache`: when true, the top-level model fetch is issued with
    /// the `no-store` cache mode so the browser HTTP cache is skipped. This crate
    /// sets NO policy — it only exposes the mechanism; the CALLER decides. The
    /// editor passes `true` for URL imports (a local-MCP DEV workflow: you re-bake
    /// to the SAME filename and re-import to iterate, where a cache hit would
    /// silently return the STALE prior bake). Runtime/player callers pass `false`
    /// to keep normal HTTP caching for load performance.
    pub async fn load(
        url: &str,
        // Kept for caller API stability; the container form is sniffed from
        // CONTENT below, so the hint is no longer consulted (Draco, the one
        // hint that mattered, is dropped entirely — see the compression plan).
        _file_type: Option<GltfFileType>,
        bypass_http_cache: bool,
    ) -> anyhow::Result<Self> {
        let url = url.to_owned();

        // Auto-detect GLB (binary, "glTF" magic) vs glTF (JSON) from the fetched
        // CONTENT, not the URL extension. The old code chose the fetch mode purely
        // from the filename and defaulted to JSON when there was no `.glb`/`.gltf`
        // extension — so a GLB served at an extensionless or query-suffixed URL
        // (`/glb/arena88`, `model.glb?v=1` — `rsplit('.')` yields `glb?v=1`) was
        // fetched as text and mis-parsed as JSON, failing with a cryptic
        // "could not completely read the object". `Gltf::from_slice` (via
        // `parse_gltf_lenient`) sniffs the container form itself, so one binary
        // fetch is correct for both — a `.gltf` JSON body is just its UTF-8 bytes.

        // Bypass the browser HTTP cache via the fetch `no-store` mode when the
        // caller asked for it (see `bypass_http_cache` above) — done through the
        // cache mode rather than a `?cb=<ts>` URL cachebuster so the URL / cache
        // key stays clean.
        let mut req = gloo_net::http::Request::get(&url);
        if bypass_http_cache {
            req = req.cache(web_sys::RequestCache::NoStore);
        }
        let bytes = req.send().await?.binary().await?;

        let Gltf {
            document: doc,
            blob,
        } = parse_gltf_lenient(&bytes)?;

        let base_path = get_base_path(&url);
        let buffers = import_buffer_data(&doc, base_path, blob).await?;

        //info!("loaded {} buffers", buffer_data.len());

        let (images, encoded_images) = import_image_data(&doc, base_path, &buffers).await?;

        //info!("loaded {} images", image_data.len());

        Ok(Self {
            doc,
            buffers,
            images,
            encoded_images,
        })
    }

    /// Loads a glTF asset from in-memory GLB bytes (no network fetch).
    ///
    /// For self-contained GLB only: external-URI buffers or images would need a
    /// base path to resolve against, and none is available here. The runtime
    /// bundle's per-mesh glbs are geometry-only single-BIN files, so this is the
    /// path `awsm-renderer-scene-loader` uses to feed them through `populate_gltf`.
    pub async fn from_glb_bytes(bytes: &[u8]) -> anyhow::Result<Self> {
        let Gltf {
            document: doc,
            blob,
        } = parse_gltf_lenient(bytes)?;

        let buffers = import_buffer_data(&doc, "", blob).await?;
        let (images, encoded_images) = import_image_data(&doc, "", &buffers).await?;

        Ok(Self {
            doc,
            buffers,
            images,
            encoded_images,
        })
    }

    /// Clones the loaded glTF data and buffers.
    pub fn heavy_clone(&self) -> Self {
        Self {
            doc: self.doc.clone(),
            buffers: self.buffers.clone(),
            images: self.images.clone(),
            encoded_images: self.encoded_images.clone(),
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

/// The ENCODED bytes (original PNG/JPEG) of a glTF image + its mime, retained
/// alongside the DECODED [`ImageData`] so an importer can re-embed the image into
/// our-format (`reexport_clean`) — the renderer only keeps decoded pixels. `None`
/// for an image whose encoded bytes couldn't be obtained (e.g. an external URI
/// fetch failed). Indexed by glTF image index.
pub type EncodedImage = Option<(Vec<u8>, String)>;

#[allow(clippy::type_complexity)]
async fn import_image_data<'a>(
    document: &'a Document,
    base: &'a str,
    buffer_data: &'a [Vec<u8>],
) -> anyhow::Result<(Vec<ImageData>, Vec<EncodedImage>)> {
    let futures = get_image_futures(document, base, buffer_data);
    let results = try_join_all(futures).await?;
    Ok(results.into_iter().unzip())
}

/// Infer an image mime from a URI extension (glTF `Source::Uri` may omit it).
/// `application/octet-stream` for unknown so the re-embed step (PNG/JPEG only) skips it.
fn guess_image_mime(uri: &str) -> String {
    let path = uri.split('?').next().unwrap_or(uri).to_ascii_lowercase();
    if path.ends_with(".png") {
        "image/png".to_string()
    } else if path.ends_with(".jpg") || path.ends_with(".jpeg") {
        "image/jpeg".to_string()
    } else {
        "application/octet-stream".to_string()
    }
}

#[allow(clippy::type_complexity)]
fn get_image_futures<'a>(
    document: &'a Document,
    base: &str,
    buffer_data: &'a [Vec<u8>],
) -> Vec<impl Future<Output = anyhow::Result<(ImageData, EncodedImage)>> + 'a> {
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
                    image::Source::Uri { uri, mime_type } => {
                        let url = get_url(base.as_ref(), uri)?;
                        // DECODE via the unchanged URL path (format handling intact).
                        let data = ImageData::load_url(&url, options).await?;
                        // Best-effort: retain the ENCODED bytes for our-format re-embed.
                        // A failed fetch just means this image can't round-trip through
                        // reexport_clean — the direct render is unaffected. The browser
                        // already fetched the URL for decode, so this hits its cache.
                        let encoded = match gloo_net::http::Request::get(&url).send().await {
                            Ok(resp) => resp.binary().await.ok().map(|bytes| {
                                let mime = mime_type
                                    .map(str::to_string)
                                    .unwrap_or_else(|| guess_image_mime(uri));
                                (bytes, mime)
                            }),
                            Err(_) => None,
                        };
                        Ok((data, encoded))
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
                        // Embedded bytes are already in hand — retain them (free).
                        let encoded = Some((encoded_image.to_vec(), mime_type.to_string()));
                        Ok((ImageData::Bitmap { image, options }, encoded))
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

#[cfg(test)]
mod tests {
    use super::parse_gltf_lenient;

    const MIN_JSON: &str = r#"{"asset":{"version":"2.0"}}"#;

    /// Build a minimal valid GLB container around `MIN_JSON` (header + one
    /// 4-byte-aligned JSON chunk), per the glTF 2.0 binary container spec.
    fn min_glb() -> Vec<u8> {
        let mut json = MIN_JSON.as_bytes().to_vec();
        while json.len() % 4 != 0 {
            json.push(b' ');
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"glTF"); // magic
        out.extend_from_slice(&2u32.to_le_bytes()); // version
        out.extend_from_slice(&((12 + 8 + json.len()) as u32).to_le_bytes()); // total length
        out.extend_from_slice(&(json.len() as u32).to_le_bytes()); // chunk length
        out.extend_from_slice(b"JSON"); // chunk type
        out.extend_from_slice(&json);
        out
    }

    /// The loader decides GLB-vs-JSON from the CONTENT (the "glTF" magic), not
    /// the URL extension — the regression that made extensionless or
    /// query-suffixed URLs (`…/glb/arena88`, `model.glb?v=1`) fetch as text and
    /// fail with "could not completely read the object" (MCP-BUGS 2026-07-08).
    #[test]
    fn content_sniff_parses_glb_and_json_bytes() {
        // Binary GLB bytes → sniffed as GLB.
        let glb = min_glb();
        let parsed = parse_gltf_lenient(&glb).expect("glb bytes parse");
        assert_eq!(parsed.document.nodes().len(), 0);

        // Raw JSON bytes (what a .gltf fetch yields) → sniffed as JSON.
        let parsed = parse_gltf_lenient(MIN_JSON.as_bytes()).expect("json bytes parse");
        assert_eq!(parsed.document.nodes().len(), 0);
    }
}
