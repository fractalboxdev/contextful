//! Workload-role credentials, behind the `transport-ureq` feature: the ECS container
//! credential endpoint, then a web identity token exchanged through STS, then IMDSv2
//! (`store.endpoint.role-credentials`). One provider caches what a source answered and
//! refreshes it ahead of its stated expiry (`store.endpoint.role-refresh`).
//!
//! Each fetch is a host-side request to the workload's own credential source, never a
//! source request, so it bypasses the mediated client and any configured proxy.

use contextful_core::connector::reference::Hydrated;
use contextful_core::ports::Clock;
use contextful_core::time::Instant;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// Lead time before a credential's stated expiry at which the provider refreshes it: 300 s
/// (`store.endpoint.role-refresh`).
pub const REFRESH_LEAD_SECS: u64 = 300;

/// The ECS container credential endpoint a relative URI resolves against.
pub const CONTAINER_BASE: &str = "http://169.254.170.2";
/// The instance metadata endpoint.
pub const IMDS_BASE: &str = "http://169.254.169.254";

pub const CONTAINER_RELATIVE_URI: &str = "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI";
pub const CONTAINER_FULL_URI: &str = "AWS_CONTAINER_CREDENTIALS_FULL_URI";
pub const CONTAINER_TOKEN: &str = "AWS_CONTAINER_AUTHORIZATION_TOKEN";
pub const CONTAINER_TOKEN_FILE: &str = "AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE";
pub const WEB_IDENTITY_TOKEN_FILE: &str = "AWS_WEB_IDENTITY_TOKEN_FILE";
pub const ROLE_ARN: &str = "AWS_ROLE_ARN";
pub const ROLE_SESSION_NAME: &str = "AWS_ROLE_SESSION_NAME";
/// Overrides the regional STS endpoint a web identity token is exchanged at.
pub const STS_ENDPOINT: &str = "AWS_ENDPOINT_URL_STS";
/// Overrides the instance metadata endpoint.
pub const IMDS_ENDPOINT: &str = "AWS_EC2_METADATA_SERVICE_ENDPOINT";
/// `true` skips the instance metadata source.
pub const IMDS_DISABLED: &str = "AWS_EC2_METADATA_DISABLED";

/// Hosts a cleartext full container URI may name beside loopback: the ECS and EKS
/// credential endpoints.
const CONTAINER_HOSTS: [&str; 3] = ["169.254.170.2", "169.254.170.23", "fd00:ec2::23"];

/// Connection budget for one credential request; a link-local source absent from the
/// network fails within it.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
/// Wall clock for one credential request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Lifetime the provider asks of an IMDSv2 session token.
const IMDS_TOKEN_TTL_SECS: &str = "21600";

/// One signing identity: the key pair, the session token, and the stated expiry.
pub struct RoleCredentials {
    pub access_key_id: Hydrated,
    pub secret_access_key: Hydrated,
    pub session_token: Option<Hydrated>,
    pub expires_at: Option<Instant>,
}

impl RoleCredentials {
    /// A copy for one caller; the provider keeps its own.
    pub fn copy(&self) -> RoleCredentials {
        RoleCredentials {
            access_key_id: Hydrated::new(self.access_key_id.reveal()),
            secret_access_key: Hydrated::new(self.secret_access_key.reveal()),
            session_token: self.session_token.as_ref().map(|t| Hydrated::new(t.reveal())),
            expires_at: self.expires_at,
        }
    }

    /// Whether the credential outlives the refresh lead at `now`.
    fn fresh(&self, now: Instant) -> bool {
        self.expires_at.is_none_or(|e| now.plus_secs(REFRESH_LEAD_SECS) < e)
    }

    /// Whether the credential has not yet expired at `now`.
    fn valid(&self, now: Instant) -> bool {
        self.expires_at.is_none_or(|e| now < e)
    }
}

impl std::fmt::Debug for RoleCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoleCredentials").field("expires_at", &self.expires_at).finish_non_exhaustive()
    }
}

/// No role source answered, and no still-valid credential is held. The message names each
/// source tried and why it did not answer (`store.endpoint.role-unavailable`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleUnavailable(pub String);

impl std::fmt::Display for RoleUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RoleUnavailable {}

struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
        Instant::from_unix_nanos(i128::try_from(nanos).unwrap_or_default()).unwrap_or_else(|_| Instant::from_unix_secs(0).expect("the epoch is an instant"))
    }
}

/// The workload-role provider over an injected variable map.
pub struct RoleProvider {
    vars: BTreeMap<String, String>,
    container_base: String,
    clock: Arc<dyn Clock + Send + Sync>,
    agent: ureq::Agent,
    held: Mutex<Option<RoleCredentials>>,
}

impl std::fmt::Debug for RoleProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RoleProvider")
    }
}

/// The text between `<tag>` and `</tag>` in an XML answer.
fn xml<'a>(body: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let (_, rest) = body.split_once(open.as_str())?;
    rest.split_once(close.as_str()).map(|(v, _)| v.trim())
}

