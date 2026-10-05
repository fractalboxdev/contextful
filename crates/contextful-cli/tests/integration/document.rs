//! The `file` document source through the built binary, over a folder in the project.

use crate::pipeline::{cf, ok, project, stderr};
use serde_json::{json, Value};
use std::path::Path;

fn fire(dir: &Path, run: &str, now: &str) -> std::process::Output {
    cf(dir, &["pipeline", "run", "handbook", "--project", "research", "--run-id", run, "--site-id", "site-a", "--now", now])
}

fn query(dir: &Path, sql: &str) -> Value {
    serde_json::from_str(&ok(&cf(dir, &["query", "--json", "--project", "research", sql]))).unwrap()
}

fn handbook(extra: &str) -> tempfile::TempDir {
    project(&format!(
        "[[pipeline]]\nid = \"handbook\"\n{extra}tables = [{{ name = \"documents\", primary_key = [\"slug\", \"ordinal\"] }}]\n[pipeline.source]\nname = \"file\"\nconfig = {{ root = \"handbook\", exclude = [\"drafts/**\"], base_url = \"https://handbook.example.org\" }}\n"
    ))
}

fn write(dir: &Path, path: &str, body: impl AsRef<[u8]>) {
    let p = dir.join("handbook").join(path);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

/// A fire lands the folder's notes keyed by slug and ordinal, records what it declined by extension on the run
/// record, and a second fire lands the changed note alone.
#[test]
fn a_document_folder_lands_by_slug_and_a_second_fire_lands_what_changed() {
    let dir = handbook("");
    write(dir.path(), "Onboarding/First Week.md", "---\nowner: People Ops\n---\n# First week\n\nMeet the team.\n");
    write(dir.path(), "policies/expenses.txt", "Keep receipts.");
    write(dir.path(), "drafts/wip.md", "unfinished");
    write(dir.path(), "logo.png", "png");
    let out = ok(&fire(dir.path(), "run-1", "2031-03-01T00:00:00Z"));
    assert!(out.contains("run-1 success · 2 rows") && out.contains("· 1 skipped (1 .png)"), "{out}");
    let history = ok(&cf(dir.path(), &["run", "history", "--project", "research", "--export"]));
    let runs: Vec<Value> = history.lines().skip(1).map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(runs.iter().find(|r| r["run_id"] == "run-1").map(|r| (r["skipped"].clone(), r["declined"].clone())), Some((json!(1), json!({"png": 1}))), "{history}");

    let rows = query(dir.path(), "SELECT slug, ordinal, title, owner, url FROM handbook_documents ORDER BY slug");
    assert_eq!(
        rows["rows"],
        json!([
            ["onboarding/first-week", "1", "First week", "People Ops", "https://handbook.example.org/onboarding/first-week"],
            ["policies/expenses", "1", "expenses", null, "https://handbook.example.org/policies/expenses"]
        ])
    );

    write(dir.path(), "policies/expenses.txt", "Keep every receipt.");
    let out = ok(&fire(dir.path(), "run-2", "2031-03-02T00:00:00Z"));
    assert!(out.contains("run-2 success · 1 rows"), "{out}");
    let text = query(dir.path(), "SELECT text FROM handbook_documents WHERE slug = 'policies/expenses'");
    assert_eq!(text["rows"], json!([["Keep every receipt."]]));
}

#[test]
fn a_complete_empty_file_walk_replaces_seeded_rows() {
    let dir = project("[[pipeline]]\nid = \"handbook\"\ntables = [{ name = \"documents\", primary_key = [\"slug\", \"ordinal\"], write_mode = \"replace\" }]\n[pipeline.source]\nname = \"file\"\nconfig = { root = \"handbook\" }\n");
    std::fs::create_dir_all(dir.path().join("handbook")).unwrap();
    std::fs::write(dir.path().join("seed.toml"), "pipeline = \"seed\"\ntable = \"handbook_documents\"\n[cursor]\nkind = \"snapshot-id\"\n[connector]\nid = \"fixture\"\nversion = \"1\"\ncommand = [\"sh\", \"source.sh\"]\n").unwrap();
    std::fs::write(dir.path().join("source.sh"), "cat payload.json\n").unwrap();
    std::fs::write(dir.path().join("payload.json"), json!({ "rows": [{ "slug": "held", "ordinal": 1 }], "cursor": "v1", "more": false }).to_string()).unwrap();
    ok(&cf(dir.path(), &["run", "start", "--plan", "seed.toml", "--project", "research", "--run-id", "filled", "--site-id", "site-a", "--now", "2031-03-01T00:00:00Z"]));
    assert_eq!(query(dir.path(), "SELECT slug FROM handbook_documents")["rows"], json!([["held"]]));

    ok(&fire(dir.path(), "empty", "2031-03-01T00:02:00Z"));
    assert_eq!(query(dir.path(), "SELECT slug FROM handbook_documents")["rows"], json!([]));
    let marker: Value = serde_json::from_slice(&std::fs::read(dir.path().join(".contextful/context/research/tables/handbook_documents/data/runs/empty/ingest-a/_manifest.json")).unwrap()).unwrap();
    assert_eq!(marker["replace_frontier"], json!(true));
}

/// `incremental` beside the `file` source refuses as {{connector.package.component-position}} at validation.
// spec: connector.source.file-position-owned@0e097207
#[test]
fn an_incremental_field_beside_the_file_source_refuses_at_validation() {
    ok(&cf(handbook("").path(), &["pipeline", "validate"]));
    let out = cf(handbook("incremental = \"modified\"\n").path(), &["pipeline", "validate"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("ConnectorPositionOwned") && stderr(&out).contains("`file` source"), "{}", stderr(&out));
    let tables = project("[[pipeline]]\nid = \"handbook\"\ntables = [\"pages\"]\n[pipeline.source]\nname = \"file\"\nconfig = { root = \"handbook\" }\n");
    let out = cf(tables.path(), &["pipeline", "validate"]);
    assert!(stderr(&out).contains("ConnectorTableUnmatched"), "{}", stderr(&out));
}

/// A PDF lands one row per page through the binary's decode worker.
#[cfg(feature = "pdf")]
#[test]
fn a_pdf_lands_its_pages_through_the_decode_worker() {
    let dir = handbook("");
    let report = Path::new(env!("CARGO_MANIFEST_DIR")).join("../contextful-connectors/tests/fixtures/drive/report.pdf");
    write(dir.path(), "board/report.pdf", std::fs::read(report).unwrap());
    ok(&fire(dir.path(), "run-1", "2031-03-01T00:00:00Z"));
    let rows = query(dir.path(), "SELECT slug, ordinal, text, url FROM handbook_documents");
    assert_eq!(rows["rows"], json!([["board/report", "1", "Board report", "https://handbook.example.org/board/report#page=1"]]));
}

/// A build without the PDF decoder refuses a PDF with the feature to rebuild with.
#[cfg(not(feature = "pdf"))]
#[test]
fn a_pdf_in_a_build_without_the_decoder_names_the_feature() {
    let dir = handbook("");
    write(dir.path(), "board/report.pdf", "%PDF-1.4");
    let out = fire(dir.path(), "run-1", "2031-03-01T00:00:00Z");
    assert!(!out.status.success());
    assert!(stderr(&out).contains("board/report.pdf") && stderr(&out).contains("--features pdf"), "{}", stderr(&out));
}
