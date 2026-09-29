//! The table write verbs — `context land`, `run start`, `pipeline run` — under each
//! authoring posture, with and without an accompanying credential.

use std::path::Path;
use std::process::{Command, Output};

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
    c.output().unwrap()
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
    let mut args = vec!["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", DANA, "--table", "filings", "--ttl", "600"];
    for a in actions {
        args.extend(["--action", a]);
    }
    (public, ok(&cf(dir, &args, None)))
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
    let all: serde_json::Value = serde_json::from_str(&ok(&cf(dir, &["query", "--json", "--project", "research", "SELECT * FROM filings"], None))).unwrap();
    if !all["columns"].as_array().unwrap().iter().any(|c| c == "_authored_by") {
        return None;
    }
    let q = "SELECT DISTINCT _authored_by FROM filings ORDER BY 1";
    let r: serde_json::Value = serde_json::from_str(&ok(&cf(dir, &["query", "--json", "--project", "research", q], None))).unwrap();
    Some(r["rows"].as_array().unwrap().iter().map(|row| row[0].clone()).collect())
}

fn landed(dir: &Path) -> bool {
    dir.join(".contextful/context/research/tables/filings/schema.json").exists()
        && std::fs::read_dir(dir.join(".contextful/context/research/tables/filings/data/runs"))
            .map(|runs| runs.flatten().any(|r| std::fs::read_dir(r.path()).unwrap().flatten().any(|n| n.path().join("_manifest.json").exists())))
            .unwrap_or(false)
}

/// Every project load declares an authoring posture. `session` authors every write by one verified ambient
/// credential's principal; `per_request` holds no ambient principal, leaving an unaccompanied write unauthored.
// spec: authority.issue.authoring-posture@80ff2e8a
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
// spec: authority.verify.write-commit@c3052f0f
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
