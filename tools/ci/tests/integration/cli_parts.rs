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

/// Each `pipeline` test marks one file per run, so a part's runs show as the markers it adds.
const GROUPED: &str = r#"fn mark(name: &str) {
    use std::io::Write;
    std::fs::OpenOptions::new().create(true).append(true).open(format!("{name}.ran")).unwrap().write_all(b"x").unwrap();
}
mod pipeline { #[test] fn case() { super::mark("pipeline") } }
mod other { #[test] fn case() { super::mark("other") } }
mod differential { #[test] fn case() { super::mark("differential") } }
"#;

/// The markers `part` adds, each with the number of runs it recorded, the markers removed.
fn ran(r: &crate::Repo, part: &str) -> Vec<(String, usize)> {
    let out = r.gate(&["--stage", part]);
    assert!(out.status.success(), "{part}: {}", stderr(&out));
    let package = r.root.join("crates/contextful-cli");
    let mut marks: Vec<(String, usize)> = std::fs::read_dir(&package)
        .unwrap()
        .filter_map(|e| {
            let path = e.unwrap().path();
            let name = path.file_name()?.to_str()?.strip_suffix(".ran")?.to_string();
            let runs = std::fs::read_to_string(&path).unwrap().len();
            std::fs::remove_file(&path).unwrap();
            Some((name, runs))
        })
        .collect();
    marks.sort();
    marks
}

// spec: assurance.gate.cli-groups@cb30b536
#[test]
fn a_data_plane_build_runs_each_cli_group_in_its_own_part_and_every_test_once() {
    let r = Repo::init();
    let features = "\n[features]\ndata-plane = []\n\n[package.metadata.contextful]\nfeature-runs = [\"data-plane\"]\n";
    r.write("crates/contextful-cli/Cargo.toml", &(manifest("contextful-cli", "") + features));
    r.write("crates/contextful-cli/src/lib.rs", "");
    r.write("crates/contextful-cli/tests/integration/main.rs", GROUPED);
    r.lock();
    r.commit("a CLI suite with a grouped module");
    let listed = r.run_ci(&["stages", "--parts"]);
    assert!(listed.status.success(), "{}", stderr(&listed));
    let parts: Vec<String> = String::from_utf8_lossy(&listed.stdout).lines().map(str::to_string).collect();
    assert!(parts.iter().any(|p| p == "features.binary-none"), "no binary-none part: {parts:?}");
    assert!(!parts.iter().any(|p| p.starts_with("features.binary-none-")), "a build without the data plane splits its CLI suite: {parts:?}");
    let one = |name: &str| vec![(name.to_string(), 1)];
    for (ordinary, formal) in [("workspace.cli", "workspace.cli-formal"), ("features.binary-data-plane", "features.formal-data-plane")] {
        let groups: Vec<&String> = parts.iter().filter(|p| p.strip_prefix(ordinary).is_some_and(|s| s.starts_with('-') && *p != formal)).collect();
        assert!(groups.iter().any(|p| p.ends_with("-pipeline")), "{ordinary} has no pipeline group: {parts:?}");
        assert_eq!(ran(&r, ordinary), one("other"), "{ordinary} ran a grouped or differential test");
        assert_eq!(ran(&r, formal), one("differential"), "{formal}");
        for group in groups {
            let expected = if group.ends_with("-pipeline") { one("pipeline") } else { Vec::new() };
            assert_eq!(ran(&r, group), expected, "{group}");
        }
    }
}
