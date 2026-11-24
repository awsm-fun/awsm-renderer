// The harness's modes are deep async state machines built at opt-level 1,
// and the scene TOML parser recurses below them: with the default 1 MiB wasm
// stack a session opened from the deepest mode overflowed ("Uncaught
// RangeError" in toml_edit, the page stuck on "booting…"). 8 MiB.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") {
        println!("cargo:rustc-link-arg=-zstack-size=8388608");
    }
}
