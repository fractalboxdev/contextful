//! The disclosure budget behind the `BudgetLedger` port: a reservation is one transaction
//! over every contributing unit, so concurrent releases never overspend a cap.

use contextful_core::disclosure::noise::{Entropy, Spend};
use contextful_core::disclosure::release::{release, BudgetLedger, Breakdown, ReleaseError, ReleaseRequest, UnitBudget};
use contextful_core::pipeline::model::DisclosureDecl;
use contextful_sqlite::SqliteBudgetLedger;
use serde_json::json;
use std::collections::BTreeSet;
use std::sync::{Arc, Barrier};

struct Seeded(u64);

impl Entropy for Seeded {
    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn units(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|s| s.to_string()).collect()
}

fn micros(n: u64) -> Spend {
    Spend::from_micros(n).unwrap()
}

#[test]
fn concurrent_releases_over_one_unit_never_overspend_its_cap() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("budget.sqlite");
    let ledger = SqliteBudgetLedger::open(&path).unwrap();
    for t in ["t1", "t2", "t3"] {
        ledger.grant(t, 1_000_000).unwrap();
    }
    let policy: DisclosureDecl = serde_json::from_value(json!({
        "grouping_allowlist": ["industry"],
        "contributor_key": "tenant_id",
        "min_group_size": 2,
        "metric_bounds": { "revenue": { "lower": 0.0, "upper": 100.0, "quantum": 1.0 } },
    }))
    .unwrap();
    let breakdown = Breakdown {
        columns: vec!["industry".into(), "tenant_id".into(), "revenue".into()],
        grain: vec!["industry".into(), "tenant_id".into()],
        rows: ["t1", "t2", "t3"].iter().map(|t| json!({"industry": "energy", "tenant_id": t, "revenue": 10}).as_object().unwrap().clone()).collect(),
    };

    // Eight releases, each on its own connection, each asking 0.3 of a 1.0 cap.
    let threads = 8;
    let barrier = Arc::new(Barrier::new(threads));
    let handles: Vec<_> = (0..threads)
        .map(|i| {
            let (path, barrier, policy, breakdown) = (path.clone(), barrier.clone(), policy.clone(), breakdown.clone());
            std::thread::spawn(move || {
                let ledger = SqliteBudgetLedger::open(&path).unwrap();
                let request = ReleaseRequest { release_id: format!("r-{i}"), table: "revenue_by_industry".into(), spend: micros(300_000) };
                barrier.wait();
                release(&policy, &breakdown, &request, &ledger, &mut Seeded(i as u64 + 1))
            })
        })
        .collect();
    let outcomes: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let published = outcomes.iter().filter(|o| o.is_ok()).count();
    assert_eq!(published, 3, "{outcomes:?}");
    for o in outcomes.iter().filter_map(|o| o.as_ref().err()) {
        match o {
            ReleaseError::Refused(e) => assert_eq!(e.identifier(), "DisclosureUnitBudgetExhausted"),
            other => panic!("{other:?}"),
        }
    }
    for t in ["t1", "t2", "t3"] {
        assert_eq!(ledger.budget(t).unwrap(), Some(UnitBudget { cap: 1_000_000, spent: 900_000 }), "{t}");
    }
}

#[test]
fn a_reservation_charges_every_unit_or_none_and_a_grant_raises_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = SqliteBudgetLedger::open(&dir.path().join("budget.sqlite")).unwrap();
    ledger.grant("a", 500).unwrap();
    ledger.grant("b", 100).unwrap();

    match ledger.reserve("r-1", &units(&["a", "b"]), micros(200)) {
        Err(ReleaseError::Refused(e)) => assert_eq!(e.identifier(), "DisclosureUnitBudgetExhausted"),
        other => panic!("{other:?}"),
    }
    assert_eq!(ledger.budget("a").unwrap().unwrap().spent, 0);
    // A unit never granted holds no cap.
    assert!(ledger.reserve("r-2", &units(&["a", "c"]), micros(1)).is_err());
    assert_eq!(ledger.budget("c").unwrap(), None);

    ledger.grant("b", 300).unwrap();
    ledger.reserve("r-3", &units(&["a", "b"]), micros(200)).unwrap();
    assert_eq!(ledger.budget("a").unwrap(), Some(UnitBudget { cap: 500, spent: 200 }));
    assert_eq!(ledger.budget("b").unwrap(), Some(UnitBudget { cap: 300, spent: 200 }));

    // Reopening the file keeps every balance.
    drop(ledger);
    let reopened = SqliteBudgetLedger::open(&dir.path().join("budget.sqlite")).unwrap();
    assert_eq!(reopened.budget("b").unwrap(), Some(UnitBudget { cap: 300, spent: 200 }));
}
