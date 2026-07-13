use std::path::Path;

/// The paid acceptance fixtures live in the gitignored `fixtures/local/` and
/// are never committed, so their tests only exist when the bytes are present
/// on this machine (see `tests/fixture_decode.rs`).
fn main() {
    println!("cargo::rustc-check-cfg=cfg(has_local_fixtures)");
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let fixture = Path::new(&manifest_dir).join("../../../fixtures/local/police-meshopt.glb");
    println!("cargo::rerun-if-changed={}", fixture.display());
    if fixture.exists() {
        println!("cargo::rustc-cfg=has_local_fixtures");
    }
}
