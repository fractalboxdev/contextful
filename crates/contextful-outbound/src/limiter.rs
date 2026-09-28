//! The shared-quota limiter a metered client reserves against: one permit per outbound
//! request, drawn from a batch the limiter grants, and a usage report surrendering what
//! went unspent.

use crate::client::{classify, Client, HeaderValue};
use crate::egress::{Intent, PreSendHook, Reserve, Transport};
use crate::secrets::Resolver;
use contextful_core::connector::attach::{scrub, Allowlist};
use contextful_core::connector::meter::{acquire_body, require_binding, Decision, LimiterBinding, LimiterDeclaration, Pool, Report, Usage};
use contextful_core::connector::reference::Hydrated;
use contextful_core::connector::ConnectorError;
use contextful_core::ports::Clock;
use contextful_core::run::{Failure, FailureTag};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Delivery attempts one report gets when the run finishes. A report owed ahead of a
/// refill gets one attempt and, undelivered, rides the next report.
pub const REPORT_ATTEMPTS: u32 = 3;
/// Wall clock one limiter call may take, so a stalled limiter delays a vendor request by
/// seconds rather than by a vendor request's own timeout.
pub const LIMITER_TIMEOUT: Duration = Duration::from_secs(5);
/// The wait before the second attempt; each later wait doubles it.
pub const REPORT_BACKOFF: Duration = Duration::from_millis(25);
/// Limiter events a run records verbatim; later ones are counted.
pub const MAX_AUDIT_ENTRIES: usize = 32;

fn unmetered(quota: &str, why: String) -> ConnectorError {
    ConnectorError::ConnectorUnmetered(format!("quota `{quota}`: {why}; the request is not sent"))
}

#[derive(Debug, Default)]
struct Audit {
    entries: Vec<String>,
    suppressed: u64,
}

/// One bound quota's limiter for one run.
pub struct Limiter {
    binding: LimiterBinding,
    resolver: Arc<Resolver>,
    run_id: String,
    clock: Arc<dyn Clock + Send + Sync>,
    /// The client every acquire and report takes; its allowlist is the limiter host alone.
    client: Client,
    pools: Mutex<BTreeMap<String, Pool>>,
    audit: Mutex<Audit>,
}

impl std::fmt::Debug for Limiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Limiter").field("quota", &self.binding.quota).field("endpoint", &scrub(&self.binding.endpoint)).finish_non_exhaustive()
    }
}

impl Limiter {
    /// The limiter behind `binding`, for run `run_id`.
    pub fn new(binding: LimiterBinding, resolver: Arc<Resolver>, run_id: &str, clock: Arc<dyn Clock + Send + Sync>) -> Result<Limiter, Failure> {
        let host = binding.endpoint.host_str().unwrap_or_default().to_string();
        let allow = Allowlist::parse(&[host]).map_err(|e| Failure::deterministic(FailureTag::Config, e.to_string()))?;
        let client = Client::new(allow, binding.endpoint.clone()).admitting_internal().with_timeout(LIMITER_TIMEOUT).for_run(run_id);
        Ok(Limiter { binding, resolver, run_id: run_id.to_string(), clock, client, pools: Mutex::default(), audit: Mutex::default() })
    }

    /// Load the limiter answering `declaration` from the operator's bindings, raising
    /// `ConnectorQuotaUnbound` when none names its quota.
    pub fn load(
        declaration: &LimiterDeclaration,
        bindings: &BTreeMap<String, LimiterBinding>,
        resolver: Arc<Resolver>,
        run_id: &str,
        clock: Arc<dyn Clock + Send + Sync>,
    ) -> Result<Limiter, Failure> {
        let binding = require_binding(declaration, bindings).map_err(|e| Failure::deterministic(FailureTag::Config, e.to_string()))?;
        Limiter::new(binding.clone(), resolver, run_id, clock)
    }

    /// The limiter reaching its endpoint through `transport`.
    pub fn with_transport(mut self, transport: Arc<dyn Transport>) -> Limiter {
        self.client = self.client.with_transport(transport);
        self
    }

    /// The limiter passing each of its own calls through the operator's `hook`
    /// (`connector.attach.mediation-covers-every-egress`).
    pub fn with_hook(mut self, hook: Arc<dyn PreSendHook>) -> Limiter {
        self.client = self.client.with_hook(hook);
        self
    }

