use crate::repo_root;
use std::{fs, path::Path, process::Command};

fn write(root: &Path, path: &str, content: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn fixture(root: &Path, label: &str) {
    let manifest: toml::Value = toml::from_str(
        &fs::read_to_string(repo_root().join("crates/contextful-core/Cargo.toml")).unwrap(),
    )
    .unwrap();
    let build = manifest["package"]
        .get("build")
        .and_then(toml::Value::as_str)
        .map(|path| format!("build = {path:?}\n"))
        .unwrap_or_default();
    write(
        root,
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/demo\"]\nresolver = \"2\"\n",
    );
    write(root, "crates/demo/Cargo.toml", &format!(
        "[package]\nname = \"source-binding-demo\"\nversion = \"0.0.0\"\nedition = \"2021\"\n{build}\n[dependencies]\nshared-fixture = {{ path = \"../../../shared-dep\" }}\n"
    ));
    write(root, "crates/demo/src/lib.rs", &format!(
        "/// ```\n/// assert_eq!(source_binding_demo::value(), {label:?});\n/// ```\npub fn value() -> &'static str {{ let _ = shared_fixture::value(); {label:?} }}\n"
    ));
    write(
        root,
        "crates/demo/src/main.rs",
        "fn main() { println!(\"{}\", source_binding_demo::value()); }\n",
    );
    write(root, "crates/demo/tests/integration.rs", &format!(
        "#[test]\nfn {label}_source_test() {{ assert_eq!(source_binding_demo::value(), {label:?}); }}\n"
    ));
    for path in [".cargo/config.toml", "build/source.rs"] {
        if let Ok(content) = fs::read_to_string(repo_root().join(path)) {
            let content = if path == "build/source.rs" {
                content.replace(
                    "fn main() {",
                    &format!(
                        "fn main() {{ println!(\"cargo:warning={label}_source_build_script\");"
                    ),
                )
            } else {
                content
            };
            write(root, path, &content);
        }
    }
}

fn cargo(root: &Path, shared: &Path, command: &str) -> std::process::Output {
    // The std-only comparison shares one scratch build directory on every host;
    // the repository pool and its dependencies remain outside the fixture.
    let config = format!("build.target-dir={:?}", shared.to_str().unwrap());
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let out = Command::new(cargo)
        .args(["--config", &config, command, "--offline", "-vv"])
        .current_dir(root)
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CONTEXTFUL_PROVENANCE_ROOT")
        .env_remove("CONTEXTFUL_SOURCE_ROOT")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{command}: {}\n{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    out
}

#[test]
fn shared_artifacts_execute_the_selected_checkout_source() {
    let temp = tempfile::tempdir().unwrap();
    let a = temp.path().join("a");
    let b = temp.path().join("b");
    let shared = temp.path().join("shared");
    let dependency = temp.path().join("shared-dep");
    write(&dependency, "Cargo.toml", "[package]\nname = \"shared-fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n[workspace]\n");
    write(&dependency, "src/lib.rs", "pub fn value() -> u32 { 1 }\n");
    // Both source trees predate the first compiled artifact.
    fixture(&a, "alpha");
    fixture(&b, "bravo");
    for (root, label) in [(&a, "alpha"), (&b, "bravo"), (&a, "alpha")] {
        let tests = cargo(root, &shared, "test");
        assert!(
            String::from_utf8_lossy(&tests.stderr)
                .contains(&format!("{label}_source_build_script")),
            "the selected checkout's build script did not execute"
        );
        if label == "bravo" {
            assert!(
                String::from_utf8_lossy(&tests.stderr).contains("Fresh shared-fixture"),
                "the unchanged dependency recompiles between checkouts"
            );
        }
        let stdout = String::from_utf8(tests.stdout).unwrap();
        assert!(
            stdout.contains(&format!("test {label}_source_test ... ok")),
            "the selected checkout's nonempty suite did not execute: {stdout}"
        );
        let ran = cargo(root, &shared, "run");
        assert_eq!(
            String::from_utf8(ran.stdout).unwrap().trim(),
            label,
            "the selected checkout's binary differs from its source"
        );
    }
}
