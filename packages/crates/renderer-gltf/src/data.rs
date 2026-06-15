//! glTF loader data containers.

use awsm_renderer_core::image::ImageData;

use crate::{buffers::GltfBuffers, error::Result, loader::GltfLoader};

/// Loaded glTF document data with buffers and images.
pub struct GltfData {
    pub doc: gltf::Document,
    pub buffers: GltfBuffers,
    pub images: Vec<ImageData>,
    pub hints: GltfDataHints,
}

impl GltfData {
    /// Clones the document and backing buffers for independent use.
    pub fn heavy_clone(&self) -> Self {
        Self {
            doc: self.doc.clone(),
            buffers: self.buffers.heavy_clone(),
            images: self.images.clone(),
            hints: self.hints.clone(),
        }
    }
}

/// Optional hints used during glTF population.
/// Override which draw-geometry `populate_gltf` builds per primitive, for a
/// caller that applies its OWN material so the glb's own material alpha is not
/// authoritative (e.g. the bundle loader's `GltfMaterialSource::Single` over a
/// geometry-only glb whose materials were stripped).
///
/// Transparency is a per-**instance** material property — one glb mesh asset can
/// be shared by several nodes with different materials (opaque vs transparent) —
/// so this is chosen per *load*, NOT baked into the glb.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum GltfGeometryOverride {
    /// Decide per primitive from the glb's own material alpha (the default).
    #[default]
    FromMaterial,
    /// Visibility (opaque-pass) geometry only.
    Opaque,
    /// Transparency-pass geometry only.
    Transparent,
    /// Both visibility + transparency geometry.
    Both,
}

#[derive(Default, Clone)]
pub struct GltfDataHints {
    pub hud: bool,
    pub hidden: bool,
    pub render_timings: bool,
    /// Per-load override of the draw-geometry built for every primitive — used
    /// when the caller applies its own material (the glb's material alpha can't be
    /// trusted, e.g. a stripped geometry-only bundle glb). [`FromMaterial`] keeps
    /// the per-primitive decision from the glb material. Without this, a geometry-
    /// only glb reads Opaque → no transparency geometry → the transparency pass
    /// errors `TransparencyGeometryBufferNotFound` once a transparent material is
    /// applied. [`FromMaterial`]: [`GltfGeometryOverride::FromMaterial`]
    pub geometry_override: GltfGeometryOverride,
}

impl GltfDataHints {
    /// Sets whether this data is for a HUD overlay.
    pub fn with_hud(mut self, hud: bool) -> Self {
        self.hud = hud;
        self
    }

    /// Override the per-primitive draw-geometry kind (see
    /// [`GltfGeometryOverride`]).
    pub fn with_geometry_override(mut self, geometry_override: GltfGeometryOverride) -> Self {
        self.geometry_override = geometry_override;
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
            buffers,
            hints,
        })
    }
}
