//! The table write verbs — `context land`, `run start`, `pipeline run` — under each
//! authoring posture, with and without an accompanying credential.

use std::path::Path;
use std::process::{Command, Output, Stdio};

const AUD: &str = "contextful://acme-research";
const DANA: &str = "user://dana@acme.example";

/// A project whose manifest opens with `posture` and declares `filings`, a plan landing
/// one row into it, and a pipeline whose vendor nothing answers.
fn project(posture: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    let store = p.join(".contextful/context/research");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("config.toml"), "[node]\nid = \"ingest-a\"\n").unwrap();
    std::fs::write(p.join(".contextful/issuance.toml"), format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 3600\n")).unwrap();
    std::fs::write(p.join("contextful.toml"), format!("{posture}\n[[pipeline.tables]]\nname = \"filings\"\n")).unwrap();
    std::fs::create_dir_all(p.join("pipelines")).unwrap();
    std::fs::write(
        p.join("pipelines/feed.toml"),
        "id = \"feed\"\ntables = [\"filings\"]\n[source]\nname = \"http\"\nconfig = { endpoint = \"http://127.0.0.1:9/v1\" }\n",
    )
    .unwrap();
    std::fs::write(p.join("rows.jsonl"), "{\"doc\":\"a\"}\n").unwrap();
    std::fs::write(p.join("one.sh"), "[ -f revoke.sh ] && sh revoke.sh\nprintf '{\"rows\":[{\"doc\":\"b\"}],\"more\":false}'\n").unwrap();
    std::fs::write(
        p.join("plan.toml"),
        "pipeline = \"vendor\"\ntable = \"filings\"\n[connector]\nid = \"vendor\"\nversion = \"1\"\ncommand = [\"sh\", \"one.sh\"]\n",
    )
    .unwrap();
    dir
}

fn cf(dir: &Path, args: &[&str], token: Option<&str>) -> Output {
    command(dir, args, token).output().unwrap()
}

fn command(dir: &Path, args: &[&str], token: Option<&str>) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_contextful"));
    c.args(args)
        .current_dir(dir)
        .env_remove("CONTEXTFUL_NODE_ID")
        .env_remove("CONTEXTFUL_TOKEN")
        .env_remove("CONTEXTFUL_ISSUER_PUBKEY")
        .env_remove("CONTEXTFUL_AUDIENCE")
        .env_remove("CONTEXTFUL_SECRETS_BACKEND");
    if let Some(t) = token {
        c.env("CONTEXTFUL_TOKEN", t);
    }
    c
}

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn refused(out: &Output, error: &str) -> String {
    assert!(!out.status.success(), "expected {error}, got success: {}", String::from_utf8_lossy(&out.stdout));
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(stderr.contains(error), "expected {error}, got: {stderr}");
    stderr
}

/// The issuer's public key, and a credential for Dana granting `actions` over `filings`.
fn credential(dir: &Path, actions: &[&str]) -> (String, String) {
    let public = ok(&cf(dir, &["token", "keygen", "--out", ".contextful/issuer.seed"], None));
    (public, mint(dir, actions, &["filings"], "600"))
}

/// A credential for Dana granting `actions` over `tables` for `ttl` seconds, under the issuer key already generated.
fn mint(dir: &Path, actions: &[&str], tables: &[&str], ttl: &str) -> String {
    let mut args = vec!["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", DANA, "--ttl", ttl];
    for a in actions {
        args.extend(["--action", a]);
    }
    for t in tables {
        args.extend(["--table", t]);
    }
    ok(&cf(dir, &args, None))
}

fn land<'a>(extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec!["context", "land", "filings", "--project", "research", "--rows", "rows.jsonl", "--run-id", "run-1", "--site-id", "s"];
    args.extend_from_slice(extra);
    args
}

fn start<'a>(extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec!["run", "start", "--plan", "plan.toml", "--project", "research", "--run-id", "run-2", "--site-id", "s"];
    args.extend_from_slice(extra);
    args
}

fn fire<'a>(extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec!["pipeline", "run", "feed", "--project", "research", "--run-id", "run-3", "--site-id", "s"];
    args.extend_from_slice(extra);
    args
}

/// The distinct `_authored_by` values of `filings`, or `None` where no row carries the column.
fn authors(dir: &Path) -> Option<Vec<serde_json::Value>> {
    authors_of(dir, "filings")
}

