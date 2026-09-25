//! The plan a run pins, and the journal declarations it is held to.

use contextful_core::run::plan::Plan;
use contextful_core::run::RunError;

const PLAN: &str = "pipeline = \"feed\"\ntable = \"filings\"\n[connector]\nid = \"vendor\"\nversion = \"1\"\ncommand = [\"sh\", \"vendor.sh\"]\n";

#[test]
fn a_plan_is_named_by_the_sha256_of_its_bytes() {
    let p = Plan::compile(PLAN.as_bytes()).unwrap();
    assert_eq!(p.content_hash, contextful_core::run::journal::sha256_hex(PLAN.as_bytes()));
    assert_ne!(Plan::compile(format!("{PLAN}\n").as_bytes()).unwrap().content_hash, p.content_hash);
    assert!(p.spec.journal, "pull journaling defaults on");
}

/// A pipeline declaring write-path redaction over a source that journals its pulls raises
/// `JournalRedactionConflict` at manifest validation and again at run open, before the first pull.
// spec: run.journal.redacting-source@377b6627
#[test]
fn redaction_over_a_journaling_source_is_refused_at_validation() {
    let redacting = format!("redact = [\"ssn\"]\n{PLAN}");
    match Plan::compile(redacting.as_bytes()) {
        Err(RunError::JournalRedactionConflict(m)) => assert!(m.contains("ssn") && m.contains("feed"), "{m}"),
        other => panic!("{other:?}"),
    }
    // Run open validates again: a plan altered after compile refuses there.
    let mut p = Plan::compile(PLAN.as_bytes()).unwrap();
    p.spec.redact = vec!["ssn".into()];
    assert!(matches!(p.validate(), Err(RunError::JournalRedactionConflict(_))));
}

#[test]
fn declared_redaction_this_build_cannot_apply_refuses_rather_than_landing_cleartext() {
    let opted_out = format!("redact = [\"ssn\"]\njournal = false\n{PLAN}");
    match Plan::compile(opted_out.as_bytes()) {
        Err(RunError::Invalid(m)) => assert!(m.contains("`ssn`") && m.contains("no write-path redaction"), "{m}"),
        other => panic!("{other:?}"),
    }
}
