//! `connector.reference`: the scheme, the template grammar and the material check.

use contextful_core::connector::reference::{check_material, Hydrated, Part, SecretName, Template, NAME_MAX};
use contextful_core::connector::ConnectorError;

/// `secret://<name>` carries a logical name matching `[a-z0-9-]+` of at most 128 chars. A reference is never a
/// storage path, a file name or a key identifier.
// spec: connector.reference.reference-scheme@c250c1a5
#[test]
fn a_reference_is_a_lowercase_logical_name_of_at_most_128_chars() {
    assert_eq!(NAME_MAX, 128);
    for ok in ["vendor-token", "a", "k8s-2", &"x".repeat(128)] {
        assert!(SecretName::parse(ok).is_ok(), "{ok}");
    }
    for bad in ["", &"x".repeat(129), "Vendor", "vendor_token", "vault/kv/vendor", "token.json", "arn:aws:kms:key/1"] {
        assert!(SecretName::parse(bad).is_err(), "{bad}");
    }
}

/// A value embedding a credential is a template of literal text with `${secret://<name>}` placeholders, such as
/// `Bearer ${secret://vendor-token}`.
// spec: connector.reference.value-template@0c2d87f3
#[test]
fn a_template_is_literal_text_and_secret_placeholders() {
    let t = Template::parse("Bearer ${secret://vendor-token}").unwrap();
    assert_eq!(t.parts, [Part::Literal("Bearer ".into()), Part::Secret(SecretName::parse("vendor-token").unwrap())]);
    let two = Template::parse("${secret://user}:${secret://pass}").unwrap();
    assert_eq!(two.names().map(|n| n.as_str()).collect::<Vec<_>>(), ["user", "pass"]);
    let filled = two.render(|n| Ok::<_, ()>(Hydrated::new(format!("<{n}>")))).unwrap();
    assert_eq!(filled.reveal(), "<user>:<pass>");
    assert!(!Template::parse("application/json").unwrap().has_reference());
}

/// `${env://NAME}` or a bare `${NAME}` inside a template raises `SecretForeignPlaceholder` at parse, naming the
/// value and the span.
// spec: connector.reference.foreign-placeholder@b4e26f61
#[test]
fn an_environment_or_bare_placeholder_is_foreign() {
    for (value, span) in [("Bearer ${env://VENDOR_TOKEN}", "${env://VENDOR_TOKEN}"), ("Bearer ${VENDOR_TOKEN}", "${VENDOR_TOKEN}")] {
        match Template::parse(value) {
            Err(ConnectorError::SecretForeignPlaceholder(m)) => assert!(m.contains(value) && m.contains(span), "{m}"),
            other => panic!("{value}: {other:?}"),
        }
    }
}

/// An unclosed, empty or nested placeholder raises `SecretMalformedTemplate` while the declaration is read, ahead
/// of any text leaving the process.
// spec: connector.reference.malformed-placeholder@7e945f39
#[test]
fn an_unclosed_empty_or_nested_placeholder_is_malformed() {
    for value in ["Bearer ${secret://vendor-token", "Bearer ${}", "Bearer ${ }", "${secret://${secret://inner}}"] {
        assert!(matches!(Template::parse(value), Err(ConnectorError::SecretMalformedTemplate(_))), "{value}");
    }
}

/// A credential-shaped literal standing where a reference belongs raises `SecretMaterialInDeclaration` at
/// validation, naming the key.
// spec: connector.reference.material-in-a-declaration@3ddcdc4d
#[test]
fn a_credential_literal_in_a_declaration_is_refused() {
    for value in ["Bearer ghp_0123456789abcdefghijABCDEFGHIJ012345", "Bearer 9f8e7d6c5b4a39218a7b", "Basic dXNlcjpodW50ZXIy", "AKIAIOSFODNN7EXAMPLE"] {
        match check_material("headers.Authorization", value) {
            Err(ConnectorError::SecretMaterialInDeclaration(m)) => {
                assert!(m.contains("headers.Authorization"), "{m}");
                assert!(!m.contains(value), "the message repeats the material: {m}");
            }
            other => panic!("{value}: {other:?}"),
        }
    }
    for value in ["Bearer ${secret://vendor-token}", "application/json", "contextful/0.1"] {
        assert!(check_material("headers.X", value).is_ok(), "{value}");
    }
}

#[test]
fn a_hydrated_value_prints_as_a_sentinel() {
    let h = Hydrated::new("lease-9f8e7d6c5b4a3921");
    assert_eq!(format!("{h} {h:?}"), "[secret] [secret]");
    assert_eq!(h.reveal(), "lease-9f8e7d6c5b4a3921");
}
