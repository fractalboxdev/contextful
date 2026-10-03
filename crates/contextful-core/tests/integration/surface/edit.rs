//! `surface.edit`: the control document holds references to credentials and connectors, never either one.

use contextful_core::surface::edit::check_document;

fn doc(text: &str) -> toml::Value {
    toml::from_str(text).unwrap()
}

fn refusal(text: &str) -> String {
    check_document(&doc(text)).expect_err(text).to_string()
}

/// A document holding references alone passes.
#[test]
fn references_pass() {
    check_document(&doc(
        "[[pipeline]]\nid = \"team\"\ntables = [\"files\"]\n[pipeline.source]\nname = \"drive\"\n[pipeline.source.config]\nfolder_id = \"1AbCdEfGhIjKlMnOp\"\n\
         [pipeline.source.config.oauth]\nrefresh_token = \"${secret://drive-refresh}\"\nclient_secret = \"${secret://drive-client-secret}\"\n\
         [pipeline.source.config.headers]\nAuthorization = \"Bearer ${secret://vendor-token}\"\n[pipeline.destination]\nname = \"s3\"\n\
         [pipeline.destination.config]\naccess_key_id = \"env://AWS_ACCESS_KEY_ID\"\nsecret_access_key = \"secret://aws-secret\"\n",
    ))
    .unwrap();
}

/// A credential value typed into a configuration field raises `SecretMaterialInDocument`, naming the key path and
/// never the value.
#[test]
fn a_credential_value_is_refused_by_its_key_path() {
    for (text, path) in [
        ("[[pipeline]]\n[pipeline.source.config.oauth]\nclient_secret = \"hunter2hunter2\"\n", "pipeline.0.source.config.oauth.client_secret"),
        ("[[pipeline]]\n[pipeline.source.config]\napi_key = \"k-12345678\"\n", "pipeline.0.source.config.api_key"),
        ("[[pipeline]]\n[pipeline.source.config]\npassword = \"correct horse\"\n", "pipeline.0.source.config.password"),
        ("[[pipeline]]\n[pipeline.source.config.headers]\nAuthorization = \"Bearer abcdef0123456789abcdef\"\n", "pipeline.0.source.config.headers.Authorization"),
        ("[[pipeline]]\n[pipeline.source.config]\nnote = \"AKIAIOSFODNN7EXAMPLE\"\n", "pipeline.0.source.config.note"),
    ] {
        let e = refusal(text);
        assert!(e.starts_with("SecretMaterialInDocument") && e.contains(path), "{text}: {e}");
        for secret in ["hunter2hunter2", "k-12345678", "correct horse", "abcdef0123456789abcdef", "AKIAIOSFODNN7EXAMPLE"] {
            assert!(!e.contains(secret), "{e}");
        }
    }
}

/// An artifact offered through the document raises `ConnectorUploadRefused`: a `data:` URI, a base64 WebAssembly
/// body, a byte array opening with the WebAssembly magic, or an upload key.
#[test]
fn an_artifact_value_is_refused() {
    for text in [
        "[[pipeline]]\n[pipeline.source]\nname = \"vendor\"\n[pipeline.source.config]\nartifact = \"data:application/wasm;base64,AGFzbQEAAAA=\"\n",
        "[[pipeline]]\n[pipeline.source.config]\ncomponent = \"AGFzbQEAAAABBgFgAX8BfwMCAQA=\"\n",
        "[[pipeline]]\n[pipeline.source.config]\nwasm = [0, 97, 115, 109, 1, 0, 0, 0]\n",
        "[[pipeline]]\n[pipeline.source.config]\nwasm_bytes = \"ignored\"\n",
    ] {
        let e = refusal(text);
        assert!(e.starts_with("ConnectorUploadRefused") && e.contains("registered connector"), "{text}: {e}");
    }
}
