//! `contextful run` through the built binary: history, and the refusals each surface raises.

use std::path::Path;
use std::process::{Command, Output};

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join(".contextful/context/research");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("config.toml"), "[node]\nid = \"ingest-a\"\n").unwrap();
    std::fs::write(dir.path().join("contextful.toml"), "[[pipeline.tables]]\nname = \"filings\"\n").unwrap();
    std::fs::write(dir.path().join("empty.sh"), "printf '{\"rows\":[],\"more\":false}'\n").unwrap();
    for p in ["feed-a", "feed-b"] {
        std::fs::write(
            dir.path().join(format!("{p}.toml")),
            format!("pipeline = \"{p}\"\ntable = \"filings\"\n[connector]\nid = \"vendor\"\nversion = \"1\"\ncommand = [\"sh\", \"empty.sh\"]\n"),
        )
        .unwrap();
    }
    dir
}

fn cf(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful")).args(args).current_dir(dir).env_remove("CONTEXTFUL_NODE_ID").output().unwrap()
}

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn refused(out: &Output, error: &str) {
    assert!(!out.status.success(), "expected {error}, got success: {}", String::from_utf8_lossy(&out.stdout));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains(error), "expected {error}, got: {stderr}");
}

fn start(dir: &Path, plan: &str, run: &str, now: &str) -> Output {
    cf(dir, &["run", "start", "--plan", plan, "--project", "research", "--run-id", run, "--site-id", "site-a", "--now", now])
}

fn history(dir: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["run", "history", "--project", "research"];
    args.extend_from_slice(extra);
    cf(dir, &args)
}

/// History leaves the machine as a process listing and an NDJSON export over one run projection; the export's
/// first line carries store, window, count and truncation flag, then one run per line. It resolves no bucket.
// spec: run.record.history-export@f66be52e
#[test]
fn history_exports_a_header_then_one_run_per_line() {
    let dir = project();
    for (i, run) in ["a1", "a2", "a3"].iter().enumerate() {
        ok(&start(dir.path(), "feed-a.toml", run, &format!("2030-01-01T00:0{i}:00Z")));
    }
    let listing: serde_json::Value = serde_json::from_str(&ok(&history(dir.path(), &[]))).unwrap();
    assert_eq!(listing["runs"].as_array().unwrap().len(), 3);
    let text = ok(&history(dir.path(), &["--export", "--since", "2030-01-01T00:01:00Z"]));
    let lines: Vec<serde_json::Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines[0]["store"], "research");
    assert_eq!(lines[0]["window"]["since"], "2030-01-01T00:01:00Z");
    assert_eq!(lines[0]["count"], 2);
    assert_eq!(lines[0]["truncated"], false);
    assert_eq!(lines[1..].iter().map(|r| r["run_id"].as_str().unwrap()).collect::<Vec<_>>(), ["a3", "a2"]);
    // Each run line is the run row the listing serves.
    assert_eq!(lines[1], listing["runs"][0]);
}

/// The export answers with 500 rows by default and clamps at 5000 rows, each pipeline's window taking the full
/// ceiling before the merged result is clipped once.
// spec: run.record.export-ceiling@81c87e7e
#[test]
fn each_pipeline_takes_the_full_ceiling_and_the_merge_clips_once() {
    use contextful_core::run::record::export_ceiling;
    assert_eq!((export_ceiling(None), export_ceiling(Some(9_999))), (500, 5_000));
    let dir = project();
    for (i, run) in ["a1", "a2", "a3"].iter().enumerate() {
        ok(&start(dir.path(), "feed-a.toml", run, &format!("2030-01-01T00:0{i}:00Z")));
    }
    ok(&start(dir.path(), "feed-b.toml", "b1", "2030-01-01T00:05:00Z"));
    let text = ok(&history(dir.path(), &["--export", "--limit", "2", "--pipeline", "feed-a", "--pipeline", "feed-b"]));
    let lines: Vec<serde_json::Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!((lines[0]["count"].as_u64(), lines[0]["truncated"].as_bool()), (Some(2), Some(true)));
    // feed-a contributes its two newest, feed-b its one; the merge keeps the two newest overall.
    assert_eq!(lines[1..].iter().map(|r| r["run_id"].as_str().unwrap()).collect::<Vec<_>>(), ["b1", "a3"]);
    let unasked = ok(&history(dir.path(), &["--export"]));
    assert_eq!(unasked.lines().count(), 1 + 4);
}

