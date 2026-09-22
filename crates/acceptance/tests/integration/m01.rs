//! Milestone 1 — the authority core.
//!
//! Reach: an authority is admitted once, travels as a value, and is re-read at the
//! effect about to act.

use contextful_acceptance::{bin, GitRepo};
use std::process::Output;

const AUD: &str = "contextful://acme-research";
const MINTED: &str = "2030-01-01T00:00:00Z";

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn refused(out: &Output, error: &str) {
    assert!(!out.status.success(), "expected {error}, got success: {}", String::from_utf8_lossy(&out.stdout));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains(error), "expected {error}, got: {stderr}");
}

#[test]
fn m01_authority_core() {
    let cf = bin("contextful");
    let p = GitRepo::init();
    p.write(
        ".contextful/issuance.toml",
        &format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 3600\n"),
    );

    let public = ok(&p.run(&cf, &["token", "keygen", "--out", ".contextful/issuer.seed"]));

    let mint = |extra: &[&str]| {
        let mut args = vec![
            "token", "mint", "--issuer-key", ".contextful/issuer.seed", "--now", MINTED,
            "--on-behalf-of", "user://dana@acme.example", "--agent", "agent://research-loop",
        ];
        args.extend_from_slice(extra);
        p.run(&cf, &args)
    };
    let verify = |token: &str, at: &str, extra: &[&str]| {
        let mut args = vec!["token", "verify", "--public-key", &public, "--audience", AUD, "--at", at, "--token", token];
        args.extend_from_slice(extra);
        p.run(&cf, &args)
    };

    // Admitted once: a mint within the persisted ceiling verifies into an authority value.
    let parent = ok(&mint(&["--action", "read", "--table", "research/*", "--ttl", "900"]));
    let admitted = ok(&verify(&parent, "2030-01-01T00:05:00Z", &[]));
    assert!(admitted.contains("user://dana@acme.example"), "{admitted}");
    assert!(admitted.contains("research/*"), "{admitted}");
    refused(&mint(&["--action", "read", "--table", "research/*", "--ttl", "7200"]), "IssuanceLifetimeAboveCeiling");

    // Travels as a value: a holder narrows it offline; widening refuses.
    let child = ok(&p.run(&cf, &["token", "attenuate", "--token", &parent, "--table", "research/filings"]));
    let narrowed = ok(&verify(&child, "2030-01-01T00:05:00Z", &[]));
    assert!(narrowed.contains("research/filings"), "{narrowed}");
    refused(&p.run(&cf, &["token", "attenuate", "--token", &parent, "--table", "sales/*"]), "AttenuationWidens");

    // Nobody signed a tampered chain, and a credential for another store is refused.
    let mut tampered = child.clone().into_bytes();
    let mid = tampered.len() / 2;
    tampered[mid] = if tampered[mid] == b'A' { b'B' } else { b'A' };
    refused(&verify(&String::from_utf8(tampered).unwrap(), "2030-01-01T00:05:00Z", &[]), "SignatureInvalid");
    refused(
        &p.run(&cf, &["token", "verify", "--public-key", &public, "--audience", "contextful://other", "--at", "2030-01-01T00:05:00Z", "--token", &child]),
        "AudienceMismatch",
    );

    // Re-read at the effect: past expiry, or once denylisted, the same value stops.
    refused(&verify(&child, "2030-01-01T00:20:00Z", &[]), "AuthorityExpired");
    let introspected = ok(&p.run(&cf, &["token", "introspect", "--token", &child]));
    let rev: serde_json_lite::Rev = serde_json_lite::rev(&introspected);
    p.write(".contextful/denylist", &format!("{}\n", rev.0));
    refused(&verify(&child, "2030-01-01T00:05:00Z", &["--denylist", ".contextful/denylist"]), "AuthorityRevoked");
    ok(&verify(&parent, "2030-01-01T00:05:00Z", &["--denylist", ".contextful/denylist"]));
}

/// The acceptance package links no JSON library; the revocation identifier is read off
/// introspection's `"rev_id": "<id>"` field by hand.
mod serde_json_lite {
    pub struct Rev(pub String);

    pub fn rev(json: &str) -> Rev {
        let key = "\"rev_id\"";
        let at = json.find(key).unwrap_or_else(|| panic!("introspection names no rev_id: {json}"));
        let rest = &json[at + key.len()..];
        let open = rest.find('"').unwrap() + 1;
        let close = rest[open..].find('"').unwrap() + open;
        Rev(rest[open..close].to_string())
    }
}
