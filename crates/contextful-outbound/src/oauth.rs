//! The OAuth refresh a connector manifest's `[auth]` table declares: the host holds the
//! access token in memory, refreshes it at its retirement margin or once on a vendor `401`,
//! and writes each replacement refresh token forward through the [`Rotator`]
//! (`connector.rotate.oauth-credential`).

use crate::client::{Client, HeaderValue};
use crate::egress::Transport;
use crate::secrets::{Resolver, Rotator};
use contextful_core::connector::attach::{scrub, Allowlist};
use contextful_core::connector::reference::{Hydrated, SecretName};
use contextful_core::connector::rotate::{classify_refresh, read_token_answer, refresh_at, AuthPolicy, VersionedStore};
use contextful_core::ports::Clock;
use contextful_core::run::{Failure, FailureTag};
use contextful_core::time::Instant;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// The access token the host holds, and when it refreshes; `None` holds it until a `401`.
struct Held {
    value: Arc<Hydrated>,
    refresh_at: Option<Instant>,
}

/// One source's refresh loop over its manifest's [`AuthPolicy`].
pub struct Refresher {
    policy: AuthPolicy,
    resolver: Arc<Resolver>,
    rotator: Rotator,
    client: Client,
    clock: Arc<dyn Clock + Send + Sync>,
    /// The held token; its lock also makes one refresh at a time per source.
    held: Mutex<Option<Held>>,
    grants: AtomicU32,
}

impl Refresher {
    /// A refresher posting to the policy's endpoint through a mediated client admitting that
    /// host alone, writing replacements through `writers`.
    pub fn new(policy: AuthPolicy, resolver: Arc<Resolver>, writers: BTreeMap<String, Arc<dyn VersionedStore>>, clock: Arc<dyn Clock + Send + Sync>) -> Result<Refresher, Failure> {
        let host = policy.token_endpoint.host_str().unwrap_or_default().to_string();
        let allow = Allowlist::parse(&[host]).map_err(|e| Failure::deterministic(FailureTag::Config, e.to_string()))?;
        let client = Client::new(allow, policy.token_endpoint.clone());
        let rotator = Rotator::new(resolver.clone(), writers);
        Ok(Refresher { policy, resolver, rotator, client, clock, held: Mutex::default(), grants: AtomicU32::new(0) })
    }

    /// The refresher sending through `transport` in place of the system's.
    pub fn with_transport(mut self, transport: Arc<dyn Transport>) -> Refresher {
        self.client = self.client.with_transport(transport);
        self
    }

    /// The name attach templates reference for the held access token.
    pub fn access_name(&self) -> &SecretName {
        &self.policy.access_token
    }

    /// The `[auth]` references a static binding hydrates ahead of the first request.
    pub fn references(&self) -> [&SecretName; 2] {
        [&self.policy.client_secret, &self.policy.refresh_token]
    }

    /// Refresh requests this refresher has sent.
    pub fn grants(&self) -> u32 {
        self.grants.load(Ordering::SeqCst)
    }

    /// The held access token, refreshed when none is held or its refresh point has passed
    /// (`connector.rotate.access-token`).
    pub fn access(&self) -> Result<Arc<Hydrated>, Failure> {
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        let now = self.clock.now();
        if let Some(h) = held.as_ref().filter(|h| h.refresh_at.is_none_or(|at| now < at)) {
            return Ok(h.value.clone());
        }
        self.refresh(&mut held)
    }

    /// A vendor answered `401` to a request carrying a token `sent` still names: refresh
    /// once, unless another request already replaced that token
    /// (`connector.rotate.rejected-once`).
    pub fn rejected(&self, sent: impl FnOnce(&Hydrated) -> bool) -> Result<(), Failure> {
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        if held.as_ref().is_some_and(|h| !sent(&h.value)) {
            return Ok(());
        }
        self.refresh(&mut held).map(|_| ())
    }

    /// One refresh grant, its replacement refresh token put forward before the access
    /// token is held; a refused put leaves no token held (`connector.rotate.write-back-denied`).
    fn refresh(&self, held: &mut Option<Held>) -> Result<Arc<Hydrated>, Failure> {
        *held = None;
        let secret = self.resolver.hydrate(&self.policy.client_secret)?;
        let endpoint = scrub(&self.policy.token_endpoint);
        let now = self.clock.now();
        let mut answer = None;
        self.rotator.rotate(&self.policy.refresh_token, |refresh| {
            let form = self.policy.form(refresh, &secret);
            let headers = [
                ("Content-Type".to_string(), HeaderValue::Plain("application/x-www-form-urlencoded".into())),
                ("Accept".to_string(), HeaderValue::Plain("application/json".into())),
            ];
            self.grants.fetch_add(1, Ordering::SeqCst);
            // One send, following nothing: the run's retry schedule is the one retry layer.
            let resp = self.client.send_once("POST", &self.policy.token_endpoint, &headers, Some(form.reveal().as_bytes()))?;
            if !(200..300).contains(&resp.status) {
                let retry_after = resp.header("retry-after").and_then(|v| v.trim().parse().ok());
                return Err(classify_refresh(resp.status, retry_after, &endpoint));
            }
            let mut got = read_token_answer(&resp.body, now, &endpoint)?;
            let replacement = got.refresh_token.take().map(|r| (r, None));
            answer = Some(got);
            Ok(replacement)
        })?;
        let got = answer.ok_or_else(|| Failure::new(FailureTag::Permanent, format!("`{endpoint}` answered no refresh")))?;
        let value = Arc::new(got.access_token);
        *held = Some(Held { value: value.clone(), refresh_at: got.expires_at.map(|e| refresh_at(now, e)) });
        Ok(value)
    }
}
