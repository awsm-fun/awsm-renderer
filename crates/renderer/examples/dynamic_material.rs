//! Minimal end-to-end example of registering a custom dynamic material
//! against an `AwsmRenderer` without any editor or scene-schema
//! dependency.
//!
//! Mirrors the recipe documented on
//! [`AwsmRenderer::register_material`](awsm_renderer::AwsmRenderer::register_material)
//! and serves as the integration example the Public-API gate calls out.
//!
//! Run with:
//! ```bash
//! cargo build --example dynamic_material -p awsm-renderer
//! ```
//!
//! This file deliberately depends ONLY on `awsm-renderer` +
//! `awsm-materials` + `awsm-renderer-core`. No `awsm-scene-schema`, no
//! `awsm-web-shared`, no editor crates. A game runtime can reproduce
//! this recipe verbatim.
//!
//! Note: the example doesn't actually drive a render loop — that
//! requires a real wasm/native WebGPU host. The point is to exercise
//! the public-API surface (registration, layout types, default value
//! construction) in a way that the compiler enforces.

use awsm_materials::{
    dynamic_layout::{
        BufferSlotRuntime, FieldType, MaterialLayout, TextureSlotRuntime, UniformFieldRuntime,
    },
    MaterialAlphaMode,
};

#[allow(dead_code)]
fn build_scanline_registration() -> awsm_renderer::dynamic_materials::MaterialRegistration {
    // 1. Define the layout (uniforms + textures + buffers).
    let layout = MaterialLayout {
        uniforms: vec![
            UniformFieldRuntime {
                name: "tint".into(),
                ty: FieldType::Color3,
            },
            UniformFieldRuntime {
                name: "scan_freq".into(),
                ty: FieldType::F32,
            },
            UniformFieldRuntime {
                name: "scan_speed".into(),
                ty: FieldType::F32,
            },
            UniformFieldRuntime {
                name: "scan_strength".into(),
                ty: FieldType::F32,
            },
        ],
        textures: vec![TextureSlotRuntime {
            name: "base".into(),
        }],
        buffers: Vec::<BufferSlotRuntime>::new(),
    };

    // 2. Build the WGSL fragment. In a real game this comes from a
    //    `shader.wgsl` file shipped alongside the binary; here we
    //    inline a minimal stub.
    let wgsl_fragment = r#"
let fg = frame_globals_from_raw(frame_globals_raw);
let uv = vec2<f32>(f32(input.coords.x), f32(input.coords.y))
       / vec2<f32>(f32(input.screen_dims.x), f32(input.screen_dims.y));
let scan = sin(uv.y * input.material.scan_freq
             + fg.time * input.material.scan_speed);
let overlay = mix(vec3<f32>(0.0), input.material.tint,
                  scan * input.material.scan_strength);
return OpaqueShadingOutput(overlay + vec3<f32>(0.5), 1.0);
"#
    .to_string();

    // 3. Compute stable hashes. Real consumers can use any stable
    //    hash (xxhash, blake3, etc.); the example uses the std
    //    DefaultHasher for portability.
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    "scanline".hash(&mut h);
    for u in &layout.uniforms {
        u.name.hash(&mut h);
    }
    let layout_hash = h.finish();
    let mut h2 = std::collections::hash_map::DefaultHasher::new();
    wgsl_fragment.hash(&mut h2);
    let wgsl_hash = h2.finish();

    // 4. Build the registration.
    awsm_renderer::dynamic_materials::MaterialRegistration {
        name: "scanline".into(),
        alpha_mode: MaterialAlphaMode::Opaque,
        double_sided: false,
        layout,
        layout_hash,
        wgsl_hash,
        wgsl_fragment,
        buffer_defaults: Vec::new(),
    }
}

// In a real consumer:
//
// ```no_run
// async fn boot() -> awsm_renderer::error::Result<()> {
//     let mut renderer = awsm_renderer::AwsmRendererBuilder::new(gpu).build().await?;
//
//     // Register.
//     let shader_id = renderer.register_material(build_scanline_registration())?;
//
//     // Compile the new per-shader-id pipelines in one batched pool.
//     renderer.prewarm_pipelines().await?;
//
//     // ... insert a Material::Custom instance pointing at `shader_id`
//     //     and render normally.
//     Ok(())
// }
// ```

fn main() {
    let _reg = build_scanline_registration();
    println!("Constructed a MaterialRegistration for `scanline`. ");
    println!("In a real consumer, pass it to AwsmRenderer::register_material.");
}
