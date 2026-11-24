//! TAA textures. All of them exist only while TAA is on (the pass is dropped,
//! and these destroyed, when it turns off).
//!
//! - **history** pair: `rgba16float` at POST size, ping-ponged by the pass's
//!   own history index (flipped only after a submitted resolve). `.rgb` =
//!   tonemapped colour, `.a` = history length `n`. Filterable: the resolve
//!   fetches it with a 5-tap Catmull-Rom.
//! - **out**: `rgba16float` at post size, linear HDR. What bloom, effects and
//!   display read instead of `composite` in TAA mode. Also a render
//!   attachment: plan phase 3 draws the HUD into it after the resolve.
//! - **prev_depth**: last frame's scene depth at LAST frame's render size, in
//!   the depth format (a depth-to-depth copy; WebGPU cannot copy
//!   `depth32float` into `r32float`), bound as `texture_depth_2d`.
//! - **prev_velocity**: last frame's motion vectors, same size, a copy of the
//!   velocity target in its own format (`rg16float` from the geometry pass,
//!   `rg32float` from the prep fallback). The own-surface disocclusion test
//!   reads it where depth cannot tell two surfaces apart: an object resting
//!   on the surface behind it (03 §4.7, the contact line).
//!
//! The history lives at post size, not render size, so a render-scale change
//! (the resolution governor, plan phase 5) neither recreates it nor resets
//! it (03 §8). That is why these textures belong to the pass and not to
//! `RenderTexturesInner`, which rebuilds everything on any of its triggers.
//!
//! The previous depth (and velocity) has to outlive a render-size change by one frame: the
//! frame after the change still reads last frame's depth, at the old size,
//! while the copy of this frame's depth needs the new size. So a size change
//! allocates `next_prev_depth`, a SUBMITTED copy fills it
//! ([`TaaTextures::mark_next_prev_depth_filled`]), and the next
//! [`TaaTextures::ensure_render_size`] swaps it in.
//!
//! Textures replaced by a resize are destroyed [`DESTROY_DELAY_FRAMES`]
//! frames later, not at once: the previous frame's submit may still be in
//! flight and reference them, and destroying a texture an in-flight submit
//! uses rejects that submit ("Destroyed texture used in a submit"; the same
//! reason `RenderTextures` defers its destroys).

use std::cell::Cell;

use awsm_renderer_core::{
    error::{AwsmCoreError, Result},
    renderer::AwsmRendererWebGpu,
    texture::{Extent3d, TextureDescriptor, TextureFormat, TextureUsage},
};

/// History and output format. Half floats hold the tonemapped history to
/// about 2.4e-4 below 1.0, which the resolve's clip-box floor is sized for.
pub const TAA_COLOR_FORMAT: TextureFormat = TextureFormat::Rgba16float;

/// Frames a replaced texture is kept before `destroy()`: past the 1-2 frames
/// a submit can stay in flight (same value as `RenderTextures`).
const DESTROY_DELAY_FRAMES: u32 = 3;

pub struct TaaTextures {
    history: [web_sys::GpuTexture; 2],
    pub history_views: [web_sys::GpuTextureView; 2],
    out: web_sys::GpuTexture,
    pub out_view: web_sys::GpuTextureView,
    /// Size of the history pair and `out`.
    pub post_size: (u32, u32),
    prev_depth: PrevTargets,
    /// The copy targets for this frame's depth and velocity when the render
    /// size (or the velocity format) changed since `prev_depth` was
    /// allocated. Swapped in once a submitted copy filled them.
    next_prev_depth: Option<PrevTargets>,
    /// A submitted frame copied its depth into `next_prev_depth`.
    next_prev_depth_filled: Cell<bool>,
    depth_format: TextureFormat,
    /// Replaced textures waiting out [`DESTROY_DELAY_FRAMES`].
    retired: Vec<(web_sys::GpuTexture, u32)>,
}

/// Last frame's depth and velocity, always the same size.
struct PrevTargets {
    texture: web_sys::GpuTexture,
    view: web_sys::GpuTextureView,
    velocity: web_sys::GpuTexture,
    velocity_view: web_sys::GpuTextureView,
    velocity_format: TextureFormat,
    size: (u32, u32),
}

