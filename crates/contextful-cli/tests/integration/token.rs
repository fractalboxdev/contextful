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

fn policy_text(dir: &Path) -> String {
    std::fs::read_to_string(dir.join(".contextful/issuance.toml")).unwrap()
}

fn verify_at(dir: &Path, pins: &str, at: &str, token: &str) -> Output {
    run(dir, &["token", "verify", "--public-key", pins, "--audience", AUD, "--at", at, "--token", token])
}

/// `contextful token policy init` writes the issuance policy naming its audience with a ceiling of the {{authority.verify.bearer-lifetime}} bound, and refuses to overwrite an existing policy.
// spec: authority.issue.policy-init@25493aa4
#[test]
fn a_fresh_project_writes_its_policy_and_mints_under_the_default_issuer_key() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    let public = stdout(&run(p, &["token", "keygen"]));
    assert!(p.join(".contextful/issuer.seed").is_file());
    stdout(&run(p, &["token", "policy", "init", "--audience", AUD]));
    let text = policy_text(p);
    assert!(text.contains(&format!("default_audience = \"{AUD}\"")) && text.contains("max_lifetime_secs = 3600"), "{text}");
    assert!(stderr(&run(p, &["token", "policy", "init", "--audience", AUD])).contains("exists"));
    assert_eq!(policy_text(p), text, "an existing policy is left as it is");

    let token = stdout(&run(p, &["token", "mint", "--on-behalf-of", "user://dana@acme.example", "--table", "t", "--now", MINTED]));
    stdout(&verify_at(p, &public, "2030-01-01T00:01:00Z", &token));
}

/// A mint naming no issuer key reads `.contextful/issuer.seed`; no file there raises {{authority.issue.missing-key}}.
// spec: authority.issue.default-key@b934db75
#[test]
fn a_mint_naming_no_issuer_key_reads_the_default_seed() {
    let p = project();
    let err = stderr(&run(p.path(), &["token", "mint", "--on-behalf-of", "user://dana@acme.example", "--table", "t"]));
    assert!(err.contains("IssuerKeyMissing") && err.contains("contextful token keygen"), "{err}");
    let public = keygen(p.path());
    let token = stdout(&run(p.path(), &["token", "mint", "--on-behalf-of", "user://dana@acme.example", "--table", "t", "--now", MINTED]));
    stdout(&verify_at(p.path(), &public, "2030-01-01T00:01:00Z", &token));
}

