//! `disclosure.mirror` and the identity links of `authority.identify`: the `--directory`
//! read, the links and memberships a run commits, a servable binding's completeness, and the
//! resources a subject reaches through a source's current rows.

use contextful_core::disclosure::declare::Binding;
use contextful_core::disclosure::sweep::{
    check_bindings, commit, parse_directory, reachable, Commit, GrantLine, Governed, MemberRow, Membership, Observation, SourceState, SweepBlock, SweepError, SweepKind,
};
use contextful_core::disclosure::VisibilityError;
use contextful_core::identify::{LinkMethod, NormalizedSubject, Subject};
use contextful_core::store::declare::TableDecl;
use contextful_core::time::Instant;
use contextful_core::AuthorityError;

fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap()
}

fn grant(resource: &str, principal: &str, kind: &str) -> GrantLine {
    GrantLine { resource_id: resource.into(), principal: principal.into(), principal_kind: kind.into(), level: "viewer".into() }
}

fn governed(ids: &[&str]) -> Vec<Governed> {
    ids.iter().map(|id| Governed { resource_id: id.to_string(), resource_kind: "file".into(), visibility_class: "mirrored".into() }).collect()
}

fn subject(on_behalf_of: &str) -> NormalizedSubject {
    Subject { on_behalf_of: Some(on_behalf_of.into()), agent: Some("agent://drive-loop".into()), ..Subject::default() }.normalize()
}

fn member(group: &str, member: &str, kind: &str) -> MemberRow {
    MemberRow { group: group.into(), member: member.into(), member_kind: kind.into(), observed_at: at("2030-01-01T00:00:00Z") }
}

fn after(c: &Commit) -> SourceState {
    SourceState { resources: c.resources.clone(), grants: c.grants.clone(), members: c.members.clone(), links: c.links.clone(), freshness: Some(c.freshness.clone()) }
}

const DIRECTORY: &str = r#"
{"source_principal": "u-1001", "subject": "user://dana@acme.example", "method": "scim_email", "confidence": 0.9}
{"source_principal": "u-1002", "subject": "user://erin@acme.example", "method": "oidc_sub", "confidence": 0.4}
{"group": "g-finance", "member": "u-1001", "member_kind": "user"}
"#;

