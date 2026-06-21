//! glTF loader data containers.

use awsm_renderer_core::image::ImageData;

use crate::{
    buffers::GltfBuffers,
    error::Result,
    loader::{EncodedImage, GltfLoader},
};

/// Loaded glTF document data with buffers and images.
pub struct GltfData {
    pub doc: gltf::Document,
    pub buffers: GltfBuffers,
    pub images: Vec<ImageData>,
    /// Encoded image bytes (PNG/JPEG) by glTF image index — retained so an importer
    /// (`reexport_clean`) can re-embed them into our-format. See [`EncodedImage`].
    pub encoded_images: Vec<EncodedImage>,
    pub hints: GltfDataHints,
}

impl GltfData {
    /// Clones the document and backing buffers for independent use.
    pub fn heavy_clone(&self) -> Self {
        Self {
            doc: self.doc.clone(),
            buffers: self.buffers.heavy_clone(),
            images: self.images.clone(),
            encoded_images: self.encoded_images.clone(),
            hints: self.hints.clone(),
        }
    }
}

/// Optional hints used during glTF population.
///
/// (Historically this also carried a per-load `geometry_override` to force the
/// draw-geometry KIND when the caller applied its own material. That's gone: the
/// renderer now derives the kind at commit from the union of materials bound to
/// each geometry. The bound material is authoritative,
/// so the bundle loader's `GltfMaterialSource::Single` case just works.)
#[derive(Default, Clone)]
pub struct GltfDataHints {
    pub hud: bool,
    pub hidden: bool,
    pub render_timings: bool,
}

impl GltfDataHints {
    /// Sets whether this data is for a HUD overlay.
    pub fn with_hud(mut self, hud: bool) -> Self {
        self.hud = hud;
        self
    }

    /// Sets whether this data is initially hidden.
    pub fn with_hidden(mut self, hidden: bool) -> Self {
        self.hidden = hidden;
        self
    }

    /// Sets whether glTF loading/population timing spans are emitted.
    pub fn with_render_timings(mut self, render_timings: bool) -> Self {
        self.render_timings = render_timings;
        self
    }
}

impl GltfLoader {
    /// Consumes the loader and returns a `GltfData` bundle.
    pub fn into_data(self, hints: Option<GltfDataHints>) -> Result<GltfData> {
        let hints = hints.unwrap_or_default();
        let buffers = GltfBuffers::new(&self.doc, self.buffers, hints.clone())?;

        Ok(GltfData {
            doc: self.doc,
            images: self.images,
            encoded_images: self.encoded_images,
            buffers,
            hints,
        })
    }
}
