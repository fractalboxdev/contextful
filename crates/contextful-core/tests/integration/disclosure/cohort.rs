//! `disclosure.bound-cohort`: the `governance` key a governed model declares, the singleton
//! cohort refused at declaration, and exact figures refused on a per-person model.

use contextful_core::disclosure::cohort::admit_governance;
use contextful_core::disclosure::noise::{Entropy, Spend};
use contextful_core::disclosure::release::{release, Breakdown, MemoryLedger, ReleaseError, ReleaseRequest, UnitBudget};
use contextful_core::disclosure::DisclosureError;
use contextful_core::pipeline::declare::ManifestFile;
use contextful_core::pipeline::model::{collect_models, DisclosureDecl, Figures, Governance};
use contextful_core::run::RunError;
use serde_json::{json, Map, Value};

struct Seeded(u64);

impl Entropy for Seeded {
    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

const POLICY: &str = r#"
grouping_allowlist = ["industry"]
contributor_key = "tenant_id"
min_group_size = 3
emit_sentinel = true
forbidden_columns = ["tenant_id", "account_email"]
[metric_bounds]
revenue = { lower = 0, upper = 10000, quantum = 1 }
"#;

/// The benchmark policy with `extra` keys prepended to its top level.
fn policy(extra: &str) -> DisclosureDecl {
    toml::from_str(&format!("{extra}\n{POLICY}")).unwrap()
}

fn grain() -> Vec<String> {
    vec!["industry".into(), "tenant_id".into()]
}

fn breakdown() -> Breakdown {
    let rows: Vec<Map<String, Value>> = [("t1", 100.0), ("t2", 110.0), ("t3", 95.0)]
        .iter()
        .map(|(t, v)| json!({"industry": "energy", "tenant_id": t, "revenue": v}).as_object().unwrap().clone())
        .collect();
    Breakdown { columns: vec!["industry".into(), "tenant_id".into(), "revenue".into()], grain: grain(), rows }
}

fn ledger() -> MemoryLedger {
    let l = MemoryLedger::default();
    for u in ["t1", "t2", "t3"] {
        l.grant(u, 10_000_000);
    }
    l
}

fn run(p: &DisclosureDecl, l: &MemoryLedger) -> Result<usize, ReleaseError> {
    let request = ReleaseRequest { release_id: "r-1".into(), table: "revenue_by_industry".into(), spend: Spend::from_micros(1_000_000).unwrap() };
    release(p, &breakdown(), &request, l, &mut Seeded(7)).map(|r| r.table.rows.len())
}

fn refused_release(r: Result<usize, ReleaseError>) -> DisclosureError {
    match r {
        Err(ReleaseError::Refused(e)) => e,
        other => panic!("{other:?}"),
    }
}

fn model(disclosure: &str) -> Result<Vec<contextful_core::pipeline::model::DeclaredModel>, RunError> {
    let text = format!(
        "[[model]]\nid = \"revenue_by_industry\"\nsql = \"SELECT industry, tenant_id, SUM(revenue) AS revenue FROM revenue GROUP BY industry, tenant_id\"\nunique_key = [\"industry\", \"tenant_id\"]\npublish = false\n[model.disclosure]\ncontributor_key = \"tenant_id\"\n{disclosure}\n"
    );
    collect_models(&[ManifestFile { path: "contextful.toml".into(), text }], &[])
}

/// `[model.disclosure]` declares `governance`, `per-person` or `cohort`, naming whether each published figure describes one person or a cohort; an absent `governance` is `per-person`, and another value raises `PipelineSpecInvalid`.
// spec: disclosure.bound-cohort.governance-declaration@d8b7b310
#[test]
fn a_model_declares_per_person_or_cohort_governance_and_defaults_to_per_person() {
    assert_eq!(policy("").governance(), Governance::PerPerson);
    assert_eq!(policy("governance = \"per-person\"").governance(), Governance::PerPerson);
    assert_eq!(policy("governance = \"cohort\"").governance(), Governance::Cohort);
    let declared = model("governance = \"cohort\"").unwrap();
    assert_eq!(declared.len(), 1);
    assert_eq!(declared[0].spec.disclosure.as_ref().unwrap().governance(), Governance::Cohort);
    for bad in ["governance = \"individual\"", "governance = \"Cohort\"", "governance = 1"] {
        match model(bad) {
            Err(RunError::PipelineSpecInvalid(m)) => assert!(m.contains("governance"), "{bad}: {m}"),
            other => panic!("{bad}: {other:?}"),
        }
    }
    // An absent `governance` leaves the policy digest of a model declaring none unchanged.
    let absent = serde_json::to_value(policy("")).unwrap();
    assert!(absent.get("governance").is_none(), "{absent}");
}

/// A `cohort` model whose permitted grouping columns or non-contributor grain columns name its contributor key or a forbidden column raises `DisclosureSingletonCohort` at declaration, before any row lands under it.
// spec: disclosure.bound-cohort.singleton-cohort@4a844bb2
#[test]
fn a_cohort_key_naming_one_person_raises_singleton_cohort_at_declaration() {
    let declared = |allowlist: &str| -> DisclosureDecl {
        let mut p = policy("governance = \"cohort\"");
        p.grouping_allowlist = serde_json::from_str(allowlist).unwrap();
        p
    };
    assert!(admit_governance(&declared(r#"["industry"]"#), &grain()).is_ok());
    for (allowlist, grain) in [
        (r#"["industry", "tenant_id"]"#, grain()),
        (r#"["industry", "account_email"]"#, grain()),
        (r#"["industry"]"#, vec!["industry".to_string(), "account_email".into(), "tenant_id".into()]),
    ] {
        let e = admit_governance(&declared(allowlist), &grain).unwrap_err();
        assert_eq!(e.identifier(), "DisclosureSingletonCohort", "{allowlist} {grain:?}: {e}");
        assert!(e.to_string().starts_with("DisclosureSingletonCohort: "), "{e}");
    }
    // A per-person model with the same allowlist is not a cohort key.
    let mut per_person = declared(r#"["industry", "tenant_id"]"#);
    per_person.governance = Some(Governance::PerPerson);
    assert!(admit_governance(&per_person, &grain()).is_ok());

    // The release refuses before reserving any unit's spend.
    let l = ledger();
    let e = refused_release(run(&declared(r#"["industry", "tenant_id"]"#), &l));
    assert_eq!(e.identifier(), "DisclosureSingletonCohort");
    assert_eq!(l.budget("t1"), Some(UnitBudget { cap: 10_000_000, spent: 0 }), "no unit was charged");
}

/// A policy declaring `figures = "exact"` on a `per-person` table raises `DisclosureExactPerPerson`. `figures` defaults to `"noised"`, and exact figures publish only from a `cohort` table.
// spec: disclosure.bound-cohort.exact-figures@0c5b5fc3
#[test]
fn exact_figures_on_a_per_person_model_raise_exact_per_person() {
    assert_eq!(policy("").figures(), Figures::Noised);
    assert_eq!(policy("figures = \"exact\"").figures(), Figures::Exact);
    for per_person in ["figures = \"exact\"", "figures = \"exact\"\ngovernance = \"per-person\""] {
        let e = admit_governance(&policy(per_person), &grain()).unwrap_err();
        assert_eq!(e.identifier(), "DisclosureExactPerPerson", "{per_person}: {e}");
    }
    assert!(admit_governance(&policy("figures = \"exact\"\ngovernance = \"cohort\""), &grain()).is_ok());
    assert!(admit_governance(&policy("figures = \"noised\""), &grain()).is_ok());
    match model("figures = \"rounded\"") {
        Err(RunError::PipelineSpecInvalid(m)) => assert!(m.contains("figures"), "{m}"),
        other => panic!("{other:?}"),
    }
}

/// One policy declaring exact figures releases under `cohort` governance and refuses under
/// `per-person`; one allowlisting its contributor key releases under `per-person` and refuses
/// under `cohort`.
#[test]
fn a_per_person_model_and_a_cohort_model_release_differently() {
    let l = ledger();
    let e = refused_release(run(&policy("figures = \"exact\""), &l));
    assert_eq!(e.identifier(), "DisclosureExactPerPerson");
    assert_eq!(l.budget("t1"), Some(UnitBudget { cap: 10_000_000, spent: 0 }), "the refused release charged no unit");
    assert_eq!(run(&policy("figures = \"exact\"\ngovernance = \"cohort\""), &l).unwrap(), 1);

    let mut allowlisted = policy("");
    allowlisted.grouping_allowlist.push("tenant_id".into());
    let l = ledger();
    assert_eq!(run(&allowlisted, &l).unwrap(), 1, "a per-person release groups by the permitted columns less the contributor key");
    allowlisted.governance = Some(Governance::Cohort);
    assert_eq!(refused_release(run(&allowlisted, &l)).identifier(), "DisclosureSingletonCohort");
}
