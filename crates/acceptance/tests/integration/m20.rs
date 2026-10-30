//! Milestone 20 — Drive permission custody.
//!
//! Reach: a Drive permission withdrawal stops an enterprise reader within the declared
//! visibility bound, while two connection refreshes resolve one versioned credential.

use contextful_acceptance::http::{Response, Server};
use contextful_acceptance::{bin, workspace_root, GitRepo};
use std::process::Output;

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A token endpoint rotating the refresh token: `refresh-<n>` answers `access-<n+1>` and
/// replacement `refresh-<n+1>`.
fn token(request: &contextful_acceptance::http::Request) -> Response {
    let form: Vec<(String, String)> = url::form_urlencoded::parse(&request.body).into_owned().collect();
    let presented = form.iter().find(|(k, _)| k == "refresh_token").and_then(|(_, v)| v.strip_prefix("refresh-")?.parse::<u32>().ok());
    match presented {
        Some(n) => Response::json(200, &format!(r#"{{"access_token":"access-{}","refresh_token":"refresh-{}","expires_in":3600}}"#, n + 1, n + 1)),
        None => Response::json(400, r#"{"error":"invalid_grant"}"#),
    }
}

/// One connection's project: the probe component reading the vendor under the `[auth]` access token.
fn connection(token: &Server, vendor: &Server) -> GitRepo {
    let p = GitRepo::init();
    p.write(".contextful/context/research/config.toml", "[node]\nid = \"ingest-a\"\n");
    let probe = std::fs::read(workspace_root().join("crates/contextful-wasm/tests/fixtures/probe.wasm")).unwrap();
    std::fs::create_dir_all(p.root.join("connectors")).unwrap();
    std::fs::write(p.root.join("connectors/probe.wasm"), probe).unwrap();
    p.write(
        "connector.toml",
        &format!(
            "name = \"probe\"\n[capabilities]\nallow_hosts = [\"127.0.0.1\"]\nclock = true\n\n[auth]\ntoken_endpoint = \"{}\"\nclient_id = \"probe-reader\"\n\
             client_secret = \"${{secret://vendor-client-secret}}\"\nrefresh_token = \"${{secret://vendor-refresh}}\"\naccess_token = \"${{secret://vendor-token}}\"\nscope = \"records.read\"\n",
            token.url("/oauth/token")
        ),
    );
    p.write(
        "contextful.toml",
        &format!(
            "authoring_posture = \"per_request\"\n[[pipeline]]\nid = \"probe\"\ntables = [\"fetch {}\"]\n\n[pipeline.source]\nname = \"connectors/probe.wasm\"\n\n\
             [pipeline.source.config]\nmanifest = \"connector.toml\"\nallow = [\"127.0.0.1\"]\nattach = {{ Authorization = \"Bearer ${{secret://vendor-token}}\" }}\n",
            vendor.url("/v1")
        ),
    );
    p
}

#[test]
#[ignore = "milestone 20 is open: no Drive permission sweep withdraws a reader"]
fn m20_drive_permissions() {
    let cf = bin("contextful");
    let issuer = Server::start(token);
    let vendor = Server::start(|_| Response::json(200, "{}"));
    let secrets = tempfile::tempdir().unwrap();
    std::fs::write(secrets.path().join("vendor-client-secret"), "client-secret").unwrap();
    std::fs::write(secrets.path().join("vendor-refresh"), "refresh-0").unwrap();
    let dir = secrets.path().to_str().unwrap().to_string();
    let env = [("CONTEXTFUL_SECRETS_BACKEND", "file"), ("CONTEXTFUL_SECRETS_DIR", dir.as_str())];

    // Two connections refresh one stored credential at once; one version wins and both land.
    let (a, b) = (connection(&issuer, &vendor), connection(&issuer, &vendor));
    let args = ["pipeline", "run", "probe", "--project", "research", "--site-id", "site-a"];
    let outs: Vec<Output> = std::thread::scope(|s| {
        let runs = [&a, &b].map(|p| s.spawn(|| p.run_env(&cf, &args, &env)));
        runs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(outs.len(), 2);
    for out in &outs {
        assert!(ok(out).contains("success · 1 rows"));
    }
    let stored = std::fs::read_to_string(secrets.path().join("vendor-refresh")).unwrap();
    assert!(stored.starts_with("refresh-") && stored != "refresh-0", "{stored}");

    todo!("a Drive permission sweep withdraws a reader within the declared visibility bound");
}
