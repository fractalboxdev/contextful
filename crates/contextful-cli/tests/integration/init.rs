//! `contextful init` and project discovery through the built binary.

use std::path::Path;
use std::process::{Command, Output};

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(args)
        .current_dir(dir)
        .env("CONTEXTFUL_NODE_ID", "ingest-a")
        .output()
        .unwrap()
}

fn stdout(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn refused(out: &Output, error: &str) -> String {
    assert!(!out.status.success(), "expected {error}, got success: {}", String::from_utf8_lossy(&out.stdout));
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(stderr.contains(error), "expected {error}, got: {stderr}");
    stderr
}

/// `contextful init <name>` writes `contextful.toml` declaring the project and creates its store root; a repeat rewrites nothing.
#[test]
fn init_writes_the_declaration_and_a_repeat_is_a_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let first = stdout(&run(dir.path(), &["init", "research"]));
    assert!(first.contains("research"), "{first}");
    let written = std::fs::read_to_string(dir.path().join("contextful.toml")).unwrap();
    assert!(written.contains("[project]") && written.contains("name = \"research\""), "{written}");
    assert!(dir.path().join(".contextful/context/research").is_dir());

    let again = stdout(&run(dir.path(), &["init", "research"]));
    assert!(again.contains("unchanged"), "{again}");
    assert_eq!(std::fs::read_to_string(dir.path().join("contextful.toml")).unwrap(), written);

    refused(&run(dir.path(), &["init", "archive"]), "StoreProjectConflict");
    refused(&run(dir.path(), &["init", "../escape"]), "StoreProjectNameInvalid");
    assert_eq!(std::fs::read_to_string(dir.path().join("contextful.toml")).unwrap(), written);
}

/// A command given no `--project` reads the nearest `contextful.toml` upward and bases its store root and declaration on that file's directory.
// spec: store.init.default-declaration@1abc65ae
#[test]
fn a_command_without_project_discovers_it_from_a_subdirectory() {
    let dir = tempfile::tempdir().unwrap();
    stdout(&run(dir.path(), &["init", "research"]));
    let path = dir.path().join("contextful.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, format!("{text}\n[[pipeline.tables]]\nname = \"filings\"\nprimary_key = [\"doc\"]\n")).unwrap();
    let notes = dir.path().join("notes/inbox");
    std::fs::create_dir_all(&notes).unwrap();
    std::fs::write(notes.join("rows.jsonl"), "{\"doc\":\"a\",\"v\":1}\n{\"doc\":\"a\",\"v\":2}\n").unwrap();

    let landed = stdout(&run(
        &notes,
        &["context", "land", "filings", "--rows", "rows.jsonl", "--run-id", "run-1", "--site-id", "s", "--now", "2030-01-01T00:00:00Z"],
    ));
    assert!(landed.starts_with("filings: committed run-1"), "{landed}");
    assert!(dir.path().join(".contextful/context/research/tables/filings/data/runs/run-1/ingest-a/_manifest.json").is_file());
    assert!(!notes.join(".contextful").exists());

    // The discovered declaration keys the table, so the scan dedupes on `doc`.
    let scan = stdout(&run(&notes, &["context", "scan", "filings"]));
    assert!(scan.contains("PARTITION BY"), "{scan}");
    let files = stdout(&run(&notes, &["context", "files", "filings"]));
    assert_eq!(files, "tables/filings/data/runs/run-1/ingest-a/part-00000.parquet");
    let rebuilt = stdout(&run(&notes, &["context", "rebuild-catalog"]));
    assert!(rebuilt.contains("filings"), "{rebuilt}");
    assert!(dir.path().join(".contextful/context/research/derived.sqlite").is_file());
    stdout(&run(&notes, &["run", "history"]));
    assert!(dir.path().join(".contextful/context/research/machine.sqlite").is_file());
}

