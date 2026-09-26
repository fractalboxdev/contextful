//! `authority.place`: the zone grammar, allow-sets, floors, incognito and the serve outcome.

use contextful_core::enforce::EnforceError;
use contextful_policy::enforce::policy::TablePolicy;
use contextful_policy::enforce::zone::{
    evidence_floor, narrowest, serve, session_zone, AllowSet, Category, Entry, Placement, Zone, ALLOW_SET_ENTRIES,
    ZONE_IDENTIFIER_LENGTH,
};
use contextful_policy::enforce::PolicyError;
use contextful_core::store::declare::TableDecl;

fn set(entries: &[&str]) -> AllowSet {
    AllowSet::parse(&entries.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
}

fn z(s: &str) -> Zone {
    Zone::parse(s)
}

fn decl(toml: &str) -> TableDecl {
    TableDecl::parse_pipeline(toml).unwrap().remove(0)
}

/// A zone string parses, after trimming, as `local:device`, `on-prem:<id>`, `private-cloud:<id>` or `public-cloud:<id>`; any other text is undeclared. The identifier is part of the value.
// spec: authority.place.zone-string@b63a943c
#[test]
fn zone_strings_parse_by_category_and_identifier() {
    assert_eq!(z("  local:device "), Zone::LocalDevice);
    assert_eq!(z("on-prem:hq"), Zone::In(Category::OnPrem, "hq".into()));
    assert_eq!(z("private-cloud:vpc-7"), Zone::In(Category::PrivateCloud, "vpc-7".into()));
    assert_eq!(z("public-cloud:us-east-1"), Zone::In(Category::PublicCloud, "us-east-1".into()));
    assert_ne!(z("on-prem:hq"), z("on-prem:ward-3"));
    for undeclared in ["", "cloud", "on-prem:", "on-prem:*", "local:laptop", "Public-Cloud:x"] {
        assert_eq!(z(undeclared), Zone::Undeclared, "{undeclared}");
    }
}

/// An allow-set entry is `*`, `local:device`, `on-prem:*`, `private-cloud:*`, `public-cloud:*`, or a category carrying a concrete identifier.
// spec: authority.place.allow-set-entry@34f4b334
#[test]
fn allow_set_entries_take_six_forms() {
    assert_eq!(Entry::parse("*"), Ok(Entry::Any));
    assert_eq!(Entry::parse("local:device"), Ok(Entry::LocalDevice));
    assert_eq!(Entry::parse("on-prem:*"), Ok(Entry::Category(Category::OnPrem)));
    assert_eq!(Entry::parse("private-cloud:*"), Ok(Entry::Category(Category::PrivateCloud)));
    assert_eq!(Entry::parse("public-cloud:*"), Ok(Entry::Category(Category::PublicCloud)));
    assert_eq!(Entry::parse("on-prem:hq"), Ok(Entry::Exact(Category::OnPrem, "hq".into())));
}

/// An allow-set entry matching no entry form raises `EnforceZonePatternUnparsed`, naming the entry.
// spec: authority.place.unparsed-pattern@438558dc
#[test]
fn an_unparsed_entry_is_refused_by_name() {
    for bad in ["cloud:*", "on-prem:h*", "local:*", "edge"] {
        match Entry::parse(bad) {
            Err(EnforceError::ZonePatternUnparsed(why)) => assert!(why.contains(bad), "{why}"),
            other => panic!("{bad}: {other:?}"),
        }
    }
}

/// A zone is admitted when any entry matches. A bare category matches every identifier in it; an entry carrying an identifier matches that identifier alone.
// spec: authority.place.disjunctive@ed1d621c
#[test]
fn any_matching_entry_admits_the_zone() {
    let s = set(&["local:device", "on-prem:*", "private-cloud:vpc-7"]);
    assert!(s.admits(&z("local:device")));
    assert!(s.admits(&z("on-prem:hq")) && s.admits(&z("on-prem:ward-3")));
    assert!(s.admits(&z("private-cloud:vpc-7")));
    assert!(!s.admits(&z("private-cloud:vpc-8")));
    assert!(!s.admits(&z("public-cloud:us-east-1")));
}

/// Only `*` admits an undeclared zone.
// spec: authority.place.undeclared@5837db21
#[test]
fn only_the_wildcard_admits_an_undeclared_zone() {
    assert!(set(&["*"]).admits(&Zone::Undeclared));
    let every_category = set(&["local:device", "on-prem:*", "private-cloud:*", "public-cloud:*"]);
    assert!(!every_category.admits(&Zone::Undeclared));
}

/// A table declaring no zone policy resolves to the fail-closed pair `local:device` and `on-prem:*`, declared and discovered tables alike.
// spec: authority.place.fail-closed@3a710a0e
#[test]
fn an_undeclared_table_resolves_to_the_fail_closed_pair() {
    let declared = TablePolicy::from_decl(&decl("[[pipeline.tables]]\nname = \"notes\"\n")).unwrap();
    let discovered = TablePolicy::from_decl(&TableDecl::named("found")).unwrap();
    for p in [declared, discovered] {
        let eff = p.placement.effective();
        assert_eq!(eff, AllowSet::fail_closed());
        assert!(eff.admits(&z("on-prem:hq")) && eff.admits(&z("local:device")));
        assert!(!eff.admits(&z("private-cloud:vpc")));
    }
}

/// A `phi` column or table carries a floor at the fail-closed pair; a wider declared set resolves down to it at serve time unless the override names that class or column.
// spec: authority.place.protected-floor@b98f1c6b
#[test]
fn a_protected_surface_resolves_down_to_its_floor() {
    let p = Placement { declared: Some(set(&["*"])), protected: true, protected_override: true };
    assert_eq!(p.effective(), set(&["*"]), "the class-named override widens");
    let p = Placement { declared: Some(set(&["local:device", "public-cloud:*"])), protected: true, protected_override: false };
    assert_eq!(p.effective(), set(&["local:device"]));
    let t = TablePolicy::from_decl(&decl(
        "[[pipeline.tables]]\nname = \"visits\"\n[pipeline.tables.policy.zone]\nallow = [\"on-prem:*\"]\n[pipeline.tables.policy.columns]\ncase_notes = { class = \"phi\" }\n",
    ))
    .unwrap();
    assert!(t.column_set("case_notes").admits(&z("on-prem:ward-3")));
    assert!(!t.column_set("case_notes").admits(&z("public-cloud:x")));
    // A table-wide override that names another class lifts no phi column; one naming the
    // column, or its class, does.
    let wide = |names: &str| {
        decl(&format!(
            "[[pipeline.tables]]\nname = \"visits\"\n[pipeline.tables.policy.zone]\nallow = [\"on-prem:*\", \"public-cloud:*\"]\nprotected_class_override = [{names}]\n[pipeline.tables.policy.columns]\ncase_notes = {{ class = \"phi\" }}\nward_notes = {{ class = \"phi\" }}\n"
        ))
    };
    let none = TablePolicy::from_decl(&wide("\"ssn\"")).unwrap();
    assert!(!none.column_set("case_notes").admits(&z("public-cloud:x")));
    let one = TablePolicy::from_decl(&wide("\"case_notes\"")).unwrap();
    assert!(one.column_set("case_notes").admits(&z("public-cloud:x")));
    assert!(!one.column_set("ward_notes").admits(&z("public-cloud:x")));
    let class = TablePolicy::from_decl(&wide("\"phi\"")).unwrap();
    assert!(class.column_set("ward_notes").admits(&z("public-cloud:x")));
    assert!(TablePolicy::from_decl(&decl("[[pipeline.tables]]\nname = \"v\"\n[pipeline.tables.policy.zone]\nallow = [\"*\"]\nprotected_class_override = true\n")).is_err());
}

/// A manifest widening a protected-class surface past its floor without an override naming that class raises `EnforceProtectedFloorWidened`.
// spec: authority.place.floor-widened@8d125ad7
#[test]
fn widening_a_protected_surface_needs_the_override() {
    let widened = "[[pipeline.tables]]\nname = \"visits\"\nclass = \"phi\"\n[pipeline.tables.policy.zone]\nallow = [\"public-cloud:*\"]\n";
    match TablePolicy::from_decl(&decl(widened)) {
        Err(PolicyError::Enforce(EnforceError::ProtectedFloorWidened(why))) => assert!(why.contains("visits"), "{why}"),
        other => panic!("{other:?}"),
    }
    let overridden = widened.replace("allow = [\"public-cloud:*\"]", "allow = [\"public-cloud:*\"]\nprotected_class_override = [\"phi\"]");
    assert!(TablePolicy::from_decl(&decl(&overridden)).is_ok());
    let within = widened.replace("public-cloud:*", "on-prem:hq");
    assert!(TablePolicy::from_decl(&decl(&within)).is_ok());
    // A protected column declares no set of its own: it narrows at serve time instead.
    let column = "[[pipeline.tables]]\nname = \"visits\"\n[pipeline.tables.policy.zone]\nallow = [\"public-cloud:*\"]\n[pipeline.tables.policy.columns]\ncase_notes = { class = \"phi\" }\n";
    let t = TablePolicy::from_decl(&decl(column)).unwrap();
    assert!(!t.column_set("case_notes").admits(&z("public-cloud:x")));
}

/// A column's set narrows its table's; a principal's set narrows both.
// spec: authority.place.narrowest-grain@76a3a5de
#[test]
fn the_narrowest_grain_wins() {
    let table = set(&["local:device", "on-prem:*", "private-cloud:*"]);
    let column = set(&["on-prem:*", "private-cloud:vpc-7"]);
    let principal = set(&["on-prem:hq", "private-cloud:*"]);
    let cell = narrowest(&table, Some(&column), Some(&principal));
    assert!(cell.admits(&z("on-prem:hq")));
    assert!(cell.admits(&z("private-cloud:vpc-7")));
    assert!(!cell.admits(&z("on-prem:ward-3")), "the principal narrows");
    assert!(!cell.admits(&z("local:device")), "the column narrows");
    assert!(cell.within(&table) && cell.within(&column) && cell.within(&principal));
}

/// A synthesized row declaring a set wider than the intersection of its evidence tables' sets raises `EnforceEvidenceFloorExceeded` and resolves to that intersection.
// spec: authority.place.evidence-floor@1b790706
#[test]
fn a_synthesized_row_resolves_to_its_evidence_intersection() {
    let evidence = [set(&["local:device", "on-prem:*"]), set(&["on-prem:*", "public-cloud:*"])];
    let (resolved, refusal) = evidence_floor(&set(&["on-prem:hq"]), &evidence);
    assert_eq!((resolved, refusal), (set(&["on-prem:hq"]), None));
    let (resolved, refusal) = evidence_floor(&set(&["*"]), &evidence);
    assert_eq!(resolved, set(&["on-prem:*"]));
    assert!(matches!(refusal, Some(EnforceError::EvidenceFloorExceeded(_))));
}

/// A session asserting a zone wider than the incognito pin raises `EnforceIncognitoWidening`.
// spec: authority.place.incognito-widening@233200ff
#[test]
fn incognito_refuses_a_wider_zone_and_pins_an_absent_one() {
    assert_eq!(session_zone(None, None, true), Ok(Zone::LocalDevice));
    assert_eq!(session_zone(Some("on-prem:hq"), Some("on-prem:hq"), true), Ok(z("on-prem:hq")));
    assert!(matches!(session_zone(None, Some("private-cloud:vpc"), true), Err(EnforceError::IncognitoWidening(_))));
    assert_eq!(session_zone(None, Some("public-cloud:us-east-1"), false), Ok(z("public-cloud:us-east-1")));
}

/// A zone a request asserts stands only where it equals the zone its credential signs; any other assertion raises `EnforceZoneAssertionWidens`, naming both.
// spec: authority.place.asserted-zone@53fcfebd
#[test]
fn an_asserted_zone_never_replaces_the_signed_one() {
    assert_eq!(session_zone(Some(" on-prem:hq"), Some("on-prem:hq"), false), Ok(z("on-prem:hq")));
    for (asserted, signed) in [("public-cloud:us-east-1", Some("on-prem:hq")), ("on-prem:ward-3", Some("on-prem:hq")), ("on-prem:hq", None)] {
        match session_zone(Some(asserted), signed, false) {
            Err(EnforceError::ZoneAssertionWidens(why)) => assert!(why.contains(asserted), "{why}"),
            other => panic!("{asserted} over {signed:?}: {other:?}"),
        }
    }
}

/// Serving one row yields a drop flag and the list of zone-masked columns.
// spec: authority.place.serve-outcome@68c90d5c
#[test]
fn serving_a_row_yields_a_drop_flag_and_masked_columns() {
    let table = set(&["on-prem:*", "public-cloud:*"]);
    let columns = vec![("case_notes".to_string(), AllowSet::fail_closed()), ("title".to_string(), table.clone())];
    let out = serve(&table, &columns, &z("public-cloud:us-east-1"));
    assert_eq!((out.drop, out.masked_columns), (false, vec!["case_notes".to_string()]));
    let out = serve(&table, &columns, &z("local:device"));
    assert!(out.drop);
    assert!(!serve(&table, &columns, &z("on-prem:hq")).drop);
}

/// Inclusion against a floor is decided over every constructor and identifier, never by probing sample zones.
// spec: authority.place.symbolic-inclusion@4f8e5c2d
#[test]
fn inclusion_is_decided_over_constructors_and_identifiers() {
    let floor = AllowSet::fail_closed();
    assert!(set(&["on-prem:hq", "local:device"]).within(&floor));
    assert!(set(&["on-prem:*"]).within(&floor));
    // An identifier no sample would guess still decides: `private-cloud:<anything>` is outside.
    assert!(!set(&["private-cloud:zz-unguessable-0f31"]).within(&floor));
    assert!(!set(&["*"]).within(&floor));
    assert!(set(&["on-prem:hq"]).within(&set(&["on-prem:hq"])));
    assert!(!set(&["on-prem:hq"]).within(&set(&["on-prem:ward-3"])));
    assert!(!set(&["on-prem:*"]).within(&set(&["on-prem:hq"])));
}

/// An allow-set holds at most 32 entries.
// spec: authority.place.allow-set-entries@4fb50ca9
#[test]
fn an_allow_set_holds_at_most_32_entries() {
    assert_eq!(ALLOW_SET_ENTRIES, 32);
    let entries: Vec<String> = (0..32).map(|i| format!("on-prem:site-{i}")).collect();
    assert!(AllowSet::parse(&entries).is_ok());
    let over: Vec<String> = (0..33).map(|i| format!("on-prem:site-{i}")).collect();
    assert!(matches!(AllowSet::parse(&over), Err(PolicyError::Malformed(_))));
}

/// A zone identifier holds at most 128 chars.
// spec: authority.place.identifier-length@7c5e7cdb
#[test]
fn a_zone_identifier_holds_at_most_128_chars() {
    assert_eq!(ZONE_IDENTIFIER_LENGTH, 128);
    let at = "a".repeat(128);
    let over = "a".repeat(129);
    assert_eq!(z(&format!("on-prem:{at}")), Zone::In(Category::OnPrem, at.clone()));
    assert_eq!(z(&format!("on-prem:{over}")), Zone::Undeclared);
    assert!(Entry::parse(&format!("on-prem:{at}")).is_ok());
    assert!(matches!(Entry::parse(&format!("on-prem:{over}")), Err(EnforceError::ZonePatternUnparsed(_))));
}