#[test]
fn every_run_surface_raises_its_refusal_by_name() {
    let dir = project();
    refused(&history(dir.path(), &["--since", "2030-01-01T00:00:00+08:00"]), "HistoryBoundSpelling");
    refused(&cf(dir.path(), &["run", "start", "--plan", "feed-a.toml", "--project", "research"]), "SiteIdUnresolved");
    refused(
        &cf(dir.path(), &["run", "start", "--plan", "feed-a.toml", "--project", "research", "--site-id-env", "CONTEXTFUL_TEST_UNSET_SITE"]),
        "SiteIdUnresolved",
    );
    ok(&start(dir.path(), "feed-a.toml", "a1", "2030-01-01T00:00:00Z"));
    refused(&cf(dir.path(), &["run", "cancel", "a1", "--project", "research"]), "CancelTargetNotInFlight");
    refused(&cf(dir.path(), &["run", "cancel", "nope", "--project", "research"]), "CancelTargetNotInFlight");
    std::fs::write(dir.path().join("payload.json"), "{}").unwrap();
    refused(&cf(dir.path(), &["run", "awake", "0123abcd", "--project", "research", "--payload", "payload.json"]), "AwakeableUnknown");
    std::fs::write(dir.path().join("redacting.toml"), format!("redact = [\"ssn\"]\n{}", std::fs::read_to_string(dir.path().join("feed-a.toml")).unwrap())).unwrap();
    refused(&start(dir.path(), "redacting.toml", "r1", "2030-01-01T00:00:00Z"), "JournalRedactionConflict");
}

#[test]
fn an_unknown_stop_scope_is_refused() {
    let dir = project();
    ok(&start(dir.path(), "feed-a.toml", "a1", "2030-01-01T00:00:00Z"));
    let out = cf(dir.path(), &["run", "cancel", "a1", "--project", "research", "--scope", "fire"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("fire") && stderr.contains("pipeline"), "{stderr}");
}

/// The binary keeps its run rows, cursor cache and lease rows in the store root's
/// `machine.sqlite`, and every run surface reads them back from there.
#[test]
fn run_rows_live_in_the_store_roots_machine_catalog() {
    use contextful_core::coordinate::Catalog;
    use contextful_core::ports::FixedClock;
    use contextful_core::time::Instant;
    let dir = project();
    ok(&start(dir.path(), "feed-a.toml", "a1", "2030-01-01T00:00:00Z"));
    let file = dir.path().join(".contextful/context/research/machine.sqlite");
    assert!(file.is_file(), "no machine catalog at {}", file.display());
    assert!(!dir.path().join(".contextful/run/research/runs").exists(), "no run row lands beside the journal");

    let clock = std::sync::Arc::new(FixedClock(Instant::parse("2030-01-01T00:00:00Z").unwrap()));
    let catalog = contextful_sqlite::MachineCatalog::open(&file, clock).unwrap();
    let row = catalog.run("a1").unwrap().expect("the run row");
    assert_eq!((row.pipeline_id.as_str(), row.table.as_str()), ("feed-a", "filings"));
    let shown: serde_json::Value = serde_json::from_str(&ok(&cf(dir.path(), &["run", "show", "a1", "--project", "research"]))).unwrap();
    assert_eq!(shown["run_id"], "a1");
    assert_eq!(shown["status"], serde_json::to_value(row.status).unwrap());
}

/// A pull's `types` object maps a column to a type spelled as {{store.reconcile.typed-landing}} reads it, and the
/// run commit lands that column in it; an unreadable spelling fails the pull as {{store.reconcile.incompatible}}.
// spec: run.land.typed-pull@cb800296
#[test]
fn a_pulled_type_lands_its_column_in_that_type() {
    let dir = project();
    let schema = |d: &Path| -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(d.join(".contextful/context/research/tables/filings/schema.json")).unwrap()).unwrap()
    };
    let field = |s: &serde_json::Value, name: &str| s["fields"].as_array().unwrap().iter().find(|f| f["name"] == name).cloned().unwrap_or_else(|| panic!("no `{name}` in {s}"));
    std::fs::write(
        dir.path().join("typed.sh"),
        "printf '{\"rows\":[{\"id\":\"a\",\"digest\":\"3q2+7w==\",\"embedding\":[0.5,-1,0.25]}],\"types\":{\"digest\":\"binary(4)\",\"embedding\":\"float16[3]\"},\"more\":false}'\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("typed.toml"), "pipeline = \"typed\"\ntable = \"filings\"\n[connector]\nid = \"vendor\"\nversion = \"1\"\ncommand = [\"sh\", \"typed.sh\"]\n").unwrap();
    ok(&start(dir.path(), "typed.toml", "t1", "2030-01-01T00:00:00Z"));
    let s = schema(dir.path());
    assert_eq!(field(&s, "digest")["type"], serde_json::json!({"name": "fixedsizebinary", "byteWidth": 4}), "{s}");
    assert_eq!(field(&s, "embedding")["type"], serde_json::json!({"name": "fixedsizelist", "listSize": 3}), "{s}");
    assert_eq!(field(&s, "embedding")["children"][0]["type"]["precision"], "HALF", "{s}");

    // A later pull declaring nothing lands in the stored types.
    std::fs::write(dir.path().join("untyped.sh"), "printf '{\"rows\":[{\"id\":\"b\",\"digest\":\"AAECAw==\",\"embedding\":[1,0,0]}],\"more\":false}'\n").unwrap();
    std::fs::write(dir.path().join("untyped.toml"), "pipeline = \"untyped\"\ntable = \"filings\"\n[connector]\nid = \"vendor\"\nversion = \"1\"\ncommand = [\"sh\", \"untyped.sh\"]\n").unwrap();
    ok(&start(dir.path(), "untyped.toml", "u1", "2030-01-01T00:01:00Z"));
    assert_eq!(schema(dir.path()), s);

    // An unreadable spelling fails the pull.
    std::fs::write(dir.path().join("bad.sh"), "printf '{\"rows\":[{\"id\":\"c\",\"v\":\"qg==\"}],\"types\":{\"v\":\"bytes(4)\"},\"more\":false}'\n").unwrap();
    std::fs::write(dir.path().join("bad.toml"), "pipeline = \"bad\"\ntable = \"filings\"\n[connector]\nid = \"vendor\"\nversion = \"1\"\ncommand = [\"sh\", \"bad.sh\"]\n").unwrap();
    refused(&start(dir.path(), "bad.toml", "b1", "2030-01-01T00:02:00Z"), "StoreSchemaIncompatible");
    assert_eq!(schema(dir.path()), s, "the failed pull lands nothing");
}

