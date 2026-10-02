//! A credential bound to a holder key over the process transport: the command-line write
//! verbs and the stdio tool server admit it through a holder seed or a proof presented in
//! the environment.

use contextful_core::time::Instant;
use contextful_policy::possession::{jwk_thumbprint, sign_proof, ProofRequest};
use ed25519_dalek::SigningKey;
use serde_json::{json, Value};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

const AUD: &str = "contextful://acme-research";
const DANA: &str = "user://dana@acme.example";
const HOLDER: [u8; 32] = [11; 32];

/// A `per_request` project declaring `filings`, a row file to land, and its issuer key.
fn project() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    let store = p.join(".contextful/context/research");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("config.toml"), "[node]\nid = \"ingest-a\"\n").unwrap();
    std::fs::write(p.join(".contextful/issuance.toml"), format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 3600\n")).unwrap();
    std::fs::write(p.join("contextful.toml"), "authoring_posture = \"per_request\"\n[[pipeline.tables]]\nname = \"filings\"\n").unwrap();
    std::fs::write(p.join("rows.jsonl"), "{\"doc\":\"a\"}\n").unwrap();
    let public = ok(&cf(p, &["token", "keygen", "--out", ".contextful/issuer.seed"], &[]));
    (dir, public)
}

fn command(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_contextful"));
    c.args(args).current_dir(dir);
    for var in ["CONTEXTFUL_NODE_ID", "CONTEXTFUL_TOKEN", "CONTEXTFUL_ISSUER_PUBKEY", "CONTEXTFUL_AUDIENCE", "CONTEXTFUL_DPOP", "CONTEXTFUL_HOLDER_KEY"] {
        c.env_remove(var);
    }
    for (k, v) in env {
        c.env(k, v);
    }
    c
}

fn cf(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    command(dir, args, env).output().unwrap()
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

/// A credential for Dana granting `read` and `write` over `filings`, bound to `jkt`.
fn bound(dir: &Path, jkt: &str) -> String {
    // `=` keeps a thumbprint opening with `-` a value, not a flag.
    let holder = format!("--holder={jkt}");
    let args = [
        "token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", DANA, "--action", "read", "--action", "write",
        "--table", "filings", "--ttl", "600", &holder,
    ];
    ok(&cf(dir, &args, &[]))
}

/// Write `key` as a holder seed file in the issuer seed's text form.
fn seed_file(dir: &Path, name: &str, key: &[u8; 32]) -> String {
    let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::write(dir.join(name), format!("ed25519-private/{hex}\n")).unwrap();
    name.to_string()
}

fn thumbprint(key: &[u8; 32]) -> String {
    jwk_thumbprint(SigningKey::from_bytes(key).verifying_key().as_bytes())
}

/// A proof by `key` for the verb `target`, issued now under `nonce`.
fn proof(key: &[u8; 32], target: &str, nonce: &str) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let request = ProofRequest { method: "CLI", target, body: b"" };
    sign_proof(&SigningKey::from_bytes(key), &request, Instant::from_unix_secs(now).unwrap(), nonce)
}

/// `context land` arguments under a run id no earlier call used.
fn land<'a>(public: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    static RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let run_id: &'static str = Box::leak(format!("run-{}", RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)).into_boxed_str());
    let mut args = vec![
        "context", "land", "filings", "--project", "research", "--rows", "rows.jsonl", "--run-id", run_id, "--site-id", "s",
        "--public-key", public, "--audience", AUD,
    ];
    args.extend_from_slice(extra);
    args
}

/// The distinct `_authored_by` values of `filings`.
fn authors(dir: &Path) -> Vec<Value> {
    let q = "SELECT DISTINCT _authored_by FROM filings ORDER BY 1";
    let r: Value = serde_json::from_str(&ok(&cf(dir, &["query", "--json", "--project", "research", q], &[]))).unwrap();
    r["rows"].as_array().unwrap().iter().map(|row| row[0].clone()).collect()
}

fn landed(dir: &Path) -> bool {
    dir.join(".contextful/context/research/tables/filings/schema.json").exists()
}

/// Run the stdio tool server over one `initialize`.
fn serve(dir: &Path, public: &str, env: &[(&str, &str)]) -> Output {
    let mut cmd = command(dir, &["mcp", "--project", "research", "--public-key", public, "--audience", AUD], env);
    let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    writeln!(child.stdin.take().unwrap(), "{}", json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} })).ok();
    child.wait_with_output().unwrap()
}

