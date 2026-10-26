//! `authority.attenuate`: the narrowing legality of a proposed child against its parent.

use contextful_core::attenuate::{attenuate, check_chain, Authority, Proposal};
use contextful_core::grant::{AggregateGrant, Action, Grant, TablePattern, TenantScope};
use contextful_core::identify::{Subject, SubjectDerivation};
use contextful_core::AuthorityError;

const EXP: i64 = 1_770_000_900;

fn grant(actions: &[Action], tables: &[&str]) -> Grant {
    Grant {
        horizon: None,
        actions: actions.to_vec(),
        tables: tables.iter().map(|t| TablePattern::parse(t).unwrap()).collect(),
        tenant: None,
        aggregate: None,
        templates: None,
        max_rows: None,
        max_duration_ms: None,
        max_response_bytes: None,
    }
}

fn tenant(table: &str, value: &str) -> Option<TenantScope> {
    Some(TenantScope { table: table.to_string(), value: value.to_string() })
}

fn aggregate(min_group_size: u64, max_groups: u64) -> AggregateGrant {
    AggregateGrant {
        min_group_size,
        max_contributor_share: 0.5,
        functions: vec!["count".to_string(), "sum".to_string()],
        max_groups,
        max_rows: None,
    }
}

fn parent(grants: Vec<Grant>) -> Authority {
    Authority {
        grants,
        exp: EXP,
        subject: Subject { on_behalf_of: Some("user://dana@acme.example".to_string()), ..Subject::default() }.normalize(),
    }
}

fn child(grants: Vec<Grant>) -> Proposal {
    Proposal { grants: Some(grants), exp: None, subject: SubjectDerivation::default() }
}

fn widened(parent: &Authority, proposal: &Proposal) -> String {
    match attenuate(parent, proposal) {
        Err(AuthorityError::AttenuationWidens(m)) => m,
        other => panic!("expected AttenuationWidens, got {other:?}"),
    }
}

/// A child broader than its parent on actions, tables, templates, aggregate constraints, or a `max_rows`, `max_duration_ms` or
/// `max_response_bytes` ceiling raises `AttenuationWidens`, naming the dimension.
// spec: authority.attenuate.widens@6643708e
#[test]
fn widens() {
    let base = Grant {
        templates: Some(vec!["quarterly_rollup".to_string()]),
        ..grant(&[Action::Read, Action::Write], &["research/*"])
    };
    let p = parent(vec![base.clone()]);

    // Equal and narrower admit.
    assert!(attenuate(&p, &child(vec![base.clone()])).is_ok());
    let narrower = Grant { templates: None, ..grant(&[Action::Read], &["research/filings"]) };
    assert!(attenuate(&p, &child(vec![narrower])).is_ok());

    let actions = Grant { actions: vec![Action::Read, Action::Admin], ..base.clone() };
    assert!(matches!(attenuate(&p, &child(vec![actions])), Err(AuthorityError::AttenuationWidens(m)) if m.starts_with("actions")));

    for tables in [&["*"][..], &["research/filings", "sales/invoices"][..], &["research*"][..]] {
        let g = Grant { tables: tables.iter().map(|t| TablePattern::parse(t).unwrap()).collect(), ..base.clone() };
        assert!(widened(&p, &child(vec![g])).starts_with("tables"), "{tables:?}");
    }

    let templates = Grant { templates: Some(vec!["*".to_string()]), ..base.clone() };
    assert!(widened(&p, &child(vec![templates])).starts_with("templates"));

    // Aggregate: every constraint narrows or holds; absent on the child inherits the parent's.
    let agg_parent = parent(vec![Grant { aggregate: Some(aggregate(10, 100)), ..base.clone() }]);
    let tighter = Grant { aggregate: Some(aggregate(20, 50)), ..base.clone() };
    assert!(attenuate(&agg_parent, &child(vec![tighter])).is_ok());
    for looser in [aggregate(5, 100), aggregate(10, 200)] {
        let g = Grant { aggregate: Some(looser), ..base.clone() };
        assert!(widened(&agg_parent, &child(vec![g])).starts_with("aggregate"));
    }
    let extra_fn = AggregateGrant { functions: vec!["avg".to_string()], ..aggregate(10, 100) };
    assert!(widened(&agg_parent, &child(vec![Grant { aggregate: Some(extra_fn), ..base.clone() }])).starts_with("aggregate"));
    let inherited = attenuate(&agg_parent, &child(vec![base.clone()])).unwrap();
    assert_eq!(inherited.grants[0].aggregate, Some(aggregate(10, 100)));

    // A child grant needs one parent grant covering it whole; two halves from two grants do not combine.
    let split = parent(vec![grant(&[Action::Read], &["research/*"]), grant(&[Action::Write], &["sales/*"])]);
    assert!(attenuate(&split, &child(vec![grant(&[Action::Write], &["sales/invoices"])])).is_ok());
    assert!(widened(&split, &child(vec![grant(&[Action::Read, Action::Write], &["research/filings"])])).starts_with("actions"));

    // A child restating no grants inherits its parent's.
    let same = attenuate(&p, &Proposal { grants: None, exp: None, subject: SubjectDerivation::default() }).unwrap();
    assert_eq!(same.grants, p.grants);
}