const AWS_KEY: &str = "AKIAIOSFODNN7EXAMPLE";
const GITHUB_TOKEN: &str = "ghp_0123456789abcdefghijABCDEFGHIJ012345";
const MARKER: &str = "[REDACTED:secret]";

/// The count of files under `dir` whose bytes hold `needle`.
fn files_holding(dir: &Path, needle: &str) -> usize {
    let mut n = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            if e.path().is_dir() {
                stack.push(e.path());
            } else if String::from_utf8_lossy(&std::fs::read(e.path()).unwrap()).contains(needle) {
                n += 1;
            }
        }
    }
    n
}

/// A journaled pull records the batch as the source handed it over, after the secret guard and ahead of the
/// land path.
// spec: run.journal.recorded-batch@ac3aadba
#[test]
fn run_start_journals_and_lands_only_masked_credentials() {
    let dir = project();
    // The first pull serves two credentials; the second fails until `ready` exists.
    let script = format!(
        "if [ \"$CONTEXTFUL_STEP\" = pull-0 ]; then printf '{{\"rows\":[{{\"id\":\"d1\",\"aws\":\"key {AWS_KEY}\",\"gh\":\"{GITHUB_TOKEN}\"}}],\"more\":true}}'\n\
         elif [ -f ready ]; then printf '{{\"rows\":[],\"more\":false}}'\n\
         else printf '{{\"error\":{{\"tag\":\"Permanent\",\"message\":\"feed down\"}}}}'; exit 1; fi\n"
    );
    std::fs::write(dir.path().join("leaky.sh"), script).unwrap();
    std::fs::write(
        dir.path().join("leaky.toml"),
        "pipeline = \"leaky\"\ntable = \"filings\"\n[connector]\nid = \"vendor\"\nversion = \"1\"\ncommand = [\"sh\", \"leaky.sh\"]\n",
    )
    .unwrap();

    let first = start(dir.path(), "leaky.toml", "l1", "2030-01-01T00:00:00Z");
    assert!(!first.status.success(), "the second pull fails the first attempt");
    let journal = dir.path().join(".contextful/run/research");
    assert!(files_holding(&journal, MARKER) >= 1, "the journal records the masked pull");
    for secret in [AWS_KEY, GITHUB_TOKEN] {
        assert_eq!(files_holding(&journal, secret), 0, "the journal holds `{secret}`");
    }

    std::fs::write(dir.path().join("ready"), "").unwrap();
    ok(&start(dir.path(), "leaky.toml", "l2", "2030-01-01T00:01:00Z"));
    let store = contextful_context::Store::open(dir.path(), "research").unwrap();
    let decl = contextful_core::store::declare::TableDecl::named("filings");
    let rows = contextful_context::rows::table_rows(&store, &decl, &["aws", "gh"]).unwrap();
    assert_eq!(rows.len(), 1, "the replay lands the recorded pull");
    assert_eq!(rows[0]["aws"], format!("key {MARKER}"));
    assert_eq!(rows[0]["gh"], MARKER);
    for secret in [AWS_KEY, GITHUB_TOKEN] {
        assert_eq!(files_holding(dir.path(), secret), 1, "only the connector script holds `{secret}`");
    }
}