/// The distinct `_authored_by` values of `table`, or `None` where no row carries the column.
fn authors_of(dir: &Path, table: &str) -> Option<Vec<serde_json::Value>> {
    let all = format!("SELECT * FROM {table}");
    let all: serde_json::Value = serde_json::from_str(&ok(&cf(dir, &["query", "--json", "--project", "research", &all], None))).unwrap();
    if !all["columns"].as_array().unwrap().iter().any(|c| c == "_authored_by") {
        return None;
    }
    let q = format!("SELECT DISTINCT _authored_by FROM {table} ORDER BY 1");
    let r: serde_json::Value = serde_json::from_str(&ok(&cf(dir, &["query", "--json", "--project", "research", &q], None))).unwrap();
    Some(r["rows"].as_array().unwrap().iter().map(|row| row[0].clone()).collect())
}

fn landed(dir: &Path) -> bool {
    dir.join(".contextful/context/research/tables/filings/schema.json").exists()
        && std::fs::read_dir(dir.join(".contextful/context/research/tables/filings/data/runs"))
            .map(|runs| runs.flatten().any(|r| std::fs::read_dir(r.path()).unwrap().flatten().any(|n| n.path().join("_manifest.json").exists())))
            .unwrap_or(false)
}

/// Every table write verb runs under its manifest's authoring posture. `session` authors every write by one verified ambient
/// credential's principal; `per_request` holds no ambient principal, leaving an unaccompanied write unauthored.
// spec: authority.issue.authoring-posture@387b01f4
#[test]
fn per_request_leaves_an_unaccompanied_write_unauthored_and_session_authors_every_write() {
    let dir = project("authoring_posture = \"per_request\"");
    ok(&cf(dir.path(), &land(&[]), None));
    ok(&cf(dir.path(), &start(&[]), None));
    assert_eq!(authors(dir.path()), None, "an unaccompanied write carries no `_authored_by`");

    let dir = project("authoring_posture = \"session\"");
    let (public, token) = credential(dir.path(), &["write"]);
    ok(&cf(dir.path(), &land(&["--public-key", &public, "--audience", AUD]), Some(&token)));
    ok(&cf(dir.path(), &start(&["--public-key", &public, "--audience", AUD]), Some(&token)));
    assert_eq!(authors(dir.path()), Some(vec![serde_json::json!(DANA)]));
}

/// A manifest's missing or unknown `authoring_posture` refuses every table write verb before its first effect.
// spec: authority.issue.posture-key@4a2c649f
#[test]
fn an_undeclared_posture_refuses_every_write_verb() {
    for posture in ["", "authoring_posture = \"shared\""] {
        let dir = project(posture);
        refused(&cf(dir.path(), &land(&[]), None), "AuthoringPostureUndeclared");
        refused(&cf(dir.path(), &start(&[]), None), "AuthoringPostureUndeclared");
        refused(&cf(dir.path(), &fire(&[]), None), "AuthoringPostureUndeclared");
        assert!(!landed(dir.path()));
    }
}

/// Under `session`, a table write verb run with no credential refuses before its first effect.
// spec: authority.issue.session-credential@1601c0e3
#[test]
fn a_session_write_without_a_credential_is_refused() {
    let dir = project("authoring_posture = \"session\"");
    refused(&cf(dir.path(), &land(&[]), None), "AuthoringCredentialMissing");
    refused(&cf(dir.path(), &start(&[]), None), "AuthoringCredentialMissing");
    refused(&cf(dir.path(), &fire(&[]), None), "AuthoringCredentialMissing");
    assert!(!landed(dir.path()));
    assert!(!dir.path().join(".contextful/run/research").exists(), "no run state is written");
}

/// A credentialed write needs `write` over its destination table.
// spec: authority.grant.write-not-covered@cd7ea483
#[test]
fn a_credential_without_a_write_grant_lands_nothing() {
    for posture in ["session", "per_request"] {
        let dir = project(&format!("authoring_posture = \"{posture}\""));
        let (public, token) = credential(dir.path(), &["read"]);
        let pins = ["--public-key", public.as_str(), "--audience", AUD];
        let err = refused(&cf(dir.path(), &land(&pins), Some(&token)), "GrantWriteNotCovered");
        assert!(err.contains("filings"), "{err}");
        refused(&cf(dir.path(), &start(&pins), Some(&token)), "GrantWriteNotCovered");
        refused(&cf(dir.path(), &fire(&pins), Some(&token)), "GrantWriteNotCovered");
        assert!(!landed(dir.path()));
    }
}