fn expiry(text: Option<&str>) -> Result<Option<Instant>, String> {
    text.map(|t| Instant::parse(t).map_err(|e| format!("the stated expiry does not parse: {e}"))).transpose()
}

/// Credentials from a container or IMDS JSON answer: `AccessKeyId`, `SecretAccessKey`,
/// `Token` and `Expiration`.
fn from_json(body: &str) -> Result<RoleCredentials, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| format!("the answer is no JSON object: {e}"))?;
    let field = |k: &str| v.get(k).and_then(|x| x.as_str()).filter(|s| !s.is_empty());
    if let Some(code) = field("Code").filter(|c| *c != "Success") {
        return Err(format!("the answer carries code `{code}`"));
    }
    let (Some(id), Some(secret)) = (field("AccessKeyId"), field("SecretAccessKey")) else {
        return Err("the answer carries no `AccessKeyId` and `SecretAccessKey`".into());
    };
    Ok(RoleCredentials {
        access_key_id: Hydrated::new(id),
        secret_access_key: Hydrated::new(secret),
        session_token: field("Token").map(Hydrated::new),
        expires_at: expiry(field("Expiration"))?,
    })
}

fn read(path: &str) -> Result<String, String> {
    std::fs::read_to_string(path).map(|t| t.trim().to_string()).map_err(|e| format!("`{path}` is unreadable: {e}"))
}

impl RoleProvider {
    /// A provider reading its sources from `vars`, the process environment in production.
    pub fn new(vars: BTreeMap<String, String>) -> RoleProvider {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .proxy(None)
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_global(Some(REQUEST_TIMEOUT))
            .build()
            .new_agent();
        RoleProvider { vars, container_base: CONTAINER_BASE.to_string(), clock: Arc::new(SystemClock), agent, held: Mutex::new(None) }
    }

    /// The provider reading the time from `clock`.
    pub fn with_clock(mut self, clock: Arc<dyn Clock + Send + Sync>) -> RoleProvider {
        self.clock = clock;
        self
    }

    /// The provider resolving a relative container URI against `base` in place of
    /// [`CONTAINER_BASE`].
    pub fn with_container_base(mut self, base: &str) -> RoleProvider {
        self.container_base = base.trim_end_matches('/').to_string();
        self
    }

    fn var(&self, k: &str) -> Option<&str> {
        self.vars.get(k).map(String::as_str).filter(|v| !v.is_empty())
    }

    /// The credential to sign one request with: the held one while it outlives the refresh
    /// lead, otherwise a fresh fetch. A failed fetch keeps a still-valid held credential, and
    /// the next call retries; no call answers an expired one (`store.endpoint.role-refresh`).
    pub fn credentials(&self) -> Result<RoleCredentials, RoleUnavailable> {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let now = self.clock.now();
        if let Some(c) = held.as_ref().filter(|c| c.fresh(now)) {
            return Ok(c.copy());
        }
        match self.fetch(now) {
            Ok(c) => {
                let out = c.copy();
                *held = Some(c);
                Ok(out)
            }
            Err(why) => match held.as_ref().filter(|c| c.valid(now)) {
                Some(c) => Ok(c.copy()),
                None => Err(RoleUnavailable(why)),
            },
        }
    }

    /// The first source in chain order that answers an unexpired credential, or each
    /// source's reason.
    fn fetch(&self, now: Instant) -> Result<RoleCredentials, String> {
        let mut reasons = Vec::new();
        for (source, answer) in [("container", self.container()), ("web-identity", self.web_identity()), ("imds", self.imds())] {
            match answer.and_then(|c| if c.valid(now) { Ok(c) } else { Err("the answered credential has expired".into()) }) {
                Ok(c) => return Ok(c),
                Err(why) => reasons.push(format!("{source}: {why}")),
            }
        }
        Err(format!("no workload-role source answers: {}", reasons.join("; ")))
    }

    fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<(u16, String), String> {
        let mut request = self.agent.get(url);
        for (k, v) in headers {
            request = request.header(*k, *v);
        }
        let mut response = request.call().map_err(|e| format!("`{url}` is unreachable: {e}"))?;
        let status = response.status().as_u16();
        Ok((status, response.body_mut().read_to_string().map_err(|e| format!("`{url}`: {e}"))?))
    }

