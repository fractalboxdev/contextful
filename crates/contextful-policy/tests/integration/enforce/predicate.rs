//! `authority.filter-rows`: the predicate grammar, its bounds and exception conditions.

use contextful_core::enforce::EnforceError;
use contextful_core::store::declare::TableDecl;
use contextful_policy::enforce::policy::TablePolicy;
use contextful_policy::enforce::predicate::{Predicate, MEMBERSHIP_LIST_ENTRIES, PREDICATE_SIZE};
use contextful_policy::enforce::PolicyError;

fn outside(src: &str) -> String {
    match Predicate::parse(src) {
        Err(EnforceError::PredicateOutsideGrammar(why)) => why,
        other => panic!("{src}: expected EnforcePredicateOutsideGrammar, got {other:?}"),
    }
}

/// A row predicate is the typed boolean subset of SQL in Shapes: column references, comparisons, conjunction, disjunction, negation, membership lists, pattern matching, null tests and declared-safe scalar functions.
// spec: authority.filter-rows.grammar@8d499271
#[test]
fn the_grammar_admits_its_typed_boolean_subset() {
    let p = Predicate::parse(
        "region = subject.host AND classification <= 3 OR NOT (kind IN ('memo', 'note')) \
         OR title LIKE 'Q%' OR retired_at IS NOT NULL OR lower(owner) <> 'bot' OR \"Vendor Field\" >= 2.5",
    )
    .unwrap();
    assert_eq!(
        p.sql(),
        "(((\"region\" = (SELECT \"host\" FROM \"__contextful_subject\")) AND (\"classification\" <= 3)) \
         OR (NOT (\"kind\" IN ('memo', 'note'))) OR (\"title\" LIKE 'Q%') OR (\"retired_at\" IS NOT NULL) \
         OR (lower(\"owner\") <> 'bot') OR (\"Vendor Field\" >= 2.5))"
    );
    assert_eq!(Predicate::parse("true").unwrap().sql(), "true");
    assert_eq!(p.columns(), ["region", "classification", "kind", "title", "retired_at", "owner", "Vendor Field"]);
}

/// Predicates parse at manifest load; a node outside the grammar raises `EnforcePredicateOutsideGrammar`, naming the node.
// spec: authority.filter-rows.outside-grammar@dcc31f42
#[test]
fn a_node_outside_the_grammar_is_refused_at_manifest_load() {
    assert!(outside("region IN (SELECT region FROM other)").contains("SELECT"));
    assert!(outside("read_text('/etc/passwd') = 'x'").contains("read_text"));
    assert!(outside("subject.clearance = 1").contains("clearance"));
    assert!(outside("a = 1; DROP TABLE t").contains(';'));
    assert!(outside("a = 1 AND").contains("ends"));
    let decl = TableDecl::parse_pipeline(
        "[[pipeline.tables]]\nname = \"orders\"\n[pipeline.tables.policy.rows]\npredicate = \"region ~ 'x'\"\n",
    )
    .unwrap()
    .remove(0);
    assert!(matches!(TablePolicy::from_decl(&decl), Err(PolicyError::Enforce(EnforceError::PredicateOutsideGrammar(_)))));
}

/// An exception condition is a boolean expression over subject fields in the same grammar, bound the same way.
// spec: authority.filter-rows.exception@efb6a4f3
#[test]
fn an_exception_condition_reads_subject_fields_alone() {
    let ok = "[[pipeline.tables]]\nname = \"orders\"\n[pipeline.tables.policy.rows]\npredicate = \"region = subject.host\"\n\
              [[pipeline.tables.policy.rows.exception]]\nwhen = \"subject.agent = 'agent://auditor'\"\npredicate = \"true\"\n";
    let policy = TablePolicy::from_decl(&TableDecl::parse_pipeline(ok).unwrap().remove(0)).unwrap();
    assert_eq!(
        policy.rows.unwrap().sql(),
        "(CASE WHEN ((SELECT \"agent\" FROM \"__contextful_subject\") = 'agent://auditor') THEN true \
         ELSE (\"region\" = (SELECT \"host\" FROM \"__contextful_subject\")) END)"
    );
    let reads_column = ok.replace("when = \"subject.agent = 'agent://auditor'\"", "when = \"region = 'emea'\"");
    assert!(matches!(
        TablePolicy::from_decl(&TableDecl::parse_pipeline(&reads_column).unwrap().remove(0)),
        Err(PolicyError::Enforce(EnforceError::PredicateOutsideGrammar(_)))
    ));
}

/// A declared predicate holds at most 4 KiB of source text.
// spec: authority.filter-rows.predicate-size@badc8d4e
#[test]
fn a_predicate_holds_at_most_4_kib() {
    assert_eq!(PREDICATE_SIZE, 4096);
    let clause = "region = 'emea' OR ";
    let mut src = clause.repeat(4096 / clause.len());
    src.push_str("true");
    assert!(src.len() <= 4096);
    assert!(Predicate::parse(&src).is_ok());
    let over = format!("{src}{}", " OR true".repeat(1 + (4096 - src.len()) / 8));
    assert!(over.len() > 4096);
    assert!(outside(&over).contains("4096"));
}

/// A membership list inside a predicate holds at most 512 entries.
// spec: authority.filter-rows.membership-list@08d9ed52
#[test]
fn a_membership_list_holds_at_most_512_entries() {
    assert_eq!(MEMBERSHIP_LIST_ENTRIES, 512);
    let list = |n: usize| format!("site IN ({})", (0..n).map(|i| i.to_string()).collect::<Vec<_>>().join(","));
    assert!(Predicate::parse(&list(512)).is_ok());
    assert!(outside(&list(513)).contains("512"));
}