    /// The limiter with a shorter wall clock per call than [`LIMITER_TIMEOUT`].
    pub fn with_timeout(mut self, timeout: Duration) -> Limiter {
        self.client = self.client.with_timeout(timeout.min(LIMITER_TIMEOUT));
        self
    }

    pub fn quota(&self) -> &str {
        &self.binding.quota
    }

    /// Limiter events of this run: denials and undelivered reports, names and counts only.
    pub fn audit(&self) -> Vec<String> {
        let audit = self.audit.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = audit.entries.clone();
        if audit.suppressed > 0 {
            out.push(format!("{} further limiter events", audit.suppressed));
        }
        out
    }

    fn note(&self, entry: String) {
        let mut audit = self.audit.lock().unwrap_or_else(|e| e.into_inner());
        if audit.entries.len() < MAX_AUDIT_ENTRIES {
            audit.entries.push(entry);
        } else {
            audit.suppressed += 1;
        }
    }

    fn with_pool<T>(&self, class: &str, f: impl FnOnce(&mut Pool) -> T) -> T {
        let mut pools = self.pools.lock().unwrap_or_else(|e| e.into_inner());
        f(pools.entry(class.to_string()).or_default())
    }

    /// Reserve one permit for one outbound request under `class`. A live batch pays for
    /// it; otherwise the owed report gets one attempt, surrendering the unspent before the
    /// limiter grants again, and an acquire refills the batch. A denial fails as the
    /// vendor `429` it stands for; an unreachable limiter, an unresolvable token and a
    /// failing answer raise `ConnectorUnmetered`; an unreadable answer raises
    /// `ConnectorLimiterUnreadable`. No outcome but a granted permit returns `Ok`.
    pub fn reserve(&self, class: &str) -> Result<(), Failure> {
        let now = self.clock.now();
        if self.with_pool(class, |p| p.take(now)) {
            return Ok(());
        }
        if let Some((report, _)) = self.deliver(class, 1) {
            self.with_pool(class, |p| p.restore(report));
        }
        let quota = self.binding.quota.clone();
        match self.acquire(class)? {
            Decision::Granted { permits, ttl_secs } => {
                let now = self.clock.now();
                let taken = self.with_pool(class, |p| {
                    p.grant(permits, ttl_secs, now);
                    p.take(now)
                });
                if taken {
                    Ok(())
                } else {
                    Err(Failure::new(FailureTag::Transient, unmetered(&quota, "the granted batch expired before its first permit".into()).to_string()))
                }
            }
            Decision::Denied { retry_after_secs } => {
                self.note(format!("quota `{quota}` class `{class}`: denied, retry after {retry_after_secs}s"));
                Err(classify(429, Some(retry_after_secs), &format!("the limiter for quota `{quota}`")))
            }
        }
    }

    fn bearer(&self) -> Result<HeaderValue, Failure> {
        let token = self.resolver.hydrate(&self.binding.token).map_err(|f| Failure {
            message: unmetered(&self.binding.quota, format!("the limiter token does not resolve ({})", f.message)).to_string(),
            ..f
        })?;
        Ok(HeaderValue::Sensitive(Hydrated::new(format!("Bearer {}", token.reveal()))))
    }

    fn post(&self, call: &str, bearer: HeaderValue, body: &serde_json::Value) -> Result<crate::client::Response, Failure> {
        let headers = [("Authorization".to_string(), bearer), ("Content-Type".to_string(), HeaderValue::Plain("application/json".into()))];
        let url = self.binding.call_url(call);
        self.client.send_once("POST", &url, &headers, Some(body.to_string().as_bytes()))
    }

    fn acquire(&self, class: &str) -> Result<Decision, Failure> {
        let quota = &self.binding.quota;
        let body = acquire_body(quota, class, self.binding.permits);
        let bearer = self.bearer()?;
        let resp = self.post("acquire", bearer, &body).map_err(|f| Failure { message: unmetered(quota, format!("the limiter is unreachable ({})", f.message)).to_string(), ..f })?;
        match resp.status {
            429 => Ok(Decision::throttled(resp.header("retry-after"), self.clock.now())),
            200..=299 => Decision::read(&resp.body).map_err(|e| Failure::new(FailureTag::Permanent, e.to_string())),
            // One tag for every failing answer: the limiter's refusal of its own token is no
            // statement about the vendor credential.
            status => Err(Failure::new(FailureTag::Transient, unmetered(quota, format!("the limiter answered {status}")).to_string())),
        }
    }

