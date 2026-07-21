//! `intel_tex_2` ships PREBUILT ispc_texcomp static libraries (C++/ISPC
//! objects) and compiles no C++ of its own, so cargo never links the C++
//! runtime for them. Their exception tables reference
//! `__gxx_personality_v0`, which on Linux lives in libstdc++ — without this
//! the CI link fails (`rust-lld: undefined symbol: __gxx_personality_v0`).
//! macOS resolves it via libc++; linked explicitly there too so both
//! platforms are deliberate rather than lucky. Windows/MSVC prebuilt libs
//! carry their own CRT references — leave it alone.

fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.contains("apple") {
        println!("cargo:rustc-link-lib=dylib=c++");
    } else if target.contains("linux") {
        println!("cargo:rustc-link-lib=dylib=stdc++");
    }
}
