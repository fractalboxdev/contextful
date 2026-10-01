//! `disclosure.explain`: the decision and its path from the exchange policy and the
//! table's declaration, and the refusals the sealed explanation holds.

use contextful_core::exchange::ExchangePolicy;
use contextful_core::store::declare::TableDecl;
use contextful_policy::explain::{decide, steps, Audience, Coverage, Decision, Explanation, Step, Verdict, ROLE};

const POLICY: &str = r#"
expected_iss = "https://login.acme.example/"
role_claim   = "roles"
tenant_claim = "org_id"

[[default_grants]]
actions = ["read"]
tables  = ["public/*"]

[[role_grants.analyst]]
actions = ["read"]
tables  = ["research/*"]

[[role_grants.loader]]
actions = ["write"]
tables  = ["research/notes"]
"#;

const MANIFEST: &str = r#"
[[pipeline.tables]]
name = "research/notes"

[pipeline.tables.visibility]
source        = "wiki"
resource_key  = "page_id"
fidelity      = "mirrored"
family        = "item-exception"

[pipeline.tables.policy.rows]
predicate = "owner = 'team'"

[pipeline.tables.policy.zone]
allow = ["on-prem:*"]

[pipeline.tables.policy.columns.author]
strategy = "drop"
"#;

fn policy() -> ExchangePolicy {
    ExchangePolicy::parse(POLICY).unwrap()
}

fn decl() -> TableDecl {
    TableDecl::parse_pipeline(MANIFEST).unwrap().remove(0)
}

fn roles(r: &[&str]) -> Vec<String> {
    r.iter().map(|s| s.to_string()).collect()
}

fn layers(d: &Decision) -> Vec<&str> {
    d.path.iter().map(|s| s.layer).collect()
}

fn coverage() -> Coverage {
    Coverage { segments: 1, entries: 0, unplaced: 0, gaps: Vec::new(), visibility: Vec::new(), grants: None }
}

/// `audit explain --table <t> --subject <principal>` answers `admit` when a grant the
/// exchange policy issues for the `--role` names, else its `default_grants`, carries `read`
/// over a pattern covering the table, and `deny` otherwise.
// spec: disclosure.explain.decision@d864386e
#[test]
fn a_role_grant_reading_the_table_admits_and_a_write_grant_or_the_default_set_does_not() {
    let p = policy();
    assert_eq!(decide(Some(&p), &roles(&["analyst"]), "research/notes", Vec::new()).verdict, Verdict::Admit);
    // A matching role's write grant reads nothing; the default set applies only where no role matches.
    assert_eq!(decide(Some(&p), &roles(&["loader"]), "research/notes", Vec::new()).verdict, Verdict::Deny);
    assert_eq!(decide(Some(&p), &roles(&["loader"]), "public/faq", Vec::new()).verdict, Verdict::Deny);
    assert_eq!(decide(Some(&p), &roles(&["visitor"]), "public/faq", Vec::new()).verdict, Verdict::Admit);
    assert_eq!(decide(Some(&p), &[], "research/notes", Vec::new()).verdict, Verdict::Deny);
    assert_eq!(decide(None, &roles(&["analyst"]), "research/notes", Vec::new()).verdict, Verdict::Deny);
}

/// A decision's path names the role or default set, the covering grant, then each step
/// the table declares in relation order; a deny's path ends at the default deny.
// spec: disclosure.explain.path@208b0766
#[test]
fn an_admitting_path_runs_role_grant_then_the_declared_steps_and_a_denial_ends_at_default_deny() {
    let p = policy();
    let d = decide(Some(&p), &roles(&["analyst"]), "research/notes", steps(&decl(), p.tenant_claim.as_deref()).unwrap());
    assert_eq!(layers(&d), ["role", "grant", "visibility", "tenant", "table-policy", "zone", "mask"]);
    assert_eq!(d.path[0].name, "analyst");
    assert_eq!(d.path[1].name, "read research/*");
    assert_eq!(d.path[6].name, "author");

    let denied = decide(Some(&p), &roles(&["visitor"]), "research/notes", steps(&decl(), None).unwrap());
    assert_eq!(layers(&denied), ["default-grants", "default-deny"]);
}

/// A negative assurance answer emitted without its coverage block raises
/// `VisibilityUnqualifiedAssurance`.
// spec: disclosure.explain.unqualified-assurance@1a5c7fed
#[test]
fn a_denial_without_coverage_is_refused_and_with_coverage_releases() {
    let denied = decide(Some(&policy()), &roles(&["visitor"]), "research/notes", Vec::new());
    let mut e = Explanation {
        table: "research/notes".into(),
        subject: Some("user://bo@acme.example".into()),
        decision: Some(denied),
        replay: None,
        audience: None,
        coverage: None,
    };
    let err = e.seal(&|_| Vec::new()).unwrap_err().to_string();
    assert!(err.starts_with("VisibilityUnqualifiedAssurance"), "{err}");

    // An audience naming nobody is negative too.
    let mut empty = e.clone();
    empty.decision = None;
    empty.audience = Some(Audience { roles: Vec::new(), default_grants: false, classes: Vec::new() });
    assert!(empty.seal(&|_| Vec::new()).unwrap_err().to_string().starts_with("VisibilityUnqualifiedAssurance"));

    e.coverage = Some(coverage());
    let out = e.seal(&|_| Vec::new()).unwrap();
    assert_eq!(out["decision"]["verdict"], "deny");
    assert_eq!(out["coverage"]["segments"], 1);
}

/// A reader-facing explanation rendering the member identities of a group on the path
/// raises `VisibilityIndividualNamed`.
// spec: disclosure.explain.groups-not-members@bc65beeb
#[test]
fn a_rendering_carrying_a_member_of_a_role_on_the_path_is_refused() {
    let members = |g: &str| {
        if g == "analyst" {
            vec!["user://ada@acme.example".to_string()]
        } else {
            Vec::new()
        }
    };
    let admitted = decide(Some(&policy()), &roles(&["analyst"]), "research/notes", Vec::new());
    let e = Explanation {
        table: "research/notes".into(),
        subject: Some("user://bo@acme.example".into()),
        decision: Some(admitted.clone()),
        replay: None,
        audience: None,
        coverage: Some(coverage()),
    };
    // The group renders by name alone.
    let out = e.seal(&members).unwrap();
    assert_eq!(out["decision"]["path"][0]["name"], "analyst");

    // A step naming a member, forced onto the path, is refused.
    let mut forced = e.clone();
    let mut path = admitted.path;
    path.push(Step { layer: ROLE, name: "analyst: user://ada@acme.example".into() });
    forced.decision = Some(Decision { verdict: Verdict::Admit, path });
    let err = forced.seal(&members).unwrap_err().to_string();
    assert!(err.starts_with("VisibilityIndividualNamed"), "{err}");
    assert!(!err.contains("ada"), "{err}");

    // The subject the caller named echoes back though it belongs to the group.
    let mut own = e;
    own.subject = Some("user://ada@acme.example".into());
    assert_eq!(own.seal(&members).unwrap()["subject"], "user://ada@acme.example");
}
