//! Durable D1 callback guards (`run.callback-guard`): the rows each guard protects, the
//! effect identity it records, the holder identity whose exit it waits on, unattended
//! release over a replay-safe effect, and reconciliation against matching evidence.

use crate::catalog::{storage, D1Catalog, NOW_US};
use crate::{Statement, D1};
use contextful_core::coordinate::{ExecutionAdmission, ExecutionGuard};
use contextful_core::run::journal::EntryKey;
use contextful_core::run::own::OwnerScope;
use contextful_core::run::record::RunRow;
use contextful_core::run::{Failure, FailureTag};
use contextful_core::time::Instant;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// The variable a serve process sets on each child it dispatches, naming its own holder
/// identity, so its reaping of that child records the child's exit
/// (`run.callback-guard.holder-gone`).
pub const GUARD_PARENT_VAR: &str = "CONTEXTFUL_GUARD_PARENT";

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A random 64-hex identity: the process, the clock, a counter and the standard library's
/// per-process random hasher seed.
pub fn draw_identity() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u64(NEXT.fetch_add(1, Ordering::SeqCst));
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!(
        "{:x}",
        Sha256::digest(format!("{}:{nanos}:{}", std::process::id(), hasher.finish()))
    )
}

/// The protected-row key of an owner scope.
pub(crate) fn owner_row(scope: &OwnerScope) -> Result<String, Failure> {
    Ok(format!("owner:{}", serde_json::to_string(scope).map_err(storage)?))
}

/// The protected-row key of a lease.
pub(crate) fn lease_row(key: &str) -> String {
    format!("lease:{key}")
}

/// The holder identity of one engine: drawn at start, registered in D1 before its first
/// guard, and recorded as exited when its last guard and catalog drop
/// (`run.callback-guard.holder-identity`, `run.callback-guard.holder-gone`).
pub(crate) struct Holder {
    endpoint: String,
    pub(crate) id: String,
    parent: Option<String>,
    registered: AtomicBool,
}

impl Holder {
    pub(crate) fn draw(endpoint: &str) -> Arc<Self> {
        Arc::new(Self {
            endpoint: endpoint.into(),
            id: draw_identity(),
            parent: std::env::var(GUARD_PARENT_VAR).ok().filter(|p| !p.trim().is_empty()),
            registered: AtomicBool::new(false),
        })
    }

    fn register(&self, db: &D1) -> Result<(), Failure> {
        if self.registered.load(Ordering::SeqCst) {
            return Ok(());
        }
        let sql = format!("INSERT OR IGNORE INTO contextful_guard_holder (holder, parent, pid, started_at) VALUES (?, ?, ?, {NOW_US})");
        db.query(&sql, vec![json!(self.id), json!(self.parent), json!(std::process::id())])
            .map_err(storage)?;
        self.registered.store(true, Ordering::SeqCst);
        Ok(())
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        if !self.registered.load(Ordering::SeqCst) {
            return;
        }
        if let Ok(db) = D1::connect(&self.endpoint) {
            let sql = format!("UPDATE contextful_guard_holder SET exited_at = {NOW_US} WHERE holder = ? AND exited_at IS NULL");
            let _ = db.query(&sql, vec![json!(self.id)]);
        }
    }
}

/// A held guard; dropping it when the callback returns deletes it
/// (`run.callback-guard.release`).
struct Guard {
    endpoint: String,
    token: String,
    _holder: Arc<Holder>,
}
impl ExecutionGuard for Guard {}
impl Drop for Guard {
    fn drop(&mut self) {
        if let Ok(db) = D1::connect(&self.endpoint) {
            let _ = db.batch(&[
                Statement { sql: "DELETE FROM contextful_guard WHERE token = ?", params: vec![json!(self.token)] },
                Statement { sql: "DELETE FROM contextful_callback_guard WHERE token = ?", params: vec![json!(self.token)] },
            ]);
        }
    }
}

/// An admission protecting no row holds nothing.
struct Unprotected;
impl ExecutionGuard for Unprotected {}

/// A lease a guard's callback admitted, by its exact holder and fence.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LeaseClaim {
    key: String,
    holder: String,
    fence: u64,
}

/// A guard's evidence class (`run.callback-guard.class`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceClass {
    ReplaySafe,
    ReceiptRequired,
    Unknown,
}

impl EvidenceClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReplaySafe => "replay-safe",
            Self::ReceiptRequired => "receipt-required",
            Self::Unknown => "unknown",
        }
    }
}