impl TaaTextures {
    pub fn new(
        gpu: &AwsmRendererWebGpu,
        post_size: (u32, u32),
        render_size: (u32, u32),
        depth_format: TextureFormat,
        velocity_format: TextureFormat,
    ) -> Result<Self> {
        let post_size = clamp_size(post_size);
        let (history, history_views) = create_history(gpu, post_size)?;
        let (out, out_view) = create_out(gpu, post_size)?;
        let prev_depth =
            PrevTargets::new(gpu, depth_format, velocity_format, clamp_size(render_size))?;
        Ok(Self {
            history,
            history_views,
            out,
            out_view,
            post_size,
            prev_depth,
            next_prev_depth: None,
            next_prev_depth_filled: Cell::new(false),
            depth_format,
            retired: Vec::new(),
        })
    }

    /// Whether the history pair is allocated for `post_size`. When it is not,
    /// this frame recreates it and its resolve must reset; `render` asks
    /// before choosing the frame's jitter, so the reset restarts the cycle.
    pub fn history_matches(&self, post_size: (u32, u32)) -> bool {
        self.post_size == clamp_size(post_size)
    }

    /// Once per frame, before any resize: destroys retired textures whose
    /// delay is over.
    pub fn tick_retired(&mut self) {
        self.retired.retain_mut(|(texture, frames_left)| {
            if *frames_left == 0 {
                texture.destroy();
                false
            } else {
                *frames_left -= 1;
                true
            }
        });
    }

    /// Recreate the history pair and `out` if the post size changed. Returns
    /// `true` when they were rebuilt: `out` is a new view (bloom / effects
    /// rebind) and the history is empty (the resolve must reset).
    pub fn ensure_post_size(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        post_size: (u32, u32),
    ) -> Result<bool> {
        if self.history_matches(post_size) {
            return Ok(false);
        }
        let post_size = clamp_size(post_size);
        let (history, history_views) = create_history(gpu, post_size)?;
        let (out, out_view) = create_out(gpu, post_size)?;
        for old in std::mem::replace(&mut self.history, history) {
            self.retire(old);
        }
        let old_out = std::mem::replace(&mut self.out, out);
        self.retire(old_out);
        self.history_views = history_views;
        self.out_view = out_view;
        self.post_size = post_size;
        Ok(true)
    }

    /// Keep the previous-depth pair in step with the render size (see the
    /// module doc). Returns `true` when the BOUND previous depth changed, so
    /// the resolve's bind groups must be rebuilt.
    pub fn ensure_render_size(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        render_size: (u32, u32),
        velocity_format: TextureFormat,
    ) -> Result<bool> {
        let render_size = clamp_size(render_size);
        let mut rebound = false;
        // A submitted frame copied its depth into the pending target: that is
        // now "last frame's depth".
        if self.next_prev_depth_filled.replace(false) {
            if let Some(next) = self.next_prev_depth.take() {
                let old = std::mem::replace(&mut self.prev_depth, next);
                self.retire(old.texture);
                self.retire(old.velocity);
                rebound = true;
            }
        }
        let wanted = (render_size, velocity_format);
        if (self.prev_depth.size, self.prev_depth.velocity_format) == wanted {
            // Back at the bound size before a pending target was filled.
            if let Some(next) = self.next_prev_depth.take() {
                self.retire(next.texture);
                self.retire(next.velocity);
            }
        } else if self
            .next_prev_depth
            .as_ref()
            .map(|n| (n.size, n.velocity_format))
            != Some(wanted)
        {
            let next = PrevTargets::new(gpu, self.depth_format, velocity_format, render_size)?;
            if let Some(old) = self.next_prev_depth.replace(next) {
                self.retire(old.texture);
                self.retire(old.velocity);
            }
        }
        Ok(rebound)
    }

    /// The previous depth the resolve reads (last frame's depth).
    pub fn prev_depth_view(&self) -> &web_sys::GpuTextureView {
        &self.prev_depth.view
    }

    /// The previous velocity the resolve reads (last frame's vectors).
    pub fn prev_velocity_view(&self) -> &web_sys::GpuTextureView {
        &self.prev_depth.velocity_view
    }

    /// Where this frame's depth and velocity are copied to, their size (this
    /// frame's render size), and whether they are the pending targets
    /// (`next_prev_depth`).
    pub fn prev_copy_targets(
        &self,
    ) -> (&web_sys::GpuTexture, &web_sys::GpuTexture, (u32, u32), bool) {
        match self.next_prev_depth.as_ref() {
            Some(next) => (&next.texture, &next.velocity, next.size, true),
            None => (
                &self.prev_depth.texture,
                &self.prev_depth.velocity,
                self.prev_depth.size,
                false,
            ),
        }
    }

