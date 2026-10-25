//! The `aws-secrets-manager` adapter, behind the `aws-secrets-manager` feature: it answers
//! `secret://<name>` with the `AWSCURRENT` value of secret id `CONTEXTFUL_AWS_SECRET_PREFIX`
//! plus the name, each request signed with Signature Version 4 under the workload role
//! (`connector.resolve.aws-secrets-manager`). An absent secret is a miss, so the chain
//! continues behind it.

use crate::client::{Client, HeaderValue};
use crate::role::RoleProvider;
use crate::sigv4::{sign, SigningKey};
use contextful_core::connector::attach::{scrub, Allowlist};
use contextful_core::connector::record::{Expiry, Observation, Observe, Unavailable};
use contextful_core::connector::reference::{Hydrated, SecretName};
use contextful_core::connector::resolve::{Answer, Provider};
use contextful_core::ports::Clock;
use contextful_core::run::{Failure, FailureTag};
use contextful_core::time::Instant;
use std::collections::BTreeMap;
use std::sync::Arc;
use url::Url;

/// The adapter's name in `CONTEXTFUL_SECRETS_BACKEND` and in attribution.
pub const NAME: &str = "aws-secrets-manager";
/// The text prefixed to every logical name to form its secret id.
pub const SECRET_PREFIX: &str = "CONTEXTFUL_AWS_SECRET_PREFIX";
/// Overrides the regional Secrets Manager endpoint.
pub const ENDPOINT: &str = "AWS_ENDPOINT_URL_SECRETS_MANAGER";
/// The version stage every answer reads.
const STAGE: &str = "AWSCURRENT";
/// The error type Secrets Manager answers for an absent secret.
const ABSENT: &str = "ResourceNotFoundException";

fn config(why: String) -> Failure {
    Failure::deterministic(FailureTag::Config, why)
}

/// What one Secrets Manager call came to.
enum Call {
    Answered(serde_json::Value),
    Absent,
}

/// The Secrets Manager adapter over the workload role.
pub struct AwsSecretsManager {
    endpoint: Url,
    region: String,
    prefix: String,
    role: Arc<RoleProvider>,
    clock: Arc<dyn Clock + Send + Sync>,
}

impl AwsSecretsManager {
    /// The adapter `vars` configure: the region from `AWS_REGION` or `AWS_DEFAULT_REGION`, the
    /// prefix from [`SECRET_PREFIX`], and the regional endpoint unless [`ENDPOINT`] names one.
    pub fn from_vars(vars: &BTreeMap<String, String>, clock: Arc<dyn Clock + Send + Sync>) -> Result<AwsSecretsManager, Failure> {
        let var = |k: &str| vars.get(k).filter(|v| !v.is_empty()).cloned();
        let region = var("AWS_REGION")
            .or_else(|| var("AWS_DEFAULT_REGION"))
            .ok_or_else(|| config(format!("the `{NAME}` backend is selected and neither `AWS_REGION` nor `AWS_DEFAULT_REGION` is set")))?;
        let base = var(ENDPOINT).unwrap_or_else(|| format!("https://secretsmanager.{region}.amazonaws.com"));
        let endpoint = Url::parse(&format!("{}/", base.trim_end_matches('/'))).map_err(|e| config(format!("`{ENDPOINT}` is not a URL: {e}")))?;
        let role = Arc::new(RoleProvider::new(vars.clone()).with_clock(clock.clone()));
        Ok(AwsSecretsManager { endpoint, region, prefix: var(SECRET_PREFIX).unwrap_or_default(), role, clock })
    }

    /// The secret id a logical name reads.
    fn secret_id(&self, name: &SecretName) -> String {
        format!("{}{}", self.prefix, name.as_str())
    }

