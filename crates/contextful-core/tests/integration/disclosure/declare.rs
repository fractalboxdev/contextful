//! `disclosure.declare-fidelity` and `disclosure.bound-staleness`: the visibility block,
//! the family bound on fidelity, and the budget grammar.

use contextful_core::disclosure::declare::{parse_budget, Binding, DeclareError, Family, Fidelity, OnStale};
use contextful_core::disclosure::VisibilityError;
use contextful_core::store::declare::TableDecl;

fn manifest(block: &str) -> String {
    format!("[[pipeline.tables]]\nname = \"wiki/pages\"\n\n[pipeline.tables.visibility]\n{block}\n")
}

/// The table block as declared, before the load checks run.
fn table(block: &str) -> TableDecl {
    let text = format!("name = \"wiki/pages\"\n\n[visibility]\n{block}\n");
    toml::from_str(&text).unwrap()
}

/// Loading the manifest refuses, naming `identifier` and the table.
fn load_refused(block: &str, identifier: &str) {
    match TableDecl::parse_pipeline(&manifest(block)) {
        Err(e) => {
            let shown = e.to_string();
            assert!(shown.contains(identifier) && shown.contains("wiki/pages"), "{shown}");
        }
        Ok(tables) => panic!("loaded {tables:?}"),
    }
}

fn block(fidelity: &str, family: Option<&str>, budget: &str) -> String {
    let family = family.map(|f| format!("family = \"{f}\"\n")).unwrap_or_default();
    format!(
        "source = \"wiki\"\nresource_key = \"page_id\"\nresource_kind = \"page\"\nfidelity = \"{fidelity}\"\n{family}max_acl_staleness = \"{budget}\"\n"
    )
}

fn visibility(t: &TableDecl) -> VisibilityError {
    match Binding::of(t) {
        Err(DeclareError::Visibility(e)) => e,
        other => panic!("{other:?}"),
    }
}

/// `max_acl_staleness` is a positive integer with one suffix from `s`, `m`, `h`, `d`. A compound form, a bare number or another suffix raises `VisibilityBudgetMalformed`, naming the table and the text read.
// spec: disclosure.bound-staleness.budget-grammar@e6cf1d86
#[test]
fn a_budget_outside_the_grammar_is_refused_naming_table_and_text() {
    for (text, secs) in [("90s", 90), ("15m", 900), ("1h", 3_600), ("2d", 172_800)] {
        assert_eq!(parse_budget("wiki/pages", text), Ok(secs), "{text}");
    }
    let binding = Binding::of(&table(&block("mirrored", Some("item-exception"), "15m"))).unwrap().unwrap();
    assert_eq!(binding.max_acl_staleness_secs, Some(900));
    assert_eq!(binding.on_stale, OnStale::Refuse);

    for text in ["1h30m", "900", "15w", "15M", "0m", "+15m", "-15m", " 15m", "15m ", "", "m", "1.5h", "15é", "99999999999999999999d", "213503982334602d"] {
        match parse_budget("wiki/pages", text) {
            Err(VisibilityError::BudgetMalformed { table, text: read }) => {
                assert_eq!(table, "wiki/pages");
                assert_eq!(read, text);
            }
            other => panic!("`{text}` read as {other:?}"),
        }
    }
    let e = visibility(&table(&block("mirrored", Some("item-exception"), "1h30m")));
    assert_eq!(e.identifier(), "VisibilityBudgetMalformed");
    let shown = e.to_string();
    assert!(shown.starts_with("VisibilityBudgetMalformed") && shown.contains("wiki/pages") && shown.contains("1h30m"), "{shown}");
    load_refused(&block("mirrored", Some("item-exception"), "1h30m"), "VisibilityBudgetMalformed");
}

/// A `person-container` table at a servable level, or a `directory` table above `excluded`, raises `VisibilityFamilyBound` and the table does not load.
// spec: disclosure.declare-fidelity.family-bound@8ace3542
#[test]
fn a_level_the_family_does_not_permit_is_refused() {
    let permitted = |family: Family, fidelity: Fidelity| match family {
        Family::ContainerRoster | Family::ItemException => true,
        Family::PersonContainer => matches!(fidelity, Fidelity::Federated | Fidelity::Excluded),
        Family::Directory => fidelity == Fidelity::Excluded,
    };
    for family in Family::ALL {
        for fidelity in Fidelity::ALL {
            assert_eq!(family.permits(fidelity), permitted(family, fidelity), "{} at {}", family.as_str(), fidelity.as_str());
            let loaded = Binding::of(&table(&block(fidelity.as_str(), Some(family.as_str()), "15m")));
            if permitted(family, fidelity) {
                let b = loaded.unwrap().unwrap();
                assert_eq!((b.family, b.fidelity), (family, fidelity));
                assert_eq!(TableDecl::parse_pipeline(&manifest(&block(fidelity.as_str(), Some(family.as_str()), "15m"))).unwrap().len(), 1);
            } else {
                load_refused(&block(fidelity.as_str(), Some(family.as_str()), "15m"), "VisibilityFamilyBound");
                match loaded {
                    Err(DeclareError::Visibility(e @ VisibilityError::FamilyBound { .. })) => {
                        assert_eq!(e.identifier(), "VisibilityFamilyBound");
                        assert!(e.to_string().contains("wiki/pages"), "{e}");
                    }
                    other => panic!("{} at {} loaded as {other:?}", family.as_str(), fidelity.as_str()),
                }
            }
        }
    }
}

/// A mapping landing a table without naming its `family` raises `VisibilityFamilyUndeclared`.
// spec: disclosure.declare-fidelity.family-undeclared@56217b6d
#[test]
fn a_block_naming_no_family_is_refused() {
    for fidelity in Fidelity::ALL {
        let e = visibility(&table(&block(fidelity.as_str(), None, "15m")));
        assert_eq!(e, VisibilityError::FamilyUndeclared { table: "wiki/pages".into() });
        assert!(e.to_string().starts_with("VisibilityFamilyUndeclared"), "{e}");
        load_refused(&block(fidelity.as_str(), None, "15m"), "VisibilityFamilyUndeclared");
    }
    assert!(matches!(Binding::of(&TableDecl::named("wiki/pages")), Ok(None)));
    assert!(matches!(
        Binding::of(&table(&block("mirrored", Some("mailbox"), "15m"))),
        Err(DeclareError::Malformed(_))
    ));
}
