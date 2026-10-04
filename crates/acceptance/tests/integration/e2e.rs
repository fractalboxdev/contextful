//! One consumer flow carries an S3 source through a published model to a second node.

use contextful_acceptance::s3::{S3Server, ACCESS_KEY, SECRET_KEY};
use contextful_acceptance::{bin, GitRepo};
use serde_json::{json, Value};
use std::process::Output;

const PROJECT: &str = "research";

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn manifest(endpoint: &str) -> String {
    format!(
        "authoring_posture = \"per_request\"\n\
         [[pipeline]]\nid = \"feed\"\n\
         [pipeline.source]\nname = \"s3\"\n\
         config = {{ bucket = \"source\", endpoint = \"{endpoint}\", key = \"notes.jsonl\", format = \"jsonl\", access_key_id = \"secret://source-key-id\", secret_access_key = \"secret://source-secret\" }}\n\
         [[pipeline.tables]]\nname = \"notes\"\n\
         [[model]]\nid = \"research/titles\"\nsql = \"SELECT note_id, title FROM feed_notes\"\nunique_key = [\"note_id\"]\n\
         [model.contract]\nversion = \"1.0.0\"\ncolumns = [{{ name = \"note_id\", type = \"utf8\", nullable = false }}, {{ name = \"title\", type = \"utf8\" }}]\n"
    )
}

fn node(p: &GitRepo, source: &str, shared: &str, id: &str) {
    p.write("contextful.toml", &manifest(source));
    p.write(
        ".contextful/context/research/config.toml",
        &format!(
            "[node]\nid = \"{id}\"\n\n[sync]\nendpoint = \"{shared}\"\nbucket = \"shared\"\nprefix = \"team\"\ncoordination = \"cas\"\naccess_key_id = \"env://CONTEXTFUL_SYNC_ACCESS_KEY_ID\"\nsecret_access_key = \"env://CONTEXTFUL_SYNC_SECRET_ACCESS_KEY\"\n"
        ),
    );
}

fn run(p: &GitRepo, cf: &std::path::Path, args: &[&str]) -> String {
    let out = p.run_env(
        cf,
        args,
        &[
            ("CONTEXTFUL_SECRETS_ALLOW_ENV_TEMPLATES", "1"),
            ("SOURCE_KEY_ID", ACCESS_KEY),
            ("SOURCE_SECRET", SECRET_KEY),
            ("CONTEXTFUL_SYNC_ACCESS_KEY_ID", ACCESS_KEY),
            ("CONTEXTFUL_SYNC_SECRET_ACCESS_KEY", SECRET_KEY),
        ],
    );
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    ok(&out)
}

fn query(p: &GitRepo, cf: &std::path::Path) -> Value {
    serde_json::from_str(&run(p, cf, &["query", "--json", "--project", PROJECT, "SELECT note_id, title FROM \"research/titles\" ORDER BY note_id"])).unwrap()
}

#[test]
fn e2e_consumer_round_trip() {
    let cf = bin("contextful");
    let source = S3Server::start("source");
    source.seed("notes.jsonl", b"{\"note_id\":\"n1\",\"title\":\"Solar battery storage\"}\n");
    let shared = S3Server::start("shared");
    let first = GitRepo::init();
    node(&first, &source.endpoint, &shared.endpoint, "node-a");
    run(&first, &cf, &["pipeline", "run", "feed", "--project", PROJECT, "--run-id", "run-0001", "--site-id", "site-a"]);
    let built: Value = serde_json::from_str(&run(&first, &cf, &["build", "research/titles", "--project", PROJECT, "--site-id", "site-a", "--json"])).unwrap();
    assert_eq!(built["published"], json!(true), "{built}");
    let before = query(&first, &cf);
    assert_eq!(before["rows"], json!([["n1", "Solar battery storage"]]));
    run(&first, &cf, &["sync", "push", "--project", PROJECT]);

    let second = GitRepo::init();
    node(&second, &source.endpoint, &shared.endpoint, "node-b");
    run(&second, &cf, &["sync", "pull", "--project", PROJECT]);
    let after = query(&second, &cf);
    assert_eq!(after, before, "the second node reads the published model byte for byte");
    assert!(source.gets() > 0, "the source bucket was read");
    assert!(shared.keys().iter().any(|k| k == "team/manifest.json"));
}
