# Renderer core

This is a core crate for [awsm renderer](https://crates.io/crates/awsm-renderer).

At this level, it's just a thin wrapper around the WebGPU API. It is intended to be used as a low-level primitive, without the headache of dealing with the raw `web-sys` bindings directly.

The overall approach is to allow native web-sys types throughout the main methods, but have Rust-friendly data types that can be used to create all the descriptors, pipelines, etc. These Rust-friendly data types `impl Into<web_sys::...>` and so they can be passed like `foo.into()`. This allows for a more idiomatic Rust API for all the heavy lifting, while still allowing for the raw web-sys types to be used when needed. 

In some cases like the command encoder, the custom type holds an inner raw web-sys type, and impls Deref to it, so you get a mixture of the original methods and nicer new ones as they are added.
