//! `authority.compose`: the session's relation set.

use crate::support::{admit, grant, issuer, plan_for, dana, DURING};
use contextful_core::grant::Action;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::reconcile::{Column, ColumnType};
use contextful_policy::enforce::mask::Pepper;
use contextful_policy::enforce::policy::TablePolicy;
use contextful_policy::enforce::session::{Request, Session, TableSource, RELATIONS_PER_SESSION};
use contextful_policy::enforce::PolicyError;
use contextful_policy::issue::{mint, MintClaims};

fn source(name: &str) -> TableSource {
    let decl = TableDecl::named(name);
    TableSource {
        policy: TablePolicy::from_decl(&decl).unwrap(),
        decl,
        base: "SELECT 1 AS \"x\"".into(),
        columns: vec![Column::new("x", ColumnType::Int32, false)],
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