/// One guard as `contextful storage guard` prints it (`run.callback-guard.inspect`).
#[derive(Debug, Clone, Serialize)]
pub struct GuardView {
    pub token: String,
    pub scopes: Vec<OwnerScope>,
    pub lease_keys: Vec<String>,
    pub execution: String,
    pub attempt: Option<String>,
    pub holder: String,
    pub entry_key: EntryKey,
    pub idempotency_key: String,
    pub admitted_at: Instant,
    /// The declared deduplication window in seconds, or `"none"`.
    pub window: Value,
    pub class: EvidenceClass,
}

/// A guard's recorded identity (`run.callback-guard.identity`).
struct Record {
    token: String,
    holder: String,
    execution: String,
    attempt: Option<String>,
    entry_key: EntryKey,
    idempotency_key: String,
    admitted_at_us: i64,
    window_us: Option<i64>,
    publication: bool,
    scopes: Vec<OwnerScope>,
    runs: Vec<String>,
    leases: Vec<LeaseClaim>,
}

/// What the guard's holder and owner lease show now, and the rows that showed it.
struct Observed {
    holder_gone: bool,
    lease_lapsed: bool,
    covered: bool,
    runs: Vec<(String, Option<String>)>,
}

impl Record {
    fn parse(row: &Value) -> Result<Self, Failure> {
        let text = |key: &str| row.get(key).and_then(Value::as_str).map(str::to_owned);
        let need = |key: &str| text(key).ok_or_else(|| storage(format!("guard missing {key}")));
        let json_of = |key: &str| -> Result<Value, Failure> { serde_json::from_str(&need(key)?).map_err(storage) };
        Ok(Self {
            token: need("token")?,
            holder: need("holder")?,
            execution: need("execution_id")?,
            attempt: text("attempt"),
            entry_key: serde_json::from_value(json_of("entry_key")?).map_err(storage)?,
            idempotency_key: need("idempotency_key")?,
            admitted_at_us: row.get("admitted_at").and_then(Value::as_i64).ok_or_else(|| storage("guard missing admitted_at"))?,
            window_us: row.get("window_us").and_then(Value::as_i64),
            publication: row.get("publication").and_then(Value::as_i64) == Some(1),
            scopes: serde_json::from_value(json_of("scopes")?).map_err(storage)?,
            runs: serde_json::from_value(json_of("runs")?).map_err(storage)?,
            leases: serde_json::from_value(json_of("leases")?).map_err(storage)?,
        })
    }

    /// The window covers the key from admission until admission plus the window; `none`
    /// covers nothing (`run.callback-guard.window`).
    fn covers(&self, now_us: i64) -> bool {
        self.window_us.is_some_and(|w| now_us < self.admitted_at_us.saturating_add(w))
    }

    fn class(&self, covered: bool) -> EvidenceClass {
        if covered {
            EvidenceClass::ReplaySafe
        } else if self.publication {
            EvidenceClass::ReceiptRequired
        } else {
            EvidenceClass::Unknown
        }
    }

    /// SQL predicates, with their arguments, that hold while the guard's owner lease stays
    /// lapsed as observed: each admitted attempt's row unchanged, each admitted lease unheld.
    fn lapsed_predicates(&self, observed: &Observed) -> (String, Vec<Value>) {
        let mut sql = String::from("1");
        let mut args = Vec::new();
        for (run_id, row) in &observed.runs {
            match row {
                Some(row) => {
                    sql.push_str(" AND EXISTS (SELECT 1 FROM contextful_run WHERE run_id = ? AND row = ?)");
                    args.extend([json!(run_id), json!(row)]);
                }
                None => {
                    sql.push_str(" AND NOT EXISTS (SELECT 1 FROM contextful_run WHERE run_id = ?)");
                    args.push(json!(run_id));
                }
            }
        }
        for lease in &self.leases {
            sql.push_str(&format!(" AND NOT EXISTS (SELECT 1 FROM contextful_lease WHERE key = ? AND holder = ? AND fence = ? AND expires_us > {NOW_US})"));
            args.extend([json!(lease.key), json!(lease.holder), json!(lease.fence)]);
        }
        (sql, args)
    }
}

fn blocked(token: &str, class: EvidenceClass) -> Failure {
    let next = if class == EvidenceClass::ReplaySafe {
        "; it releases once its holder stops and its owner lease lapses".to_string()
    } else {
        format!("; release it with `contextful storage reconcile-guard --token {token}`")
    };
    Failure::new(
        FailureTag::Transient,
        format!("execution callback guard `{token}` ({}) blocks D1 lease or owner transfer{next}", class.as_str()),
    )
}

