use std::{env, path::PathBuf};

const _: &str = env!("CONTEXTFUL_SOURCE_ROOT");

fn main() {
    println!("cargo:rerun-if-env-changed=CONTEXTFUL_SOURCE_ROOT");
    let root = PathBuf::from(
        env::var_os("CONTEXTFUL_SOURCE_ROOT")
            .expect("Cargo requires the workspace source-binding configuration"),
    );
    let manifest = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR").expect("Cargo supplies the package manifest directory"),
    );
    let expected = manifest
        .join("../..")
        .canonicalize()
        .expect("the workspace source root exists");
    assert_eq!(
        root.canonicalize()
            .expect("the configured source root exists"),
        expected,
        "the source-binding configuration belongs to this checkout"
    );
    println!(
        "cargo:rustc-env=CONTEXTFUL_COMPILED_SOURCE_ROOT={}",
        expected.display()
    );
}
