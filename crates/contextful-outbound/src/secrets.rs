//! The credential resolver: the provider chain, its cache, and the adapters this build
//! links — the process environment and the lease provider.

use crate::client::{Client, HeaderValue};
use contextful_core::connector::attach::{is_loopback_host, scrub, Allowlist};
use contextful_core::connector::lease::{classify, read_response, retires_at, Scopes};
use contextful_core::connector::reference::{Hydrated, SecretName, Template};
use contextful_core::connector::resolve::{Answer, Provider};
use contextful_core::connector::ConnectorError;
use contextful_core::ports::Clock;
use contextful_core::run::{Failure, FailureTag};
use contextful_core::time::Instant;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, Once};
use url::Url;

/// Re-admits the environment adapter to template hydration (`connector.reference.environment-opt-in`).
pub const ALLOW_ENV_TEMPLATES: &str = "CONTEXTFUL_SECRETS_ALLOW_ENV_TEMPLATES";
/// Selects the adapters the chain assembles, comma-separated.
pub const BACKEND: &str = "CONTEXTFUL_SECRETS_BACKEND";
/// Names hydrating as leases.
pub const LEASE_SCOPES: &str = "CONTEXTFUL_LEASE_SCOPES";
/// The lease provider's base URL; the mint endpoint is `<url>/leases`.
pub const LEASE_PROVIDER_URL: &str = "CONTEXTFUL_LEASE_PROVIDER_URL";
/// The logical name of the standing mint credential; `lease-mint` when unset.
pub const LEASE_BOOTSTRAP: &str = "CONTEXTFUL_LEASE_BOOTSTRAP";

fn config(e: ConnectorError) -> Failure {
    Failure::deterministic(FailureTag::Config, e.to_string())
}

/// `name` answered by `serving` and by `behind` after it (`connector.resolve.shadowed-name`).
fn shadowed(name: &SecretName, serving: &dyn Provider, behind: &dyn Provider) -> Failure {
    config(ConnectorError::SecretNameShadowed(format!("`secret://{name}` is answered by `{}` and by `{}` behind it", serving.name(), behind.name())))
}

/// The environment variable an environment adapter reads for `name`: upper-cased, `-` as `_`.
pub fn env_var(name: &SecretName) -> String {
    name.as_str().to_ascii_uppercase().replace('-', "_")
}

/// The process-environment adapter over an injected variable map.
pub struct EnvProvider {
    vars: BTreeMap<String, String>,
}

impl EnvProvider {
    pub fn new(vars: BTreeMap<String, String>) -> EnvProvider {
        EnvProvider { vars }
    }
}

impl Provider for EnvProvider {
    fn name(&self) -> &str {
        "env"
    }

    fn answer(&self, name: &SecretName) -> Result<Option<Answer>, Failure> {
        Ok(self.vars.get(&env_var(name)).map(|v| Answer { value: Hydrated::new(v.clone()), expires_at: None }))
    }

    /// Template hydration treats the environment as a miss (`connector.reference.environment-is-a-miss`).
    fn serves_templates(&self) -> bool {
        false
    }
}

/// The lease provider at the head of the chain.
pub struct LeaseProvider {
    endpoint: Url,
    scopes: Scopes,
    bootstrap: SecretName,
    /// The chain as assembled before this provider joined; the mint reference hydrates here alone.
    behind: Vec<Arc<dyn Provider>>,
    clock: Arc<dyn Clock + Send + Sync>,
    mints: AtomicU32,
}

impl LeaseProvider {
    /// A provider minting at `<base>/leases`. The endpoint is TLS or loopback
    /// (`connector.lease.mint-transport`).
    pub fn new(base: &str, scopes: Scopes, bootstrap: SecretName, behind: Vec<Arc<dyn Provider>>, clock: Arc<dyn Clock + Send + Sync>) -> Result<LeaseProvider, Failure> {
        let endpoint = Url::parse(&format!("{}/leases", base.trim_end_matches('/')))
            .map_err(|e| Failure::deterministic(FailureTag::Config, format!("`{LEASE_PROVIDER_URL}` is not a URL: {e}")))?;
        let host = endpoint.host_str().unwrap_or_default().to_string();
        if endpoint.scheme() != "https" && !is_loopback_host(&host) {
            return Err(config(ConnectorError::SecretCleartextEndpoint(format!("the mint endpoint `{}` is neither TLS nor loopback", scrub(&endpoint)))));
        }
        scopes.check(&bootstrap).map_err(config)?;
        Ok(LeaseProvider { endpoint, scopes, bootstrap, behind, clock, mints: AtomicU32::new(0) })
    }