    /// One signed call of `action` with `body`.
    fn call(&self, action: &str, body: &serde_json::Value) -> Result<Call, Failure> {
        let credential = self.role.credentials().map_err(|e| Failure::new(FailureTag::Transient, format!("the `{NAME}` backend signs under the workload role: {e}")))?;
        let payload = body.to_string();
        let host = match self.endpoint.port() {
            Some(port) => format!("{}:{port}", self.endpoint.host_str().unwrap_or_default()),
            None => self.endpoint.host_str().unwrap_or_default().to_string(),
        };
        let target = format!("secretsmanager.{action}");
        let plain = [("content-type", "application/x-amz-json-1.1"), ("host", host.as_str()), ("x-amz-target", target.as_str())];
        let key = SigningKey {
            access_key_id: credential.access_key_id.reveal(),
            secret_access_key: credential.secret_access_key.reveal(),
            session_token: credential.session_token.as_ref().map(Hydrated::reveal),
        };
        let signed = sign("POST", self.endpoint.path(), "", &plain, payload.as_bytes(), &key, &self.region, "secretsmanager", self.clock.now());
        let mut headers: Vec<(String, HeaderValue)> =
            plain.iter().filter(|(k, _)| *k != "host").map(|(k, v)| (k.to_string(), HeaderValue::Plain(v.to_string()))).collect();
        for (k, v) in signed {
            let value = if k == "x-amz-date" { HeaderValue::Plain(v) } else { HeaderValue::Sensitive(Hydrated::new(v).into()) };
            headers.push((k, value));
        }
        let allow = Allowlist::parse(&[self.endpoint.host_str().unwrap_or_default()]).map_err(|e| config(e.to_string()))?;
        let resp = Client::new(allow, self.endpoint.clone()).send_once("POST", &self.endpoint, &headers, Some(payload.as_bytes()))?;
        let answer: serde_json::Value = serde_json::from_slice(&resp.body).unwrap_or_default();
        if resp.status == 200 {
            return Ok(Call::Answered(answer));
        }
        let kind = answer.get("__type").and_then(|t| t.as_str()).unwrap_or_default();
        if resp.status == 400 && kind.rsplit('#').next() == Some(ABSENT) {
            return Ok(Call::Absent);
        }
        let what = format!("`{}` {action}", scrub(&self.endpoint));
        Err(match resp.status {
            429 | 500.. => Failure::new(FailureTag::Transient, format!("{what} answered {} {kind}", resp.status)),
            _ => Failure::deterministic(FailureTag::Permanent, format!("{what} answered {} {kind}", resp.status)),
        })
    }
}

impl Provider for AwsSecretsManager {
    fn name(&self) -> &str {
        NAME
    }

    fn answer(&self, name: &SecretName) -> Result<Option<Answer>, Failure> {
        let id = self.secret_id(name);
        let Call::Answered(v) = self.call("GetSecretValue", &serde_json::json!({ "SecretId": id, "VersionStage": STAGE }))? else { return Ok(None) };
        let value = v
            .get("SecretString")
            .and_then(|s| s.as_str())
            .ok_or_else(|| config(format!("secret `{id}` holds no `SecretString`; a credential is text")))?;
        let version = v.get("VersionId").and_then(|s| s.as_str()).map(str::to_string);
        Ok(Some(Answer { value: Hydrated::new(value), expires_at: None, version }))
    }
}

/// The observe half: `DescribeSecret`, which reads the secret's metadata and never its value.
impl Observe for AwsSecretsManager {
    fn adapter(&self) -> &str {
        NAME
    }

    fn observe(&self, name: &SecretName, at: Instant) -> Observation {
        let observed = |present, version, created| Observation::Observed { adapter: NAME.into(), at, present, version, created, expires: Expiry::Unknown };
        match self.call("DescribeSecret", &serde_json::json!({ "SecretId": self.secret_id(name) })) {
            Ok(Call::Absent) => observed(false, None, None),
            Ok(Call::Answered(v)) => {
                let version = v.get("VersionIdsToStages").and_then(|m| m.as_object()).and_then(|m| {
                    m.iter().find(|(_, stages)| stages.as_array().is_some_and(|s| s.iter().any(|x| x == STAGE))).map(|(id, _)| id.clone())
                });
                let created = v.get("CreatedDate").and_then(|d| d.as_f64()).and_then(|s| Instant::from_unix_secs(s as i64).ok());
                observed(true, version, created)
            }
            Err(f) if f.tag == FailureTag::Permanent => Observation::Unavailable { adapter: NAME.into(), at, reason: Unavailable::Denied(f.message) },
            Err(f) => Observation::Unavailable { adapter: NAME.into(), at, reason: Unavailable::Failed(f.message) },
        }
    }
}
