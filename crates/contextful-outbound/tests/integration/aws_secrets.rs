//! The `aws-secrets-manager` backend against a loopback Secrets Manager and container
//! credential endpoint, and the Signature Version 4 header signer against the AWS suite.

use crate::support::{at, name, proxy_env, Response, Server, SetClock, T0};
use contextful_core::connector::record::Observation;
use contextful_outbound::secrets::assemble_all;
use contextful_outbound::sigv4::{sign, SigningKey};
use std::collections::BTreeMap;
use std::sync::Arc;

/// The AWS Signature Version 4 suite's `get-vanilla` and `post-vanilla` requests sign to the
/// suite's published signatures.
#[test]
fn the_signer_reproduces_the_aws_suite_vectors() {
    let key = SigningKey { access_key_id: "AKIDEXAMPLE", secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY", session_token: None };
    let when = at("2015-08-30T12:36:00Z");
    for (method, signature) in [("GET", "5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"), ("POST", "5da7c1a2acd57cee7505fc6676e4e544621c30862966e37dddb68e92efbe5d6b")] {
        let headers = sign(method, "/", "", &[("Host", "example.amazonaws.com")], b"", &key, "us-east-1", "service", when);
        assert_eq!(headers[0], ("x-amz-date".to_string(), "20150830T123600Z".to_string()));
        assert_eq!(
            headers.last().unwrap().1,
            format!("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, SignedHeaders=host;x-amz-date, Signature={signature}")
        );
    }
    let temporary = SigningKey { session_token: Some("session"), ..key };
    let headers = sign("POST", "/", "", &[("host", "example.amazonaws.com")], b"{}", &temporary, "us-east-1", "service", when);
    assert!(headers.contains(&("x-amz-security-token".to_string(), "session".to_string())));
    assert!(headers.last().unwrap().1.contains("SignedHeaders=host;x-amz-date;x-amz-security-token"), "{headers:?}");
}

/// A loopback Secrets Manager holding `secrets` by id, answering an absent id as
/// `ResourceNotFoundException`.
fn manager(secrets: &'static [(&'static str, &'static str)]) -> Server {
    Server::start(move |r| {
        let body: serde_json::Value = serde_json::from_slice(&r.body).unwrap_or_default();
        let id = body["SecretId"].as_str().unwrap_or_default();
        let held = secrets.iter().find(|(k, _)| *k == id);
        match (r.header("x-amz-target"), held) {
            (_, None) => Response::json(400, r#"{"__type":"com.amazonaws.secretsmanager#ResourceNotFoundException","message":"Secrets Manager can't find the specified secret."}"#),
            (Some("secretsmanager.GetSecretValue"), Some((k, v))) => Response::json(200, &format!(r#"{{"ARN":"arn","Name":"{k}","SecretString":"{v}","VersionId":"v-{k}","VersionStages":["AWSCURRENT"]}}"#)),
            (Some("secretsmanager.DescribeSecret"), Some((k, _))) => {
                Response::json(200, &format!(r#"{{"Name":"{k}","CreatedDate":1893456000.25,"VersionIdsToStages":{{"v-old":["AWSPREVIOUS"],"v-{k}":["AWSCURRENT"]}}}}"#))
            }
            _ => Response::json(400, r#"{"__type":"InvalidRequestException"}"#),
        }
    })
}

fn role() -> Server {
    Server::start(|_| Response::json(200, r#"{"AccessKeyId":"ASIAROLE","SecretAccessKey":"role-secret","Token":"role-token","Expiration":"2999-01-01T00:00:00Z"}"#))
}

fn vars(manager: &Server, role: &Server, backend: &str) -> BTreeMap<String, String> {
    [
        ("CONTEXTFUL_SECRETS_BACKEND", backend.to_string()),
        ("CONTEXTFUL_AWS_SECRET_PREFIX", "prod/".to_string()),
        ("AWS_REGION", "eu-west-1".to_string()),
        ("AWS_ENDPOINT_URL_SECRETS_MANAGER", manager.url("")),
        ("AWS_CONTAINER_CREDENTIALS_FULL_URI", role.url("/creds")),
        ("VENDOR_TOKEN", "from-the-environment".to_string()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

/// The `aws-secrets-manager` backend answers `secret://<name>` with the `AWSCURRENT` value of secret id
/// `CONTEXTFUL_AWS_SECRET_PREFIX` plus the name, signing under the workload role; an absent secret is a miss.
// spec: connector.resolve.aws-secrets-manager@5ca293a6
#[test]
fn the_secrets_manager_backend_answers_the_current_value_signed_under_the_role() {
    let _env = proxy_env();
    let (sm, creds) = (manager(&[("prod/vendor-token", "from-the-manager"), ("prod/api-key", "k-123")]), role());
    let assembly = assemble_all(&vars(&sm, &creds, "aws-secrets-manager"), Arc::new(SetClock::new())).unwrap();
    assert_eq!(assembly.resolver.resolve(&name("api-key")).unwrap().reveal(), "k-123");
    let sent = &sm.received("/")[0];
    let body: serde_json::Value = serde_json::from_slice(&sent.body).unwrap();
    assert_eq!(body, serde_json::json!({"SecretId": "prod/api-key", "VersionStage": "AWSCURRENT"}));
    assert_eq!(sent.header("x-amz-target"), Some("secretsmanager.GetSecretValue"));
    assert_eq!(sent.header("content-type"), Some("application/x-amz-json-1.1"));
    assert_eq!(sent.header("x-amz-security-token"), Some("role-token"));
    assert_eq!(sent.header("x-amz-date"), Some("20300101T000000Z"));
    let authorization = sent.header("authorization").unwrap();
    assert!(
        authorization.starts_with("AWS4-HMAC-SHA256 Credential=ASIAROLE/20300101/eu-west-1/secretsmanager/aws4_request, SignedHeaders=content-type;host;x-amz-date;x-amz-security-token;x-amz-target, Signature="),
        "{authorization}"
    );
    assert_eq!(assembly.resolver.attribution().get("api-key").map(String::as_str), Some("aws-secrets-manager"));

    // An absent secret is a miss: the environment behind it answers, and alone it leaves the name unresolved.
    let behind = assemble_all(&vars(&sm, &creds, "aws-secrets-manager,env"), Arc::new(SetClock::new())).unwrap();
    let both = behind.resolver.resolve(&name("vendor-token")).unwrap_err();
    assert!(both.message.contains("SecretNameShadowed") && both.message.contains("aws-secrets-manager"), "{}", both.message);
    let mut only_env = vars(&sm, &creds, "aws-secrets-manager,env");
    only_env.insert("OTHER_TOKEN".into(), "env-only".into());
    let fallthrough = assemble_all(&only_env, Arc::new(SetClock::new())).unwrap();
    assert_eq!(fallthrough.resolver.resolve(&name("other-token")).unwrap().reveal(), "env-only");
    let missing = assembly.resolver.resolve(&name("absent-name")).unwrap_err();
    assert!(missing.message.contains("SecretUnresolvedReference"), "{}", missing.message);
}

/// The observe half reads `DescribeSecret`: presence, the `AWSCURRENT` version and the creation
/// time, never the value.
#[test]
fn the_secrets_manager_observe_half_reads_metadata_alone() {
    let _env = proxy_env();
    let (sm, creds) = (manager(&[("prod/api-key", "k-123")]), role());
    let assembly = assemble_all(&vars(&sm, &creds, "aws-secrets-manager"), Arc::new(SetClock::new())).unwrap();
    assert_eq!(assembly.configured(&name("api-key")), ["aws-secrets-manager"]);
    match assembly.observers[0].observe(&name("api-key"), at(T0)) {
        Observation::Observed { adapter, present, version, created, .. } => {
            assert_eq!((adapter.as_str(), present, version.as_deref()), ("aws-secrets-manager", true, Some("v-prod/api-key")));
            assert_eq!(created, Some(at("2030-01-01T00:00:00Z")));
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(assembly.observers[0].observe(&name("absent-name"), at(T0)), Observation::Observed { present: false, .. }));
    let targets: Vec<Option<String>> = sm.requests.lock().unwrap().iter().map(|r| r.header("x-amz-target").map(str::to_string)).collect();
    assert_eq!(targets, [Some("secretsmanager.DescribeSecret".to_string()), Some("secretsmanager.DescribeSecret".to_string())]);

    // The backend selected without a region refuses at assembly.
    let mut regionless = vars(&sm, &creds, "aws-secrets-manager");
    regionless.remove("AWS_REGION");
    let refused = assemble_all(&regionless, Arc::new(SetClock::new())).err().unwrap();
    assert!(refused.message.contains("AWS_REGION"), "{}", refused.message);
}
