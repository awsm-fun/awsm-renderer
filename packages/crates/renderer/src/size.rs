//! Shared integer size math for viewport-derived render targets.
//!
//! The single home for "how big is the half-res version of this viewport"
//! so that a size computed at texture-allocation time and the same size
//! reconstructed at dispatch/compare time land on the *identical* number.
//! Hand-rolling the arithmetic at each site is how the bloom per-frame
//! rebuild storm crept in: `BloomTexture::new` allocated at `view / 2`
//! (floor) while `ensure_size` reconstructed the viewport as `base * 2`,
//! so any odd viewport dimension made the equality check perpetually
//! false — rebuilding the pyramid AND re-firing `TextureViewRecreate`
//! (which recreates every texture-view-dependent bind group) every frame.
//!
//! Rule: whenever a render target is a *fraction* of the viewport, derive
//! its extent through these helpers on BOTH the allocation side and every
//! reconstruction/dispatch side. Never reverse the math (`base * 2`) to
//! recover the viewport — halving is lossy, the round trip is not exact.
//!
//! Mip-chain extents (`base >> level`) are a *different*, WebGPU-mandated
//! (floor) convention and live in
//! [`awsm_renderer_core::texture::mipmap::get_mipmap_size_for_level`];
//! use that for mip levels, these helpers for half-res targets.

/// Half of a viewport extent, **rounded up**, clamped to ≥ 1.
///
/// Ceil (not floor) so the half-res buffer fully covers the viewport when
/// it is bilinearly upsampled back to full-res: `half_extent(705) == 353`
/// and `353 * 2 = 706 ≥ 705`, so the last full-res column always has a
/// source texel. Floor would leave `704 < 705`, dropping the edge column.
/// This is the convention SSR's half-res trace target already relied on;
/// bloom's prefilter downsample shares it (a blur is indifferent to the
/// ±1 texel, but a single convention keeps the round trip exact).
#[inline]
pub fn half_extent(full: u32) -> u32 {
    full.div_ceil(2).max(1)
}

/// A viewport extent scaled by the supersampling `render_scale`,
/// **rounded to nearest**, clamped to >= 1. The single home for the
/// canvas->render-resolution mapping (same rule as the half-res helpers
/// above: derive on BOTH the allocation side and every dispatch/compare
/// side; never invert the math to recover the canvas size).
pub fn scale_extent(extent: u32, render_scale: f32) -> u32 {
    ((extent as f64 * render_scale as f64).round() as u32).max(1)
}

/// The three sizes a frame runs at, snapped once per frame.
///
/// - `surface`: the swap-chain (canvas backing buffer). Only the display
///   pass's colour attachment and swap-chain readbacks are this size.
/// - `render`: what the scene is rasterised and shaded at (geometry, prep,
///   shading, transparents, `composite`, SSR, HZB, light culling, froxels,
///   picker). `surface × render_scale`.
/// - `post`: what runs after the (future) TAA resolve: bloom, SMAA, the
///   effects pass (haze, DoF, bloom composite, SMAA blend) and the display
///   pass's input.
///
/// Every render-texture consumer reads `render` or `post` explicitly (pinned
/// by `size::tests::render_texture_consumers_choose_a_size`). Without TAA the
/// whole frame, post-processing included, runs at render size, which is the
/// 0.45 behaviour (supersampling included: bloom and effects run at the
/// supersampled size and display downsamples). With TAA the resolve writes
/// surface-sized output, so `post == surface` while `render` may shrink
/// (`docs/plans/taa-upscaling/03-design.md` §0, §6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameSizes {
    pub surface: (u32, u32),
    pub render: (u32, u32),
    pub post: (u32, u32),
}