/// A `--directory` file is the identity provider's directory read, one JSON object per line: a link carrying `source_principal`, `subject`, `method` and `confidence`, or a membership carrying `group`, `member` and `member_kind`; a run without one keeps the current rows.
// spec: disclosure.sweep.directory-file@498e0ed4
#[test]
fn a_directory_file_carries_links_and_memberships_and_a_run_without_one_keeps_them() {
    let d = parse_directory("drive", DIRECTORY).unwrap();
    assert_eq!(d.links.len(), 2);
    assert_eq!((d.links[0].source.as_str(), d.links[0].source_principal.as_str(), d.links[0].method), ("drive", "u-1001", LinkMethod::ScimEmail));
    assert_eq!(d.members, [Membership { group: "g-finance".into(), member: "u-1001".into(), member_kind: "user".into() }]);

    for (line, why) in [
        (r#"{"source_principal": "u-1", "subject": "user://x", "method": "scim_email"}"#, "a link lacks confidence"),
        (r#"{"group": "g", "member": "u-1"}"#, "a membership lacks member_kind"),
        (r#"{"group": "", "member": "u-1", "member_kind": "user"}"#, "an empty group"),
        (r#"{"source_principal": "u-1", "subject": "user://x", "method": "profile_lookup", "confidence": 1.0}"#, "an unknown method"),
        (r#"{"source_principal": "u-1", "subject": "user://x", "method": "scim_email", "confidence": 1.5}"#, "a confidence above 1"),
    ] {
        match parse_directory("drive", &format!("\n{line}\n")) {
            Err(SweepError::DirectoryLine { line: 2, .. }) => {}
            other => panic!("{why}: {other:?}"),
        }
    }

    let first = Observation { directory: Some(d), ..Observation::from_grants_file("drive", SweepKind::Full, "s1", at("2030-01-01T00:00:00Z"), governed(&["f1"]), vec![grant("f1", "g-finance", "group")]) };
    let c1 = commit(&SourceState::default(), &first).unwrap();
    assert_eq!((c1.links.len(), c1.members.len()), (2, 1));
    assert_eq!(c1.members[0].observed_at, at("2030-01-01T00:00:00Z"));

    // A run given no directory carries the links and memberships forward.
    let second = Observation::from_grants_file("drive", SweepKind::Incremental, "s2", at("2030-01-01T00:05:00Z"), governed(&["f1"]), vec![]);
    let c2 = commit(&after(&c1), &second).unwrap();
    assert_eq!((c2.links, c2.members), (c1.links.clone(), c1.members.clone()));

    // A run given one replaces them.
    let third = Observation {
        directory: Some(parse_directory("drive", r#"{"source_principal": "u-1001", "subject": "user://dana@acme.example", "method": "scim_email", "confidence": 0.9}"#).unwrap()),
        ..Observation::from_grants_file("drive", SweepKind::Incremental, "s3", at("2030-01-01T00:10:00Z"), governed(&["f1"]), vec![])
    };
    let c3 = commit(&after(&c1), &third).unwrap();
    assert_eq!(c3.links.iter().map(|l| l.source_principal.as_str()).collect::<Vec<_>>(), ["u-1001"]);
    assert!(c3.members.is_empty());
}

/// An identity link maps a source principal onto a subject over fields the identity provider verified, recording `method` and `confidence`. Confidence relaxes no check.
// spec: authority.identify.identity-link@010fcd98
#[test]
fn an_identity_link_records_method_and_confidence_and_confidence_relaxes_nothing() {
    let d = parse_directory("drive", DIRECTORY).unwrap();
    assert_eq!((d.links[1].on_behalf_of.as_str(), d.links[1].method, d.links[1].confidence), ("user://erin@acme.example", LinkMethod::OidcSub, 0.4));

    // The link's subject passes the mint's hygiene; a padded or control-bearing value refuses.
    for bad in ["\" user://dana@acme.example\"", "\"user://dana\\u0007\""] {
        let line = format!(r#"{{"source_principal": "u-1", "subject": {bad}, "method": "scim_email", "confidence": 1.0}}"#);
        match parse_directory("drive", &line) {
            Err(SweepError::DirectoryLine { reason, .. }) => assert!(reason.contains("AuthoritySubjectMalformed"), "{reason}"),
            other => panic!("{bad}: {other:?}"),
        }
    }

    // A low-confidence verified link authorizes; a full-confidence operator-asserted one does not.
    let state = |line: &str| SourceState {
        grants: commit(&SourceState::default(), &Observation::from_grants_file("drive", SweepKind::Full, "s1", at("2030-01-01T00:00:00Z"), governed(&["f1"]), vec![grant("f1", "u-1001", "user")])).unwrap().grants,
        links: parse_directory("drive", line).unwrap().links,
        ..SourceState::default()
    };
    let low = state(r#"{"source_principal": "u-1001", "subject": "user://dana@acme.example", "method": "oidc_sub", "confidence": 0.1}"#);
    assert_eq!(reachable(&subject("user://dana@acme.example"), &low).unwrap().into_iter().collect::<Vec<_>>(), ["f1"]);
    let asserted = state(r#"{"source_principal": "u-1001", "subject": "user://dana@acme.example", "method": "operator_asserted", "confidence": 1.0}"#);
    assert!(matches!(reachable(&subject("user://dana@acme.example"), &asserted), Err(AuthorityError::AuthorityLinkUnverified(_))));
}

/// A subject reaches each resource granted to a principal a verified link maps onto it, and to
/// every group those principals belong to.
#[test]
fn a_subject_reaches_through_its_links_and_their_groups() {
    let mut s = SourceState {
        links: parse_directory("drive", DIRECTORY).unwrap().links,
        members: vec![
            member("g-finance", "u-1001", "user"),
            member("g-all", "g-finance", "group"),
            member("g-finance", "g-all", "group"),
        ],
        ..SourceState::default()
    };
    let obs = Observation::from_grants_file(
        "drive",
        SweepKind::Full,
        "s1",
        at("2030-01-01T00:00:00Z"),
        governed(&["f1", "f2", "f3"]),
        vec![grant("f1", "u-1001", "user"), grant("f2", "g-all", "group"), grant("f3", "u-1002", "user")],
    );
    s.grants = commit(&SourceState::default(), &obs).unwrap().grants;
    // The group cycle ends at the visited set.
    assert_eq!(reachable(&subject("user://dana@acme.example"), &s).unwrap().into_iter().collect::<Vec<_>>(), ["f1", "f2"]);
    assert_eq!(reachable(&subject("user://erin@acme.example"), &s).unwrap().into_iter().collect::<Vec<_>>(), ["f3"]);
    // No on_behalf_of: no identity, no reach.
    let anonymous = Subject { agent: Some("agent://drive-loop".into()), ..Subject::default() }.normalize();
    assert!(reachable(&anonymous, &s).unwrap().is_empty());
}

fn binding(block: &str) -> Binding {
    let manifest = format!("[[pipeline.tables]]\nname = \"drive/files\"\n\n[pipeline.tables.visibility]\nsource = \"drive\"\nresource_key = \"file_id\"\nfamily = \"item-exception\"\n{block}\n");
    Binding::of(&TableDecl::parse_pipeline(&manifest).unwrap().remove(0)).unwrap().unwrap()
}

const SWEEP: &str = "[[acl_sweep]]\nsource = \"drive\"\nenumerate = \"files\"\nfull = { every = \"1h\" }\nincremental = { every = \"5m\" }\n";

/// A binding at a servable level lacking a declared sweep for its source or a `max_acl_staleness` raises `VisibilityBindingIncomplete`.
#[test]
fn a_servable_binding_without_a_sweep_or_a_budget_is_incomplete() {
    let sweeps = SweepBlock::parse_manifest(SWEEP).unwrap();
    for fidelity in ["mirrored", "coarse"] {
        let complete = binding(&format!("fidelity = \"{fidelity}\"\nmax_acl_staleness = \"15m\""));
        check_bindings(std::slice::from_ref(&complete), &sweeps).unwrap();
        for (b, sweeps, missing) in [
            (complete.clone(), &[][..], "no `[[acl_sweep]]` block"),
            (binding(&format!("fidelity = \"{fidelity}\"")), &sweeps[..], "no `max_acl_staleness`"),
        ] {
            match check_bindings(&[b], sweeps) {
                Err(e @ VisibilityError::BindingIncomplete { .. }) => {
                    let shown = e.to_string();
                    assert!(shown.starts_with("VisibilityBindingIncomplete") && shown.contains("drive/files") && shown.contains(missing), "{shown}");
                }
                other => panic!("{fidelity} {missing}: {other:?}"),
            }
        }
    }
    // A level served from no store needs neither.
    for fidelity in ["federated", "excluded"] {
        check_bindings(&[binding(&format!("fidelity = \"{fidelity}\""))], &[]).unwrap();
    }
}
