use crate::{manifest, stderr, Repo};

#[test]
fn remote_cli_parts_run_every_test_once_and_propagate_differential_failures() {
    let r = Repo::init();
    r.write("crates/contextful-cli/Cargo.toml", &(manifest("contextful-cli", "") + "\n[features]\nextra = []\n"));
    r.write("crates/contextful-cli/src/lib.rs", "");
    r.write("crates/contextful-cli/tests/integration/main.rs", "#[test] fn ordinary() { std::fs::write(\"ordinary.ran\", \"yes\").unwrap(); }\nmod differential { #[test] fn reference() { std::fs::write(\"reference.ran\", \"yes\").unwrap(); } }\n");
    r.lock();
    r.commit("CLI ordinary and differential suites");
    let package = r.root.join("crates/contextful-cli");
    for (ordinary, differential) in [("workspace.cli", "workspace.cli-formal"), ("features.binary-all", "features.formal-all")] {
        let out = r.gate(&["--stage", ordinary]);
        assert!(out.status.success(), "{ordinary}: {}", stderr(&out));
        assert!(package.join("ordinary.ran").exists(), "{ordinary} missed ordinary tests");
        assert!(!package.join("reference.ran").exists(), "{ordinary} also ran the differential partition");
        std::fs::remove_file(package.join("ordinary.ran")).unwrap();
        let out = r.gate(&["--stage", differential]);
        assert!(out.status.success(), "{differential}: {}", stderr(&out));
        assert!(package.join("reference.ran").exists(), "{differential} missed reference tests");
        assert!(!package.join("ordinary.ran").exists(), "{differential} repeated ordinary tests");
        std::fs::remove_file(package.join("reference.ran")).unwrap();
    }
    r.write("crates/contextful-cli/tests/integration/main.rs", "mod differential { #[test] fn reference() { panic!(\"reference regression\"); } }\n");
    r.commit("a failing differential case");
    for part in ["workspace.cli-formal", "features.formal-all"] {
        let out = r.gate(&["--stage", part]);
        assert!(!out.status.success(), "{part} hid a differential failure");
        assert!(String::from_utf8_lossy(&out.stdout).contains("reference regression"), "{part}: {}", stderr(&out));
    }
}
