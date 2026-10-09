//! `authority.place`: the floor a synthesized row's evidence list resolves.

use contextful_core::enforce::EnforceError;
use contextful_core::place::{evidence_floor, AllowSet, Zone};

fn set(entries: &[&str]) -> AllowSet {
    AllowSet::parse(&entries.iter().map(|e| e.to_string()).collect::<Vec<_>>()).unwrap()
}

/// A synthesized row naming no evidence table raises `EnforceEvidenceListEmpty` and resolves to the fail-closed pair of {{authority.place.fail-closed}}.
// spec: authority.place.empty-evidence@b9f6d392
#[test]
fn a_row_naming_no_evidence_resolves_fail_closed() {
    for declared in [set(&["*"]), set(&["public-cloud:*"]), set(&["local:device"])] {
        let (resolved, refusal) = evidence_floor(&declared, &[]);
        assert_eq!(resolved, AllowSet::fail_closed(), "{:?}", declared.labels());
        let refusal = refusal.expect("an empty evidence list refuses");
        assert!(matches!(refusal, EnforceError::EvidenceListEmpty(_)), "{refusal}");
        assert!(refusal.to_string().starts_with("EnforceEvidenceListEmpty"), "{refusal}");
        assert_eq!(refusal.identifier(), "EnforceEvidenceListEmpty");
    }
    let (resolved, _) = evidence_floor(&set(&["*"]), &[]);
    assert!(!resolved.admits(&Zone::parse("public-cloud:x")) && resolved.admits(&Zone::parse("local:device")));

    // One evidence table is a list, and its set bounds the row as before.
    let (resolved, refusal) = evidence_floor(&set(&["on-prem:hq"]), &[set(&["on-prem:*"])]);
    assert_eq!(resolved, set(&["on-prem:hq"]));
    assert!(refusal.is_none());
}