/// A command given `--project` bases every project path on the working directory and runs no discovery.
// spec: store.init.explicit-project@4f59d11a
#[test]
fn an_explicit_project_uses_the_working_directory() {
    let dir = tempfile::tempdir().unwrap();
    stdout(&run(dir.path(), &["init", "research"]));
    let sub = dir.path().join("elsewhere");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("contextful.toml"), "[[pipeline.tables]]\nname = \"events\"\n").unwrap();
    std::fs::write(sub.join("rows.jsonl"), "{\"e\":1}\n").unwrap();
    stdout(&run(
        &sub,
        &["context", "land", "events", "--project", "local", "--rows", "rows.jsonl", "--run-id", "run-1", "--site-id", "s", "--now", "2030-01-01T00:00:00Z"],
    ));
    assert!(sub.join(".contextful/context/local/tables/events/schema.json").is_file());
    assert!(!dir.path().join(".contextful/context/local").exists());
}

/// A command given no `--project` with no named `contextful.toml` from its working directory up raises `StoreProjectUndiscovered`.
#[test]
fn a_command_without_project_or_declaration_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let err = refused(&run(dir.path(), &["context", "files", "filings"]), "StoreProjectUndiscovered");
    assert!(err.contains("contextful init"), "{err}");
    std::fs::write(dir.path().join("contextful.toml"), "[[pipeline.tables]]\nname = \"filings\"\n").unwrap();
    refused(&run(dir.path(), &["run", "history"]), "StoreProjectUndiscovered");
    assert!(!dir.path().join(".contextful").exists());
}

/// A derive pipeline fired from a subdirectory resolves the discovered declaration's `media_root` against the project directory.
// spec: store.init.declaration-base@ee048fb7
#[test]
fn a_derive_pipeline_resolves_declared_paths_against_the_project_directory() {
    let dir = tempfile::tempdir().unwrap();
    stdout(&run(dir.path(), &["init", "research"]));
    let path = dir.path().join("contextful.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    let pipeline = concat!(
        "[[pipeline]]\nid = \"doc-text\"\ntables = [{ name = \"passages\", primary_key = [\"unit_ref\", \"derivation_key\", \"cue_seq\"] }]\n",
        "[pipeline.source]\nname = \"derive\"\n",
        "config = { engine = \"reader\", source_table = \"documents\", media_column = \"path\", parent_id_column = \"doc_id\" }\n\n",
        "[derive.reader]\ndriver = \"exec\"\nmedia_root = \"media\"\n\n",
        "[derive.reader.engine]\ncommand = [\"cat\", \"{input}\"]\noutput_format = \"srt\"\n",
    );
    std::fs::write(&path, format!("{text}\n{pipeline}")).unwrap();
    std::fs::create_dir_all(dir.path().join("media")).unwrap();
    std::fs::write(dir.path().join("media/memo.srt"), "1\n00:00:00,000 --> 00:00:01,000\nRevenue rose.\n\n").unwrap();
    let notes = dir.path().join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    std::fs::write(notes.join("documents.jsonl"), "{\"doc_id\":\"d1\",\"path\":\"memo.srt\"}\n").unwrap();
    stdout(&run(
        &notes,
        &["context", "land", "documents", "--rows", "documents.jsonl", "--run-id", "load-1", "--site-id", "s", "--now", "2030-01-01T00:00:00Z"],
    ));

    let fired = stdout(&run(&notes, &["pipeline", "run", "doc-text", "--run-id", "derive-1", "--site-id", "s", "--now", "2030-01-01T01:00:00Z"]));
    assert!(fired.contains("success"), "{fired}");
    let store = contextful_context::Store::open(dir.path(), "research").unwrap();
    let decl = contextful_core::store::declare::TableDecl::named("doc_text_passages");
    let rows = contextful_context::rows::table_rows(&store, &decl, &["kind", "text"]).unwrap();
    let kinds: Vec<(String, String)> = rows
        .iter()
        .map(|r| {
            let text = |c: &str| r.get(c).and_then(|v| v.as_str()).unwrap_or_default().to_string();
            (text("kind"), text("text"))
        })
        .collect();
    assert_eq!(kinds, [("passage".to_string(), "Revenue rose.".to_string())]);
}
