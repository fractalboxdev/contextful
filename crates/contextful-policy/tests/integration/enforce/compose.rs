//! `authority.compose`: the session's relation set.

use crate::support::{admit, grant, issuer, plan_for, dana, DURING};
use contextful_core::grant::Action;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::reconcile::{Column, ColumnType};
use contextful_policy::enforce::mask::Pepper;
use contextful_policy::enforce::policy::TablePolicy;
use contextful_policy::enforce::session::{ReachFilter, Request, Session, TableSource, RELATIONS_PER_SESSION, REACH_RELATION};
use contextful_policy::enforce::PolicyError;
use contextful_policy::issue::{mint, MintClaims};

fn source(name: &str) -> TableSource {
    let decl = TableDecl::named(name);
    TableSource {
        policy: TablePolicy::from_decl(&decl).unwrap(),
        decl,
        base: "SELECT 1 AS \"x\"".into(),
        files: Vec::new(),
        columns: vec![Column::new("x", ColumnType::Int32, false)],
        landed: true,
        ledger: Vec::new(),
        resolved: None,
    }
}

/// A session carries at most 1024 entries in its relation set.
// spec: authority.compose.relations-per-session@ee795f7c
#[test]
fn a_session_holds_at_most_1024_relations() {
    assert_eq!(RELATIONS_PER_SESSION, 1024);
    let signer = issuer();
    let token = mint(&plan_for(&signer, dana(), vec![grant(&[Action::Read], &["research/*"])]), &MintClaims::default(), &signer).unwrap();
    let authority = admit(&token, &signer, DURING).unwrap();
    let pepper = Pepper::resolve(|_| None);
    let tables = |n: usize| (0..n).map(|i| source(&format!("research/t{i:04}"))).collect::<Vec<_>>();

    let mut at_bound = tables(1024);
    // Ungranted tables are no part of the relation set.
    at_bound.push(source("hr/salaries"));
    let session = Session::open(&authority, &Request::default(), at_bound, &pepper).unwrap();
    assert_eq!(session.relations().count(), 1024);
    assert!(!session.reads("hr/salaries"));

    let over = Session::open(&authority, &Request::default(), tables(1025), &pepper);
    assert!(matches!(over, Err(PolicyError::Malformed(ref m)) if m.0.contains("1024")), "{over:?}");
}

#[test]
fn a_named_ledger_counts_toward_the_relation_set() {
    let signer = issuer();
    let token = mint(&plan_for(&signer, dana(), vec![grant(&[Action::Read], &["research/*"])]), &MintClaims::default(), &signer).unwrap();
    let authority = admit(&token, &signer, DURING).unwrap();
    let pepper = Pepper::resolve(|_| None);
    let tables = |n: usize| (0..n).map(|i| source(&format!("research/t{i:04}"))).collect::<Vec<_>>();
    let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<std::collections::BTreeSet<String>>();

    let under = Session::open(&authority, &Request::default(), tables(1023), &pepper).unwrap();
    assert_eq!(under.ledgers_named(&names(&["research/t0000__requests", "research/t0000"])).unwrap().len(), 1);

    let full = Session::open(&authority, &Request::default(), tables(1024), &pepper).unwrap();
    assert!(full.ledgers_named(&names(&["research/t0000"])).unwrap().is_empty());
    let over = full.ledgers_named(&names(&["research/t0000__requests"]));
    assert!(matches!(over, Err(PolicyError::Malformed(ref m)) if m.0.contains("1024")), "{over:?}");
}

#[test]
fn session_records_whether_each_registered_table_had_landed() {
    let signer = issuer();
    let token = mint(&plan_for(&signer, dana(), vec![grant(&[Action::Read], &["research/*"])]), &MintClaims::default(), &signer).unwrap();
    let authority = admit(&token, &signer, DURING).unwrap();
    let mut quiet = source("research/quiet");
    quiet.landed = false;
    let session = Session::open(&authority, &Request::default(), vec![source("research/notes"), quiet], &Pepper::resolve(|_| None)).unwrap();
    assert_eq!(session.landed("research/notes"), Some(true));
    assert_eq!(session.landed("research/quiet"), Some(false));
    assert_eq!(session.landed("hr/salaries"), None);
}

/// The mirrored-permission semi-join opens a bound table's relation ahead of every other
/// step, reads its ids from the reach relation by parameter, and binds no ungranted table.
#[test]
fn the_semi_join_opens_a_bound_relation_and_carries_its_ids_as_rows() {
    let signer = issuer();
    let token = mint(&plan_for(&signer, dana(), vec![grant(&[Action::Read], &["research/*"])]), &MintClaims::default(), &signer).unwrap();
    let authority = admit(&token, &signer, DURING).unwrap();
    let pepper = Pepper::resolve(|_| None);
    let filter = |ids: &[&str]| ReachFilter { resource_key: "x".into(), resources: ids.iter().map(|s| s.to_string()).collect() };
    let reach = std::collections::BTreeMap::from([("research/pages".to_string(), filter(&["p1", "p2"])), ("hr/salaries".to_string(), filter(&["s1"]))]);
    let session = Session::open_reaching(&authority, &Request::default(), vec![source("research/pages"), source("research/notes"), source("hr/salaries")], reach, &pepper).unwrap();
    let sql = session.relation("research/pages").unwrap().sql().to_string();
    let filter_at = sql.find(REACH_RELATION).unwrap_or_else(|| panic!("{sql}"));
    assert!(sql[sql.find("WHERE").unwrap()..].starts_with("WHERE CAST(\"x\" AS VARCHAR) IN"), "{sql}");
    assert!(filter_at > 0 && !sql.contains("p1"), "{sql}");
    assert!(!session.relation("research/notes").unwrap().sql().contains(REACH_RELATION));
    assert_eq!(session.reach_rows(), [("research/pages".to_string(), "p1".to_string()), ("research/pages".to_string(), "p2".to_string())]);
}