/// An accompanying credential is admitted before the first effect, so a bad one lands nothing under `per_request`
/// either, and its `on_behalf_of` authors the rows it lands.
// spec: authority.verify.write-verbs@cede3d75
#[test]
fn a_per_request_write_accompanied_by_a_credential_is_authored_by_it() {
    let dir = project("authoring_posture = \"per_request\"");
    let (public, token) = credential(dir.path(), &["write"]);
    refused(&cf(dir.path(), &land(&["--public-key", &public, "--audience", "contextful://elsewhere"]), Some(&token)), "AudienceMismatch");
    assert!(!landed(dir.path()));
    ok(&cf(dir.path(), &land(&["--public-key", &public, "--audience", AUD]), Some(&token)));
    assert_eq!(authors(dir.path()), Some(vec![serde_json::json!(DANA)]));
}

/// A credential revoked between admission and commit commits nothing.
#[test]
fn a_revocation_after_admission_stops_the_commit() {
    let dir = project("authoring_posture = \"session\"");
    let (public, token) = credential(dir.path(), &["write"]);
    let verified: serde_json::Value =
        serde_json::from_str(&ok(&cf(dir.path(), &["token", "verify", "--token", &token, "--public-key", &public], None))).unwrap();
    let rev = verified["rev_ids"][0].as_str().unwrap().to_string();
    std::fs::write(dir.path().join("denylist.txt"), "# none yet\n").unwrap();
    let pins = ["--public-key", public.as_str(), "--audience", AUD, "--denylist", "denylist.txt"];

    // Admission passes; the connector revokes the credential before the run commits.
    std::fs::write(dir.path().join("revoke.sh"), format!("echo {rev} >> denylist.txt\n")).unwrap();
    let err = refused(&cf(dir.path(), &start(&pins), Some(&token)), "AuthorityRevoked");
    assert!(!landed(dir.path()), "{err}");

    // The same denylist now refuses at admission.
    refused(&cf(dir.path(), &land(&pins), Some(&token)), "AuthorityRevoked");
    assert!(!landed(dir.path()));
}

/// `context land` with its rows read from a FIFO: the land admits its credential before it opens the rows, so
/// `between` runs after admission and before the commit.
#[cfg(unix)]
fn land_across(dir: &Path, extra: &[&str], token: &str, between: impl FnOnce()) -> Output {
    use std::io::Write;
    let fifo = dir.join("rows.fifo");
    let _ = std::fs::remove_file(&fifo);
    assert!(Command::new("mkfifo").arg(&fifo).status().unwrap().success());
    let mut args = vec!["context", "land", "filings", "--project", "research", "--rows", "rows.fifo", "--run-id", "run-4", "--site-id", "s"];
    args.extend_from_slice(extra);
    let mut child = command(dir, &args, Some(token)).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || tx.send(std::fs::OpenOptions::new().write(true).open(fifo).unwrap()));
    let mut writer = loop {
        if let Ok(w) = rx.recv_timeout(std::time::Duration::from_millis(50)) {
            break w;
        }
        if child.try_wait().unwrap().is_some() {
            let out = child.wait_with_output().unwrap();
            panic!("the land exited before opening its rows: {}", String::from_utf8_lossy(&out.stderr));
        }
    };
    between();
    writer.write_all(b"{\"doc\":\"a\"}\n").unwrap();
    drop(writer);
    child.wait_with_output().unwrap()
}

/// A credential revoked or lapsed between a `context land`'s admission and its commit commits nothing.
// spec: authority.verify.write-commit@c3052f0f
#[cfg(unix)]
#[test]
fn a_land_whose_credential_lapses_before_its_commit_commits_nothing() {
    let dir = project("authoring_posture = \"session\"");
    let (public, token) = credential(dir.path(), &["write"]);
    let verified: serde_json::Value =
        serde_json::from_str(&ok(&cf(dir.path(), &["token", "verify", "--token", &token, "--public-key", &public], None))).unwrap();
    let rev = verified["rev_ids"][0].as_str().unwrap().to_string();
    let denylist = dir.path().join("denylist.txt");
    std::fs::write(&denylist, "# none yet\n").unwrap();
    let pins = ["--public-key", public.as_str(), "--audience", AUD, "--denylist", "denylist.txt"];

    let out = land_across(dir.path(), &pins, &token, || std::fs::write(&denylist, format!("{rev}\n")).unwrap());
    refused(&out, "AuthorityRevoked");
    assert!(!landed(dir.path()));

    // Six seconds of lifetime outlive admission and lapse before the commit.
    std::fs::write(&denylist, "# none yet\n").unwrap();
    let brief = mint(dir.path(), &["write"], &["filings"], "6");
    let out = land_across(dir.path(), &pins, &brief, || std::thread::sleep(std::time::Duration::from_secs(8)));
    refused(&out, "AuthorityExpired");
    assert!(!landed(dir.path()));
}

