use super::Repo;

fn refused(out: &std::process::Output, code: &str) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        !out.status.success(),
        "source lint passed unexpectedly: {stderr}"
    );
    assert!(stderr.contains(code), "{stderr}");
    stderr
}

/// A source lint over the runtime crates finding a subject claim formatted into SQL text raises `EnforceInterpolatedSubjectClaim`, naming the file and line.
// spec: assurance.gate.interpolated-claim@e7112d93
#[test]
fn interpolated_subject_claim_names_the_source_line() {
    let r = Repo::init();
    r.write(
        "crates/demo/src/lib.rs",
        "pub fn query(subject: &str) -> String {\n    format!(\"SELECT * FROM rows WHERE subject = '{subject}'\")\n}\n",
    );

    let err = refused(
        &r.run_ci(&["source-lint"]),
        "EnforceInterpolatedSubjectClaim",
    );
    assert!(err.contains("crates/demo/src/lib.rs:2"), "{err}");
}

#[test]
fn subject_binding_and_non_sql_formatting_pass_source_lint() {
    let r = Repo::init();
    r.write(
        "crates/demo/src/lib.rs",
        "pub fn query(subject: &str) -> (&'static str, &str) {\n    (\"SELECT * FROM rows WHERE subject = ?\", subject)\n}\npub fn log(subject: &str) -> String { format!(\"subject: {subject}\") }\n",
    );

    let out = r.run_ci(&["source-lint"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn crate_graph_stage_runs_the_source_lint() {
    let r = Repo::init();
    r.write(
        "crates/demo/src/lib.rs",
        "pub fn query(subject: &str) -> String {\n    format!(\"SELECT * FROM rows WHERE subject = '{subject}'\")\n}\n",
    );
    r.lock();
    r.commit("fixture");

    let err = refused(
        &r.gate(&["--stage", "crate-graph"]),
        "EnforceInterpolatedSubjectClaim",
    );
    assert!(err.contains("crates/demo/src/lib.rs:2"), "{err}");
}

#[test]
fn positional_subject_claim_is_refused() {
    let r = Repo::init();
    r.write(
        "crates/demo/src/lib.rs",
        "pub fn query(subject_id: &str) -> String {\n    format!(\"SELECT * FROM rows WHERE subject = '{}'\", subject_id)\n}\n",
    );

    let err = refused(
        &r.run_ci(&["source-lint"]),
        "EnforceInterpolatedSubjectClaim",
    );
    assert!(err.contains("crates/demo/src/lib.rs:2"), "{err}");
}

#[test]
fn escaped_subject_field_is_literal_sql_text() {
    let r = Repo::init();
    r.write(
        "crates/demo/src/lib.rs",
        "pub fn query() -> String { format!(\"SELECT '{{subject}}'\") }\n",
    );

    let out = r.run_ci(&["source-lint"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn passes(out: &std::process::Output) {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Every row-returning function of the store crate takes the enforcement token type, whose constructor is private to the enforcement module; a row path outside a registered relation raises `EnforceUnmediatedPath`, naming the function.
// spec: assurance.gate.row-token@8d65ca4e
#[test]
fn row_returning_store_function_without_the_token_names_the_function() {
    let r = Repo::init();
    r.write(
        "crates/contextful-context/src/read/face.rs",
        "use contextful_core::read::respond::Response;\npub struct Face;\nimpl Face {\n    pub fn rows(&self, table: &str) -> Result<Response, String> {\n        unimplemented!(\"{table}\")\n    }\n}\n",
    );

    let err = refused(&r.run_ci(&["source-lint"]), "EnforceUnmediatedPath");
    assert!(
        err.contains("crates/contextful-context/src/read/face.rs::Face::rows"),
        "{err}"
    );
}

#[test]
fn free_row_returning_store_function_without_the_token_is_refused() {
    let r = Repo::init();
    r.write(
        "crates/contextful-context/src/rows.rs",
        "pub fn dump(\n    table: &str,\n) -> Option<contextful_core::read::respond::Response> {\n    let _ = table;\n    None\n}\n",
    );

    let err = refused(&r.run_ci(&["source-lint"]), "EnforceUnmediatedPath");
    assert!(
        err.contains("crates/contextful-context/src/rows.rs::dump"),
        "{err}"
    );
}

#[test]
fn token_taking_crate_private_and_foreign_row_functions_pass() {
    let r = Repo::init();
    r.write(TOKEN_HOME, SESSION);
    r.write(
        "crates/contextful-context/src/read/face.rs",
        "use contextful_core::read::respond::Response;\nuse contextful_policy::enforce::Session;\npub struct Face;\nimpl Face {\n    pub fn rows(&self, session: &Session, table: &str) -> Result<Response, String> {\n        unimplemented!(\"{table}\")\n    }\n    pub(crate) fn respond(&self) -> Response {\n        unimplemented!()\n    }\n}\n",
    );
    r.write(
        "crates/demo/src/http.rs",
        "pub struct Response;\npub fn answer() -> Response { Response }\n",
    );

    passes(&r.run_ci(&["source-lint"]));
}

#[test]
fn a_public_field_on_the_enforcement_token_is_refused() {
    let r = Repo::init();
    r.write(
        "crates/contextful-policy/src/enforce/session.rs",
        "pub struct Session {\n    pub grants: Vec<String>,\n}\n",
    );

    let err = refused(&r.run_ci(&["source-lint"]), "EnforceUnmediatedPath");
    assert!(err.contains("Session"), "{err}");
}

/// An outbound call reachable under the default configuration — usage ping, license check, update probe — raises `UnconfiguredEgress` in the crate-graph stage, naming the call site.
// spec: assurance.gate.unconfigured-egress@ce46df87
#[test]
fn outbound_call_outside_a_configured_adapter_names_the_call_site() {
    let r = Repo::init();
    r.write(
        "crates/demo/src/lib.rs",
        "pub fn double(x: i32) -> i32 {\n    x * 2\n}\npub fn ping() {\n    let _ = ureq::get(\"https://usage.example/ping\").call();\n}\n",
    );
    r.lock();
    r.commit("fixture");

    let err = refused(&r.gate(&["--stage", "crate-graph"]), "UnconfiguredEgress");
    assert!(err.contains("crates/demo/src/lib.rs:5"), "{err}");
}

#[test]
fn socket_dial_and_imported_client_are_outbound_calls() {
    let r = Repo::init();
    r.write(
        "crates/demo/src/probe.rs",
        "pub fn probe() {\n    let _ = std::net::TcpStream::connect(\"updates.example:443\");\n}\n",
    );
    let err = refused(&r.run_ci(&["source-lint"]), "UnconfiguredEgress");
    assert!(err.contains("crates/demo/src/probe.rs:2"), "{err}");

    r.write("crates/demo/src/probe.rs", "pub fn probe() {}\n");
    r.write(
        "crates/demo/src/license.rs",
        "use reqwest::blocking::Client;\npub fn check() {\n    let _ = Client::new();\n}\n",
    );
    let err = refused(&r.run_ci(&["source-lint"]), "UnconfiguredEgress");
    assert!(err.contains("crates/demo/src/license.rs:1"), "{err}");
}

#[test]
fn configured_adapters_listeners_and_test_modules_pass_the_egress_lint() {
    let r = Repo::init();
    r.write(
        "crates/contextful-outbound/src/ureq_transport.rs",
        "pub fn send() {\n    let _ = ureq::get(\"https://configured.example\").call();\n}\n",
    );
    r.write(
        "crates/demo/src/serve.rs",
        "pub fn serve() {\n    let _ = std::net::TcpListener::bind(\"127.0.0.1:0\");\n}\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn dials() {\n        let _ = std::net::TcpStream::connect(\"127.0.0.1:1\");\n    }\n}\n",
    );

    passes(&r.run_ci(&["source-lint"]));
}

#[test]
fn a_registered_operator_path_masks_no_later_row_function() {
    let r = Repo::init();
    r.write(
        "crates/contextful-context/src/read/face.rs",
        "use contextful_core::read::respond::Response;\npub struct Face;\nimpl Face {\n    pub fn operator_query(&self, sql: &str) -> Response {\n        unimplemented!(\"{sql}\")\n    }\n}\npub fn dump() -> Response {\n    unimplemented!()\n}\n",
    );

    let err = refused(&r.run_ci(&["source-lint"]), "EnforceUnmediatedPath");
    assert!(
        err.contains("crates/contextful-context/src/read/face.rs::dump"),
        "{err}"
    );
}

/// A private-field admission value whose one public constructor takes an admitted authority.
const SESSION: &str = "pub struct AdmittedAuthority;\npub struct Session {\n    grants: Vec<String>,\n}\nimpl Session {\n    pub fn open(authority: &AdmittedAuthority) -> Result<Session, String> {\n        let _ = authority;\n        Ok(Session { grants: Vec::new() })\n    }\n    pub fn grants(&self) -> &[String] {\n        &self.grants\n    }\n}\n";
const TOKEN_HOME: &str = "crates/contextful-policy/src/enforce/session.rs";

#[test]
fn stored_row_readers_without_the_token_are_refused() {
    let r = Repo::init();
    r.write(TOKEN_HOME, SESSION);
    r.write(
        "crates/contextful-context/src/rows.rs",
        "use serde_json::{Map, Value};\npub fn dump_rows(table: &str) -> Vec<Map<String, Value>> {\n    let _ = table;\n    Vec::new()\n}\n",
    );
    let err = refused(&r.run_ci(&["source-lint"]), "EnforceUnmediatedPath");
    assert!(err.contains("crates/contextful-context/src/rows.rs::dump_rows"), "{err}");

    r.write(
        "crates/contextful-context/src/rows.rs",
        "pub fn batches(each: &mut dyn FnMut(Vec<serde_json::Map<String, serde_json::Value>>)) {\n    let _ = each;\n}\n",
    );
    let err = refused(&r.run_ci(&["source-lint"]), "EnforceUnmediatedPath");
    assert!(err.contains("crates/contextful-context/src/rows.rs::batches"), "{err}");

    r.write(
        "crates/contextful-context/src/rows.rs",
        "pub fn read_parts(path: &str) -> Result<Vec<arrow::record_batch::RecordBatch>, String> {\n    Err(path.into())\n}\n",
    );
    let err = refused(&r.run_ci(&["source-lint"]), "EnforceUnmediatedPath");
    assert!(err.contains("crates/contextful-context/src/rows.rs::read_parts"), "{err}");
}

#[test]
fn an_optional_token_is_not_the_token() {
    let r = Repo::init();
    r.write(TOKEN_HOME, SESSION);
    r.write(
        "crates/contextful-context/src/read/face.rs",
        "use contextful_core::read::respond::Response;\nuse contextful_policy::enforce::Session;\npub struct Face;\nimpl Face {\n    pub fn rows(&self, session: Option<&Session>, table: &str) -> Response {\n        unimplemented!(\"{table}\")\n    }\n}\n",
    );

    let err = refused(&r.run_ci(&["source-lint"]), "EnforceUnmediatedPath");
    assert!(err.contains("crates/contextful-context/src/read/face.rs::Face::rows"), "{err}");
}

#[test]
fn a_constructor_outside_the_admitted_authority_is_refused() {
    let r = Repo::init();
    r.write("crates/contextful-context/src/lib.rs", "pub fn double(x: i32) -> i32 { x * 2 }\n");
    r.write(TOKEN_HOME, SESSION);
    passes(&r.run_ci(&["source-lint"]));

    for extra in [
        "impl Session {\n    pub fn blank() -> Self {\n        Session { grants: Vec::new() }\n    }\n}\n",
        "impl Default for Session {\n    fn default() -> Self {\n        Session { grants: Vec::new() }\n    }\n}\n",
        "impl From<Vec<String>> for Session {\n    fn from(grants: Vec<String>) -> Self {\n        Session { grants }\n    }\n}\n",
    ] {
        r.write(TOKEN_HOME, &format!("{SESSION}{extra}"));
        let err = refused(&r.run_ci(&["source-lint"]), "EnforceUnmediatedPath");
        assert!(err.contains("Session"), "{err}");
    }
}

#[test]
fn a_missing_token_home_is_refused() {
    let r = Repo::init();
    r.write(
        "crates/contextful-context/Cargo.toml",
        "[package]\nname = \"contextful-context\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\ncontextful-policy = { path = \"../contextful-policy\" }\n",
    );
    r.write("crates/contextful-context/src/lib.rs", "pub fn double(x: i32) -> i32 { x * 2 }\n");

    let err = refused(&r.run_ci(&["source-lint"]), "EnforceUnmediatedPath");
    assert!(err.contains(TOKEN_HOME), "{err}");

    r.write(TOKEN_HOME, "pub struct Admission {\n    grants: Vec<String>,\n}\n");
    let err = refused(&r.run_ci(&["source-lint"]), "EnforceUnmediatedPath");
    assert!(err.contains(TOKEN_HOME), "{err}");
}

#[test]
fn a_client_named_in_any_position_is_an_outbound_call() {
    let r = Repo::init();
    for (source, line) in [
        ("pub fn ping() {\n    let c: reqwest::blocking::Client = Default::default();\n    let _ = c;\n}\n", 2),
        ("pub fn ping() {\n    let _ = <ureq::Agent as Default>::default();\n}\n", 2),
        ("use {ureq::Agent};\npub fn ping() {}\n", 1),
        ("extern crate ureq as http;\npub fn ping() {}\n", 1),
    ] {
        r.write("crates/demo/src/ping.rs", source);
        let err = refused(&r.run_ci(&["source-lint"]), "UnconfiguredEgress");
        assert!(err.contains(&format!("crates/demo/src/ping.rs:{line}")), "{err}");
    }
}

#[test]
fn a_renamed_client_dependency_is_an_outbound_call() {
    let r = Repo::init();
    r.write(
        "crates/demo/Cargo.toml",
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nhttp = { package = \"ureq\", version = \"2\" }\n",
    );
    r.write("crates/demo/src/ping.rs", "pub fn ping() {\n    let _ = http::get(\"https://usage.example\");\n}\n");

    let err = refused(&r.run_ci(&["source-lint"]), "UnconfiguredEgress");
    assert!(err.contains("crates/demo/src/ping.rs:2"), "{err}");
}

#[test]
fn database_clients_and_raw_component_requests_are_outbound_calls() {
    let r = Repo::init();
    r.write(
        "crates/demo/src/telemetry.rs",
        "pub fn report() {\n    let _ = postgres::Client::connect(\"host=telemetry.example\", postgres::NoTls);\n}\n",
    );
    let err = refused(&r.run_ci(&["source-lint"]), "UnconfiguredEgress");
    assert!(err.contains("crates/demo/src/telemetry.rs:2"), "{err}");

    r.write(
        "crates/demo/src/telemetry.rs",
        "pub fn report(request: R, config: C) {\n    let _ = wasmtime_wasi_http::p2::default_send_request(request, config);\n}\n",
    );
    let err = refused(&r.run_ci(&["source-lint"]), "UnconfiguredEgress");
    assert!(err.contains("crates/demo/src/telemetry.rs:2"), "{err}");

    r.write("crates/demo/src/telemetry.rs", "pub fn report() {}\n");
    r.write(
        "crates/contextful-pg/src/lib.rs",
        "pub fn open(dsn: &str) {\n    let _ = postgres::Client::connect(dsn, postgres::NoTls);\n}\n",
    );
    passes(&r.run_ci(&["source-lint"]));
}

#[test]
fn test_only_items_pass_the_egress_lint() {
    let r = Repo::init();
    r.write(
        "crates/demo/src/serve.rs",
        "#[cfg(test)]\nuse ureq::Agent;\n#[cfg(test)]\nconst PROBE: &str = ureq::VERSION;\n#[cfg(test)]\nimpl Probe {\n    fn dial() {\n        let _ = std::net::TcpStream::connect(\"127.0.0.1:1\");\n    }\n}\npub struct Probe;\n",
    );

    passes(&r.run_ci(&["source-lint"]));
}