    /// Mint calls this provider has issued.
    pub fn mint_calls(&self) -> u32 {
        self.mints.load(Ordering::SeqCst)
    }

    fn mint_credential(&self) -> Result<Hydrated, Failure> {
        for p in &self.behind {
            if let Some(a) = p.answer(&self.bootstrap)? {
                return Ok(a.value);
            }
        }
        Err(config(ConnectorError::SecretBootstrapUnresolved(format!("no adapter behind the lease provider answers `secret://{}`", self.bootstrap))))
    }
}

impl Provider for LeaseProvider {
    fn name(&self) -> &str {
        "lease"
    }

    fn answer(&self, name: &SecretName) -> Result<Option<Answer>, Failure> {
        // An undeclared name sends the provider no request.
        let Some(scope) = self.scopes.scope(name) else { return Ok(None) };
        let mint = self.mint_credential()?;
        let host = self.endpoint.host_str().unwrap_or_default().to_string();
        let client = Client::new(Allowlist::parse(&[host]).map_err(config)?, self.endpoint.clone());
        let body = serde_json::json!({ "scope": scope }).to_string();
        let headers = [
            ("Authorization".to_string(), HeaderValue::Sensitive(Hydrated::new(format!("Bearer {}", mint.reveal())).into())),
            ("Content-Type".to_string(), HeaderValue::Plain("application/json".into())),
        ];
        self.mints.fetch_add(1, Ordering::SeqCst);
        let resp = client.send_once("POST", &self.endpoint, &headers, Some(body.as_bytes()))?;
        if resp.status != 200 {
            let retry_after = resp.header("retry-after").and_then(|v| v.trim().parse().ok());
            return Err(classify(resp.status, retry_after, name));
        }
        let lease = read_response(&resp.body, self.clock.now())?;
        Ok(Some(Answer { value: lease.value, expires_at: Some(lease.expires_at) }))
    }
}

struct Cached {
    value: Arc<Hydrated>,
    retires_at: Instant,
}

/// One resolver per source: the chain, a cache, a single-flight gate per name, and the
/// adapter that answered each name.
pub struct Resolver {
    chain: Vec<Arc<dyn Provider>>,
    allow_env_templates: bool,
    clock: Arc<dyn Clock + Send + Sync>,
    cache: Mutex<HashMap<SecretName, Cached>>,
    gates: Mutex<HashMap<SecretName, Arc<Mutex<()>>>>,
    answered_by: Mutex<BTreeMap<String, String>>,
}

impl Resolver {
    pub fn new(chain: Vec<Arc<dyn Provider>>, allow_env_templates: bool, clock: Arc<dyn Clock + Send + Sync>) -> Resolver {
        if allow_env_templates {
            static WARN: Once = Once::new();
            WARN.call_once(|| eprintln!("warning: {ALLOW_ENV_TEMPLATES}=1 admits the process environment to template hydration"));
        }
        Resolver { chain, allow_env_templates, clock, cache: Mutex::default(), gates: Mutex::default(), answered_by: Mutex::default() }
    }

    /// Which adapter answered each name, by logical name (`connector.record.provider-attribution`).
    pub fn attribution(&self) -> BTreeMap<String, String> {
        self.answered_by.lock().map(|m| m.clone()).unwrap_or_default()
    }

    fn gate(&self, name: &SecretName) -> Arc<Mutex<()>> {
        let mut gates = self.gates.lock().unwrap_or_else(|e| e.into_inner());
        gates.entry(name.clone()).or_default().clone()
    }

    fn cached(&self, name: &SecretName, now: Instant) -> Option<Arc<Hydrated>> {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        match cache.get(name) {
            Some(cached) if now < cached.retires_at => Some(cached.value.clone()),
            Some(_) => {
                cache.remove(name);
                None
            }
            None => None,
        }
    }

