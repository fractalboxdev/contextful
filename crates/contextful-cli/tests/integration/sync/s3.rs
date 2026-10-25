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

/// A loopback container credential endpoint answering the S3 server's key pair as a role
/// credential with a session token, counting each answer. A cleartext loopback full URI is one
/// the role provider admits.
pub(crate) fn container_endpoint() -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}/v2/credentials", listener.local_addr().unwrap().port());
    let served = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = served.clone();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 0) && line != "\r\n" {
                line.clear();
            }
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let body = format!(r#"{{"AccessKeyId":"{ACCESS_KEY}","SecretAccessKey":"{SECRET_KEY}","Token":"task-role-session","Expiration":"2999-01-01T00:00:00Z"}}"#);
            let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        }
    });
    (url, served)
}

/// The variables that leave every role source unconfigured, the metadata endpoint pointing at a
/// closed loopback port. A test sets the full container URI, index 1, to configure one.
pub(crate) const NO_ROLE: [(&str, &str); 5] = [
    ("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI", ""),
    ("AWS_CONTAINER_CREDENTIALS_FULL_URI", ""),
    ("AWS_WEB_IDENTITY_TOKEN_FILE", ""),
    ("AWS_ROLE_ARN", ""),
    ("AWS_EC2_METADATA_SERVICE_ENDPOINT", "http://127.0.0.1:9"),
];

/// `[sync] credentials = "role"` signs with workload-role credentials in place of both keys: a push and a cold
/// pull each sign with the container endpoint's credential, which the backend verifies.
// spec: store.endpoint.role-credentials@c7d11c89
#[test]
fn role_credentials_sign_a_push_and_a_pull() {
    let server = S3Server::start("context-team");
    let (url, served) = container_endpoint();
    let mut env = NO_ROLE.to_vec();
    env[1].1 = &url;
    let sync = s3(&server.endpoint, "credentials = \"role\"\n");
    let _a = seed(&sync, &env);
    assert!(server.keys().iter().any(|k| k.ends_with(LANDED)), "{:?}", server.keys());
    let b = project("ingest-b", &sync);
    ok(&cf(b.path(), &["sync", "pull", "--project", "research"], &env));
    assert_eq!(files(b.path()), [LANDED]);
    assert!(served.load(std::sync::atomic::Ordering::SeqCst) >= 2, "each process fetches its role credential");
}

/// Under role credentials, a bucket open that no role source answers raises `SyncRoleUnavailable`, naming each
/// source tried, before any request reaches the bucket; a key beside the role, or an R2 endpoint, conflicts.
// spec: store.endpoint.role-unavailable@a599a0b5
#[test]
fn a_role_no_source_answers_refuses_naming_each_source() {
    let server = S3Server::start("context-team");
    let none = project("ingest-a", &s3(&server.endpoint, "credentials = \"role\"\n"));
    let out = cf(none.path(), &["sync", "push", "--project", "research"], &NO_ROLE);
    refused(&out, "SyncRoleUnavailable");
    let said = String::from_utf8_lossy(&out.stderr);
    for source in ["container:", "web-identity:", "imds:"] {
        assert!(said.contains(source), "{said}");
    }
    assert!(server.keys().is_empty() && server.puts() == 0, "no request reaches the bucket");
    let both = project("ingest-a", &s3(&server.endpoint, "credentials = \"role\"\naccess_key_id = \"env://SYNC_KEY_ID\"\n"));
    refused(&cf(both.path(), &["sync", "pull", "--project", "research"], &NO_ROLE), "SyncCredentialConflict");
    let r2 = project("ingest-a", &s3("r2://account", "credentials = \"role\"\n"));
    refused(&cf(r2.path(), &["sync", "probe", "--project", "research"], &NO_ROLE), "SyncCredentialConflict");
}
