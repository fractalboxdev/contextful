//! `authority.identify`: mint hygiene, normalization, attestation, identity links and
//! the subject rules a derivation obeys.

use contextful_core::claims::AuthorityBlock;
use contextful_core::identify::{
    authorize_link, check_value, resolve_link, Attestation, IdentityLink, LinkMethod, Member, MintSurface,
    Subject, SubjectDerivation, SUBJECT_VALUE_BYTES,
};
use contextful_core::AuthorityError;

fn subject(on_behalf_of: Option<&str>, agent: Option<&str>) -> Subject {
    Subject {
        on_behalf_of: on_behalf_of.map(str::to_string),
        agent: agent.map(str::to_string),
        ..Subject::default()
    }
}

fn link(method: LinkMethod, confidence: f64) -> IdentityLink {
    IdentityLink {
        source: "drive".to_string(),
        source_principal: "dana@acme.example".to_string(),
        on_behalf_of: "user://dana@acme.example".to_string(),
        method,
        confidence,
    }
}

/// The mint checks every subject value: non-empty, at most 256 B, no control character, no leading or trailing whitespace.
// spec: authority.identify.value-hygiene@434df386
#[test]
fn value_hygiene() {
    assert_eq!(SUBJECT_VALUE_BYTES, 256);
    assert!(check_value(Member::Agent, "agent://research-loop").is_ok());
    assert!(check_value(Member::Agent, &"a".repeat(SUBJECT_VALUE_BYTES)).is_ok());
    // 255 ASCII bytes plus one two-byte character crosses the bound by bytes, not characters.
    assert!(check_value(Member::Agent, &format!("{}é", "a".repeat(SUBJECT_VALUE_BYTES - 1))).is_err());
    assert!(check_value(Member::Agent, &"a".repeat(SUBJECT_VALUE_BYTES + 1)).is_err());
    assert!(check_value(Member::Agent, "").is_err());
    assert!(check_value(Member::Agent, "agent://a\u{7}b").is_err());
    assert!(check_value(Member::Agent, "agent://a\nb").is_err());
    assert!(check_value(Member::Agent, " agent://a").is_err());
    assert!(check_value(Member::Agent, "agent://a\t").is_err());
}

/// A value failing {{authority.identify.value-hygiene}} raises `AuthoritySubjectMalformed`, except that the command-line mint trims a padded value where the automated exchange refuses it.
// spec: authority.identify.malformed-value@eb970670
#[test]
fn malformed_value() {
    let padded = subject(Some("user://dana@acme.example"), Some("  agent://research-loop "));

    let minted = padded.clone().mint(MintSurface::CommandLine).unwrap();
    assert_eq!(minted.agent(), Some("agent://research-loop"));

    let refused = padded.mint(MintSurface::Exchange).unwrap_err();
    assert!(matches!(&refused, AuthorityError::AuthoritySubjectMalformed(m) if m.contains("agent")));
    assert!(refused.to_string().starts_with("AuthoritySubjectMalformed"));

    // A control character refuses on both surfaces, and the refusal does not echo the value.
    let control = subject(Some("user://dana\u{1b}[2J"), None);
    for surface in [MintSurface::CommandLine, MintSurface::Exchange] {
        let e = control.clone().mint(surface).unwrap_err();
        assert!(matches!(&e, AuthorityError::AuthoritySubjectMalformed(m) if m.contains("on_behalf_of")));
        assert!(!e.to_string().contains("dana"));
    }

    // Trimming to nothing leaves an empty value, which the command line refuses too.
    let blank = subject(None, Some("   "));
    assert!(matches!(blank.mint(MintSurface::CommandLine), Err(AuthorityError::AuthoritySubjectMalformed(_))));
}

