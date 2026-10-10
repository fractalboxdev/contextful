//! `contextful audit` through the built binary: verify, prove, check a proof offline, and
//! query `audit_reads` over the project's chain.
#![cfg(feature = "data-plane")]

use contextful_policy::audit::{attr, AuditLog, AuditOptions, ChainHeader};
use contextful_policy::issue::SeedSigner;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

fn run(dir: &Path, args: &[&str]) -> Output {
    run_env(dir, args, &[])
}

fn run_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_contextful"));
    cmd.args(args).current_dir(dir).env_remove("CONTEXTFUL_TOKEN").env_remove("CONTEXTFUL_ISSUER_PUBKEY");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

fn stdout(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn stderr(out: &Output) -> String {
    assert!(!out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn chain_dir(root: &Path) -> PathBuf {
    root.join(".contextful/audit")
}

/// An RFC 3339 UTC instant `ago` before now.
fn instant_before(ago: Duration) -> String {
    let secs = (std::time::SystemTime::now() - ago).duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since the epoch.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn read(ago: Duration, who: &str, tables: &[&str], outcome: &str, rows: u64) -> Value {
    json!({
        attr::READ_AT: instant_before(ago),
        attr::ON_BEHALF_OF: who,
        attr::AGENT: "agent://research-loop",
        attr::TABLES: tables,
        attr::POLICY: "rev-1",
        attr::OUTCOME: outcome,
        attr::ROWS: rows,
    })
}

/// A project directory with an issuer seed; returns it and the public key pin.
fn project() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".contextful")).unwrap();
    let public = stdout(&run(dir.path(), &["token", "keygen", "--out", ".contextful/issuer.seed"]));
    (dir, public)
}

fn signer(root: &Path) -> SeedSigner {
    SeedSigner::from_seed(&std::fs::read_to_string(root.join(".contextful/issuer.seed")).unwrap()).unwrap()
}

/// Append `batch` to the project's chain under segments of `segment_entries`.
fn append(root: &Path, segment_entries: u64, batch: Vec<Value>) {
    let options = AuditOptions { header: ChainHeader { segment_entries, ..ChainHeader::default() }, ..AuditOptions::default() };
    let log = AuditLog::open_with(chain_dir(root), signer(root), options).unwrap();
    log.append_all(batch).unwrap();
    log.export().unwrap();
}

/// `audit verify` passes over an intact chain under the issuer pin and fails at the
/// rewritten entry's index once one entry's `on_behalf_of` changes.
// spec: disclosure.attest.verify-verb@12e0ae5c
#[test]
fn verify_passes_an_intact_chain_and_names_the_rewritten_index() {
    let (dir, public) = project();
    let p = dir.path();
    append(p, 4096, vec![
        read(Duration::from_secs(60), "user://ada@acme.example", &["research/notes"], attr::SERVED, 1),
        read(Duration::from_secs(30), "user://bo@acme.example", &["research/notes"], attr::SERVED, 2),
    ]);
    let end: Value = serde_json::from_str(&stdout(&run(p, &["audit", "verify", "--project", "research", "--public-key", &public]))).unwrap();
    assert_eq!(end["seq"], 2);

    let segment = chain_dir(p).join("segments/000001.jsonl");
    let text = std::fs::read_to_string(&segment).unwrap();
    std::fs::write(&segment, text.replace("user://bo@acme.example", "user://eve@acme.example")).unwrap();
    let err = stderr(&run(p, &["audit", "verify", "--project", "research", "--public-key", &public]));
    assert!(err.contains("AuditChainBroken: entry 2"), "{err}");
    let err = stderr(&run(p, &["audit", "verify", "--project", "research"]));
    assert!(err.contains("AuditChainBroken: entry 2"), "{err}");
}

/// A chain the read path appended to unanchored verifies under the issuer pin only once
/// `audit anchor` signs its tip through the issuer's seed.
// spec: disclosure.attest.anchor-verb@2558778e
#[test]
fn anchor_signs_an_unanchored_chain_so_it_verifies_under_the_issuer_pin() {
    let (dir, public) = project();
    let p = dir.path();
    let log = AuditLog::unanchored(chain_dir(p)).unwrap();
    log.append_all(vec![read(Duration::from_secs(5), "user://ada@acme.example", &["research/notes"], attr::SERVED, 1)]).unwrap();
    drop(log);
    let err = stderr(&run(p, &["audit", "verify", "--project", "research", "--public-key", &public]));
    assert!(err.contains("AuditLogUnanchored"), "{err}");

    let end: Value = serde_json::from_str(&stdout(&run(p, &["audit", "anchor", "--project", "research", "--issuer-key", ".contextful/issuer.seed"]))).unwrap();
    assert_eq!(end["seq"], 1);
    let verified: Value = serde_json::from_str(&stdout(&run(p, &["audit", "verify", "--project", "research", "--public-key", &public]))).unwrap();
    assert_eq!(verified, end);
}

/// `audit verify` under another issuer's pin refuses the chain's signatures.
#[test]
fn verify_under_a_foreign_key_refuses_the_signatures() {
    let (dir, _) = project();
    let p = dir.path();
    append(p, 4096, vec![read(Duration::from_secs(5), "user://ada@acme.example", &["research/notes"], attr::SERVED, 1)]);
    let other = SeedSigner::generate(contextful_core::issue::SignatureAlgorithm::Ed25519).public_key_text();
    let err = stderr(&run(p, &["audit", "verify", "--project", "research", "--public-key", &other]));
    assert!(err.contains("AuditChainBroken"), "{err}");
}

/// `audit prove` prints a proof `audit check-proof` accepts under the pin alone, and a
/// proof with an altered entry fails the check.
// spec: disclosure.attest.prove-verb@3fc91307
#[test]
fn a_printed_proof_checks_offline_and_an_altered_one_does_not() {
    let (dir, public) = project();
    let p = dir.path();
    append(p, 4, (0..5).map(|i| read(Duration::from_secs(10), "user://ada@acme.example", &["research/notes"], attr::SERVED, i)).collect());
    let proof = stdout(&run(p, &["audit", "prove", "--project", "research", "--seq", "3"]));
    std::fs::write(p.join("proof.json"), &proof).unwrap();
    let checked: Value = serde_json::from_str(&stdout(&run(p, &["audit", "check-proof", "--proof", "proof.json", "--public-key", &public]))).unwrap();
    assert_eq!(checked["seq"], 3);

    std::fs::write(p.join("altered.json"), proof.replace("user://ada@acme.example", "user://eve@acme.example")).unwrap();
    let err = stderr(&run(p, &["audit", "check-proof", "--proof", "altered.json", "--public-key", &public]));
    assert!(err.contains("AuditProofInvalid"), "{err}");

    // Entry 5 sits in the open second segment, which no signed root closes yet.
    let err = stderr(&run(p, &["audit", "prove", "--project", "research", "--seq", "5"]));
    assert!(err.contains("AuditProofUnavailable"), "{err}");
}

/// `audit query` answers which tables a person's agent read over the last hour, from
/// `audit_reads` computed on the call.
// spec: disclosure.record.reads-view@4278490c
#[test]
fn query_answers_who_read_what_over_a_window() {
    let (dir, _) = project();
    let p = dir.path();
    append(p, 4096, vec![
        read(Duration::from_secs(2 * 3600), "user://ada@acme.example", &["hr/salaries"], attr::SERVED, 1),
        read(Duration::from_secs(600), "user://ada@acme.example", &["research/notes", "research/links"], attr::SERVED, 3),
        read(Duration::from_secs(300), "user://bo@acme.example", &["hr/salaries"], attr::SERVED, 1),
        read(Duration::from_secs(60), "user://ada@acme.example", &["hr/salaries"], attr::REFUSED, 0),
    ]);
    let sql = "SELECT DISTINCT table_name FROM audit_reads WHERE on_behalf_of = 'user://ada@acme.example' \
               AND outcome = 'served' AND read_at >= now() - INTERVAL 1 HOUR ORDER BY table_name";
    let out: Value = serde_json::from_str(&stdout(&run(p, &["audit", "query", "--project", "research", "--sql", sql]))).unwrap();
    assert_eq!(out["rows"], json!([["research/links"], ["research/notes"]]), "{out}");

    let sql = "SELECT agent, policy, outcome, row_count FROM audit_reads WHERE outcome = 'refused'";
    let out: Value = serde_json::from_str(&stdout(&run(p, &["audit", "query", "--project", "research", "--sql", sql]))).unwrap();
    assert_eq!(out["rows"], json!([["agent://research-loop", "rev-1", "refused", "0"]]), "{out}");
}

/// The projection reaches no file: a statement reading one raises rather than answering.
#[test]
fn the_projection_reaches_no_file() {
    let (dir, _) = project();
    let p = dir.path();
    append(p, 4096, vec![read(Duration::from_secs(1), "user://ada@acme.example", &["research/notes"], attr::SERVED, 1)]);
    let err = stderr(&run(p, &["audit", "query", "--project", "research", "--sql", "SELECT * FROM read_text('.contextful/issuer.seed')"]));
    assert!(!err.contains("private"), "{err}");
}

/// Every chain-reading verb refuses a capability credential before it reads the chain.
// spec: disclosure.attest.owner-only@cc656a14
#[test]
fn a_capability_credential_refuses_every_chain_verb() {
    let (dir, public) = project();
    let p = dir.path();
    append(p, 4096, vec![read(Duration::from_secs(1), "user://ada@acme.example", &["research/notes"], attr::SERVED, 1)]);
    let token = [("CONTEXTFUL_TOKEN", "a-capability-credential")];
    for args in [
        vec!["audit", "verify", "--project", "research", "--public-key", public.as_str()],
        vec!["audit", "prove", "--project", "research", "--seq", "1"],
        vec!["audit", "anchor", "--project", "research", "--issuer-key", ".contextful/issuer.seed"],
        vec!["audit", "query", "--project", "research", "--sql", "SELECT count(*) FROM audit_reads"],
    ] {
        let err = stderr(&run_env(p, &args, &token));
        assert!(err.contains("AuditRequiresOwner"), "{args:?}: {err}");
    }
}

/// Declare a file-endpoint `[sync]` bucket under `bucket` for the project's store.
fn declare_bucket(root: &Path, bucket: &Path) {
    let store = root.join(".contextful/context/research");
    std::fs::create_dir_all(&store).unwrap();
    let sync = format!("endpoint = \"file://{}\"\nbucket = \"context-team\"\nprefix = \"team\"\ncoordination = \"single-writer\"\n", bucket.display());
    std::fs::write(store.join("config.toml"), format!("[node]\nid = \"ingest-a\"\n\n[sync]\n{sync}")).unwrap();
}

/// `audit anchor` copies the roots it signs to `<prefix>/<project>/audit/roots/` in the
/// `[sync]` bucket, and `audit replicate` then finds nothing to send; once the local roots,
/// `chain.held` and the tip's signature are gone, `audit verify` passes and
/// `audit verify --bucket` breaks at the first replicated segment.
// spec: disclosure.attest.replicate-verb@147ee155
#[test]
fn verify_against_the_bucket_catches_deleted_roots_and_tip() {
    let (dir, public) = project();
    let p = dir.path();
    let bucket = tempfile::tempdir().unwrap();
    declare_bucket(p, bucket.path());
    let options = AuditOptions { header: ChainHeader { segment_entries: 2, ..ChainHeader::default() }, ..AuditOptions::default() };
    let log = AuditLog::unanchored_with(chain_dir(p), options).unwrap();
    log.append_all((0..5).map(|i| read(Duration::from_secs(10), "user://ada@acme.example", &["research/notes"], attr::SERVED, i)).collect()).unwrap();
    drop(log);
    stdout(&run(p, &["audit", "anchor", "--project", "research", "--issuer-key", ".contextful/issuer.seed"]));
    let replicated = bucket.path().join("context-team/team/research/audit/roots/000001.json");
    assert_eq!(std::fs::read(&replicated).unwrap(), std::fs::read(chain_dir(p).join("segments/000001.root.json")).unwrap());
    let sent: Value = serde_json::from_str(&stdout(&run(p, &["audit", "replicate", "--project", "research"]))).unwrap();
    assert_eq!(sent["sent"], json!([]));
    std::fs::remove_file(&replicated).unwrap();
    let sent: Value = serde_json::from_str(&stdout(&run(p, &["audit", "replicate", "--project", "research"]))).unwrap();
    assert_eq!(sent["sent"], json!([1]));
    let end: Value = serde_json::from_str(&stdout(&run(p, &["audit", "verify", "--project", "research", "--bucket", "--public-key", &public]))).unwrap();
    assert_eq!(end["seq"], 5);

    let audit = chain_dir(p);
    for f in ["segments/000001.root.json", "segments/000002.root.json", "chain.held"] {
        std::fs::remove_file(audit.join(f)).unwrap();
    }
    let mut tip: Value = serde_json::from_str(&std::fs::read_to_string(audit.join("chain.tip")).unwrap()).unwrap();
    tip.as_object_mut().unwrap().remove("signature");
    std::fs::write(audit.join("chain.tip"), tip.to_string()).unwrap();
    stdout(&run(p, &["audit", "verify", "--project", "research"]));
    let err = stderr(&run(p, &["audit", "verify", "--project", "research", "--bucket"]));
    assert!(err.contains("AuditChainBroken: entry 2"), "{err}");
}

/// `audit verify --bucket` over a store declaring no `[sync]` refuses rather than passing.
#[test]
fn verify_against_the_bucket_needs_a_sync_block() {
    let (dir, _) = project();
    let p = dir.path();
    append(p, 4096, vec![read(Duration::from_secs(1), "user://ada@acme.example", &["research/notes"], attr::SERVED, 1)]);
    let err = stderr(&run(p, &["audit", "verify", "--project", "research", "--bucket"]));
    assert!(err.contains("[sync]"), "{err}");
}

/// `audit anchor` over a `[sync]` bucket that cannot open still signs, exports and prints
/// the chain end, names the failed copy on stderr, and leaves the roots for
/// `audit replicate`, which refuses until the bucket opens.
#[test]
fn anchor_keeps_its_roots_and_exits_zero_when_the_bucket_cannot_open() {
    let (dir, public) = project();
    let p = dir.path();
    let bucket = tempfile::tempdir().unwrap();
    declare_bucket(p, bucket.path());
    let config = p.join(".contextful/context/research/config.toml");
    let text = std::fs::read_to_string(&config).unwrap().replace("prefix = \"team\"", "prefix_from = \"CONTEXTFUL_TEST_UNSET_PREFIX\"");
    std::fs::write(&config, text).unwrap();
    let options = AuditOptions { header: ChainHeader { segment_entries: 2, ..ChainHeader::default() }, ..AuditOptions::default() };
    let log = AuditLog::unanchored_with(chain_dir(p), options).unwrap();
    log.append_all((0..3).map(|i| read(Duration::from_secs(10), "user://ada@acme.example", &["research/notes"], attr::SERVED, i)).collect()).unwrap();
    drop(log);

    let out = run(p, &["audit", "anchor", "--project", "research", "--issuer-key", ".contextful/issuer.seed"]);
    let end: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(end["seq"], 3);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("root replication") && err.contains("SyncPrefixUnbound"), "{err}");
    assert!(chain_dir(p).join("segments/000001.root.json").is_file());
    let verified: Value = serde_json::from_str(&stdout(&run(p, &["audit", "verify", "--project", "research", "--public-key", &public]))).unwrap();
    assert_eq!(verified, end);

    let err = stderr(&run(p, &["audit", "replicate", "--project", "research"]));
    assert!(err.contains("SyncPrefixUnbound"), "{err}");
    let sent: Value = serde_json::from_str(&stdout(&run_env(p, &["audit", "replicate", "--project", "research"], &[("CONTEXTFUL_TEST_UNSET_PREFIX", "team")]))).unwrap();
    assert_eq!(sent["sent"], json!([1]));
}
