//! Durable admission for bounded external callbacks. Each token owns a separate
//! connection, so run metadata and keeper updates never reuse its transaction.
use super::*;
use contextful_core::coordinate::{ExecutionAdmission, ExecutionGuard};
use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

struct Active {
    id: u64,
    database: String,
    scopes: Vec<OwnerScope>,
    leases: Vec<String>,
}
thread_local! { static ACTIVE: RefCell<Vec<Active>> = const { RefCell::new(Vec::new()) }; }
static NEXT: AtomicU64 = AtomicU64::new(0);
static HELD: AtomicUsize = AtomicUsize::new(0);
pub(super) fn active() -> bool {
    HELD.load(Ordering::SeqCst) != 0
}

struct Guard {
    client: Client,
    id: u64,
    _thread: PhantomData<Rc<()>>,
}
impl ExecutionGuard for Guard {}
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.client.batch_execute("ROLLBACK");
        ACTIVE.with(|active| active.borrow_mut().retain(|a| a.id != self.id));
        HELD.fetch_sub(1, Ordering::SeqCst);
    }
}

pub(super) fn mutation(
    db: &PgCatalog,
    scope: Option<&OwnerScope>,
    lease: Option<&str>,
) -> Result<(), Failure> {
    let held = ACTIVE.with(|active| {
        active.borrow().iter().any(|a| {
            a.database == db.name
                && (scope.is_some_and(|s| a.scopes.contains(s))
                    || lease.is_some_and(|l| a.leases.iter().any(|k| k == l)))
        })
    });
    if held {
        Err(Failure::new(
            FailureTag::Permanent,
            "a guarded callback cannot mutate its admitted owner or lease",
        ))
    } else {
        Ok(())
    }
}

pub(super) fn acquire(
    db: &PgCatalog,
    admission: &ExecutionAdmission,
) -> Result<Box<dyn ExecutionGuard>, Failure> {
    // A nested callback may retain the same rows, but cannot invert lock order by
    // introducing another scope or lease after its caller has acquired locks.
    let nested = ACTIVE.with(|active| {
        let rows = active.borrow();
        let held: Vec<_> = rows.iter().filter(|a| a.database == db.name).collect();
        held.is_empty()
            || (admission
                .owners()
                .iter()
                .all(|o| held.iter().any(|a| a.scopes.contains(&o.scope)))
                && admission
                    .leases()
                    .iter()
                    .all(|l| held.iter().any(|a| a.leases.contains(&l.key))))
    });
    if !nested {
        return Err(Failure::new(
            FailureTag::Permanent,
            "a nested guarded callback cannot admit additional rows",
        ));
    }
    let fail = |e: &dyn std::fmt::Display| storage(&db.name, e);
    let client = connect(&db.config, &db.tls).map_err(|e| fail(&e))?;
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    HELD.fetch_add(1, Ordering::SeqCst);
    let mut guard = Guard {
        client,
        id,
        _thread: PhantomData,
    };
    guard.client.batch_execute("BEGIN").map_err(|e| fail(&e))?;
    for expected in admission.owners() {
        let [p, t, k, c] = key(&expected.scope);
        let sql = format!("SELECT owner FROM contextful_scope WHERE {WHERE_KEY} FOR SHARE");
        let row = guard
            .client
            .query_opt(&sql, &[&p, &t, &k, &c])
            .map_err(|e| fail(&e))?;
        let owner: Option<ExecutionOwner> = row
            .and_then(|r| r.get::<_, Option<String>>(0))
            .map(|text| from_text(&text, &fail))
            .transpose()?;
        if !owner.as_ref().is_some_and(|o| {
            o.execution_id == expected.execution_id
                && o.pins == expected.pins
                && (expected.scope.host_id().is_none()
                    || o.attempts.last() == Some(&expected.run_id))
        }) {
            return Err(Failure::new(
                FailureTag::Transient,
                "execution admission was superseded",
            ));
        }
        // No run row lock spans the callback: stop and heartbeat remain reentrant.
        let sql = format!("SELECT row, {NOW_US} FROM contextful_run WHERE run_id = $1");
        let row = guard
            .client
            .query_opt(&sql, &[&expected.run_id])
            .map_err(|e| fail(&e))?;
        let run: Option<(RunRow, Instant)> = row
            .map(|r| {
                Ok((
                    from_text(r.get::<_, &str>(0), &fail)?,
                    instant_us(r.get(1), &fail)?,
                ))
            })
            .transpose()?;
        if !run.as_ref().is_some_and(|(r, now)| {
            r.execution_id == expected.execution_id
                && r.status.is_in_flight()
                && r.owner.as_ref().is_some_and(|o| !o.expired(*now))
                && (admission.publication || r.stop.is_none())
        }) {
            return Err(Failure::new(
                FailureTag::Transient,
                "execution attempt no longer admits the callback",
            ));
        }
    }
    for lease in admission.leases() {
        let fence = fence_i64(lease.fence, &fail)?;
        let sql = format!("SELECT 1 FROM contextful_lease WHERE key = $1 AND holder = $2 AND fence = $3 AND expires_us > {NOW_US} FOR SHARE");
        if guard
            .client
            .query_opt(&sql, &[&lease.key, &lease.holder, &fence])
            .map_err(|e| fail(&e))?
            .is_none()
        {
            return Err(Failure::new(
                FailureTag::Transient,
                "execution lease was superseded or expired",
            ));
        }
    }
    ACTIVE.with(|active| {
        active.borrow_mut().push(Active {
            id,
            database: db.name.clone(),
            scopes: admission.owners().iter().map(|o| o.scope.clone()).collect(),
            leases: admission.leases().iter().map(|l| l.key.clone()).collect(),
        })
    });
    Ok(Box::new(guard))
}
