//! `contextful sync` over an S3 endpoint: credential binding, plaintext and the secrets chain.
#![cfg(feature = "s3-sync")]

use super::{cf, files, ok, project, refused, seed, LANDED};
use contextful_acceptance::s3::{S3Server, ACCESS_KEY, SECRET_KEY};

fn s3(endpoint: &str, keys: &str) -> String {
    format!("endpoint = \"{endpoint}\"\nbucket = \"context-team\"\nprefix = \"team\"\n{keys}")
}

/// An S3 endpoint refuses before a request leaves: an unset credential variable, a literal key, and plaintext
/// off loopback.
#[test]
fn an_s3_endpoint_refuses_unbound_credentials_and_plaintext() {
    let unset = project("ingest-a", &s3("http://127.0.0.1:9", "access_key_id = \"env://CONTEXTFUL_TEST_UNSET_KEY\"\nsecret_access_key = \"env://CONTEXTFUL_TEST_UNSET_SECRET\"\n"));
    refused(&cf(unset.path(), &["sync", "probe", "--project", "research"], &[]), "SyncCredentialUnbound");
    let literal = project("ingest-a", &s3("r2://account", "access_key_id = \"AKIAIOSFODNN7EXAMPLE\"\nsecret_access_key = \"env://S\"\n"));
    let out = cf(literal.path(), &["sync", "push", "--project", "research"], &[]);
    refused(&out, "SyncCredentialUnbound");
    assert!(!String::from_utf8_lossy(&out.stderr).contains("AKIAIOSFODNN7EXAMPLE"), "the refusal never repeats the literal");
    let plaintext = project("ingest-a", &s3("http://objects.example.org", "access_key_id = \"env://A\"\nsecret_access_key = \"env://S\"\n"));
    refused(&cf(plaintext.path(), &["sync", "pull", "--project", "research"], &[]), "SyncEndpointInsecure");
    // A `secret://` reference no assembled adapter answers refuses by name.
    let unresolved = project("ingest-a", &s3("http://127.0.0.1:9", "access_key_id = \"secret://sync-key-id\"\nsecret_access_key = \"secret://sync-secret\"\n"));
    refused(&cf(unresolved.path(), &["sync", "probe", "--project", "research"], &[]), "SecretUnresolvedReference");
}

/// `[sync] access_key_id`, `secret_access_key` and the optional `session_token` each bind `secret://<name>`,
/// hydrated as `connector.reference.whole-value-reference`, or `env://NAME`, read whole from the process
/// environment, as the bucket opens.
// spec: store.endpoint.credentials@d97d52c7
#[test]
fn secret_references_hydrate_through_the_default_chain() {
    let server = S3Server::start("context-team");
    let keys = "access_key_id = \"secret://sync-key-id\"\nsecret_access_key = \"secret://sync-secret\"\n";
    let env = [("SYNC_KEY_ID", ACCESS_KEY), ("SYNC_SECRET", SECRET_KEY)];
    // The default chain is the environment alone, with no template opt-in: a whole value still answers.
    let _a = seed(&s3(&server.endpoint, keys), &env);
    assert!(server.keys().iter().any(|k| k.ends_with(LANDED)), "{:?}", server.keys());
    let b = project("ingest-b", &s3(&server.endpoint, keys));
    ok(&cf(b.path(), &["sync", "pull", "--project", "research"], &env));
    assert_eq!(files(b.path()), [LANDED]);
    // `env://` reads the variable itself.
    let direct = project("ingest-c", &s3(&server.endpoint, "access_key_id = \"env://SYNC_KEY_ID\"\nsecret_access_key = \"env://SYNC_SECRET\"\n"));
    ok(&cf(direct.path(), &["sync", "pull", "--project", "research"], &env));
    assert_eq!(files(direct.path()), [LANDED]);
}

/// A bucket name the endpoint does not hold fails by name, never as an absent object or a held lease.
#[test]
fn a_missing_bucket_fails_by_name() {
    let server = S3Server::start("context-team");
    let typo = project(
        "ingest-a",
        "endpoint = \"ENDPOINT\"\nbucket = \"context-typo\"\nprefix = \"team\"\naccess_key_id = \"env://SYNC_KEY_ID\"\nsecret_access_key = \"env://SYNC_SECRET\"\n"
            .replace("ENDPOINT", &server.endpoint)
            .as_str(),
    );
    let env = [("SYNC_KEY_ID", ACCESS_KEY), ("SYNC_SECRET", SECRET_KEY)];
    let out = cf(typo.path(), &["sync", "lease", "release", "filings", "--project", "research"], &env);
    refused(&out, "NoSuchBucket");
    assert!(!String::from_utf8_lossy(&out.stderr).contains("LeaseNotHeld"), "{}", String::from_utf8_lossy(&out.stderr));
    refused(&cf(typo.path(), &["sync", "pull", "--project", "research"], &env), "NoSuchBucket");
}
