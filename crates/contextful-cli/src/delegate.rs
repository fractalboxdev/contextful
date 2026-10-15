//! Delegated bodies (`surface.delegate`): the bindings a daemon's `[control.delegates]`
//! declares, and the row body that runs each store-driven item on one over a signed `call`
//! request, recording the answer under the item's entry key so a resume sends nothing
//! (`run.items.delegated-item`).

use crate::worker::{send_with, unix_now};
use anyhow::{Context, Result};
use contextful_core::run::drive::{row_label, BodyDependencies, Emitted, InputRow, RowBody, RowCalls, RowStop, Woken};
use contextful_core::run::journal::sha256_hex;
use contextful_core::run::{Failure, FailureTag};
use contextful_core::surface::delegate::{bindings, sign_request, verify_answer, Answer, Binding, Entity, Envelope, Mode};
use contextful_core::surface::worker::{SIGNATURE_HEADER, STEP_RESULT_CAP, TIMESTAMP_HEADER};
use contextful_core::time::Instant;
use contextful_outbound::egress::Transport;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

/// The bindings a declaration's `[control]` block carries, by name
/// (`surface.delegate.binding`).
pub(crate) fn declared(text: &str) -> Result<BTreeMap<String, Binding>> {
    let value: toml::Value = if text.is_empty() { toml::Value::Table(Default::default()) } else { toml::from_str(text)? };
    let Some(control) = value.get("control") else { return Ok(BTreeMap::new()) };
    let control = control.as_table().context("`[control]` is a table")?;
    Ok(bindings(control)?)
}

/// The label the delegate call of ordinal `attempt` records under.
fn label(attempt: u32) -> String {
    format!("delegate/{attempt}")
}

/// A store-driven body that hands each item to a bound delegate.
pub(crate) struct DelegateBody {
    pub binding: Binding,
    pub job: String,
    pub tables: Vec<String>,
    /// The applied snapshot version the job runs under.
    pub version: u64,
    pub pins: BTreeMap<String, String>,
    /// The entity namespace and key column when the job isolates items.
    pub entity: Option<(String, String)>,
    pub transport: Arc<dyn Transport>,
    pub key: Vec<u8>,
}

impl DelegateBody {
    /// One `call` request: the signed envelope, held open up to the binding's timeout; a
    /// non-200 status, an unverifiable or malformed answer, or a `transient` answer closes
    /// the attempt `Transient` and records nothing (`surface.delegate.call-mode`,
    /// `surface.delegate.status`, `surface.delegate.signed`).
    fn request(&self, row: &InputRow, calls: &dyn RowCalls, attempt: u32, idempotency: &str, input: &[u8]) -> Result<Vec<u8>, Failure> {
        let name = &self.binding.name;
        let transient = |m: String| Failure::new(FailureTag::Transient, m);
        let config = |m: String| Failure::deterministic(FailureTag::Config, m);
        let context = calls.context();
        let now = unix_now();
        // The deadline rides the engine's clock, as the item's awakeables do.
        let engine_now = Instant::parse(&context.now).map(|i| i.unix_secs()).unwrap_or(now);
        let deadline = Instant::from_unix_secs(engine_now.saturating_add(i64::try_from(self.binding.timeout_secs).unwrap_or(i64::MAX))).map_err(|e| config(e.to_string()))?;
        let entity = self.entity.as_ref().map(|(namespace, column)| Entity {
            namespace: namespace.clone(),
            key: row.row.get(column).map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())).unwrap_or_default(),
            fence: context.fence.unwrap_or(0),
        });
        let envelope = Envelope {
            job: self.job.clone(),
            execution_id: context.execution_id,
            step: row_label(&row.key, &label(attempt)),
            input_hash: sha256_hex(input),
            attempt,
            idempotency_key: idempotency.to_string(),
            entity,
            as_of: context.as_of,
            version: self.version,
            pins: self.pins.clone(),
            tables: self.tables.clone(),
            row: row.row.clone(),
            deadline: deadline.to_string(),
        };
        let body = serde_json::to_vec(&envelope).map_err(|e| config(e.to_string()))?;
        let headers = vec![
            ("Content-Type".into(), "application/json".into()),
            (TIMESTAMP_HEADER.into(), now.to_string()),
            (SIGNATURE_HEADER.into(), sign_request(&self.key, now, &body)),
        ];
        let timeout = Duration::from_secs(self.binding.timeout_secs);
        let (status, answer_headers, answer) = send_with(self.transport.as_ref(), "POST", &self.binding.url, headers, &body, timeout, STEP_RESULT_CAP as u64 + 1)
            .map_err(|e| transient(format!("delegate `{name}`: {e}")))?;
        if status != 200 {
            return Err(transient(format!("delegate `{name}` answered `{status}`")));
        }
        let header = |wanted: &str| answer_headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(wanted)).map(|(_, v)| v.clone());
        let clock = Instant::from_unix_secs(unix_now()).map_err(|e| config(e.to_string()))?;
        verify_answer(&self.key, header, &answer, clock).map_err(|e| transient(format!("delegate `{name}`: {e}")))?;
        match Answer::decode(&answer, &self.tables).map_err(|e| transient(format!("delegate `{name}`: {e}")))? {
            Answer::Transient { reason } => Err(transient(format!("delegate `{name}` answered transient: {}", reason.unwrap_or_default()))),
            _ => Ok(answer),
        }
    }
}

