//! Cache-key + template glue for the fat-line shader.
//!
//! The shader itself is a static `line.wgsl` with no template
//! substitutions, but going through the `Shaders` cache means
//! `Shaders::ensure_keys` can pre-warm the line shader alongside
//! other shaders (e.g. the picker) so the browser compiles them
//! in parallel. The previous `shaders.insert_uncached` shape
//! bypassed the cache entirely, forcing the line shader's compile
//! to serialize behind whatever other shader work was in flight.

pub mod cache_key;
pub mod template;
