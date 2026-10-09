//! Where the golden set lives, what reads it, and what the harness may depend on.

use std::path::{Path, PathBuf};

use contextful_eval::case;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every file under `dir`, recursively, in path order.
fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Version-controlled JSONL under `evals/cases/` is the canonical golden set, one loader is its only ingestion door, and a reviewer's edit reaches the gate as a change to the tree.
// spec: assurance.baseline.golden-custody@b1e43238
#[test]
fn the_golden_set_is_tracked_jsonl_read_through_one_loader() {
    let cases = files(&root().join("evals/cases"));
    assert!(!cases.is_empty());
    for path in &cases {
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("jsonl"), "{}", path.display());
        let text = std::fs::read_to_string(path).unwrap();
        let loaded = case::load(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert!(!loaded.is_empty());
        // Each case's corpus sits in the tree beside the set.
        for c in &loaded {
            assert!(c.corpus_dir(path).join("contextful.toml").is_file(), "{}: `{}`", path.display(), c.corpus);
        }
    }
    // The set is what git tracks: an edit reaches the gate as a change to the tree.
    let out = std::process::Command::new("git").args(["ls-files", "--error-unmatch", "evals/cases"]).current_dir(root()).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let tracked = String::from_utf8_lossy(&out.stdout);
    for path in &cases {
        let rel = path.strip_prefix(root()).unwrap().to_string_lossy().replace('\\', "/");
        assert!(tracked.lines().any(|l| l == rel), "{rel} is untracked");
    }
    // The runner reads the set through the loader and no parser of its own.
    let runner = std::fs::read_to_string(root().join("crates/contextful-cli/src/eval.rs")).unwrap();
    assert!(runner.contains("case::load(&text)"));
    assert!(!runner.contains("from_str::<Case>") && !runner.contains("Deserialize"), "the runner parses cases itself");
}

/// Retrieval and synthesis are tuned on no gate input.
// spec: assurance.baseline.held-out@99ecdc01
#[test]
fn no_runtime_source_reads_a_gate_input() {
    let mut reached = Vec::new();
    for krate in std::fs::read_dir(root().join("crates")).unwrap() {
        let src = krate.unwrap().path().join("src");
        if !src.is_dir() {
            continue;
        }
        for file in files(&src) {
            let Ok(text) = std::fs::read_to_string(&file) else { continue };
            if text.contains("evals/") {
                reached.push(file.strip_prefix(root()).unwrap().display().to_string());
            }
        }
    }
    assert!(reached.is_empty(), "runtime source names a gate input: {reached:?}");
}

/// An evaluation run ingests, indexes, retrieves, reads, judges and scores through the surfaces a caller uses, and the harness adds no storage or runtime primitive.
// spec: assurance.evaluate.real-read-path@a8609860
#[test]
fn the_harness_depends_on_no_store_or_runtime_package() {
    let manifest: toml::Value = toml::from_str(&std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap()).unwrap();
    // The harness reaches no store, engine, query runtime or async runtime of its own.
    const PRIMITIVES: [&str; 9] =
        ["contextful-context", "contextful-engine", "contextful-sqlite", "contextful-sync", "duckdb", "rusqlite", "arrow", "parquet", "tokio"];
    for dep in manifest["dependencies"].as_table().unwrap().keys() {
        assert!(!PRIMITIVES.iter().any(|p| dep == p || dep.starts_with(&format!("{p}-"))), "the harness depends on `{dep}`");
    }
    // The runner lands, folds and reads through the store's own entry points.
    let runner = std::fs::read_to_string(root().join("crates/contextful-cli/src/eval.rs")).unwrap();
    for call in ["land(store, &decl", "fold(store, &decl", "face.retrieve(", "face.session(", "effect_boundary("] {
        assert!(runner.contains(call), "the runner bypasses `{call}`");
    }
}