fn now_us(now: Instant) -> i64 {
    i64::try_from(now.unix_nanos() / 1000).unwrap_or(i64::MAX)
}

/// Hex digits of a guard token: 96 random bits, below the 32-character run a recorded
/// error masks, so a refusal on the run record still names the token.
const TOKEN_HEX: usize = 24;

fn is_token(token: &str) -> bool {
    token.len() == TOKEN_HEX && token.bytes().all(|b| b.is_ascii_hexdigit())
}

impl D1Catalog {
    /// Admit a callback: check the exact owner, active attempt and fence of every row it
    /// protects and bind them to one new guard in one D1 statement
    /// (`run.callback-guard.scope`, `run.callback-guard.acquire`).
    pub(crate) fn admit(&self, admission: &ExecutionAdmission) -> Result<Box<dyn ExecutionGuard>, Failure> {
        let mut rows = Vec::new();
        let mut predicates = String::new();
        let mut args = Vec::new();
        let mut runs = Vec::new();
        let mut scopes = Vec::new();
        let now = self.now_at()?;
        for expected in admission.owners() {
            let row = owner_row(&expected.scope)?;
            let (owner_text, run_text, owner_expiry_us) = self.admitted_owner(expected, admission.publication, now)?;
            let scope = serde_json::to_string(&expected.scope).map_err(storage)?;
            predicates.push_str(" AND EXISTS (SELECT 1 FROM contextful_scope WHERE scope = ? AND owner = ?)");
            args.extend([json!(scope), json!(owner_text)]);
            predicates.push_str(&format!(" AND EXISTS (SELECT 1 FROM contextful_run WHERE run_id = ? AND row = ?) AND ? > {NOW_US}"));
            args.extend([json!(expected.run_id), json!(run_text), json!(owner_expiry_us)]);
            rows.push(row);
            runs.push(expected.run_id.clone());
            scopes.push(expected.scope.clone());
        }
        let mut claims = Vec::new();
        for lease in admission.leases() {
            predicates.push_str(&format!(" AND EXISTS (SELECT 1 FROM contextful_lease WHERE key = ? AND holder = ? AND fence = ? AND expires_us > {NOW_US})"));
            args.extend([json!(lease.key), json!(lease.holder), json!(lease.fence)]);
            rows.push(lease_row(&lease.key));
            claims.push(LeaseClaim { key: lease.key.clone(), holder: lease.holder.clone(), fence: lease.fence });
        }
        if rows.is_empty() {
            return Ok(Box::new(Unprotected));
        }
        rows.sort();
        rows.dedup();
        self.holder.register(&self.db)?;
        let token = draw_identity()[..TOKEN_HEX].to_string();
        let entry = admission.effect().map(|e| e.entry.clone()).unwrap_or_else(|| {
            let execution = admission.owners().first().map_or("callback", |o| o.execution_id.as_str());
            EntryKey::new(execution, "callback", token.as_bytes())
        });
        let window_us = admission.effect().and_then(|e| e.window_secs).map(|s| i64::try_from(s).unwrap_or(i64::MAX / 1_000_000).saturating_mul(1_000_000));
        let attempt = admission
            .owners()
            .iter()
            .find(|o| o.execution_id == entry.execution_id)
            .or_else(|| admission.owners().first())
            .map(|o| o.run_id.clone());
        let protected = serde_json::to_string(&rows).map_err(storage)?;
        let bind = format!("INSERT INTO contextful_guard (scope, token, admitted_at) SELECT j.value, ?, {NOW_US} FROM json_each(?) j WHERE NOT EXISTS (SELECT 1 FROM contextful_guard WHERE scope IN (SELECT value FROM json_each(?))){predicates}");
        let mut bind_args = vec![json!(token), json!(protected), json!(protected)];
        bind_args.extend(args);
        let identity = "INSERT INTO contextful_callback_guard (token, holder, execution_id, attempt, entry_key, idempotency_key, admitted_at, window_us, publication, scopes, runs, leases) SELECT ?, ?, ?, ?, ?, ?, g.admitted_at, ?, ?, ?, ?, ? FROM contextful_guard g WHERE g.token = ? LIMIT 1";
        let identity_args = vec![
            json!(token),
            json!(self.holder.id),
            json!(entry.execution_id),
            json!(attempt),
            json!(serde_json::to_string(&entry).map_err(storage)?),
            json!(entry.idempotency_key()),
            json!(window_us),
            json!(i64::from(admission.publication)),
            json!(serde_json::to_string(&scopes).map_err(storage)?),
            json!(serde_json::to_string(&runs).map_err(storage)?),
            json!(serde_json::to_string(&claims).map_err(storage)?),
            json!(token),
        ];
        let results = self
            .db
            .batch(&[Statement { sql: &bind, params: bind_args }, Statement { sql: identity, params: identity_args }])
            .map_err(storage)?;
        if results.len() != 2 || results[0].changes != rows.len() as u64 || results[1].changes != 1 {
            for row in &rows {
                self.unguarded(row)?;
            }
            return Err(Failure::new(FailureTag::Transient, "execution admission was superseded"));
        }
        Ok(Box::new(Guard { endpoint: self.db.endpoint.clone(), token, _holder: self.holder.clone() }))
    }

