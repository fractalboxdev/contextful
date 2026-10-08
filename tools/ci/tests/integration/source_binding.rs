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
    for path in [".cargo/config.toml", "tools/ci/build_source.rs"] {
        if let Ok(content) = fs::read_to_string(repo_root().join(path)) {
            let content = if path == "tools/ci/build_source.rs" {
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
    let out = cargo_output(root, shared, command);
    assert!(
        out.status.success(),
        "{command}: {}\n{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    out
}

fn cargo_output(root: &Path, shared: &Path, command: &str) -> std::process::Output {
    // The std-only comparison shares one scratch build directory on every host;
    // the repository pool and its dependencies remain outside the fixture.
    let config = format!("build.target-dir={:?}", shared.to_str().unwrap());
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    Command::new(cargo)
        .args(["--config", &config, command, "--offline", "-vv"])
        .current_dir(root)
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CONTEXTFUL_PROVENANCE_ROOT")
        .env_remove("CONTEXTFUL_SOURCE_ROOT")
        .output()
        .unwrap()
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
            "the selected build-script identity is absent from Cargo output"
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

#[test]
fn a_consumer_outside_the_checkout_compiles_its_path_dependency() {
    let temp = tempfile::tempdir().unwrap();
    let a = temp.path().join("a");
    let dependency = temp.path().join("shared-dep");
    write(&dependency, "Cargo.toml", "[package]\nname = \"shared-fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n[workspace]\n");
    write(&dependency, "src/lib.rs", "pub fn value() -> u32 { 1 }\n");
    fixture(&a, "alpha");
    let consumer = temp.path().join("consumer");
    write(&consumer, "Cargo.toml", "[package]\nname = \"consumer\"\nversion = \"0.0.0\"\nedition = \"2021\"\n[workspace]\n[dependencies]\nsource-binding-demo = { path = \"../a/crates/demo\" }\n");
    write(
        &consumer,
        "src/main.rs",
        "fn main() { assert_eq!(source_binding_demo::value(), \"alpha\"); println!(\"alpha\"); }\n",
    );
    let shared = temp.path().join("shared");
    cargo(&consumer, &shared, "build");
    let executable = if cfg!(windows) {
        "consumer.exe"
    } else {
        "consumer"
    };
    let ran = Command::new(shared.join("debug").join(executable))
        .output()
        .unwrap();
    assert!(ran.status.success());
    assert_eq!(String::from_utf8(ran.stdout).unwrap().trim(), "alpha");
}

#[test]
fn a_configured_identity_from_another_checkout_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let a = temp.path().join("a");
    let b = temp.path().join("b");
    let dependency = temp.path().join("shared-dep");
    write(&dependency, "Cargo.toml", "[package]\nname = \"shared-fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n[workspace]\n");
    write(&dependency, "src/lib.rs", "pub fn value() -> u32 { 1 }\n");
    fixture(&a, "alpha");
    fixture(&b, "bravo");
    write(
        &b,
        ".cargo/config.toml",
        "[env]\nCONTEXTFUL_SOURCE_ROOT = { value = \"../a\", relative = true, force = true }\n",
    );
    let out = cargo_output(&b, &temp.path().join("shared"), "build");
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("the source-binding configuration belongs to this checkout"),
        "the failure is the build script's identity refusal"
    );
}

#[cfg(unix)]
#[test]
fn checkout_paths_cannot_emit_additional_cargo_directives() {
    for separator in ["\n", "\r"] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join(format!("a{separator}cargo:warning=source_binding_injected_directive"));
        let dependency = temp.path().join("shared-dep");
        write(&dependency, "Cargo.toml", "[package]\nname = \"shared-fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n[workspace]\n");
        write(&dependency, "src/lib.rs", "pub fn value() -> u32 { 1 }\n");
        fixture(&root, "alpha");
        let out = cargo_output(&root, &temp.path().join("shared"), "build");
        assert!(!out.status.success(), "a checkout path supplies Cargo directive separators: {}\n{}", String::from_utf8_lossy(&out.stderr), String::from_utf8_lossy(&out.stdout));
        assert!(String::from_utf8_lossy(&out.stderr).contains(
            "the source-binding path contains a Cargo directive separator"
        ), "the failure belongs to the source-binding owner");
    }
}
