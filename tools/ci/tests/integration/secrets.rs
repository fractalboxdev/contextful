//! `assurance.gate.secret-ciphertext` and `assurance.gate.secret-scope`.

use crate::{stderr, Repo};
use std::process::{Command, Output};

const CIPHER: &str = "encrypted:BDqz0t1nH0mYbWc3Q+Zx9r2s";

fn secrets(r: &Repo) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful-ci"))
        .arg("secrets")
        .current_dir(&r.root)
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

#[test]
fn a_plaintext_value_in_a_tracked_env_file_is_refused() {
    let r = Repo::init();
    r.write(".env", "# GitHub PAT — repo:read on the demo repository, no expiry\nAPI_TOKEN=hunter2-plaintext\n");
    r.commit("env");
    let o = secrets(&r);
    assert!(!o.status.success());
    let err = stderr(&o);
    assert!(err.contains("SecretPlaintext"), "{err}");
    assert!(err.contains(".env") && err.contains("API_TOKEN"), "{err}");
    assert!(!err.contains("hunter2") && !stdout(&o).contains("hunter2"), "the value was printed: {err}");
}

#[test]
fn a_key_with_no_scope_comment_above_it_is_refused() {
    let r = Repo::init();
    r.write(
        ".env.staging",
        &format!("# Cloudflare token — Workers Scripts:Edit on the staging account\nFIRST=\"{CIPHER}\"\nSECOND=\"{CIPHER}\"\n\n#\nTHIRD={CIPHER}\n"),
    );
    r.commit("env");
    let o = secrets(&r);
    assert!(!o.status.success());
    let err = stderr(&o);
    assert!(err.contains("SecretScopeMissing"), "{err}");
    assert!(err.contains(".env.staging") && err.contains("THIRD"), "{err}");
    assert!(!err.contains("FIRST") && !err.contains("SECOND"), "a key under a scope comment was refused: {err}");
}

#[test]
fn a_tracked_env_keys_file_is_refused() {
    let r = Repo::init();
    r.write(".env.keys", "# private decryption keys\nDOTENV_PRIVATE_KEY=\"9f2c\"\n");
    r.commit("keys");
    let o = secrets(&r);
    assert!(!o.status.success());
    let err = stderr(&o);
    assert!(err.contains("SecretPlaintext") && err.contains(".env.keys"), "{err}");
    assert!(!err.contains("9f2c"), "the value was printed: {err}");
}

#[test]
fn ciphertext_keys_with_scope_comments_pass() {
    let r = Repo::init();
    r.write(
        ".env.production",
        &format!(
            "#/---[DOTENV_PUBLIC_KEY]---/\nDOTENV_PUBLIC_KEY_PRODUCTION=\"03ab77\"\n\n# npm token — publish on @demo, 90-day expiry\nNPM_TOKEN=\"{CIPHER}\"\n# AWS key — s3:GetObject on the demo bucket\nexport AWS_KEY={CIPHER}\n"
        ),
    );
    r.write("crates/demo/.env", &format!("# GCP — roles/viewer on the demo project\nGCP='{CIPHER}'\n"));
    r.commit("env");
    let o = secrets(&r);
    assert!(o.status.success(), "{}", stderr(&o));
    // The check saw the keys it passed: three secrets across two files.
    assert!(stdout(&o).contains("3 key(s) in 2 file(s)"), "{}", stdout(&o));
}

#[test]
fn an_untracked_env_file_is_out_of_scope() {
    let r = Repo::init();
    r.write(".env.dev", "TOKEN=plaintext-local\n");
    r.write(".env.keys", "DOTENV_PRIVATE_KEY=\"9f2c\"\n");
    assert!(r.root.join(".env.dev").exists());
    let o = secrets(&r);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stdout(&o).contains("0 key(s) in 0 file(s)"), "{}", stdout(&o));
}

#[test]
fn the_schema_stage_runs_the_secret_check() {
    let r = Repo::init();
    r.write(".env", "# scope\nAPI_TOKEN=plaintext\n");
    r.commit("env");
    let o = r.gate(&["--stage", "schema"]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("SecretPlaintext"), "{}", stderr(&o));
}

#[test]
fn one_comment_block_covers_the_keys_below_it() {
    let r = Repo::init();
    r.write(
        ".env",
        &format!("# Stripe pair — restricted key and its webhook secret, rotate together\nSTRIPE_KEY=\"{CIPHER}\"\nSTRIPE_WEBHOOK=\"{CIPHER}\"\n\nSTRIPE_ACCOUNT=\"{CIPHER}\"\n"),
    );
    r.commit("env");
    let o = secrets(&r);
    assert!(!o.status.success());
    let err = stderr(&o);
    assert!(err.contains("STRIPE_ACCOUNT"), "a key past the blank line kept its cover: {err}");
    assert!(!err.contains("STRIPE_WEBHOOK"), "a key under the block's comment was refused: {err}");
}

#[test]
fn an_example_file_carries_placeholders_in_plain_text() {
    let r = Repo::init();
    r.write(".env.example", "# Cloudflare token — Workers Scripts:Edit; fill from the operator vault\nCLOUDFLARE_API_TOKEN=\"<token>\"\n");
    r.commit("example");
    let o = secrets(&r);
    assert!(o.status.success(), "{}", stderr(&o));

    r.write(".env.example", "CLOUDFLARE_API_TOKEN=\"<token>\"\n");
    r.commit("uncommented");
    let o = secrets(&r);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("SecretScopeMissing"), "{}", stderr(&o));
}
