//! A component source on a binary built without the `component-host` feature.
#![cfg(not(feature = "component-host"))]

use std::process::Command;

/// Dispatching a component connector on a profile with no component host raises `ComponentHostMissing`, naming the
/// connector and the profile, with no fallback to a similarly named native source.
// spec: topology.package.host-missing@788dbe3c
#[test]
fn a_component_source_is_refused_by_name_on_a_build_without_the_host() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join(".contextful/context/research");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("config.toml"), "[node]\nid = \"ingest-a\"\n").unwrap();
    std::fs::create_dir_all(dir.path().join("connectors")).unwrap();
    std::fs::write(dir.path().join("connectors/http.wasm"), b"\0asm\x0d\0\x01\0").unwrap();
    let manifest = "[[pipeline]]\nid = \"probe\"\ntables = [\"items\"]\n\n[pipeline.source]\nname = \"connectors/http.wasm\"\n";
    std::fs::write(dir.path().join("contextful.toml"), manifest).unwrap();
    for args in [&["pipeline", "validate"][..], &["pipeline", "run", "probe", "--project", "research", "--run-id", "run-1", "--site-id", "site-a"][..]] {
        let out = Command::new(env!("CARGO_BIN_EXE_contextful")).args(args).current_dir(dir.path()).env_remove("CONTEXTFUL_NODE_ID").output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{args:?} ran a component with no host");
        assert!(stderr.contains("ComponentHostMissing"), "{args:?}: {stderr}");
        assert!(stderr.contains("connectors/http.wasm") && stderr.contains("component-host"), "{args:?}: {stderr}");
    }
}