    /// A submitted frame copied its depth into the pending target.
    pub fn mark_next_prev_depth_filled(&self) {
        if self.next_prev_depth.is_some() {
            self.next_prev_depth_filled.set(true);
        }
    }

    /// Release every GPU texture (TAA turned off). Immediate, unlike a
    /// resize: `set_anti_aliasing` runs between frames and awaits pipeline
    /// compiles before it gets here, the same lifecycle SMAA's disable
    /// destroys its textures in.
    pub fn destroy(self) {
        for tex in self.history {
            tex.destroy();
        }
        self.out.destroy();
        self.prev_depth.texture.destroy();
        self.prev_depth.velocity.destroy();
        if let Some(next) = self.next_prev_depth {
            next.texture.destroy();
            next.velocity.destroy();
        }
        for (tex, _) in self.retired {
            tex.destroy();
        }
    }

    fn retire(&mut self, texture: web_sys::GpuTexture) {
        self.retired.push((texture, DESTROY_DELAY_FRAMES));
    }
}

impl PrevTargets {
    fn new(
        gpu: &AwsmRendererWebGpu,
        format: TextureFormat,
        velocity_format: TextureFormat,
        size: (u32, u32),
    ) -> Result<Self> {
        let texture = gpu.create_texture(
            &TextureDescriptor::new(
                format,
                Extent3d::new(size.0, Some(size.1), Some(1)),
                // COPY_DST: filled by a depth-to-depth copy at the end of the
                // resolve. TEXTURE_BINDING: read as `texture_depth_2d`.
                TextureUsage::new().with_copy_dst().with_texture_binding(),
            )
            .with_label("TAA Previous Depth")
            .into(),
        )?;
        let view = create_view(&texture, "TAA Previous Depth")?;
        let velocity = gpu.create_texture(
            &TextureDescriptor::new(
                velocity_format,
                Extent3d::new(size.0, Some(size.1), Some(1)),
                // COPY_DST: a copy of the velocity target after the resolve.
                // TEXTURE_BINDING: read with textureLoad.
                TextureUsage::new().with_copy_dst().with_texture_binding(),
            )
            .with_label("TAA Previous Velocity")
            .into(),
        )?;
        let velocity_view = create_view(&velocity, "TAA Previous Velocity")?;
        Ok(Self {
            texture,
            view,
            velocity,
            velocity_view,
            velocity_format,
            size,
        })
    }
}

fn clamp_size((w, h): (u32, u32)) -> (u32, u32) {
    (w.max(1), h.max(1))
}

fn create_history(
    gpu: &AwsmRendererWebGpu,
    size: (u32, u32),
) -> Result<([web_sys::GpuTexture; 2], [web_sys::GpuTextureView; 2])> {
    let usage = || {
        TextureUsage::new()
            .with_storage_binding()
            .with_texture_binding()
    };
    let (a, a_view) = create_color(gpu, "TAA History 0", size, usage())?;
    let (b, b_view) = create_color(gpu, "TAA History 1", size, usage())?;
    Ok(([a, b], [a_view, b_view]))
}

/// `out`: storage-written by the resolve, texture-read by bloom / effects,
/// and a render attachment for the HUD drawn over it after the resolve
/// (plan phase 3).
fn create_out(
    gpu: &AwsmRendererWebGpu,
    size: (u32, u32),
) -> Result<(web_sys::GpuTexture, web_sys::GpuTextureView)> {
    create_color(
        gpu,
        "TAA Out",
        size,
        TextureUsage::new()
            .with_storage_binding()
            .with_texture_binding()
            .with_render_attachment(),
    )
}

fn create_color(
    gpu: &AwsmRendererWebGpu,
    label: &str,
    (width, height): (u32, u32),
    usage: TextureUsage,
) -> Result<(web_sys::GpuTexture, web_sys::GpuTextureView)> {
    let tex = gpu.create_texture(
        &TextureDescriptor::new(
            TAA_COLOR_FORMAT,
            Extent3d::new(width, Some(height), Some(1)),
            usage,
        )
        .with_label(label)
        .into(),
    )?;
    let view = create_view(&tex, label)?;
    Ok((tex, view))
}

fn create_view(tex: &web_sys::GpuTexture, label: &str) -> Result<web_sys::GpuTextureView> {
    tex.create_view()
        .map_err(|e| AwsmCoreError::create_texture_view(format!("{label}: {e:?}").into()))
}