    /// The ECS container credential endpoint: a relative URI under [`CONTAINER_BASE`], or a
    /// full URI that is TLS, loopback or a container credential host.
    fn container(&self) -> Result<RoleCredentials, String> {
        let url = match (self.var(CONTAINER_RELATIVE_URI), self.var(CONTAINER_FULL_URI)) {
            (Some(relative), _) => format!("{}/{}", self.container_base, relative.trim_start_matches('/')),
            (None, Some(full)) => {
                let parsed = url::Url::parse(full).map_err(|e| format!("`{CONTAINER_FULL_URI}` is no URL: {e}"))?;
                let host = parsed.host_str().unwrap_or_default().trim_start_matches('[').trim_end_matches(']');
                let admitted = parsed.scheme() == "https"
                    || (parsed.scheme() == "http" && (contextful_core::connector::attach::is_loopback_host(host) || CONTAINER_HOSTS.contains(&host)));
                if !admitted {
                    return Err(format!("`{CONTAINER_FULL_URI}` names `{host}`, which is neither TLS, loopback nor a container credential host"));
                }
                full.to_string()
            }
            (None, None) => return Err(format!("`{CONTAINER_RELATIVE_URI}` and `{CONTAINER_FULL_URI}` are unset")),
        };
        let token = match (self.var(CONTAINER_TOKEN_FILE), self.var(CONTAINER_TOKEN)) {
            (Some(file), _) => Some(read(file)?),
            (None, token) => token.map(str::to_string),
        };
        let headers: Vec<(&str, &str)> = token.as_deref().map(|t| ("Authorization", t)).into_iter().collect();
        let (status, body) = self.get(&url, &headers)?;
        if status != 200 {
            return Err(format!("the container endpoint answered {status}"));
        }
        from_json(&body)
    }

    /// The STS endpoint a web identity token is exchanged at: the override, or the regional
    /// endpoint of `AWS_REGION`, `AWS_DEFAULT_REGION`, or `us-east-1`.
    fn sts_endpoint(&self) -> String {
        if let Some(url) = self.var(STS_ENDPOINT) {
            return url.trim_end_matches('/').to_string();
        }
        let region = self.var("AWS_REGION").or_else(|| self.var("AWS_DEFAULT_REGION")).unwrap_or("us-east-1");
        format!("https://sts.{region}.amazonaws.com")
    }

    /// A web identity token exchanged through an unsigned `AssumeRoleWithWebIdentity`.
    fn web_identity(&self) -> Result<RoleCredentials, String> {
        let (Some(file), Some(role)) = (self.var(WEB_IDENTITY_TOKEN_FILE), self.var(ROLE_ARN)) else {
            return Err(format!("`{WEB_IDENTITY_TOKEN_FILE}` and `{ROLE_ARN}` are not both set"));
        };
        let token = read(file)?;
        let session = self.var(ROLE_SESSION_NAME).map_or_else(|| format!("contextful-{}", self.clock.now().unix_secs()), str::to_string);
        let form = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("Action", "AssumeRoleWithWebIdentity")
            .append_pair("Version", "2011-06-15")
            .append_pair("RoleArn", role)
            .append_pair("RoleSessionName", &session)
            .append_pair("WebIdentityToken", &token)
            .finish();
        let url = format!("{}/", self.sts_endpoint());
        let mut response = self
            .agent
            .post(&url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send(form.as_bytes())
            .map_err(|e| format!("`{url}` is unreachable: {e}"))?;
        let status = response.status().as_u16();
        let body = response.body_mut().read_to_string().map_err(|e| format!("`{url}`: {e}"))?;
        if status != 200 {
            return Err(format!("STS answered {status} {}", xml(&body, "Code").unwrap_or_default()));
        }
        let (Some(id), Some(secret)) = (xml(&body, "AccessKeyId"), xml(&body, "SecretAccessKey")) else {
            return Err("the STS answer carries no `AccessKeyId` and `SecretAccessKey`".into());
        };
        Ok(RoleCredentials {
            access_key_id: Hydrated::new(id),
            secret_access_key: Hydrated::new(secret),
            session_token: xml(&body, "SessionToken").map(Hydrated::new),
            expires_at: expiry(xml(&body, "Expiration"))?,
        })
    }

    /// IMDSv2: a session token, the instance profile's role name, then its credentials.
    fn imds(&self) -> Result<RoleCredentials, String> {
        if self.var(IMDS_DISABLED).is_some_and(|v| v.eq_ignore_ascii_case("true")) {
            return Err(format!("`{IMDS_DISABLED}` is `true`"));
        }
        let base = self.var(IMDS_ENDPOINT).unwrap_or(IMDS_BASE).trim_end_matches('/').to_string();
        let token_url = format!("{base}/latest/api/token");
        let mut response = self
            .agent
            .put(&token_url)
            .header("X-aws-ec2-metadata-token-ttl-seconds", IMDS_TOKEN_TTL_SECS)
            .send_empty()
            .map_err(|e| format!("`{token_url}` is unreachable: {e}"))?;
        let status = response.status().as_u16();
        let token = response.body_mut().read_to_string().map_err(|e| format!("`{token_url}`: {e}"))?;
        if status != 200 {
            return Err(format!("the metadata token request answered {status}"));
        }
        let header = [("X-aws-ec2-metadata-token", token.trim())];
        let roles_url = format!("{base}/latest/meta-data/iam/security-credentials/");
        let (status, roles) = self.get(&roles_url, &header)?;
        if status != 200 {
            return Err(format!("the instance profile listing answered {status}"));
        }
        let role = roles.lines().map(str::trim).find(|l| !l.is_empty()).ok_or("the instance carries no instance profile role")?;
        let (status, body) = self.get(&format!("{roles_url}{role}"), &header)?;
        if status != 200 {
            return Err(format!("the role credential request answered {status}"));
        }
        from_json(&body)
    }
}