    /// Record what one vendor response said about the quota; it rides the next report.
    pub fn observe(&self, class: &str, usage: Usage) {
        self.with_pool(class, |p| p.observe(usage));
    }

    /// Deliver the report owed on `class` in at most `attempts` tries under doubling
    /// backoff, each bounded by the limiter's call timeout, answering the report and the
    /// last fault when it does not land. Ahead of a refill it returns to the pool and
    /// rides the next report; at [`Limiter::finish`] it is recorded in
    /// [`Limiter::audit`]. Either way it fails nothing.
    fn deliver(&self, class: &str, attempts: u32) -> Option<(Report, String)> {
        let now = self.clock.now();
        let report = self.with_pool(class, |p| p.drain(&self.binding.quota, class, &self.run_id, now))?;
        let body = report.to_json();
        let (mut wait, mut last) = (REPORT_BACKOFF, String::new());
        for attempt in 1..=attempts {
            match self.bearer().and_then(|bearer| self.post("report", bearer, &body)) {
                Ok(r) if (200..300).contains(&r.status) => return None,
                Ok(r) => last = format!("answered {}", r.status),
                Err(f) => last = f.message,
            }
            if attempt < attempts {
                std::thread::sleep(wait);
                wait *= 2;
            }
        }
        Some((report, last))
    }

    fn undelivered(&self, report: &Report, last: &str) {
        self.note(format!(
            "quota `{}` class `{}`: report of {} spent of {} granted undelivered after {REPORT_ATTEMPTS} attempts ({last})",
            report.quota, report.class, report.spent, report.granted
        ));
    }

    /// Deliver every report still owed, [`REPORT_ATTEMPTS`] times at most, surrendering
    /// the unspent permits of each class.
    pub fn finish(&self) {
        let classes: Vec<String> = self.pools.lock().unwrap_or_else(|e| e.into_inner()).keys().cloned().collect();
        for class in classes {
            if let Some((report, last)) = self.deliver(&class, REPORT_ATTEMPTS) {
                self.undelivered(&report, &last);
            }
        }
    }
}

/// A connector's limiter declaration and the limiter bound to it, attached to the client
/// its vendor requests take. A declaration with no limiter refuses every request.
#[derive(Debug, Clone)]
pub struct Meter {
    pub declaration: LimiterDeclaration,
    pub limiter: Option<Arc<Limiter>>,
}

impl Meter {
    pub fn new(declaration: LimiterDeclaration, limiter: Option<Arc<Limiter>>) -> Meter {
        Meter { declaration, limiter }
    }

    /// The run the bound limiter reports for.
    pub fn run_id(&self) -> Option<String> {
        self.limiter.as_ref().map(|l| l.run_id.clone())
    }

    /// Reserve one permit ahead of one outbound request (`connector.meter.reservation-point`).
    pub fn reserve(&self) -> Result<(), Failure> {
        match &self.limiter {
            Some(l) => l.reserve(&self.declaration.class),
            None => Err(Failure::deterministic(FailureTag::Config, unmetered(&self.declaration.quota, "no limiter is bound".into()).to_string())),
        }
    }

    /// Record one vendor response's status and declared quota-state headers.
    pub fn observe(&self, status: u16, headers: &[(String, String)]) {
        if let Some(l) = &self.limiter {
            l.observe(&self.declaration.class, Usage::observe(&self.declaration, status, headers, l.clock.now()));
        }
    }
}

/// The reservation as the innermost stage of the pre-send hook (`connector.meter.hook-composition`).
impl Reserve for Meter {
    fn reserve(&self, _intent: &Intent) -> Result<(), Failure> {
        Meter::reserve(self)
    }

    fn observe(&self, status: u16, headers: &[(String, String)]) {
        Meter::observe(self, status, headers);
    }
}
