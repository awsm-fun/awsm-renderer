//! Real glTF/glb model import. Fetches the document and `populate_gltf`s it into
//! the renderer — which inserts every primitive's mesh + transform tree, so the
//! model renders immediately (Model imports are no longer passive). The
//! per-`Model`-node template/instancing binding (one editor node ⇄ one gltf
//! node's meshes, with teardown) is the deeper follow-on; this delivers visible
//! glTF rendering.

use awsm_renderer_gltf::{loader::GltfLoader, AwsmRendererGltfExt};

use crate::engine::context::renderer_handle;

/// Load + populate a glTF/glb from `url`; returns a display name from the URL.
/// File type is inferred from the URL extension (`.glb`/`.gltf`).
pub async fn import(url: &str) -> Result<String, String> {
    let loader = GltfLoader::load(url, None)
        .await
        .map_err(|e| format!("load: {e}"))?;
    let data = loader.into_data(None).map_err(|e| format!("decode: {e}"))?;
    {
        // Hold the renderer lock across the async populate.
        let handle = renderer_handle();
        let mut r = handle.lock().await;
        r.populate_gltf(data, None)
            .await
            .map_err(|e| format!("populate: {e}"))?;
    }
    Ok(model_name(url))
}

fn model_name(url: &str) -> String {
    url.rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("model")
        .to_string()
}
