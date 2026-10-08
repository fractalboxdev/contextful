//! The plan a run pins, and the journal declarations it is held to.

use contextful_core::run::plan::Plan;

#[test]
fn typed_removal_can_reach_destination_recording_admission_but_legacy_redact_still_refuses() {
    let typed = "pipeline='feed'\ntable='messages'\nredaction=[{table='messages',column='body',match='whole',operation='drop'}]\n[connector]\nid='vendor'\nversion='1'\ncommand=['unused']\n";
    assert!(Plan::compile(typed.as_bytes()).is_ok());
    let legacy = typed.replace("redaction=[{table='messages',column='body',match='whole',operation='drop'}]", "redact=['body']");
    assert!(Plan::compile(legacy.as_bytes()).is_err());
}
use contextful_core::run::RunError;

const PLAN: &str = "pipeline = \"feed\"\ntable = \"filings\"\n[connector]\nid = \"vendor\"\nversion = \"1\"\ncommand = [\"sh\", \"vendor.sh\"]\n";

#[test]
fn a_plan_is_named_by_the_sha256_of_its_bytes() {
    let p = Plan::compile(PLAN.as_bytes()).unwrap();
    assert_eq!(p.content_hash, contextful_core::run::journal::sha256_hex(PLAN.as_bytes()));
    assert_ne!(Plan::compile(format!("{PLAN}\n").as_bytes()).unwrap().content_hash, p.content_hash);
    assert!(p.spec.journal, "pull journaling defaults on");
}

/// A boolean removal assertion supplies no canonical prepared-recording admission.
// spec: run.journal.redacting-source@cbe9a121
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
