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

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

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

    /// If this source can expose `bundle_relative_path` as a URL the browser can
    /// fetch directly, return it; otherwise `None` (the default).
    ///
    /// When it returns `Some`, the loader decodes images straight from the
    /// network response (`createImageBitmap` on `fetch(url).blob()`) instead of
    /// pulling the compressed bytes into wasm via [`fetch`](Self::fetch) and
    /// copying them back out to a JS buffer to decode. On a large, HTTP-served
    /// game that removes a full round trip of *every* texture through wasm
    /// linear memory (browser→wasm→JS), plus the transient heap it occupies.
    ///
    /// This is an explicit, opt-in capability — NOT tied to [`HttpAssets`]. Any
    /// URL-backed source (a custom CDN / content-addressed store that serves
    /// over HTTP) implements it to get the fast path; byte-only sources (the
    /// in-memory map, a source with no addressable URL) keep the default `None`
    /// and the loader falls back to `fetch` + decode with no behavior change.
    /// The URL returned MUST GET the same bytes `fetch` would for that path.
    fn asset_url(&self, bundle_relative_path: &str) -> Option<String> {
        let _ = bundle_relative_path;
        None
    }
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

/// How many seed fetches [`PrefetchedAssets::seed`] drives concurrently —
/// bounds resource pressure, not parallelism opportunity (HTTP/2 multiplexes;
/// HTTP/1.1 pools ~6 per origin).
///
/// Measured against the prod CDN (2026-08-23, 277-file world, edge HITs,
/// latency-isolated via ranged requests): wall time scales near-linearly with
/// this pool up to 32 wide (~3× vs 8) with per-request latency flat, and goes
/// flat-to-worse beyond. A loose-file bundle's seed is hundreds of small
/// latency-bound files, so the pool IS the fetch phase's wall clock on
/// high-latency links.
const SEED_CONCURRENCY: usize = 32;

/// A [`SceneAssets`] wrapper that concurrently pre-fetches a KNOWN set of
/// bundle paths and serves them from memory.
///
/// Two wins over fetching at the consumption site:
///
/// * **Concurrency** — the loader's node walk is serial (it holds
///   `&mut AwsmRenderer`), so every fetch it makes is a full round trip in
///   series. Seeding runs them [`SEED_CONCURRENCY`]-wide up front.
/// * **Dedupe** — a seeded path is fetched from the source exactly once,
///   however many nodes consume it. (Mesh glbs shared by several nodes
///   previously re-downloaded per referencing node.)
///
/// Only SEEDED paths are cached: unknown paths pass straight through to the
/// inner source, so memory stays bounded by the seed set (which drops with the
/// wrapper at the end of the load), and streams with their own downstream
/// cache (texture PNGs → the loader's decoded-image cache) aren't
/// double-buffered. Seed failures are cached too — the consumption site sees
/// the same `Err` it would have seen fetching directly (e.g. a missing LOD
/// manifest still means "no LOD"), without paying the round trip again.
///
/// A PACKED bundle ([`Scene::pack`](awsm_renderer_scene::Scene)) seeds through
/// [`Self::seed_packs`] instead: a few concatenated blobs fetch
/// bandwidth-bound and slice into this same cache — including texture images,
/// which loose-file seeding deliberately leaves out. Those images are held
/// here for the load's duration on top of the decoded-image cache; that
/// transient double-buffering is the price of collapsing hundreds of
/// latency-bound texture/mesh round trips into a handful of blob fetches.
pub struct PrefetchedAssets<'a, A: SceneAssets> {
    inner: &'a A,
    /// path → fetched bytes (`None` = the seed fetch failed).
    cache: RefCell<HashMap<String, Option<Vec<u8>>>>,
}

impl<'a, A: SceneAssets> PrefetchedAssets<'a, A> {
    pub fn new(inner: &'a A) -> Self {
        Self {
            inner,
            cache: RefCell::new(HashMap::new()),
        }
    }

    /// Concurrently fetch a packed bundle's `assets/pack-<n>.bin` blobs and
    /// slice every member file into the cache, reporting `(done, total)` per
    /// BLOB completion (a packed bundle's fetch phase is a handful of
    /// bandwidth-bound blobs, so blob granularity is the honest unit).
    ///
    /// Failure semantics mirror [`Self::seed`]'s per-file caching: a blob that
    /// fails to fetch leaves its members UNCACHED (the consumption site falls
    /// through to a loose fetch and surfaces its ordinary missing-asset
    /// error), while a member whose recorded range doesn't fit its blob is
    /// cached as a failure — same `Err` a lost file would produce, without
    /// re-paying a doomed round trip.
    pub async fn seed_packs(
        &self,
        pack: &awsm_renderer_scene::ScenePack,
        mut on_progress: impl FnMut(usize, usize),
    ) {
        use futures::StreamExt;
        let total = pack.packs.len();
        if total == 0 {
            return;
        }
        on_progress(0, total);
        let mut blobs: Vec<Option<Vec<u8>>> = Vec::new();
        blobs.resize_with(total, || None);
        let mut stream = futures::stream::iter((0..total).map(|i| async move {
            let path = awsm_renderer_scene::pack_path(i);
            (i, self.inner.fetch(&path).await)
        }))
        .buffer_unordered(SEED_CONCURRENCY);
        let mut done = 0;
        while let Some((i, fetched)) = stream.next().await {
            match fetched {
                Ok(bytes) => {
                    if bytes.len() as u64 != pack.packs[i] {
                        tracing::warn!(
                            "scene-loader: pack blob {i} is {} bytes, index says {} — \
                             slicing what fits",
                            bytes.len(),
                            pack.packs[i]
                        );
                    }
                    blobs[i] = Some(bytes);
                }
                Err(e) => {
                    tracing::warn!(
                        "scene-loader: pack blob {i} unavailable ({e}) — its members fall \
                         back to loose fetches"
                    );
                }
            }
            done += 1;
            on_progress(done, total);
        }
        let mut cache = self.cache.borrow_mut();
        for (i, blob) in blobs.iter().enumerate() {
            let Some(blob) = blob else { continue };
            for (path, bytes) in pack.slice_blob(i, blob) {
                if bytes.is_none() {
                    tracing::warn!(
                        "scene-loader: packed file `{path}` has an out-of-range index entry — \
                         treating as missing"
                    );
                }
                cache.insert(path.to_string(), bytes.map(|b| b.to_vec()));
            }
        }
    }

