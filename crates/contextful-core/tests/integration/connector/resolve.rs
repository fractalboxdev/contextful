//! `connector.resolve`: a plaintext credential written inline in a manifest, by build profile.

use contextful_core::connector::reference::is_credential_key;
use contextful_core::connector::resolve::{check_inline, DEVELOPMENT_PROFILE};
use contextful_core::connector::ConnectorError;

const TOKEN: &str = "hunter2-inline";

/// An http pipeline whose header table holds `key = value`.
fn header(key: &str, value: &str) -> String {
    format!("[[pipeline]]\nid = \"orders\"\n[pipeline.source]\nname = \"http\"\n[pipeline.source.config.headers]\nAccept = \"application/json\"\n{key} = \"{value}\"\n")
}

/// A credential key ignores case and reads `-` as `_`: `authorization`, `proxy_authorization`, `password`, `passwd`,
/// `secret`, `token`, `api_key`, `apikey`, `access_key_id`, `secret_access_key`, `private_key`, or a name ending
/// `_password`, `_secret`, `_token` or `_api_key`.
// spec: connector.reference.credential-key@19b0f9df
#[test]
fn a_credential_key_folds_case_and_header_spellings() {
    for key in ["Authorization", "Proxy-Authorization", "token", "X-Api-Token", "x-api-key", "client_secret", "DB_PASSWORD", "secret_access_key", "session_token"] {
        assert!(is_credential_key(key), "{key}");
    }
    for key in ["Accept", "endpoint", "token_url", "credentials", "X-Key", "description", "next_token_path"] {
        assert!(!is_credential_key(key), "{key}");
    }
}

/// Outside the development profile of {{topology.package.version-profile}}, a manifest string under a
/// {{connector.reference.credential-key}} holding no `secret://` or `env://` reference and passing
/// {{connector.reference.material-in-a-declaration}} raises `SecretInlineCredential` at declaration read, naming file
/// and key path, never the value.
// spec: connector.resolve.inline-outside-development@856a1b98
#[test]
fn an_inline_credential_refuses_outside_development_by_key_path() {
    let toml = header("X-Api-Key", TOKEN);
    for profile in ["contextful-full", "contextful-edge", "contextful-control"] {
        match check_inline(profile, "contextful.toml", &toml) {
            Err(ConnectorError::SecretInlineCredential(m)) => {
                assert!(m.contains("contextful.toml") && m.contains("pipeline.0.source.config.headers.X-Api-Key") && m.contains(profile), "{m}");
                assert!(!m.contains(TOKEN), "the message repeats the value: {m}");
            }
            other => panic!("{profile}: {other:?}"),
        }
    }
    // A credential-shaped value refuses first as material, by the same key path.
    let shaped = "ghp_0123456789abcdefghij0123456789abcdef";
    match check_inline("contextful-full", "contextful.toml", &header("Authorization", &format!("Bearer {shaped}"))) {
        Err(ConnectorError::SecretMaterialInDeclaration(m)) => assert!(m.contains("pipeline.0.source.config.headers.Authorization") && !m.contains(shaped), "{m}"),
        other => panic!("{other:?}"),
    }
    let json = format!("{{\"pipeline\": [{{\"id\": \"orders\", \"source\": {{\"config\": {{\"token\": \"{TOKEN}\"}}}}}}]}}");
    match check_inline("contextful-full", "pipelines/orders.json", &json) {
        Err(ConnectorError::SecretInlineCredential(m)) => assert!(m.contains("pipelines/orders.json") && m.contains("pipeline.0.source.config.token") && !m.contains(TOKEN), "{m}"),
        other => panic!("{other:?}"),
    }
}

/// The development profile reads the same inline credential.
#[test]
fn the_development_profile_reads_an_inline_credential() {
    assert_eq!(DEVELOPMENT_PROFILE, "development");
    assert!(check_inline(DEVELOPMENT_PROFILE, "contextful.toml", &header("X-Api-Key", TOKEN)).is_ok());
    assert!(check_inline(DEVELOPMENT_PROFILE, "contextful.toml", &format!("[sync]\nendpoint = \"s3://eu-west-1\"\naccess_key_id = \"{TOKEN}\"\n")).is_ok());
}

/// A secret reference, an environment reference and a literal under any other key read under every profile.
#[test]
fn a_reference_reads_under_every_profile() {
    let references = ["${secret://vendor-token}", "Bearer ${secret://vendor-token}", "secret://vendor-token", "env://VENDOR_TOKEN", ""];
    for profile in [DEVELOPMENT_PROFILE, "contextful-full", "contextful-edge", "contextful-control"] {
        for value in references {
            for key in ["Authorization", "X-Api-Key", "token"] {
                assert!(check_inline(profile, "contextful.toml", &header(key, value)).is_ok(), "{profile} {key} = {value}");
            }
        }
        assert!(check_inline(profile, "contextful.toml", &header("User-Agent", "contextful/0.1")).is_ok(), "{profile}");
        assert!(check_inline(profile, "contextful.toml", "[sync]\ncredentials = \"role\"\n").is_ok(), "{profile}");
        // A manifest that does not parse is the command's own read error to report.
        assert!(check_inline(profile, "contextful.toml", "token = ").is_ok(), "{profile}");
    }
}