/// `pipeline run` admits its credential against every destination under its `<pipeline>_<table>` name and stamps
/// the credential's `on_behalf_of` on every row it lands.
#[test]
fn a_credentialed_pipeline_run_authors_the_rows_it_lands() {
    let dir = project("authoring_posture = \"session\"");
    let p = dir.path();
    let manifest = concat!(
        "authoring_posture = \"session\"\n",
        "[[pipeline]]\nid = \"doc-text\"\ntables = [{ name = \"passages\", primary_key = [\"unit_ref\", \"derivation_key\", \"cue_seq\"] }]\n",
        "[pipeline.source]\nname = \"derive\"\n",
        "config = { engine = \"reader\", source_table = \"documents\", media_column = \"path\", parent_id_column = \"doc_id\" }\n\n",
        "[derive.reader]\ndriver = \"exec\"\nmedia_root = \"media\"\n\n",
        "[derive.reader.engine]\ncommand = [\"cat\", \"{input}\"]\noutput_format = \"srt\"\n",
    );
    std::fs::write(p.join("contextful.toml"), manifest).unwrap();
    std::fs::remove_dir_all(p.join("pipelines")).unwrap();
    std::fs::create_dir_all(p.join("media")).unwrap();
    std::fs::write(p.join("media/memo.srt"), "1\n00:00:00,000 --> 00:00:01,000\nRevenue rose.\n\n").unwrap();
    std::fs::write(p.join("documents.jsonl"), "{\"doc_id\":\"d1\",\"path\":\"memo.srt\"}\n").unwrap();
    let (public, _) = credential(p, &["write"]);
    let pins = ["--public-key", public.as_str(), "--audience", AUD];
    let token = derive_credential(p, &["read", "write"]);
    let mut land = vec!["context", "land", "documents", "--project", "research", "--rows", "documents.jsonl", "--run-id", "load-1", "--site-id", "s"];
    land.extend_from_slice(&pins);
    ok(&cf(p, &land, Some(&token)));

    // A grant over the declared name `passages` does not cover the destination `doc_text_passages`.
    let bare = mint(p, &["write"], &["documents", "passages"], "600");
    let mut fire = vec!["pipeline", "run", "doc-text", "--project", "research", "--run-id", "derive-1", "--site-id", "s"];
    fire.extend_from_slice(&pins);
    let err = refused(&cf(p, &fire, Some(&bare)), "GrantWriteNotCovered");
    assert!(err.contains("doc_text_passages"), "{err}");

    let fired = ok(&cf(p, &fire, Some(&token)));
    assert!(fired.contains("success"), "{fired}");
    assert_eq!(authors_of(p, "doc_text_passages"), Some(vec![serde_json::json!(DANA)]));
}

/// A derive source keeps the admitted subject's row restriction before a task reads media.
#[test]
fn a_credentialed_derive_reads_only_its_registered_source_rows() {
    let (dir, public) = restricted_derive();
    let p = dir.path();
    let token = derive_credential(p, &["read", "write"]);
    let out = cf(p, &[
        "pipeline", "run", "doc-text", "--project", "research", "--run-id", "restricted-derive", "--site-id", "s",
        "--public-key", &public, "--audience", AUD,
    ], Some(&token));
    ok(&out);
    let query = "SELECT DISTINCT unit_ref FROM doc_text_passages ORDER BY unit_ref";
    let result: serde_json::Value = serde_json::from_str(&ok(&cf(p, &["query", "--json", "--project", "research", query], None))).unwrap();
    assert_eq!(result["rows"], serde_json::json!([["allowed"]]), "a source row another principal owns reached the task: {result}");
}

/// Destination write authority supplies no authority to read a derive source.
#[test]
fn a_write_only_derive_credential_releases_no_source_row() {
    let (dir, public) = restricted_derive();
    let p = dir.path();
    let token = derive_credential(p, &["write"]);
    let out = cf(p, &[
        "pipeline", "run", "doc-text", "--project", "research", "--run-id", "write-only-derive", "--site-id", "s",
        "--public-key", &public, "--audience", AUD,
    ], Some(&token));
    refused(&out, "EnforceUnknownRelation");
    assert!(!p.join(".contextful/context/research/tables/doc_text_passages/schema.json").exists(), "a denied source read landed task output");
}