    /// Concurrently fetch `paths` (duplicates and already-seeded paths are
    /// skipped) into the cache, reporting `(done, total)` per completion.
    pub async fn seed(&self, paths: Vec<String>, mut on_progress: impl FnMut(usize, usize)) {
        use futures::StreamExt;
        let pending: Vec<String> = {
            let cache = self.cache.borrow();
            let mut seen = HashSet::new();
            paths
                .into_iter()
                .filter(|p| !cache.contains_key(p) && seen.insert(p.clone()))
                .collect()
        };
        let total = pending.len();
        if total == 0 {
            return;
        }
        on_progress(0, total);
        let mut stream = futures::stream::iter(pending.into_iter().map(|path| async move {
            let bytes = self.inner.fetch(&path).await.ok();
            (path, bytes)
        }))
        .buffer_unordered(SEED_CONCURRENCY);
        let mut done = 0;
        while let Some((path, bytes)) = stream.next().await {
            self.cache.borrow_mut().insert(path, bytes);
            done += 1;
            on_progress(done, total);
        }
    }
}

impl<A: SceneAssets> SceneAssets for PrefetchedAssets<'_, A> {
    async fn fetch(&self, path: &str) -> anyhow::Result<Vec<u8>> {
        if let Some(cached) = self.cache.borrow().get(path) {
            return match cached {
                Some(bytes) => Ok(bytes.clone()),
                None => Err(anyhow::anyhow!(
                    "asset unavailable (cached seed failure): {path}"
                )),
            };
        }
        self.inner.fetch(path).await
    }

    fn asset_url(&self, path: &str) -> Option<String> {
        // A path in the cache is served from memory — for a PACKED bundle that
        // includes texture images, and the loose per-file URL may not even
        // exist, so never hand it out. Everything else (loose-file bundles
        // never cache images here — only glbs / buffers / material files are
        // path-seeded) exposes the inner source's URL so the loader keeps its
        // zero-copy image decode instead of a wasm byte round trip.
        if self.cache.borrow().contains_key(path) {
            return None;
        }
        self.inner.asset_url(path)
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
    /// Optional cache-bust token appended as `?cb=<token>` to every fetched
    /// URL. `None` (the default) fetches clean URLs and lets the browser HTTP
    /// cache do its normal thing — correct for stable, content-addressed
    /// bundles. A player that re-bakes assets to the SAME filename during dev
    /// (so a cache hit would silently serve the STALE prior bake) opts in via
    /// [`HttpAssets::with_cache_bust`] and supplies the token itself (a build
    /// id or a load-time timestamp). The loader stays policy-free: it only
    /// appends what the caller set.
    cache_bust: Option<String>,
}

#[cfg(feature = "http")]
impl HttpAssets {
    /// Fetch bundle files relative to `origin` (a trailing `/` is trimmed, so
    /// `https://host` and `https://host/` behave identically). For a same-origin
    /// web player pass the page origin (`window.location.origin`). Cache-busting
    /// is off by default — see [`HttpAssets::with_cache_bust`].
    pub fn new(origin: impl Into<String>) -> Self {
        Self {
            base: origin.into().trim_end_matches('/').to_string(),
            cache_bust: None,
        }
    }

    /// Append `?cb=<token>` to every fetched bundle URL to bypass the browser
    /// HTTP cache. The PLAYER owns the token — pass a build id (stable per
    /// release) or a per-load timestamp (fresh every reload, best for active
    /// asset iteration). Off unless called.
    pub fn with_cache_bust(mut self, token: impl Into<String>) -> Self {
        self.cache_bust = Some(token.into());
        self
    }

    /// The exact URL `fetch`/`asset_url` resolve for `path`, with the optional
    /// `?cb=` token applied. Bundle paths never carry their own query string, so
    /// a bare `?cb=` is always well-formed.
    fn url(&self, path: &str) -> String {
        match &self.cache_bust {
            Some(tok) => format!("{}/{}?cb={}", self.base, path, tok),
            None => format!("{}/{}", self.base, path),
        }
    }
}

#[cfg(feature = "http")]
impl SceneAssets for HttpAssets {
    async fn fetch(&self, path: &str) -> anyhow::Result<Vec<u8>> {
        let url = self.url(path);
        let bytes = gloo_net::http::Request::get(&url)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("fetch {url}: {e}"))?
            .binary()
            .await
            .map_err(|e| anyhow::anyhow!("fetch {url} body: {e}"))?;
        Ok(bytes)
    }

    fn asset_url(&self, path: &str) -> Option<String> {
        // The exact URL `fetch` GETs — so the loader's zero-copy image path
        // decodes from the same origin/response (same `?cb=`), just without
        // dragging the compressed bytes through wasm and back.
        Some(self.url(path))
    }
}
