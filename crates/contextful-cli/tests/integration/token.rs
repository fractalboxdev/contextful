//! `contextful token` through the built binary, against a scratch project.

use std::path::Path;
use std::process::{Command, Output};

const AUD: &str = "contextful://acme-research";
const MINTED: &str = "2030-01-01T00:00:00Z";

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".contextful")).unwrap();
    std::fs::write(
        dir.path().join(".contextful/issuance.toml"),
        format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 3600\n"),
    )
    .unwrap();
    dir
}

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful")).args(args).current_dir(dir).output().unwrap()
}

fn run_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_contextful"));
    cmd.args(args).current_dir(dir).env_remove("CONTEXTFUL_ISSUER_PUBKEY").env_remove("CONTEXTFUL_AUDIENCE");
    cmd.envs(env.iter().copied()).output().unwrap()
}

fn stdout(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn stderr(out: &Output) -> String {
    assert!(!out.status.success(), "expected a refusal: {}", String::from_utf8_lossy(&out.stdout));
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn keygen(dir: &Path) -> String {
    stdout(&run(dir, &["token", "keygen", "--out", ".contextful/issuer.seed"]))
}

fn mint(dir: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--now", MINTED];
    args.extend_from_slice(extra);
    run(dir, &args)
}

#[test]
fn keygen_prints_a_pin_the_verifier_accepts_and_refuses_to_overwrite_a_seed() {
    let p = project();
    let public = keygen(p.path());
    assert!(public.starts_with("ed25519/"), "{public}");
    assert!(stderr(&run(p.path(), &["token", "keygen", "--out", ".contextful/issuer.seed"])).contains("exists"));
}

#[test]
fn a_mint_with_no_persisted_issuance_policy_is_refused() {
    let p = project();
    keygen(p.path());
    std::fs::remove_file(p.path().join(".contextful/issuance.toml")).unwrap();
    let err = stderr(&mint(p.path(), &["--on-behalf-of", "user://dana@acme.example", "--table", "research/*"]));
    assert!(err.contains(".contextful/issuance.toml"), "{err}");
}

#[test]
fn a_mint_with_no_issuer_key_names_the_command_that_writes_one() {
    let p = project();
    let err = stderr(&run(p.path(), &["token", "mint", "--on-behalf-of", "user://dana@acme.example", "--table", "t"]));
    assert!(err.contains("IssuerKeyMissing") && err.contains("contextful token keygen"), "{err}");
}

#[test]
fn the_command_line_mint_trims_a_padded_subject_value() {
    let p = project();
    let public = keygen(p.path());
    let token = stdout(&mint(p.path(), &["--on-behalf-of", "  user://dana@acme.example ", "--table", "research/*"]));
    let admitted = stdout(&run(
        p.path(),
        &["token", "verify", "--public-key", &public, "--audience", AUD, "--at", "2030-01-01T00:01:00Z", "--token", &token],
    ));
    assert!(admitted.contains("\"user://dana@acme.example\""), "{admitted}");
}

#[test]
fn a_write_grant_minted_without_a_principal_is_refused() {
    let p = project();
    keygen(p.path());
    let err = stderr(&mint(p.path(), &["--action", "write", "--table", "research/filings"]));
    assert!(err.contains("IssuancePrincipalRequired"), "{err}");
}

#[test]
fn a_verify_without_an_instant_decodes_a_malformed_one_as_malformed() {
    let p = project();
    let public = keygen(p.path());
    let token = stdout(&mint(p.path(), &["--on-behalf-of", "user://dana@acme.example", "--table", "t"]));
    let err = stderr(&run(p.path(), &["token", "verify", "--public-key", &public, "--at", "2030-01-01 00:01:00", "--token", &token]));
    assert!(err.contains("TimestampMalformed"), "{err}");
}

#[test]
fn introspection_reports_declared_scope_and_the_chain_final_revocation_id() {
    let p = project();
    keygen(p.path());
    let parent = stdout(&mint(p.path(), &["--on-behalf-of", "user://dana@acme.example", "--table", "research/*"]));
    let child = stdout(&run(p.path(), &["token", "attenuate", "--token", &parent, "--table", "research/filings"]));
    let text = stdout(&run(p.path(), &["token", "introspect", "--token", &child]));
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    let ids = json["revocation_ids"].as_array().unwrap();
    assert_eq!(ids.len(), 2, "{text}");
    assert_eq!(json["rev_id"], ids[1], "{text}");
}

#[test]
fn a_mint_carries_a_tenant_scope_on_its_grant() {
    let p = project();
    keygen(p.path());
    let token = stdout(&mint(
        p.path(),
        &["--on-behalf-of", "user://dana@acme.example", "--table", "research/*", "--tenant", "research/notes=acme"],
    ));
    let text = stdout(&run(p.path(), &["token", "introspect", "--token", &token]));
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["authority"]["grants"][0]["tenant"], serde_json::json!({ "table": "research/notes", "value": "acme" }), "{text}");
    let err = stderr(&mint(p.path(), &["--on-behalf-of", "user://dana@acme.example", "--table", "research/*", "--tenant", "acme"]));
    assert!(err.contains("<table>=<value>"), "{err}");
}

/// A command-line checkpoint reads its pins from `--public-key`, else `CONTEXTFUL_ISSUER_PUBKEY`, and its audience from `--audience`, else `CONTEXTFUL_AUDIENCE`. A bare 64-hex pin reads as `ed25519/<hex>`; an unknown scheme prefix is refused as {{authority.verify.key-set-unavailable}}, naming the accepted forms.
// spec: authority.verify.pin-source@ec6e8f8a
#[test]
fn verify_reads_pins_and_audience_from_the_environment_and_accepts_a_bare_ed25519_key() {
    let p = project();
    let public = keygen(p.path());
    let token = stdout(&mint(p.path(), &["--on-behalf-of", "user://dana@acme.example", "--table", "t"]));
    let verify = ["token", "verify", "--at", "2030-01-01T00:01:00Z", "--token", &token];

    // No flags: the pins and the audience come from the environment.
    let admitted = stdout(&run_env(p.path(), &verify, &[("CONTEXTFUL_ISSUER_PUBKEY", &public), ("CONTEXTFUL_AUDIENCE", AUD)]));
    assert!(admitted.contains("\"user://dana@acme.example\""), "{admitted}");
    // The environment audience is checked, not ignored.
    let other = [("CONTEXTFUL_ISSUER_PUBKEY", public.as_str()), ("CONTEXTFUL_AUDIENCE", "contextful://other")];
    assert!(stderr(&run_env(p.path(), &verify, &other)).contains("AudienceMismatch"));
    // A flag wins over its variable.
    let mut flagged = verify.to_vec();
    flagged.extend_from_slice(&["--audience", AUD]);
    stdout(&run_env(p.path(), &flagged, &other));

    // A bare 64-hex key reads as an Ed25519 pin.
    let bare = public.strip_prefix("ed25519/").unwrap();
    assert_eq!(bare.len(), 64);
    let mut args = verify.to_vec();
    args.extend_from_slice(&["--public-key", bare, "--audience", AUD]);
    stdout(&run_env(p.path(), &args, &[]));

    // An unknown scheme prefix is a malformed pin whose message names the accepted forms.
    let rsa = format!("rsa/{bare}");
    let mut args = verify.to_vec();
    args.extend_from_slice(&["--public-key", &rsa]);
    let err = stderr(&run_env(p.path(), &args, &[]));
    assert!(err.contains("KeySetUnavailable") && err.contains("`rsa`") && err.contains("ed25519/<hex>"), "{err}");

    // Neither the flag nor the variable: the command names both.
    let err = stderr(&run_env(p.path(), &verify, &[]));
    assert!(err.contains("--public-key") && err.contains("CONTEXTFUL_ISSUER_PUBKEY"), "{err}");
}
