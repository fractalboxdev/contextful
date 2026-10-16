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

/// Whether a set reaches a category is decided over its entries' constructors: `*`, the
/// bare category, or the category carrying an identifier.
#[test]
fn a_set_reaches_a_category_through_any_entry_naming_it() {
    use contextful_core::place::Category;
    for reaching in [set(&["*"]), set(&["public-cloud:*"]), set(&["local:device", "public-cloud:us-east-1"])] {
        assert!(reaching.reaches(Category::PublicCloud), "{:?}", reaching.labels());
    }
    for closed in [AllowSet::fail_closed(), set(&["private-cloud:*", "on-prem:hq"]), set(&[])] {
        assert!(!closed.reaches(Category::PublicCloud), "{:?}", closed.labels());
    }
    assert!(AllowSet::fail_closed().reaches(Category::OnPrem));
}

/// Each enforcement refusal the write path, the relation compiler and the placement check
/// raise carries its identifier as the head of its message.
#[test]
fn the_placement_relation_and_write_refusals_name_their_identifiers() {
    for (e, id) in [
        (EnforceError::KeyMaterialInRelation("x".into()), "EnforceKeyMaterialInRelation"),
        (EnforceError::RemovalWithPublicCloud("x".into()), "EnforceRemovalWithPublicCloud"),
        (EnforceError::CredentialShapedValue("x".into()), "EnforceCredentialShapedValue"),
    ] {
        assert_eq!(e.identifier(), id);
        assert!(e.to_string().starts_with(&format!("{id}: ")), "{e}");
    }
}

/// `[project.zone]` parses into the zone a mint signs; a fixed default refuses another zone.
#[test]
fn a_project_zone_supplies_the_minted_zone_and_bounds_a_named_one() {
    use contextful_core::place::ProjectZone;
    let declared = |zone: &str| ProjectZone::declared(&format!("[project]\nname = \"research\"\n\n[project.zone]\n{zone}"));
    assert_eq!(ProjectZone::declared("[project]\nname = \"research\"\n").unwrap(), ProjectZone::default());
    let open = declared("default = \"on-prem:hq\"\nincognito_default = true\n").unwrap();
    assert_eq!(open.mint(None, false).unwrap(), (Some("on-prem:hq".into()), true));
    assert_eq!(open.mint(Some("public-cloud:eu".into()), false).unwrap(), (Some("public-cloud:eu".into()), true));
    let fixed = declared("default = \"on-prem:hq\"\nusers_may_widen = false\n").unwrap();
    assert_eq!(fixed.mint(Some("local:device".into()), false).unwrap(), (Some("local:device".into()), false));
    assert!(matches!(fixed.mint(Some("public-cloud:eu".into()), false), Err(EnforceError::ZoneDefaultWidened(m)) if m.contains("on-prem:hq")));
    for bad in ["default = \"nowhere\"\n", "users_may_widen = \"no\"\n", "fallback = \"local:device\"\n"] {
        assert!(declared(bad).is_err(), "{bad}");
    }
}