/// A broader `max_rows`, `max_duration_ms` or `max_response_bytes` ceiling refuses, naming it; an equal or narrower
/// one takes effect as the child names it, an absent one inherits its parent's, and an unbounded parent bounds nothing.
// spec: authority.attenuate.widens@6643708e
#[test]
fn a_broader_ceiling_widens_and_an_absent_one_inherits() {
    let ceilings = |rows: Option<u64>, ms: Option<u64>, bytes: Option<u64>| Grant {
        max_rows: rows,
        max_duration_ms: ms,
        max_response_bytes: bytes,
        ..grant(&[Action::Read], &["research/*"])
    };
    let p = parent(vec![ceilings(Some(500), Some(2_000), Some(65_536))]);

    let rows = attenuate(&p, &child(vec![ceilings(Some(501), Some(2_000), Some(65_536))]));
    assert!(matches!(rows, Err(AuthorityError::AttenuationWidens(m)) if m.starts_with("max_rows:")));
    assert!(widened(&p, &child(vec![ceilings(Some(500), Some(2_001), Some(65_536))])).starts_with("max_duration_ms:"));
    assert!(widened(&p, &child(vec![ceilings(Some(500), Some(2_000), Some(65_537))])).starts_with("max_response_bytes:"));

    let narrower = attenuate(&p, &child(vec![ceilings(Some(100), Some(2_000), Some(1_024))])).unwrap();
    assert_eq!(narrower.grants[0], ceilings(Some(100), Some(2_000), Some(1_024)));
    let inherited = attenuate(&p, &child(vec![ceilings(None, None, None)])).unwrap();
    assert_eq!(inherited.grants[0], ceilings(Some(500), Some(2_000), Some(65_536)));

    let unbounded = parent(vec![ceilings(None, None, None)]);
    let bounded = attenuate(&unbounded, &child(vec![ceilings(Some(10), Some(10), Some(10))])).unwrap();
    assert_eq!(bounded.grants[0], ceilings(Some(10), Some(10), Some(10)));
}

/// A child whose expiry falls past its parent's raises `AttenuationExpiryExtended`.
// spec: authority.attenuate.expiry-extended@fbddfaff
#[test]
fn expiry_extended() {
    let p = parent(vec![grant(&[Action::Read], &["*"])]);
    let at = |exp: Option<i64>| Proposal { grants: None, exp, subject: SubjectDerivation::default() };

    assert!(matches!(attenuate(&p, &at(Some(EXP + 1))), Err(AuthorityError::AttenuationExpiryExtended(_))));
    assert_eq!(attenuate(&p, &at(Some(EXP))).unwrap().exp, EXP);
    assert_eq!(attenuate(&p, &at(Some(EXP - 60))).unwrap().exp, EXP - 60);
    assert_eq!(attenuate(&p, &at(None)).unwrap().exp, EXP, "absent inherits the parent's");
}

/// A child dropping its parent's tenant scope or naming another tenant raises `AttenuationTenantDropped`. Adding a scope to an unscoped parent narrows.
// spec: authority.attenuate.tenant-dropped@e4bf86ad
#[test]
fn tenant_dropped() {
    let scoped = Grant { tenant: tenant("research/filings", "acme-eu"), ..grant(&[Action::Read], &["research/*"]) };
    let p = parent(vec![scoped.clone()]);

    assert!(attenuate(&p, &child(vec![scoped.clone()])).is_ok());
    for t in [None, tenant("research/filings", "acme-us"), tenant("research/other", "acme-eu")] {
        let g = Grant { tenant: t.clone(), ..scoped.clone() };
        assert!(
            matches!(attenuate(&p, &child(vec![g])), Err(AuthorityError::AttenuationTenantDropped(_))),
            "{t:?}"
        );
    }

    let unscoped = parent(vec![grant(&[Action::Read], &["research/*"])]);
    assert!(attenuate(&unscoped, &child(vec![scoped])).is_ok());
}