/// The verified tuple is normalized once, each value trimmed and each blank member dropped, before any consumer reads it. No point of use re-normalizes a value or interprets a scheme prefix.
// spec: authority.identify.normalization@f174a6cc
#[test]
fn normalization() {
    let raw = Subject {
        on_behalf_of: Some(" user://Dana@ACME.example ".to_string()),
        agent: Some("\tagent://research-loop".to_string()),
        host: Some("   ".to_string()),
        task: Some(String::new()),
        zone: None,
        incognito: true,
    };
    let n = raw.normalize();
    assert_eq!(n.on_behalf_of(), Some("user://Dana@ACME.example"), "no case folding, no prefix stripping");
    assert_eq!(n.agent(), Some("agent://research-loop"));
    assert_eq!(n.host(), None);
    assert_eq!(n.task(), None);
    assert_eq!(n.zone(), None);
    assert!(n.incognito());

    // A normalized subject renders back to the wire shape with the blank members gone,
    // and normalizing that again changes nothing.
    let wire = n.to_subject();
    assert_eq!(wire.host, None);
    assert_eq!(wire.clone().normalize(), n);
}

/// A credential carrying no subject member raises `AuthoritySubjectMissing` at admission, naming the refusing surface and echoing no credential value.
// spec: authority.identify.subject-missing@31018d4a
#[test]
fn subject_missing() {
    let present = subject(None, Some("agent://research-loop")).normalize();
    assert!(present.require_member("mcp").is_ok());

    for empty in [
        Subject::default(),
        Subject { incognito: true, ..Subject::default() },
        Subject { host: Some("  ".to_string()), task: Some(String::new()), ..Subject::default() },
    ] {
        let e = empty.normalize().require_member("mcp").unwrap_err();
        assert!(matches!(&e, AuthorityError::AuthoritySubjectMissing(m) if m.contains("mcp")));
    }
}

/// Each member carries an attestation, `verified` or `asserted`. Agent, host, task and zone are asserted and render labeled as such; no surface presents an asserted member as identity.
// spec: authority.identify.attestation@2e37ab58
#[test]
fn attestation() {
    let full = Subject {
        on_behalf_of: Some("user://dana@acme.example".to_string()),
        agent: Some("agent://research-loop".to_string()),
        host: Some("host://dana-laptop".to_string()),
        task: Some("task://q4-review".to_string()),
        zone: Some("local:device".to_string()),
        incognito: false,
    }
    .normalize();
    let att = full.attestations();
    assert_eq!(att.get(&Member::OnBehalfOf), Some(&Attestation::Verified));
    for m in [Member::Agent, Member::Host, Member::Task, Member::Zone] {
        assert_eq!(att.get(&m), Some(&Attestation::Asserted), "{m:?}");
    }
    assert_eq!(full.identity(), Some("user://dana@acme.example"));

    // An agent-only subject has an asserted member and no identity.
    let agent_only = subject(None, Some("agent://research-loop")).normalize();
    assert_eq!(agent_only.attestations().len(), 1);
    assert_eq!(agent_only.identity(), None);
}

/// Only `scim_email` and `oidc_sub` links authorize. An authorization join consuming an `operator_asserted` link raises `AuthorityLinkUnverified`.
// spec: authority.identify.unverified-link@b5f4c041
#[test]
fn unverified_link() {
    assert_eq!(authorize_link(&link(LinkMethod::ScimEmail, 0.4)).unwrap(), "user://dana@acme.example");
    assert_eq!(authorize_link(&link(LinkMethod::OidcSub, 0.4)).unwrap(), "user://dana@acme.example");

    // Full confidence relaxes nothing.
    let asserted = link(LinkMethod::OperatorAsserted, 1.0);
    let e = authorize_link(&asserted).unwrap_err();
    assert!(matches!(&e, AuthorityError::AuthorityLinkUnverified(m) if m.contains("operator_asserted")));

    // The join over a link table: a verified match authorizes, an asserted-only match
    // refuses, and no match resolves to nothing.
    let links = vec![asserted.clone(), link(LinkMethod::OidcSub, 0.9)];
    assert_eq!(resolve_link(&links, "drive", "dana@acme.example").unwrap(), Some("user://dana@acme.example"));
    assert!(matches!(
        resolve_link(&[asserted], "drive", "dana@acme.example"),
        Err(AuthorityError::AuthorityLinkUnverified(_))
    ));
    assert_eq!(resolve_link(&links, "drive", "erin@acme.example").unwrap(), None);
}