    fn admitted_owner(
        &self,
        expected: &contextful_core::coordinate::ExecutionExpectation,
        publication: bool,
        now: Instant,
    ) -> Result<(String, String, i64), Failure> {
        use contextful_core::run::own::ExecutionOwner;
        let superseded = || Failure::new(FailureTag::Transient, "execution admission was superseded");
        let attempt_gone = || Failure::new(FailureTag::Transient, "execution attempt no longer admits the callback");
        let scope = serde_json::to_string(&expected.scope).map_err(storage)?;
        let owner_text = self
            .db
            .query("SELECT owner FROM contextful_scope WHERE scope = ?", vec![json!(scope)])
            .map_err(storage)?
            .rows
            .first()
            .and_then(|r| r.get("owner"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(superseded)?;
        let owner: ExecutionOwner = serde_json::from_str(&owner_text).map_err(storage)?;
        if owner.execution_id != expected.execution_id
            || owner.pins != expected.pins
            || (expected.scope.host_id().is_some() && owner.attempts.last() != Some(&expected.run_id))
        {
            return Err(superseded());
        }
        let run_text = self
            .db
            .query("SELECT row FROM contextful_run WHERE run_id = ?", vec![json!(expected.run_id)])
            .map_err(storage)?
            .rows
            .first()
            .and_then(|r| r.get("row"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(attempt_gone)?;
        let run: RunRow = serde_json::from_str(&run_text).map_err(storage)?;
        let Some(lease) = run.owner.as_ref().filter(|o| !o.expired(now)) else { return Err(attempt_gone()) };
        if run.execution_id != expected.execution_id || !run.status.is_in_flight() || (!publication && run.stop.is_some()) {
            return Err(attempt_gone());
        }
        let expiry = i64::try_from(lease.lease_expires_at.unix_nanos() / 1000).map_err(storage)?;
        Ok((owner_text, run_text, expiry))
    }

    fn records(&self, row: Option<&str>) -> Result<Vec<Record>, Failure> {
        let result = match row {
            Some(row) => self.db.query(
                "SELECT * FROM contextful_callback_guard WHERE token IN (SELECT token FROM contextful_guard WHERE scope = ?) ORDER BY token",
                vec![json!(row)],
            ),
            None => self.db.query("SELECT * FROM contextful_callback_guard ORDER BY admitted_at, token", vec![]),
        }
        .map_err(storage)?;
        result.rows.iter().map(Record::parse).collect()
    }

    fn record(&self, token: &str) -> Result<Option<Record>, Failure> {
        self.db
            .query("SELECT * FROM contextful_callback_guard WHERE token = ?", vec![json!(token)])
            .map_err(storage)?
            .rows
            .first()
            .map(Record::parse)
            .transpose()
    }

    /// A holder is gone once its identity records an exit; a lapsed owner lease confirms
    /// nothing (`run.callback-guard.holder-gone`).
    fn observe(&self, record: &Record, now: Instant) -> Result<Observed, Failure> {
        let holder_gone = !self
            .db
            .query(
                "SELECT 1 FROM contextful_guard_holder WHERE holder = ? AND exited_at IS NOT NULL",
                vec![json!(record.holder)],
            )
            .map_err(storage)?
            .rows
            .is_empty();
        let mut lease_lapsed = true;
        let mut runs = Vec::new();
        for run_id in &record.runs {
            let text = self
                .db
                .query("SELECT row FROM contextful_run WHERE run_id = ?", vec![json!(run_id)])
                .map_err(storage)?
                .rows
                .first()
                .and_then(|r| r.get("row"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            if let Some(text) = &text {
                let run: RunRow = serde_json::from_str(text).map_err(storage)?;
                if run.status.is_in_flight() && run.owner.as_ref().is_some_and(|o| !o.expired(now)) {
                    lease_lapsed = false;
                }
            }
            runs.push((run_id.clone(), text));
        }
        for lease in &record.leases {
            use contextful_core::coordinate::{Catalog, Lease};
            let held = Lease { key: lease.key.clone(), holder: lease.holder.clone(), fence: lease.fence, expires_at: now };
            if self.lease_holds(&held)? {
                lease_lapsed = false;
            }
        }
        Ok(Observed { holder_gone, lease_lapsed, covered: record.covers(now_us(now)), runs })
    }

    /// Release statements for every guard protecting `row` once each is replay-safe with its
    /// holder gone and its owner lease lapsed, to commit in one batch before the transfer;
    /// any other guard blocks (`run.callback-guard.replay`, `run.callback-guard.blocked`).
    pub(crate) fn replayable(&self, row: &str) -> Result<Vec<(String, Vec<Value>)>, Failure> {
        let records = self.records(Some(row))?;
        if records.is_empty() {
            return Ok(Vec::new());
        }
        let now = self.now_at()?;
        let mut statements = Vec::new();
        for record in &records {
            let observed = self.observe(record, now)?;
            if !(observed.holder_gone && observed.lease_lapsed && observed.covered) {
                return Err(blocked(&record.token, record.class(observed.covered)));
            }
            let (lapsed, lapsed_args) = record.lapsed_predicates(&observed);
            let mut args = vec![json!(record.token), json!(record.holder)];
            args.extend(lapsed_args);
            statements.push((
                format!("DELETE FROM contextful_guard WHERE token = ? AND EXISTS (SELECT 1 FROM contextful_guard_holder WHERE holder = ? AND exited_at IS NOT NULL) AND EXISTS (SELECT 1 FROM contextful_callback_guard WHERE token = contextful_guard.token AND window_us IS NOT NULL AND {NOW_US} < admitted_at + window_us) AND {lapsed}"),
                args,
            ));
            statements.push((
                "DELETE FROM contextful_callback_guard WHERE token = ? AND NOT EXISTS (SELECT 1 FROM contextful_guard WHERE token = ?)".into(),
                vec![json!(record.token), json!(record.token)],
            ));
        }
        Ok(statements)
    }

    /// Run a transfer of `row` — its SQL carrying `NOT EXISTS` over `row`'s guards — after
    /// releasing, in the same batch, every replay-safe guard on it.
    pub(crate) fn transfer(&self, row: &str, sql: &str, args: Vec<Value>) -> Result<crate::QueryResult, Failure> {
        let releases = self.replayable(row)?;
        if releases.is_empty() {
            return self.db.query(sql, args).map_err(storage);
        }
        let mut statements: Vec<Statement<'_>> = releases.iter().map(|(sql, args)| Statement { sql, params: args.clone() }).collect();
        statements.push(Statement { sql, params: args });
        self.db.batch(&statements).map_err(storage)?.pop().ok_or_else(|| storage("empty batch answer"))
    }

    /// Refuse while a guard protects `row`, naming it (`run.callback-guard.blocked`).
    pub(crate) fn unguarded(&self, row: &str) -> Result<(), Failure> {
        let records = self.records(Some(row))?;
        if let Some(record) = records.first() {
            let now = self.now_at()?;
            return Err(blocked(&record.token, record.class(record.covers(now_us(now)))));
        }
        let legacy = self
            .db
            .query("SELECT token FROM contextful_guard WHERE scope = ?", vec![json!(row)])
            .map_err(storage)?;
        if let Some(token) = legacy.rows.first().and_then(|r| r.get("token")).and_then(Value::as_str) {
            return Err(blocked(token, EvidenceClass::Unknown));
        }
        Ok(())
    }

    /// Every guard with its protected rows and recorded identity (`run.callback-guard.inspect`).
    pub fn guards(&self) -> Result<Vec<GuardView>, Failure> {
        let now = now_us(self.now_at()?);
        self.records(None)?
            .into_iter()
            .map(|r| {
                let covered = r.covers(now);
                Ok(GuardView {
                    class: r.class(covered),
                    lease_keys: r.leases.iter().map(|l| l.key.clone()).collect(),
                    admitted_at: Instant::from_unix_nanos(i128::from(r.admitted_at_us) * 1000).map_err(storage)?,
                    window: r.window_us.map_or(json!("none"), |w| json!(w / 1_000_000)),
                    token: r.token,
                    scopes: r.scopes,
                    execution: r.execution,
                    attempt: r.attempt,
                    holder: r.holder,
                    entry_key: r.entry_key,
                    idempotency_key: r.idempotency_key,
                })
            })
            .collect()
    }

    /// Release a guard against evidence tied to its recorded key, appending an audit row
    /// naming token, evidence and operator in the same batch (`run.callback-guard.manual`,
    /// `run.callback-guard.live-holder`, `run.callback-guard.evidence`).
    pub fn reconcile_guard(&self, token: &str, evidence: &str, operator: &str) -> Result<(), Failure> {
        if !is_token(token) {
            return Err(storage(format!("ExecutionReconciliationGuardMoved: `{token}` names no callback guard")));
        }
        if operator.trim().is_empty() || operator.len() > 256 || evidence.len() > 512 {
            return Err(storage("reconciliation names no operator, or its operator or evidence exceeds its length"));
        }
        let record = self
            .record(token)?
            .ok_or_else(|| storage(format!("ExecutionReconciliationGuardMoved: `{token}` names no callback guard")))?;
        let now = self.now_at()?;
        let observed = self.observe(&record, now)?;
        if !observed.holder_gone && !observed.lease_lapsed {
            return Err(storage(format!(
                "ExecutionGuardHeld: guard `{token}` has an unexpired owner lease and its holder `{}` records no exit",
                record.holder
            )));
        }
        let mismatch = || {
            storage(format!(
                "ExecutionGuardEvidenceMismatch: guard `{token}` releases on `idempotency:{key}` within its window or `receipt:{key}:<id>`",
                key = record.idempotency_key
            ))
        };
        let windowed = if let Some(key) = evidence.strip_prefix("idempotency:") {
            if key != record.idempotency_key || !observed.covered {
                return Err(mismatch());
            }
            true
        } else if let Some(receipt) = evidence.strip_prefix("receipt:") {
            match receipt.split_once(':') {
                Some((key, id)) if key == record.idempotency_key && !id.trim().is_empty() => false,
                _ => return Err(mismatch()),
            }
        } else {
            return Err(mismatch());
        };
        let (lapsed, lapsed_args) = record.lapsed_predicates(&observed);
        let window = if windowed { format!(" AND window_us IS NOT NULL AND {NOW_US} < admitted_at + window_us") } else { String::new() };
        let audit = format!("INSERT INTO contextful_guard_reconciliation (token, evidence, operator, reconciled_at) SELECT token, ?, ?, {NOW_US} FROM contextful_callback_guard WHERE token = ? AND idempotency_key = ?{window} AND (EXISTS (SELECT 1 FROM contextful_guard_holder WHERE holder = ? AND exited_at IS NOT NULL) OR ({lapsed}))");
        let mut audit_args = vec![json!(evidence), json!(operator), json!(token), json!(record.idempotency_key), json!(record.holder)];
        audit_args.extend(lapsed_args);
        let audited = "EXISTS (SELECT 1 FROM contextful_guard_reconciliation WHERE token = ?)";
        let rows = format!("DELETE FROM contextful_guard WHERE token = ? AND {audited}");
        let identity = format!("DELETE FROM contextful_callback_guard WHERE token = ? AND {audited}");
        let changed = self
            .db
            .batch(&[
                Statement { sql: &audit, params: audit_args },
                Statement { sql: &rows, params: vec![json!(token), json!(token)] },
                Statement { sql: &identity, params: vec![json!(token), json!(token)] },
            ])
            .map_err(storage)?;
        if changed.len() != 3 || changed[0].changes != 1 || changed[2].changes != 1 {
            return Err(storage(format!("ExecutionReconciliationGuardMoved: guard `{token}` changed during reconciliation")));
        }
        Ok(())
    }

    /// Record the exit of each holder a serve process's child `pid` registered under the
    /// serve's identity `parent`, once that child is reaped (`run.callback-guard.holder-gone`).
    pub fn record_child_exit(&self, parent: &str, pid: u32) -> Result<(), Failure> {
        let sql = format!("UPDATE contextful_guard_holder SET exited_at = {NOW_US} WHERE parent = ? AND pid = ? AND exited_at IS NULL");
        self.db.query(&sql, vec![json!(parent), json!(pid)]).map_err(storage)?;
        Ok(())
    }

    /// This catalog's holder identity (`run.callback-guard.holder-identity`).
    pub fn holder_identity(&self) -> &str {
        &self.holder.id
    }
}
