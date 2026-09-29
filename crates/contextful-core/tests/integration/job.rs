//! `surface.fire`: job blocks against the closed kind union and the store-driven kind's
//! declaration.

use contextful_core::job::{parse_jobs, JobError, JobKind, KINDS, STORE_DRIVEN};

fn registered(name: &str) -> bool {
    name == "score"
}

fn store_driven(extra: &str) -> String {
    format!("[[job]]\nname = \"score-documents\"\nkind = \"store-driven\"\nbody = \"score\"\nstatement = \"SELECT doc_id FROM documents\"\ntables = [\"scores\"]\n{extra}")
}

#[test]
fn a_store_driven_block_validates_with_its_pinned_input_and_concurrency() {
    let jobs = parse_jobs(&store_driven("max_in_flight = 4\nas_of = \"2030-01-01T00:00:00Z\"\n"), &registered).unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].kind_name(), STORE_DRIVEN);
    let JobKind::StoreDriven(d) = &jobs[0].kind else { panic!("{:?}", jobs[0].kind) };
    assert_eq!((d.max_in_flight, d.tables.as_slice()), (4, ["scores".to_string()].as_slice()));
    assert_eq!((d.input.body.as_str(), d.input.as_of.as_deref()), ("score", Some("2030-01-01T00:00:00Z")));
    assert_eq!(KINDS.len(), 7, "the union holds seven kinds");
    for kind in KINDS.iter().filter(|k| **k != STORE_DRIVEN) {
        let jobs = parse_jobs(&format!("[[job]]\nname = \"m\"\nkind = \"{kind}\"\nschedule = \"every 1h\"\n"), &registered).unwrap();
        assert_eq!(jobs[0].kind_name(), *kind);
    }
}

/// A block naming a kind outside the union, an argument vector or a host command raises `JobKindUnknown` at
/// validation.
// spec: surface.fire.job-kind-unknown@b261f015
#[test]
fn a_kind_outside_the_union_or_a_command_raises_job_kind_unknown() {
    for block in [
        "[[job]]\nname = \"x\"\nkind = \"shell\"\n".to_string(),
        store_driven("max_in_flight = 4\ncommand = [\"python\", \"score.py\"]\n"),
        "[[job]]\nname = \"x\"\nkind = \"fold\"\nargv = [\"rm\"]\n".to_string(),
    ] {
        match parse_jobs(&block, &registered) {
            Err(JobError::JobKindUnknown(_)) => {}
            other => panic!("{block}: expected JobKindUnknown, got {other:?}"),
        }
    }
}

/// A `store-driven` block declaring no positive integer `max_in_flight` raises `JobConcurrencyUnset` at
/// validation; no default applies.
// spec: surface.fire.store-driven-concurrency@984ee375
#[test]
fn a_store_driven_block_without_a_positive_max_in_flight_raises_job_concurrency_unset() {
    for extra in ["", "max_in_flight = 0\n", "max_in_flight = -2\n", "max_in_flight = \"4\"\n"] {
        match parse_jobs(&store_driven(extra), &registered) {
            Err(JobError::JobConcurrencyUnset(m)) => assert!(m.contains("score-documents"), "{m}"),
            other => panic!("{extra:?}: expected JobConcurrencyUnset, got {other:?}"),
        }
    }
}

/// A `store-driven` block whose `body` names no body the embedding binary registers raises `JobBodyUnregistered`
/// at validation.
// spec: surface.fire.store-driven-body@c7d2b211
#[test]
fn a_store_driven_block_naming_an_unregistered_body_raises_job_body_unregistered() {
    let block = store_driven("max_in_flight = 2\n").replace("body = \"score\"", "body = \"rank\"");
    match parse_jobs(&block, &registered) {
        Err(JobError::JobBodyUnregistered(m)) => assert!(m.contains("`rank`"), "{m}"),
        other => panic!("expected JobBodyUnregistered, got {other:?}"),
    }
}