/// `token keygen --holder` writes an Ed25519 holder seed and prints its thumbprint, the value `token mint --holder` binds as the confirmation claim.
// spec: authority.verify.holder-keygen@765c0108
#[test]
fn holder_keygen_prints_the_thumbprint_a_mint_binds() {
    let (dir, _public) = project();
    let jkt = ok(&cf(dir.path(), &["token", "keygen", "--holder", "--out", "holder.seed"], &[]));
    assert_eq!(jkt.len(), 43, "an RFC 7638 thumbprint is 43 base64url characters: {jkt}");
    let token = bound(dir.path(), &jkt);
    let card: Value = serde_json::from_str(&ok(&cf(dir.path(), &["token", "introspect", "--token", &token], &[]))).unwrap();
    assert!(card.to_string().contains(&jkt), "the credential binds the printed thumbprint: {card}");
    refused(&cf(dir.path(), &["token", "keygen", "--holder", "--out", "holder.seed"], &[]), "never overwritten");
    // A holder key carries no activation instant, so `--now` beside `--holder` is refused.
    refused(&cf(dir.path(), &["token", "keygen", "--holder", "--out", "h2.seed", "--now", "garbage"], &[]), "cannot be used with");
    assert!(!dir.path().join("h2.seed").exists());
}

/// A credentialed command-line verb and the stdio tool server sign one holder proof at admission with the Ed25519 seed in
/// `--holder-key`, else take the proof in `CONTEXTFUL_DPOP`, else sign with the seed in `CONTEXTFUL_HOLDER_KEY`; a blank variable is unset.
// spec: authority.verify.local-proof-channel@480bab61
#[test]
fn a_key_bound_credential_admits_through_its_holder_seed_or_a_proof_in_the_environment() {
    let (dir, public) = project();
    let p = dir.path();
    let token = bound(p, &thumbprint(&HOLDER));
    let holder = seed_file(p, "holder.seed", &HOLDER);
    let stranger = seed_file(p, "stranger.seed", &[12; 32]);
    let land_with = |flags: &[&str], extra: &[(&str, &str)]| {
        let mut env = vec![("CONTEXTFUL_TOKEN", token.as_str())];
        env.extend_from_slice(extra);
        cf(p, &land(&public, flags), &env)
    };

    refused(&land_with(&[], &[]), "PossessionProofInvalid");
    refused(&land_with(&["--holder-key", &stranger], &[]), "PossessionProofInvalid");
    refused(&land_with(&[], &[("CONTEXTFUL_HOLDER_KEY", &stranger)]), "PossessionProofInvalid");
    // A seed file that cannot be read yields no proof.
    refused(&land_with(&["--holder-key", "missing.seed"], &[]), "PossessionProofInvalid");
    refused(&land_with(&[], &[("CONTEXTFUL_HOLDER_KEY", "missing.seed")]), "PossessionProofInvalid");
    assert!(!landed(p), "a refused proof lands nothing");

    ok(&land_with(&["--holder-key", &holder], &[]));
    assert_eq!(authors(p), vec![json!(DANA)]);
    ok(&land_with(&[], &[("CONTEXTFUL_DPOP", &proof(&HOLDER, "context land", "land-1"))]));
    ok(&land_with(&[], &[("CONTEXTFUL_HOLDER_KEY", &holder)]));

    // A proof never travels on the argument list, where the process table shows it.
    refused(&land_with(&["--dpop", &proof(&HOLDER, "context land", "land-2")], &[]), "unexpected argument");

    // The flag beats both variables; a presented proof beats the variable's seed; a blank proof is unset.
    let stale = proof(&HOLDER, "mcp", "stale-1");
    ok(&land_with(&["--holder-key", &holder], &[("CONTEXTFUL_DPOP", &stale), ("CONTEXTFUL_HOLDER_KEY", &stranger)]));
    refused(&land_with(&["--holder-key", &stranger], &[("CONTEXTFUL_DPOP", &proof(&HOLDER, "context land", "land-3"))]), "PossessionProofInvalid");
    ok(&land_with(&[], &[("CONTEXTFUL_DPOP", &proof(&HOLDER, "context land", "land-4")), ("CONTEXTFUL_HOLDER_KEY", &stranger)]));
    ok(&land_with(&[], &[("CONTEXTFUL_DPOP", " "), ("CONTEXTFUL_HOLDER_KEY", &holder)]));

    let refused_server = serve(p, &public, &[("CONTEXTFUL_TOKEN", &token)]);
    assert!(!refused_server.status.success() && refused_server.stdout.is_empty(), "a server with no proof writes no framing");
    let out = serve(p, &public, &[("CONTEXTFUL_TOKEN", &token), ("CONTEXTFUL_HOLDER_KEY", &holder)]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let hello: Value = serde_json::from_str(String::from_utf8_lossy(&out.stdout).lines().next().unwrap()).unwrap();
    assert_eq!(hello["result"]["serverInfo"]["name"], json!("contextful"));
    let presented = proof(&HOLDER, "mcp", "mcp-1");
    let out = serve(p, &public, &[("CONTEXTFUL_TOKEN", &token), ("CONTEXTFUL_DPOP", &presented)]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

/// A credential binding no holder key admits through the inherited pipe whatever proof
/// channel the environment also sets.
#[test]
fn an_unbound_credential_ignores_the_proof_channel() {
    let (dir, public) = project();
    let p = dir.path();
    let args = ["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", DANA, "--action", "read", "--table", "filings", "--ttl", "600"];
    let token = ok(&cf(p, &args, &[]));
    let holder = seed_file(p, "holder.seed", &HOLDER);
    let presented = proof(&HOLDER, "mcp", "mcp-1");
    let env = [("CONTEXTFUL_TOKEN", token.as_str()), ("CONTEXTFUL_HOLDER_KEY", holder.as_str()), ("CONTEXTFUL_DPOP", presented.as_str())];
    let out = serve(p, &public, &env);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

/// A local proof covers method `CLI`, the verb's command path as target, such as `context land`, `memory write` or `mcp`, and an empty body.
// spec: authority.verify.local-proof-request@70117b7c
#[test]
fn a_presented_proof_admits_only_the_verb_it_names() {
    let (dir, public) = project();
    let p = dir.path();
    let token = bound(p, &thumbprint(&HOLDER));
    let for_server = proof(&HOLDER, "mcp", "n-1");
    refused(&cf(p, &land(&public, &[]), &[("CONTEXTFUL_TOKEN", &token), ("CONTEXTFUL_DPOP", &for_server)]), "PossessionProofInvalid");
    let request = ProofRequest { method: "POST", target: "context land", body: b"" };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let http = sign_proof(&SigningKey::from_bytes(&HOLDER), &request, Instant::from_unix_secs(now).unwrap(), "n-2");
    refused(&cf(p, &land(&public, &[]), &[("CONTEXTFUL_TOKEN", &token), ("CONTEXTFUL_DPOP", &http)]), "PossessionProofInvalid");
    assert!(!landed(p));
    let for_land = proof(&HOLDER, "context land", "n-3");
    ok(&cf(p, &land(&public, &[]), &[("CONTEXTFUL_TOKEN", &token), ("CONTEXTFUL_DPOP", &for_land)]));
    assert!(landed(p));
}

/// Each verb's proof target is its own command path: a proof minted for that path passes
/// admission, and one minted for another verb refuses.
#[test]
fn each_verb_admits_a_proof_minted_for_its_command_path() {
    let (dir, public) = project();
    let p = dir.path();
    let memory = "\n[[table]]\nname = \"memory/facts\"\nshape = \"memory_facts\"\ncolumns = [\"claim_id\", \"subject\", \"predicate\", \"object\", \"scope\", \"tier\", \"confidence\", \"valid_from\", \"valid_to\", \"evidence\", \"superseded_by\", \"grant_id\", \"agent\"]\n";
    let manifest = std::fs::read_to_string(p.join("contextful.toml")).unwrap();
    std::fs::write(p.join("contextful.toml"), manifest + memory).unwrap();
    let args = [
        "token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", DANA, "--action", "read", "--action", "write",
        "--table", "filings", "--table", "memory/*", "--ttl", "600", "--holder", &thumbprint(&HOLDER),
    ];
    let token = ok(&cf(p, &args, &[]));
    let holder = seed_file(p, "holder.seed", &HOLDER);
    let source = [
        "context", "land", "filings", "--project", "research", "--rows", "rows.jsonl", "--run-id", "run-src", "--site-id", "s",
        "--public-key", &public, "--audience", AUD, "--holder-key", &holder,
    ];
    ok(&cf(p, &source, &[("CONTEXTFUL_TOKEN", &token)]));
    let claim = r#"{"subject":"a","predicate":"b","object":"c","confidence":0.5,"evidence":[{"table":"filings","run":"run-src","seq":0}]}"#;
    let tail = ["--project", "research", "--public-key", public.as_str(), "--audience", AUD];
    let synthesize = ["memory", "synthesize", "--source", "filings", "--into", "memory/facts", "--endpoint", "http://127.0.0.1:9", "--model", "m"];
    let write = ["memory", "write", "--into", "memory/facts", "--claim", claim];
    for (path, args) in [("memory synthesize", &synthesize[..]), ("memory write", &write[..])] {
        let args: Vec<&str> = args.iter().chain(tail.iter()).copied().collect();
        let wrong = proof(&HOLDER, "context land", &format!("{path}-wrong"));
        refused(&cf(p, &args, &[("CONTEXTFUL_TOKEN", &token), ("CONTEXTFUL_DPOP", &wrong)]), "PossessionProofInvalid");
        let right = proof(&HOLDER, path, &format!("{path}-right"));
        let out = cf(p, &args, &[("CONTEXTFUL_TOKEN", &token), ("CONTEXTFUL_DPOP", &right)]);
        let report = ok(&out);
        // The pass reports over its target table; the write lands its claim.
        let effect = if path == "memory write" { "curated" } else { "memory/facts: " };
        assert!(report.contains(effect), "`{path}` acts past admission: {report}");
    }
    let out = serve(p, &public, &[("CONTEXTFUL_TOKEN", &token), ("CONTEXTFUL_DPOP", &proof(&HOLDER, "mcp", "mcp-right"))]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

/// A command-line verb checks its holder proof against the nonce store `.contextful/proof-nonces` under the project root,
/// shared by every invocation on that project under an exclusive lock, so a repeated nonce raises `PossessionProofReplayed`.
// spec: authority.verify.local-nonce-store@4b4b7b79
#[test]
fn a_presented_proof_admits_once_across_invocations() {
    let (dir, public) = project();
    let p = dir.path();
    let token = bound(p, &thumbprint(&HOLDER));
    let once = proof(&HOLDER, "context land", "same-nonce");
    let env = [("CONTEXTFUL_TOKEN", token.as_str()), ("CONTEXTFUL_DPOP", once.as_str())];
    ok(&cf(p, &land(&public, &[]), &env));
    refused(&cf(p, &land(&public, &[]), &env), "PossessionProofReplayed");
    assert!(p.join(".contextful/proof-nonces").is_file());

    // A fresh nonce admits, and a seed signs a fresh nonce on every invocation.
    let other = proof(&HOLDER, "context land", "other-nonce");
    ok(&cf(p, &land(&public, &[]), &[("CONTEXTFUL_TOKEN", &token), ("CONTEXTFUL_DPOP", &other)]));
    let holder = seed_file(p, "holder.seed", &HOLDER);
    for _ in 0..2 {
        ok(&cf(p, &land(&public, &["--holder-key", &holder]), &[("CONTEXTFUL_TOKEN", &token)]));
    }

    // Concurrent invocations presenting one proof admit exactly one.
    let shared = proof(&HOLDER, "mcp", "raced");
    let env = [("CONTEXTFUL_TOKEN", token.as_str()), ("CONTEXTFUL_DPOP", shared.as_str())];
    let args = ["mcp", "--project", "research", "--public-key", &public, "--audience", AUD];
    let children: Vec<_> = (0..6)
        .map(|_| command(p, &args, &env).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap())
        .collect();
    let outs: Vec<Output> = children.into_iter().map(|c| c.wait_with_output().unwrap()).collect();
    assert_eq!(outs.iter().filter(|o| o.status.success()).count(), 1, "one of six racing invocations admits");
    for o in outs.iter().filter(|o| !o.status.success()) {
        let stderr = String::from_utf8_lossy(&o.stderr);
        assert!(stderr.contains("PossessionProofReplayed"), "{stderr}");
    }
}