    /// Hydrate `name` for a template. The earliest adapter answering wins and the ones
    /// behind it are not consulted again this run; at first hydration an adapter behind
    /// the answering one that also answers refuses as shadowing.
    pub fn hydrate(&self, name: &SecretName) -> Result<Arc<Hydrated>, Failure> {
        if let Some(v) = self.cached(name, self.clock.now()) {
            return Ok(v);
        }
        let gate = self.gate(name);
        let _held = gate.lock().unwrap_or_else(|e| e.into_inner());
        let now = self.clock.now();
        if let Some(v) = self.cached(name, now) {
            return Ok(v);
        }
        let first = !self.answered_by.lock().unwrap_or_else(|e| e.into_inner()).contains_key(name.as_str());
        let serving: Vec<&Arc<dyn Provider>> = self.chain.iter().filter(|p| p.serves_templates() || self.allow_env_templates).collect();
        for p in serving {
            let Some(answer) = p.answer(name)? else { continue };
            if first {
                let at = self.chain.iter().position(|c| Arc::ptr_eq(c, p)).unwrap_or(self.chain.len());
                for behind in &self.chain[at + 1..] {
                    if behind.answer(name)?.is_some() {
                        return Err(shadowed(name, p.as_ref(), behind.as_ref()));
                    }
                }
            }
            self.answered_by.lock().unwrap_or_else(|e| e.into_inner()).insert(name.to_string(), p.name().to_string());
            let retires = retires_at(now, answer.expires_at);
            let value = Arc::new(answer.value);
            self.cache.lock().unwrap_or_else(|e| e.into_inner()).insert(name.clone(), Cached { value: value.clone(), retires_at: retires });
            return Ok(value);
        }
        Err(config(ConnectorError::SecretUnresolvedReference(format!("no assembled adapter answers `secret://{name}`"))))
    }

    /// Hydrate `name` bound whole, outside any template: every assembled adapter serves, the
    /// process environment included (`connector.reference.whole-value-reference`). The
    /// earliest answering adapter wins and one behind it answering too refuses as shadowing.
    /// The answer stays out of the cache, so no template ever reads a value the
    /// environment served this way.
    pub fn resolve(&self, name: &SecretName) -> Result<Hydrated, Failure> {
        for (at, p) in self.chain.iter().enumerate() {
            let Some(answer) = p.answer(name)? else { continue };
            for behind in &self.chain[at + 1..] {
                if behind.answer(name)?.is_some() {
                    return Err(shadowed(name, p.as_ref(), behind.as_ref()));
                }
            }
            self.answered_by.lock().unwrap_or_else(|e| e.into_inner()).insert(name.to_string(), p.name().to_string());
            return Ok(answer.value);
        }
        Err(config(ConnectorError::SecretUnresolvedReference(format!("no assembled adapter answers `secret://{name}`"))))
    }

    /// Fill `template`, hydrating each reference just in time.
    pub fn render(&self, template: &Template) -> Result<Hydrated, Failure> {
        template.render(|n| self.hydrate(n))
    }

    /// Hydrate every reference a static binding names ahead of the first request, so an
    /// unresolved name refuses at preflight (`connector.resolve.unresolved-name`).
    pub fn preflight<'a>(&self, templates: impl IntoIterator<Item = &'a Template>) -> Result<(), Failure> {
        for t in templates {
            for n in t.names() {
                self.hydrate(n)?;
            }
        }
        Ok(())
    }
}

/// Assemble the chain from the process environment `vars`: `CONTEXTFUL_SECRETS_BACKEND`
/// names the adapters, `env` when unset; `lease` joins at the head.
pub fn assemble(vars: &BTreeMap<String, String>, clock: Arc<dyn Clock + Send + Sync>) -> Result<Resolver, Failure> {
    let backends: Vec<String> = vars.get(BACKEND).map_or(vec!["env".to_string()], |b| b.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect());
    let mut behind: Vec<Arc<dyn Provider>> = Vec::new();
    for b in &backends {
        match b.as_str() {
            "env" => behind.push(Arc::new(EnvProvider::new(vars.clone()))),
            "lease" => {}
            other => {
                return Err(Failure::deterministic(FailureTag::Config, format!("`{BACKEND}` names `{other}`; this build links the `lease` and `env` adapters")))
            }
        }
    }
    let mut chain: Vec<Arc<dyn Provider>> = Vec::new();
    if backends.iter().any(|b| b == "lease") {
        let scopes = Scopes::parse(vars.get(LEASE_SCOPES).map(String::as_str).unwrap_or_default()).map_err(config)?;
        let bootstrap = SecretName::parse(vars.get(LEASE_BOOTSTRAP).map(String::as_str).unwrap_or("lease-mint")).map_err(config)?;
        let base = vars
            .get(LEASE_PROVIDER_URL)
            .ok_or_else(|| Failure::deterministic(FailureTag::Config, format!("the lease backend is selected and `{LEASE_PROVIDER_URL}` is unset")))?;
        chain.push(Arc::new(LeaseProvider::new(base, scopes, bootstrap, behind.clone(), clock.clone())?));
    }
    chain.extend(behind);
    let allow = vars.get(ALLOW_ENV_TEMPLATES).is_some_and(|v| v == "1");
    Ok(Resolver::new(chain, allow, clock))
}