/// A derivation naming an `on_behalf_of` different from its parent's raises `AuthoritySubjectRebound`.
// spec: authority.identify.subject-rebound@86a69e4c
#[test]
fn subject_rebound() {
    let parent = subject(Some("user://dana@acme.example"), Some("agent://research-loop")).normalize();

    let same = SubjectDerivation { on_behalf_of: Some("user://dana@acme.example".to_string()), ..Default::default() };
    assert_eq!(parent.derive(&same).unwrap().on_behalf_of(), Some("user://dana@acme.example"));

    let inherited = parent.derive(&SubjectDerivation { agent: Some("agent://sub".to_string()), ..Default::default() }).unwrap();
    assert_eq!(inherited.on_behalf_of(), Some("user://dana@acme.example"));
    assert_eq!(inherited.agent(), Some("agent://sub"));

    let other = SubjectDerivation { on_behalf_of: Some("user://erin@acme.example".to_string()), ..Default::default() };
    assert!(matches!(parent.derive(&other), Err(AuthorityError::AuthoritySubjectRebound(_))));

    // A parent acting for nobody cannot gain a principal through derivation.
    let unbound = subject(None, Some("agent://research-loop")).normalize();
    assert!(matches!(unbound.derive(&other), Err(AuthorityError::AuthoritySubjectRebound(_))));
}

/// Admission carries the caller's incognito flag unchanged, and a derivation turns it on and never off.
// spec: authority.identify.incognito@c72cbed9
#[test]
fn incognito() {
    let off = subject(Some("user://dana@acme.example"), None).normalize();
    let on = Subject { incognito: true, ..subject(Some("user://dana@acme.example"), None) }.normalize();
    assert!(!off.incognito());
    assert!(on.incognito());

    let turn_on = SubjectDerivation { incognito: Some(true), ..Default::default() };
    let turn_off = SubjectDerivation { incognito: Some(false), ..Default::default() };
    assert!(off.derive(&turn_on).unwrap().incognito());
    assert!(!off.derive(&SubjectDerivation::default()).unwrap().incognito());
    assert!(on.derive(&SubjectDerivation::default()).unwrap().incognito());
    assert!(matches!(on.derive(&turn_off), Err(AuthorityError::AttenuationWidens(m)) if m.starts_with("incognito")));
}

#[test]
fn the_authority_block_round_trips_the_shape_the_profile_maps() {
    let json = r#"{
      "iss": "contextful://acme-research",
      "aud": "contextful://acme-research",
      "jti": "01JBQ7K4F3S9W2X6",
      "iat": 1770000000,
      "exp": 1770000900,
      "alg": "Ed25519",
      "cnf": { "jkt": "NzbLsXh8uDCcd-6MNwXF4W_7noWXFZAfHkxZsRGC9Xs" },
      "sub": {
        "on_behalf_of": "user://dana@acme.example",
        "agent": "agent://research-loop",
        "host": "host://dana-laptop",
        "task": "task://q4-review",
        "zone": "local:device",
        "incognito": false
      },
      "att": { "on_behalf_of": "verified", "agent": "asserted", "host": "asserted", "task": "asserted", "zone": "asserted" },
      "grants": [
        {
          "actions": ["read"],
          "tables": ["research/*"],
          "tenant": { "table": "research/filings", "value": "acme-eu" },
          "templates": ["quarterly_rollup"],
          "max_rows": 5000
        }
      ],
      "rev": { "id": "rev://01JBQ7K4F3S9W2X6", "epoch": 7 }
    }"#;
    let block: AuthorityBlock = serde_json::from_str(json).unwrap();
    assert_eq!(block.rev.epoch, 7);
    assert_eq!(block.grants[0].max_rows, Some(5000));
    assert_eq!(block.att, block.subject().attestations());
    let back = serde_json::to_value(&block).unwrap();
    assert_eq!(back, serde_json::from_str::<serde_json::Value>(json).unwrap());
}