/// Lowering the ceiling records the previous value and its instant; rotation-grace validation uses the recorded value until the last credential minted under it lapses.
// spec: authority.issue.ceiling-lowering@776caba7
#[test]
fn lower_ceiling_records_the_previous_value_and_rotation_grace_answers_to_it() {
    let p = project();
    std::fs::write(p.path().join(".contextful/issuance.toml"), format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 86400\n")).unwrap();
    stdout(&run(p.path(), &["token", "keygen", "--now", "2029-01-01T00:00:00Z"]));
    stdout(&run(p.path(), &["token", "policy", "lower-ceiling", "--max-lifetime-secs", "3600", "--now", MINTED]));
    let text = policy_text(p.path());
    assert!(text.contains("max_lifetime_secs = 3600") && text.contains("previous_max_lifetime_secs = 86400") && text.contains(MINTED), "{text}");
    // Only a lower value lowers.
    assert!(stderr(&run(p.path(), &["token", "policy", "lower-ceiling", "--max-lifetime-secs", "7200", "--now", MINTED])).contains("3600"));
    assert!(stderr(&mint(p.path(), &["--on-behalf-of", "user://dana@acme.example", "--table", "t", "--ttl", "7200"])).contains("IssuanceLifetimeAboveCeiling"));

    // A 2 h grace window is too short while a 24 h credential minted before the lowering can live.
    let rotate = |now: &str| run(p.path(), &["token", "rotate", "--grace-secs", "7200", "--now", now]);
    assert!(stderr(&rotate("2030-01-01T23:59:59Z")).contains("RotationGraceTooShort"));
    stdout(&rotate("2030-01-02T00:00:00Z"));
}

/// The issuer signing key rotates every 90 d, and at once on suspected compromise.
// spec: authority.issue.key-rotation@1080980b
#[test]
fn token_rotate_replaces_the_issuer_key_after_90_days_and_at_once_on_compromise() {
    let p = project();
    let first = stdout(&run(p.path(), &["token", "keygen", "--now", MINTED]));
    let err = stderr(&run(p.path(), &["token", "rotate", "--now", "2030-03-31T23:59:59Z"]));
    assert!(err.contains("2030-04-01T00:00:00Z"), "the refusal names when rotation falls due: {err}");
    let second = stdout(&run(p.path(), &["token", "rotate", "--now", "2030-04-01T00:00:00Z"]));
    assert_ne!(first, second);
    let third = stdout(&run(p.path(), &["token", "rotate", "--compromise", "--now", "2030-04-01T00:00:01Z"]));
    assert_ne!(second, third);
    // The seed holds the newest key: a fresh mint verifies under it alone.
    let token = stdout(&run(p.path(), &["token", "mint", "--on-behalf-of", "user://dana@acme.example", "--table", "t", "--now", "2030-04-01T00:00:02Z"]));
    stdout(&verify_at(p.path(), &third, "2030-04-01T00:00:03Z", &token));
}

/// Suspected compromise of signing material runs a project-wide epoch bump together with immediate retirement of the key version. Waiting out a grace window withdraws nothing.
// spec: authority.revoke.compromise@c14a17b8
#[test]
fn a_compromise_bumps_the_project_epoch_and_retires_the_key_at_once() {
    let p = project();
    let old = keygen(p.path());
    let before = stdout(&mint(p.path(), &["--on-behalf-of", "user://dana@acme.example", "--table", "t"]));
    std::fs::copy(p.path().join(".contextful/issuer.seed"), p.path().join("stolen.seed")).unwrap();
    let new = stdout(&run(p.path(), &["token", "rotate", "--compromise", "--now", "2030-01-01T00:00:30Z"]));
    let pins = format!("{old},{new}");

    // The retired key verifies nothing, though the pins still carry it.
    let err = stderr(&verify_at(p.path(), &pins, "2030-01-01T00:01:00Z", &before));
    assert!(err.contains("SignatureInvalid"), "{err}");
    // A credential the stolen seed signs after the bump carries the new epoch, and still admits nothing.
    let forged = stdout(&run(p.path(), &["token", "mint", "--issuer-key", "stolen.seed", "--on-behalf-of", "user://dana@acme.example", "--table", "t", "--now", MINTED]));
    assert!(!verify_at(p.path(), &pins, "2030-01-01T00:01:00Z", &forged).status.success());
    // The bump is project-wide: the store records it on the project's audience alone.
    let keyset = std::fs::read_to_string(p.path().join(".contextful/keyset.toml")).unwrap();
    assert!(keyset.contains(&format!("project = \"{AUD}\"")) && keyset.contains("epoch = 1"), "{keyset}");

    let after = stdout(&mint(p.path(), &["--on-behalf-of", "user://dana@acme.example", "--table", "t"]));
    let admitted = stdout(&verify_at(p.path(), &pins, "2030-01-01T00:01:00Z", &after));
    assert!(admitted.contains("\"epoch\":1") || admitted.contains("\"epoch\": 1"), "{admitted}");
}

/// A project persists each scope's current epoch and each issuer key version's retirement in `.contextful/keyset.toml` beside the issuer seed; a mint stamps its scope's current epoch, and a checkpoint re-reads the file at each admission.
// spec: authority.revoke.epoch-store@275939f3
#[test]
fn token_revoke_bumps_one_scoped_epoch_and_a_later_mint_carries_it() {
    let p = project();
    let public = keygen(p.path());
    let tenant = |value: &str| {
        stdout(&mint(p.path(), &["--on-behalf-of", "user://dana@acme.example", "--table", "research/*", "--tenant", &format!("research/notes={value}")]))
    };
    let (eu, us) = (tenant("acme-eu"), tenant("acme-us"));
    assert_eq!(stdout(&run(p.path(), &["token", "revoke", "--tenant", "acme-eu"])), "epoch 1");
    let err = stderr(&verify_at(p.path(), &public, "2030-01-01T00:01:00Z", &eu));
    assert!(err.contains("AuthorityRevoked"), "{err}");
    stdout(&verify_at(p.path(), &public, "2030-01-01T00:01:00Z", &us));
    stdout(&verify_at(p.path(), &public, "2030-01-01T00:01:00Z", &tenant("acme-eu")));
    // A verifier pointed at another store reads no bump.
    std::fs::write(p.path().join("empty.toml"), "").unwrap();
    stdout(&run(p.path(), &["token", "verify", "--public-key", &public, "--audience", AUD, "--at", "2030-01-01T00:01:00Z", "--keyset", "empty.toml", "--token", &eu]));
}
