//! Milestone 3 — the run path.
//!
//! Reach: a run crashes mid-step and resumes from its journal without re-issuing a
//! recorded effect.

use contextful_acceptance::{bin, GitRepo};
use parquet::file::reader::{FileReader, SerializedFileReader};
use parquet::record::Field;
use std::process::Output;

const STORE: &str = ".contextful/context/research";

/// A vendor behind a paged API. Each call is an effect: it appends the page it served
/// and the idempotency key it was sent to `effects.log`. Armed, it kills the process
/// that called it after serving page `p2`, before that page can be recorded.
const VENDOR: &str = r#"
printf 'page=%s key=%s\n' "${CONTEXTFUL_CURSOR:-start}" "$CONTEXTFUL_IDEMPOTENCY_KEY" >> effects.log
case "$CONTEXTFUL_CURSOR" in
  '')     printf '{"rows":[{"document_id":"d1","title":"draft"},{"document_id":"d2","title":"memo"}],"cursor":"p2","more":true}' ;;
  '"p2"') printf '{"rows":[{"document_id":"d3","title":"brief"}],"cursor":"p3","more":true}'
          if [ -f crash-armed ]; then rm crash-armed; kill -9 "$PPID"; fi ;;
  '"p3"') printf '{"rows":[{"document_id":"d4","title":"order"}],"cursor":"p4","more":false}' ;;
  *)      printf '{"rows":[],"cursor":"p4","more":false}' ;;
esac
"#;

const PLAN: &str = r#"
pipeline = "filings-feed"
table = "filings"

[connector]
id = "vendor"
version = "1.0.0"
world = "native"
command = ["sh", "vendor.sh"]

[cursor]
kind = "opaque-token"
"#;

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn effects(p: &GitRepo) -> Vec<(String, String)> {
    let text = std::fs::read_to_string(p.root.join("effects.log")).unwrap_or_default();
    text.lines()
        .map(|l| {
            let (page, key) = l.split_once(' ').unwrap();
            (page.trim_start_matches("page=").to_string(), key.trim_start_matches("key=").to_string())
        })
        .collect()
}

fn calls(log: &[(String, String)], page: &str) -> Vec<String> {
    log.iter().filter(|(p, _)| p == page).map(|(_, k)| k.clone()).collect()
}

fn show(p: &GitRepo, cf: &std::path::Path, run: &str) -> serde_json::Value {
    serde_json::from_str(&ok(&p.run(cf, &["run", "show", run, "--project", "research"]))).unwrap()
}

/// `(document_id, title, _batch_seq, _run_id)` of every row in one Parquet part.
fn rows(path: &std::path::Path) -> Vec<(String, String, i32, String)> {
    let reader = SerializedFileReader::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut out = Vec::new();
    for row in reader.get_row_iter(None).unwrap() {
        let row = row.unwrap();
        let (mut id, mut title, mut seq, mut run) = (String::new(), String::new(), -1, String::new());
        for (name, field) in row.get_column_iter() {
            match (name.as_str(), field) {
                ("document_id", Field::Str(s)) => id = s.clone(),
                ("title", Field::Str(s)) => title = s.clone(),
                ("_batch_seq", Field::Int(n)) => seq = *n,
                ("_run_id", Field::Str(s)) => run = s.clone(),
                _ => {}
            }
        }
        out.push((id, title, seq, run));
    }
    out
}

#[test]
fn m03_run_path() {
    let cf = bin("contextful");
    let p = GitRepo::init();
    p.write(&format!("{STORE}/config.toml"), "[node]\nid = \"ingest-a\"\n");
    p.write("contextful.toml", "[[pipeline.tables]]\nname = \"filings\"\nprimary_key = [\"document_id\"]\n");
    p.write("vendor.sh", VENDOR);
    p.write("plan.toml", PLAN);
    p.write("crash-armed", "");

    let start = |run: &str, now: &str| {
        p.run(&cf, &["run", "start", "--plan", "plan.toml", "--project", "research", "--run-id", run, "--site-id", "site-a", "--now", now])
    };

    // The run crashes mid-step: the vendor served page p2 and the process died before recording it.
    let crashed = start("run-a", "2030-01-01T00:00:00Z");
    assert!(!crashed.status.success(), "the armed vendor kills the run: {}", String::from_utf8_lossy(&crashed.stdout));
    let log = effects(&p);
    assert_eq!(log.iter().map(|(page, _)| page.as_str()).collect::<Vec<_>>(), ["start", "\"p2\""]);
    assert!(!p.root.join(STORE).join("tables/filings/data/runs/run-a").join("ingest-a/_manifest.json").exists());
    assert_eq!(show(&p, &cf, "run-a")["status"], "running", "the crashed run's row still reads running");

    // A later fire, past the dead owner's lease, resumes from the journal.
    let resumed = ok(&start("run-b", "2030-01-01T00:01:00Z"));
    assert!(resumed.contains("success"), "{resumed}");
    let log = effects(&p);
    // The recorded page is replayed, never re-issued.
    assert_eq!(calls(&log, "start").len(), 1, "{log:?}");
    // The unrecorded page is re-entered under the identical idempotency key.
    let p2 = calls(&log, "\"p2\"");
    assert_eq!(p2.len(), 2, "{log:?}");
    assert_eq!(p2[0], p2[1], "re-entry carries the same idempotency key");
    assert_eq!(calls(&log, "\"p3\"").len(), 1, "{log:?}");

    // The orphan is reaped as a partial failure; the resumed run commits every batch once.
    assert_eq!(show(&p, &cf, "run-a")["status"], "partial_failure");
    let b = show(&p, &cf, "run-b");
    assert_eq!(b["status"], "success");
    assert_eq!(b["rows"], 4);

    // One commit: a part per batch, ordinals in pull order, each row landed once.
    let listed = ok(&p.run(&cf, &["context", "files", "filings", "--project", "research"]));
    let parts: Vec<&str> = listed.lines().collect();
    assert_eq!(parts.len(), 3, "{listed}");
    assert!(parts.iter().all(|f| f.starts_with("tables/filings/data/runs/run-b/ingest-a/")), "{listed}");
    let mut landed: Vec<_> = parts.iter().flat_map(|f| rows(&p.root.join(STORE).join(f))).collect();
    landed.sort();
    let expect = |id: &str, title: &str, seq: i32| (id.to_string(), title.to_string(), seq, "run-b".to_string());
    assert_eq!(landed, [expect("d1", "draft", 0), expect("d2", "memo", 0), expect("d3", "brief", 1), expect("d4", "order", 2)]);

    // The next fire starts at the committed position under a fresh execution: one empty pull, no rows.
    let next = ok(&start("run-c", "2030-01-01T00:02:00Z"));
    assert!(next.contains("success"), "{next}");
    let log = effects(&p);
    assert_eq!(calls(&log, "\"p4\"").len(), 1, "{log:?}");
    assert_eq!(calls(&log, "start").len(), 1, "a committed position never rewinds: {log:?}");
    assert_eq!(show(&p, &cf, "run-c")["rows"], 0);
}