/// Every dimension of a child obeys the narrowing table in Shapes, checked by the deriving holder and again across the whole chain at admission.
// spec: authority.attenuate.narrowing@d61d3afa
#[test]
fn narrowing() {
    let root = parent(vec![grant(&[Action::Read], &["research/*"])]);
    let widen = child(vec![grant(&[Action::Read], &["*"])]);
    let narrow = child(vec![grant(&[Action::Read], &["research/filings"])]);

    // The last hop alone lies within the root; the chain still refuses at the widened hop.
    assert!(attenuate(&root, &narrow).is_ok());
    assert!(matches!(check_chain(&root, &[widen, narrow.clone()]), Err(AuthorityError::AttenuationWidens(_))));

    // A legal chain yields the effective authority of its final hop, inheriting what it omits.
    let later = Proposal {
        grants: None,
        exp: Some(EXP - 300),
        subject: SubjectDerivation { incognito: Some(true), ..Default::default() },
    };
    let last = check_chain(&root, &[narrow, later]).unwrap();
    assert_eq!(last.grants, vec![grant(&[Action::Read], &["research/filings"])]);
    assert_eq!(last.exp, EXP - 300);
    assert!(last.subject.incognito());
    assert_eq!(last.subject.on_behalf_of(), Some("user://dana@acme.example"));

    // The subject rules sit in the same table: a rebound on_behalf_of refuses.
    let rebound = Proposal {
        grants: None,
        exp: None,
        subject: SubjectDerivation { on_behalf_of: Some("user://erin@acme.example".to_string()), ..Default::default() },
    };
    assert!(matches!(check_chain(&root, &[rebound]), Err(AuthorityError::AuthoritySubjectRebound(_))));
}

/// Narrowing compares the groups-per-query ceilings aggregate grants take effect with: a
/// declared 0 takes effect as 1, so a child declaring 1 under it is equal, not broader.
#[test]
fn group_ceiling_narrows_by_effective_ceiling() {
    let under = |parent_groups: u64, child_groups: u64| {
        let p = parent(vec![Grant { aggregate: Some(aggregate(10, parent_groups)), ..grant(&[Action::Read], &["t"]) }]);
        attenuate(&p, &child(vec![Grant { aggregate: Some(aggregate(10, child_groups)), ..grant(&[Action::Read], &["t"]) }]))
    };
    assert!(under(0, 1).is_ok(), "1 under a declared 0 is the same ceiling of 1");
    assert!(under(1, 0).is_ok());
    assert!(under(0, 0).is_ok());
    assert!(matches!(under(0, 2), Err(AuthorityError::AttenuationWidens(_))));
    assert!(matches!(under(5, 6), Err(AuthorityError::AttenuationWidens(_))));
}

fn at(s: &str) -> contextful_core::time::Instant {
    contextful_core::time::Instant::parse(s).unwrap()
}

/// A child whose horizon falls later than its parent's is broader on `horizon` and refuses as {{authority.attenuate.widens}}, naming that dimension; a child naming no horizon inherits its parent's.
// spec: authority.attenuate.horizon-widens@89f05d6c
#[test]
fn a_later_horizon_widens_and_an_absent_one_inherits() {
    let read = |horizon: Option<&str>| Grant { horizon: horizon.map(at), ..grant(&[Action::Read], &["research/*"]) };
    let p = parent(vec![read(Some("2015-01-01T00:00:00Z"))]);

    assert!(widened(&p, &child(vec![read(Some("2016-01-01T00:00:00Z"))])).starts_with("horizon:"));
    let inherited = attenuate(&p, &child(vec![read(None)])).unwrap();
    assert_eq!(inherited.grants[0].horizon, Some(at("2015-01-01T00:00:00Z")));
    let earlier = attenuate(&p, &child(vec![read(Some("2014-06-01T00:00:00Z"))])).unwrap();
    assert_eq!(earlier.grants[0].horizon, Some(at("2014-06-01T00:00:00Z")));

    // An unbounded parent admits any horizon; the chain check holds every hop.
    let open = parent(vec![read(None)]);
    let hops = [child(vec![read(Some("2015-01-01T00:00:00Z"))]), child(vec![read(Some("2016-01-01T00:00:00Z"))])];
    match check_chain(&open, &hops) {
        Err(AuthorityError::AttenuationWidens(m)) => assert!(m.starts_with("horizon:"), "{m}"),
        other => panic!("expected AttenuationWidens, got {other:?}"),
    }
}