/// A complete-source port refuses an admitted response that carries a truncation witness.
#[test]
fn a_credentialed_derive_refuses_truncated_source_input_without_landing_output() {
    let (dir, public) = restricted_derive();
    let p = dir.path();
    std::fs::write(p.join("media/allowed-two.srt"), "1\n00:00:00,000 --> 00:00:01,000\nSecond allowed content.\n\n").unwrap();
    std::fs::write(p.join("allowed-two.jsonl"), format!("{{\"doc_id\":\"allowed-two\",\"path\":\"allowed-two.srt\",\"owner\":\"{DANA}\"}}\n")).unwrap();
    ok(&cf(p, &["context", "land", "documents", "--project", "research", "--rows", "allowed-two.jsonl", "--run-id", "source-second", "--site-id", "s"], None));
    let token = derive_credential_with_ceiling(p, &["read", "write"], Some("1"));
    let out = cf(p, &[
        "pipeline", "run", "doc-text", "--project", "research", "--run-id", "truncated-derive", "--site-id", "s",
        "--public-key", &public, "--audience", AUD,
    ], Some(&token));
    refused(&out, "Config");
    let message = String::from_utf8_lossy(&out.stderr);
    assert!(message.contains("truncated"), "the source completeness refusal is explicit: {message}");
    assert!(!p.join(".contextful/context/research/tables/doc_text_passages/schema.json").exists(), "a partial source read landed task output");
}

fn restricted_derive() -> (tempfile::TempDir, String) {
    let dir = project("authoring_posture = \"per_request\"");
    let p = dir.path();
    std::fs::remove_dir_all(p.join("pipelines")).unwrap();
    std::fs::write(p.join("contextful.toml"), concat!(
        "authoring_posture = \"per_request\"\n",
        "[[pipeline.tables]]\nname = \"documents\"\n",
        "[pipeline.tables.policy.rows]\npredicate = \"owner = subject.on_behalf_of\"\n",
        "[derive.reader]\ndriver = \"exec\"\nmedia_root = \"media\"\n",
        "[derive.reader.engine]\ncommand = [\"cat\", \"{input}\"]\noutput_format = \"srt\"\n",
    )).unwrap();
    std::fs::create_dir_all(p.join("pipelines")).unwrap();
    std::fs::write(p.join("pipelines/doc-text.toml"), concat!(
        "id = \"doc-text\"\ntables = [{ name = \"passages\", primary_key = [\"unit_ref\", \"derivation_key\", \"cue_seq\"] }]\n",
        "[source]\nname = \"derive\"\n",
        "config = { engine = \"reader\", source_table = \"documents\", media_column = \"path\", parent_id_column = \"doc_id\" }\n",
    )).unwrap();
    std::fs::create_dir_all(p.join("media")).unwrap();
    for (name, text) in [("allowed", "Allowed content."), ("forbidden", "Another principal's content.")] {
        std::fs::write(p.join(format!("media/{name}.srt")), format!("1\n00:00:00,000 --> 00:00:01,000\n{text}\n\n")).unwrap();
    }
    std::fs::write(p.join("documents.jsonl"), format!(
        "{{\"doc_id\":\"allowed\",\"path\":\"allowed.srt\",\"owner\":\"{DANA}\"}}\n{{\"doc_id\":\"forbidden\",\"path\":\"forbidden.srt\",\"owner\":\"user://other@acme.example\"}}\n",
    )).unwrap();
    ok(&cf(p, &["context", "land", "documents", "--project", "research", "--rows", "documents.jsonl", "--run-id", "source-load", "--site-id", "s"], None));
    let present: serde_json::Value = serde_json::from_str(&ok(&cf(p, &["query", "--json", "--project", "research", "SELECT doc_id FROM documents ORDER BY doc_id"], None))).unwrap();
    assert_eq!(present["rows"], serde_json::json!([["allowed"], ["forbidden"]]), "both source rows must exist before the exclusion test");
    let (public, _) = credential(p, &["write"]);
    (dir, public)
}

fn derive_credential(dir: &Path, actions: &[&str]) -> String {
    derive_credential_with_ceiling(dir, actions, None)
}

fn derive_credential_with_ceiling(dir: &Path, actions: &[&str], ceiling: Option<&str>) -> String {
    let mut args = vec!["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", DANA,
        "--zone", "on-prem:hq", "--table", "documents", "--table", "doc_text_passages", "--ttl", "600"];
    for action in actions {
        args.extend(["--action", action]);
    }
    if let Some(ceiling) = ceiling {
        args.extend(["--max-rows", ceiling]);
    }
    ok(&cf(dir, &args, None))
}