impl RowBody for DelegateBody {
    fn run(&self, row: &InputRow, calls: &dyn RowCalls) -> Result<Emitted, RowStop> {
        let name = &self.binding.name;
        let permanent = |m: String| RowStop::Failed(Failure::deterministic(FailureTag::Permanent, m));
        if self.binding.mode == Mode::Submit {
            return Err(RowStop::Failed(Failure::deterministic(FailureTag::Config, format!("delegate `{name}` binds `mode = \"submit\"`, which this build does not dispatch; bind `call`"))));
        }
        let input = serde_json::to_vec(&row.row).map_err(|e| permanent(e.to_string()))?;
        let mut attempt = 1u32;
        loop {
            let recorded = calls.call(&label(attempt), &input, &mut |idempotency| self.request(row, calls, attempt, idempotency, &input))?;
            match Answer::decode(&recorded, &self.tables).map_err(|e| permanent(format!("the recorded answer of delegate `{name}` does not decode: {e}")))? {
                Answer::Ok { rows } => return Ok(rows),
                Answer::Permanent { reason } => return Err(permanent(format!("delegate `{name}` answered permanent: {}", reason.unwrap_or_default()))),
                Answer::Transient { reason } => return Err(RowStop::Failed(Failure::new(FailureTag::Transient, reason.unwrap_or_default()))),
                // Park until `retry_at` through the row's awakeable, spending no attempt; the
                // first entry after that instant calls again under the next ordinal
                // (`surface.delegate.defer-parks`).
                Answer::Defer { retry_at, .. } => {
                    let until = Instant::parse(&retry_at).map_err(|e| permanent(e.to_string()))?;
                    let engine_now = Instant::parse(&calls.context().now).map(|i| i.unix_secs()).unwrap_or_else(|_| unix_now());
                    let ttl = u64::try_from(until.unix_secs().saturating_sub(engine_now)).unwrap_or(0).max(1);
                    let wait = format!("defer/{attempt}");
                    let token = calls.suspend(&wait, ttl)?;
                    match calls.awaited(&wait, &token)? {
                        Woken::TimedOut | Woken::Resumed(_) => attempt += 1,
                    }
                }
            }
        }
    }

    /// The delegate's name and declared version join the owner pins, so a rotated version
    /// opens a fresh execution identity (`surface.delegate.version`).
    fn dependencies(&self) -> BodyDependencies {
        let mut identities = BTreeMap::from([("delegate".to_string(), self.binding.name.clone())]);
        if let Some(version) = &self.binding.version {
            identities.insert("delegate_version".into(), version.clone());
        }
        BodyDependencies::from_identities(identities)
    }
}
