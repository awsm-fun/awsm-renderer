//! The loader's asset source — the boundary between *how* the loader walks a
//! scene and *where* the bundle bytes actually come from.
//!
//! `populate_awsm_scene` and friends never touch the filesystem or a network:
//! they pull bundle bytes (glb / png / `material.json` / `material.wgsl` /
//! `buffer-*.bin`) by **bundle-relative path** through this trait. A real game
//! streams those bytes from a CDN or content-addressed store; the model-test
//! round-trip hands the loader a prebuilt in-memory map. Both satisfy
//! [`SceneAssets`] — the map via the blanket impl below — so the loader's
//! internals are identical across the two.
//!
//! Dispatch is **static** (`&impl SceneAssets`): a single concrete `A` threads
//! through the whole load (including the recursive `materialize`), so there is no
//! `dyn SceneAssets`, no object-safety concern, and no `Send` bound (the wasm
//! target is single-threaded).

use std::collections::HashMap;

/// Async accessor the loader uses to fetch bundle bytes by bundle-relative path.
///
/// The loader requests paths like `assets/<id>.glb`, `assets/<id>.png`,
/// `<folder>/material.json`, `<folder>/material.wgsl`, or `assets/buffer-<id>.bin`
/// and gets back the raw bytes. Implementors decide the backing store: a game
/// streams from a CDN / content-addressed store; the model-test round-trip uses
/// the prebuilt [`HashMap`] blanket impl, so the same load path serves both.
///
/// Object safety is not required — the loader uses static dispatch
/// (`&impl SceneAssets`), so a single concrete type threads through the whole
/// load. There is intentionally no `Send` bound (single-threaded wasm target).
//
// `async fn` in a public trait triggers the `async_fn_in_trait` lint because the
// returned future is un-nameable (so a caller can't add a `Send`/`'static`
// bound). That's exactly what we want here: static dispatch only, single-threaded
// wasm target, no `Send` needed — so the lint is allowed deliberately.
#[allow(async_fn_in_trait)]
pub trait SceneAssets {
    /// Fetch the bytes for one bundle-relative path. `Err` means the asset is
    /// unavailable (missing / unreachable); callers map that to their existing
    /// missing-asset behavior (skip the slot, warn, or bubble the error).
    async fn fetch(&self, bundle_relative_path: &str) -> anyhow::Result<Vec<u8>>;
}

/// The model-test round-trip's in-memory bundle: a prebuilt
/// `bundle-relative path → bytes` map. `fetch` is an infallible lookup that
/// errors only when the path isn't present.
impl SceneAssets for HashMap<String, Vec<u8>> {
    async fn fetch(&self, path: &str) -> anyhow::Result<Vec<u8>> {
        self.get(path)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("asset not found: {path}"))
    }
}

/// An HTTP [`SceneAssets`] that fetches bundle bytes from a base origin — the
/// player counterpart of the model-test's in-memory [`HashMap`] source, and the
/// impl a shipped web player almost always wants.
///
/// A player bundle ships as static files (`scene.toml` + `assets/…`) served at
/// some origin; the loader asks for bundle-relative paths and this GETs
/// `<origin>/<path>`. This is why fetching is *not* baked into
/// `load_scene_for_player`: the loader stays transport-agnostic (a game might
/// stream from a CDN, a content-addressed store, or a preloaded map), and the
/// common same-origin-HTTP case is just this ready-made impl. Enable the `http`
/// feature to pull it in (it adds a web-only `gloo-net` dependency), and a
/// template needs no bespoke fetching glue:
///
/// ```ignore
/// let assets = HttpAssets::new(page_origin); // window.location.origin
/// load_scene_for_player(&mut renderer, &scene, &assets, |_| {}).await?;
/// ```
#[cfg(feature = "http")]
pub struct HttpAssets {
    base: String,
}

#[cfg(feature = "http")]
impl HttpAssets {
    /// Fetch bundle files relative to `origin` (a trailing `/` is trimmed, so
    /// `https://host` and `https://host/` behave identically). For a same-origin
    /// web player pass the page origin (`window.location.origin`).
    pub fn new(origin: impl Into<String>) -> Self {
        Self {
            base: origin.into().trim_end_matches('/').to_string(),
        }
    }
}

#[cfg(feature = "http")]
impl SceneAssets for HttpAssets {
    async fn fetch(&self, path: &str) -> anyhow::Result<Vec<u8>> {
        let url = format!("{}/{}", self.base, path);
        let bytes = gloo_net::http::Request::get(&url)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("fetch {url}: {e}"))?
            .binary()
            .await
            .map_err(|e| anyhow::anyhow!("fetch {url} body: {e}"))?;
        Ok(bytes)
    }
}
