use std::{env, path::PathBuf};

const _: Option<&str> = option_env!("CONTEXTFUL_SOURCE_ROOT");

fn main() {
    println!("cargo:rerun-if-env-changed=CONTEXTFUL_SOURCE_ROOT");
    let manifest = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR").expect("Cargo supplies the package manifest directory"),
    );
    let expected = manifest
        .join("../..")
        .canonicalize()
        .expect("the workspace source root exists");
    let identity = expected
        .to_str()
        .expect("the source-binding path is Unicode");
    assert!(
        !identity.contains(['\n', '\r']),
        "the source-binding path contains a Cargo directive separator"
    );
    if let Some(root) = env::var_os("CONTEXTFUL_SOURCE_ROOT") {
        assert_eq!(
            PathBuf::from(root)
                .canonicalize()
                .expect("the configured source root exists"),
            expected,
            "the source-binding configuration belongs to this checkout"
        );
    }
    println!(
        "cargo:rustc-env=CONTEXTFUL_COMPILED_SOURCE_ROOT={}",
        identity
    );
}