impl FrameSizes {
    /// `render_scale` is the effective scene scale (supersampling `[1, 2]`
    /// without TAA). `post_is_surface` is true only when a temporal resolve
    /// writes the output at surface size (TAA on); otherwise post size equals
    /// render size.
    pub fn new(surface: (u32, u32), render_scale: f32, post_is_surface: bool) -> Self {
        let render = (
            scale_extent(surface.0, render_scale),
            scale_extent(surface.1, render_scale),
        );
        let post = if post_is_surface {
            (surface.0.max(1), surface.1.max(1))
        } else {
            render
        };
        Self {
            surface,
            render,
            post,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_extent_is_idempotent_round_trip() {
        // The bloom churn bug: allocate at `half_extent(view)`, then compare
        // by re-deriving `half_extent(view)`. Feeding the SAME viewport must
        // yield the SAME base for EVERY dimension (odd included), or the
        // equality check that gates the rebuild is perpetually false.
        for view in 0..=4097u32 {
            assert_eq!(half_extent(view), half_extent(view));
        }
    }

    #[test]
    fn half_extent_covers_on_upsample() {
        // Ceil guarantees `2 * half >= view`, so a bilinear upsample of the
        // half-res buffer always has a source texel for the last full-res
        // row/column (SSR relies on this; floor would drop the edge).
        for view in 1..=4097u32 {
            assert!(2 * half_extent(view) >= view, "view={view}");
        }
    }

    #[test]
    fn frame_sizes_without_taa_run_post_at_render_size() {
        // TAA off: post-processing runs at render size, at every scale. This
        // is what keeps supersampling byte-identical to 0.45 (bloom/effects at
        // the supersampled size, display downsamples).
        for scale in [1.0f32, 1.25, 1.5, 2.0] {
            let s = FrameSizes::new((1920, 1435), scale, false);
            assert_eq!(s.post, s.render, "scale {scale}");
            assert_eq!(
                s.render,
                (scale_extent(1920, scale), scale_extent(1435, scale))
            );
            assert_eq!(s.surface, (1920, 1435));
        }
        let native = FrameSizes::new((705, 333), 1.0, false);
        assert_eq!(native.render, (705, 333));
        assert_eq!(native.post, (705, 333));
    }

    #[test]
    fn frame_sizes_with_taa_post_at_surface() {
        let s = FrameSizes::new((1920, 1435), 0.67, true);
        assert_eq!(s.post, (1920, 1435));
        assert_eq!(s.render, (1286, 961));
        // Native TAA: all three equal.
        let n = FrameSizes::new((1920, 1435), 1.0, true);
        assert_eq!((n.render, n.post), ((1920, 1435), (1920, 1435)));
    }

    #[test]
    fn frame_sizes_at_the_taa_presets() {
        use crate::taa::{TaaConfig, TaaPreset};
        for surface in [(1920u32, 1080u32), (1920, 1435), (2560, 1440), (705, 333)] {
            for preset in TaaPreset::ALL {
                let scale = TaaConfig::preset(preset).clamped_render_scale();
                let s = FrameSizes::new(surface, scale, true);
                assert_eq!(s.post, surface, "{preset:?} {surface:?}");
                assert_eq!(
                    s.render,
                    (
                        scale_extent(surface.0, scale),
                        scale_extent(surface.1, scale)
                    ),
                    "{preset:?} {surface:?}"
                );
                // Each side is within half a pixel of the exact scale, so the
                // render aspect differs from the surface's by under a pixel
                // (the camera projects with the surface aspect under TAA).
                for (r, o) in [(s.render.0, surface.0), (s.render.1, surface.1)] {
                    assert!((r as f64 - o as f64 * scale as f64).abs() <= 0.5);
                }
            }
        }
        // Performance at 1920 x 1080 is exactly half.
        assert_eq!(FrameSizes::new((1920, 1080), 0.5, true).render, (960, 540));
    }

    /// Every render-texture consumer reads the render size or the post size
    /// explicitly (taa-upscaling plan, phase 0). The compiler does most of the
    /// work: `RenderTextureViews` has no plain `width`/`height`, only
    /// `render_size` and `post_size`, and `RenderContext` has no
    /// `viewport_size`. This scan stops either from coming back, and stops new
    /// code from deriving a size from the swap chain on its own instead of
    /// going through `AwsmRenderer::frame_sizes()`.
    #[test]
    fn render_texture_consumers_choose_a_size() {
        // Sites allowed to read the swap-chain size directly, with why.
        const SURFACE_READERS: &[(&str, &str)] = &[
            (
                "anti_alias.rs",
                "frame_sizes(): the one source of all three sizes",
            ),
            (
                "render.rs",
                "capture_frame(): reads back the swap chain itself",
            ),
            (
                "camera/mod.rs",
                "camera viewport at render size (the same scale_extent as \
                 frame_sizes); kept local because AwsmCameraError has no \
                 From<AwsmError> for render_size()?",
            ),
        ];
        // Ambiguous spellings that must not come back anywhere.
        const FORBIDDEN: &[&str] = &[
            "render_texture_views.width",
            "render_texture_views.height",
            "ctx.viewport_size",
            "views.width",
            "views.height",
        ];

        fn scan(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<(String, String)>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    scan(root, &path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let rel = path
                        .strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/");
                    out.push((rel, std::fs::read_to_string(&path).unwrap()));
                }
            }
        }
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        scan(&src, &src, &mut files);
        assert!(
            files.iter().any(|(p, _)| p == "render_textures.rs"),
            "the scan itself is broken"
        );

        let mut failures = Vec::new();
        for (path, source) in &files {
            // This file holds the pattern strings themselves.
            if path == "size.rs" {
                continue;
            }
            for (n, line) in source.lines().enumerate() {
                let code = line.split("//").next().unwrap_or("");
                for pat in FORBIDDEN {
                    if code.contains(pat) {
                        failures.push(format!(
                            "{path}:{}: `{pat}`: read `.render_size` or `.post_size`",
                            n + 1
                        ));
                    }
                }
                if code.contains("current_context_texture_size(")
                    && !SURFACE_READERS.iter().any(|(p, _)| p == path)
                {
                    failures.push(format!(
                        "{path}:{}: reads the swap-chain size; use frame_sizes() / \
                         render_size() / post_size()",
                        n + 1
                    ));
                }
            }
        }
        // Allowlist entries must stay live, or they hide nothing and rot.
        for (path, why) in SURFACE_READERS {
            let used = files.iter().any(|(p, s)| {
                p == path
                    && s.lines().any(|l| {
                        l.split("//")
                            .next()
                            .unwrap_or("")
                            .contains("current_context_texture_size(")
                    })
            });
            if !used {
                failures.push(format!("stale allowlist entry {path} ({why}): remove it"));
            }
        }

        // The texture allocator sizes every texture from a named render or
        // post extent, never from a bare `width`/`height`.
        let (_, textures) = files
            .iter()
            .find(|(p, _)| p == "render_textures.rs")
            .unwrap();
        for (n, line) in textures.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            if code.contains("Extent3d::new(width") || code.contains("Extent3d::new(height") {
                failures.push(format!(
                    "render_textures.rs:{}: bare width/height; use render_w/h or post_w/h",
                    n + 1
                ));
            }
        }

        assert!(
            failures.is_empty(),
            "render-texture size not chosen explicitly:\n{}",
            failures.join("\n")
        );
    }

    #[test]
    fn half_extent_clamps_to_one() {
        assert_eq!(half_extent(0), 1);
        assert_eq!(half_extent(1), 1);
        assert_eq!(half_extent(2), 1);
        assert_eq!(half_extent(3), 2);
        assert_eq!(half_extent(705), 353);
    }
}
