//! Workload-role credentials: the container endpoint, a web identity token exchanged
//! through STS, then IMDSv2, each served by a loopback server, and the refresh ahead of
//! a credential's stated expiry.

use crate::support::{proxy_env, Response, Server, SetClock};
use contextful_core::ports::Clock;
use contextful_outbound::role::RoleProvider;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// A container or IMDS credential document expiring at `expires`.
fn document(id: &str, expires: &str) -> String {
    format!(r#"{{"Code":"Success","AccessKeyId":"{id}","SecretAccessKey":"secret-{id}","Token":"token-{id}","Expiration":"{expires}"}}"#)
}

fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// A metadata endpoint that refuses every connection, so the IMDS source fails at once.
const CLOSED: &str = "http://127.0.0.1:9";

/// A relative container URI resolves under the container base and answers first; a full URI
/// carries the authorization token its file holds.
#[test]
fn the_container_endpoint_answers_first_with_its_authorization_token() {
    let _env = proxy_env();
    let container = Server::start(|_| Response::json(200, &document("ASIACONTAINER", "2030-01-01T06:00:00Z")));
    let provider = RoleProvider::new(vars(&[
        ("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI", "/v2/credentials/task"),
        ("AWS_WEB_IDENTITY_TOKEN_FILE", "/nonexistent"),
        ("AWS_ROLE_ARN", "arn:aws:iam::000000000000:role/r"),
        ("AWS_EC2_METADATA_SERVICE_ENDPOINT", CLOSED),
    ]))
    .with_container_base(&container.url(""))
    .with_clock(Arc::new(SetClock::new()));
    let c = provider.credentials().unwrap();
    assert_eq!((c.access_key_id.reveal(), c.secret_access_key.reveal()), ("ASIACONTAINER", "secret-ASIACONTAINER"));
    assert_eq!(c.session_token.as_ref().map(|t| t.reveal()), Some("token-ASIACONTAINER"));
    assert_eq!(container.received("/v2/credentials/task").len(), 1);

    let dir = tempfile::tempdir().unwrap();
    let token = dir.path().join("token");
    std::fs::write(&token, "pod-identity-token\n").unwrap();
    let full = RoleProvider::new(vars(&[
        ("AWS_CONTAINER_CREDENTIALS_FULL_URI", &container.url("/pod")),
        ("AWS_CONTAINER_AUTHORIZATION_TOKEN", "ignored-beside-the-file"),
        ("AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE", token.to_str().unwrap()),
    ]))
    .with_clock(Arc::new(SetClock::new()));
    assert_eq!(full.credentials().unwrap().access_key_id.reveal(), "ASIACONTAINER");
    assert_eq!(container.received("/pod")[0].header("authorization"), Some("pod-identity-token"));

    // A cleartext full URI off loopback and the container hosts never receives the token.
    let foreign = RoleProvider::new(vars(&[
        ("AWS_CONTAINER_CREDENTIALS_FULL_URI", "http://credentials.example.org/creds"),
        ("AWS_CONTAINER_AUTHORIZATION_TOKEN", "t"),
        ("AWS_EC2_METADATA_SERVICE_ENDPOINT", CLOSED),
    ]));
    let why = foreign.credentials().unwrap_err().0;
    assert!(why.contains("credentials.example.org") && why.contains("neither TLS"), "{why}");
}

/// Without a container endpoint, a web identity token is exchanged through an unsigned
/// `AssumeRoleWithWebIdentity` at the STS endpoint.
#[test]
fn a_web_identity_token_is_exchanged_through_sts() {
    let _env = proxy_env();
    let sts = Server::start(|_| Response {
        status: 200,
        headers: vec![("Content-Type".into(), "text/xml".into())],
        body: br#"<AssumeRoleWithWebIdentityResponse><AssumeRoleWithWebIdentityResult><Credentials><AccessKeyId>ASIAWEBIDENTITY</AccessKeyId><SecretAccessKey>web-secret</SecretAccessKey><SessionToken>web-token</SessionToken><Expiration>2030-01-01T01:00:00Z</Expiration></Credentials></AssumeRoleWithWebIdentityResult></AssumeRoleWithWebIdentityResponse>"#.to_vec(),
    });
    let dir = tempfile::tempdir().unwrap();
    let token = dir.path().join("token");
    std::fs::write(&token, "service-account-jwt").unwrap();
    let provider = RoleProvider::new(vars(&[
        ("AWS_WEB_IDENTITY_TOKEN_FILE", token.to_str().unwrap()),
        ("AWS_ROLE_ARN", "arn:aws:iam::000000000000:role/reader"),
        ("AWS_ROLE_SESSION_NAME", "reader-session"),
        ("AWS_ENDPOINT_URL_STS", &sts.url("")),
        ("AWS_EC2_METADATA_SERVICE_ENDPOINT", CLOSED),
    ]))
    .with_clock(Arc::new(SetClock::new()));
    let c = provider.credentials().unwrap();
    assert_eq!((c.access_key_id.reveal(), c.secret_access_key.reveal()), ("ASIAWEBIDENTITY", "web-secret"));
    assert_eq!(c.session_token.as_ref().map(|t| t.reveal()), Some("web-token"));
    let sent = &sts.received("/")[0];
    let form = String::from_utf8_lossy(&sent.body).to_string();
    for part in ["Action=AssumeRoleWithWebIdentity", "Version=2011-06-15", "RoleArn=arn%3Aaws%3Aiam%3A%3A000000000000%3Arole%2Freader", "RoleSessionName=reader-session", "WebIdentityToken=service-account-jwt"] {
        assert!(form.contains(part), "{form}");
    }
    assert!(sent.header("authorization").is_none(), "the exchange is unsigned");
}

/// Without a container endpoint or a web identity, IMDSv2 answers: a session token, the role
/// name, then the role's credentials, each read under the token.
#[test]
fn imds_answers_last_under_a_session_token() {
    let _env = proxy_env();
    let imds = Server::start(|r| match (r.method.as_str(), r.path()) {
        ("PUT", "/latest/api/token") if r.header("x-aws-ec2-metadata-token-ttl-seconds").is_some() => Response::json(200, "imds-session"),
        ("GET", _) if r.header("x-aws-ec2-metadata-token") != Some("imds-session") => Response::json(401, ""),
        ("GET", "/latest/meta-data/iam/security-credentials/") => Response::json(200, "instance-role\n"),
        ("GET", "/latest/meta-data/iam/security-credentials/instance-role") => Response::json(200, &document("ASIAINSTANCE", "2030-01-01T06:00:00Z")),
        _ => Response::json(404, ""),
    });
    let provider = RoleProvider::new(vars(&[("AWS_EC2_METADATA_SERVICE_ENDPOINT", &imds.url(""))])).with_clock(Arc::new(SetClock::new()));
    assert_eq!(provider.credentials().unwrap().access_key_id.reveal(), "ASIAINSTANCE");
    assert_eq!(imds.requests.lock().unwrap().len(), 3);

    let disabled = RoleProvider::new(vars(&[("AWS_EC2_METADATA_SERVICE_ENDPOINT", &imds.url("")), ("AWS_EC2_METADATA_DISABLED", "true")]));
    let why = disabled.credentials().unwrap_err().0;
    for source in ["container:", "web-identity:", "imds:"] {
        assert!(why.contains(source), "{why}");
    }
    assert_eq!(imds.requests.lock().unwrap().len(), 3, "a disabled metadata source sends nothing");
}

/// Role credentials refresh 300 s before their stated expiry, and no request signs with expired
/// credentials. A failed refresh keeps still-valid credentials and retries at the next request.
// spec: store.endpoint.role-refresh@932b0a79
#[test]
fn role_credentials_refresh_ahead_of_expiry_and_a_failed_refresh_keeps_valid_ones() {
    let _env = proxy_env();
    // The container answers a credential expiring one hour after each answer's issue time, and
    // fails while `down` is set.
    let clock = SetClock::new();
    let served = Arc::new(AtomicUsize::new(0));
    let down = Arc::new(AtomicUsize::new(0));
    let (at, n, off) = (clock.clone(), served.clone(), down.clone());
    let container = Server::start(move |_| {
        if off.load(Ordering::SeqCst) == 1 {
            return Response::json(500, "");
        }
        let k = n.fetch_add(1, Ordering::SeqCst);
        Response::json(200, &document(&format!("ASIA{k}"), &at.now().plus_secs(3600).to_rfc3339()))
    });
    let provider = RoleProvider::new(vars(&[("AWS_CONTAINER_CREDENTIALS_FULL_URI", &container.url("/creds")), ("AWS_EC2_METADATA_SERVICE_ENDPOINT", CLOSED)]))
        .with_clock(Arc::new(clock.clone()));
    let id = || provider.credentials().map(|c| c.access_key_id.reveal().to_string());

    assert_eq!(id().unwrap(), "ASIA0");
    clock.advance(3600 - 301);
    assert_eq!(id().unwrap(), "ASIA0", "301 s before expiry the held credential signs");
    assert_eq!(container.received("/creds").len(), 1);
    clock.advance(2);
    assert_eq!(id().unwrap(), "ASIA1", "299 s before expiry the provider refreshes");
    assert_eq!(container.received("/creds").len(), 2);

    // A failed refresh keeps the still-valid credential and retries at the next request.
    down.store(1, Ordering::SeqCst);
    clock.advance(3600 - 200);
    assert_eq!(id().unwrap(), "ASIA1");
    assert_eq!(id().unwrap(), "ASIA1");
    assert_eq!(container.received("/creds").len(), 4, "each request inside the lead retries the refresh");

    // Past expiry, a failed refresh answers no credential at all.
    clock.advance(201);
    let why = id().unwrap_err();
    assert!(why.0.contains("container: the container endpoint answered 500"), "{why}");

    // The source recovering answers again.
    down.store(0, Ordering::SeqCst);
    assert_eq!(id().unwrap(), "ASIA2");
}
