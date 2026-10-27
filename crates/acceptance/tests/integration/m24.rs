//! Milestone 24 — Iceberg export.
//!
//! Reach: two exports of one table commit an append and then an overwrite snapshot to a
//! local Iceberg REST catalog, and a reader of the namespace sees the second export's rows
//! alone.

use arrow_array::cast::AsArray;
use contextful_acceptance::iceberg::Catalog;
use contextful_acceptance::s3::S3Server;
use contextful_acceptance::{bin, GitRepo};
use std::process::Output;

const AUD: &str = "contextful://acme-research";
const STORE: &str = ".contextful/context/research";
const CATALOG_TOKEN: &str = "catalog-token-value";

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn span_ids(catalog: &Catalog, s3: &S3Server) -> Vec<String> {
    let mut ids: Vec<String> = catalog
        .scan(s3, "mirror", "spans")
        .iter()
        .flat_map(|b| b.column_by_name("span_id").unwrap().as_string::<i32>().iter().map(|v| v.unwrap().to_string()).collect::<Vec<_>>())
        .collect();
    ids.sort();
    ids
}

#[test]
#[ignore = "milestone 24 is open: the routing Worker's route and caller-sql clauses are unperformed"]
fn m24_iceberg_export() {
    let cf = bin("contextful");
    let s3 = S3Server::start("exports");
    let catalog = Catalog::start(&s3, CATALOG_TOKEN);
    let p = GitRepo::init();
    p.write(&format!("{STORE}/config.toml"), "[node]\nid = \"ingest-a\"\n");
    p.write(".contextful/issuance.toml", &format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 3600\n"));
    p.write(
        "contextful.toml",
        &format!(
            "authoring_posture = \"per_request\"\n\n[[export]]\nname = \"spans-lake\"\ntable = \"spans\"\nformat = \"iceberg\"\n\
             credential = \"svc://mirror\"\ncatalog = \"{}\"\nstorage = \"{}\"\nwarehouse = \"exports\"\nnamespace = \"mirror\"\n\
             table_name = \"spans\"\nformat_version = 2\ntoken = \"${{secret://catalog-token}}\"\n",
            catalog.endpoint, s3.endpoint
        ),
    );
    let public = ok(&p.run(&cf, &["token", "keygen", "--out", ".contextful/issuer.seed"]));
    let token = ok(&p.run(&cf, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "svc://mirror", "--zone", "on-prem:hq", "--table", "spans", "--ttl", "600"]));
    let land = |run: &str, now: &str, ids: &[&str]| {
        let rows: String = ids.iter().map(|id| format!("{{\"span_id\":\"{id}\",\"duration_ms\":12}}\n")).collect();
        p.write(&format!("{run}.jsonl"), &rows);
        ok(&p.run(&cf, &["context", "land", "spans", "--project", "research", "--rows", &format!("{run}.jsonl"), "--run-id", run, "--site-id", "site", "--now", now]));
    };
    let export = || {
        let env = [("CONTEXTFUL_TOKEN", token.as_str()), ("CATALOG_TOKEN", CATALOG_TOKEN), ("CONTEXTFUL_SECRETS_ALLOW_ENV_TEMPLATES", "1")];
        ok(&p.run_env(&cf, &["export", "run", "spans-lake", "--project", "research", "--public-key", &public, "--audience", AUD], &env))
    };

    land("load-1", "2030-01-01T00:00:00Z", &["a1", "a2"]);
    assert!(export().contains("committed 2 rows"));
    assert_eq!(span_ids(&catalog, &s3), ["a1", "a2"]);

    land("load-2", "2030-01-01T00:01:00Z", &["b1"]);
    assert!(export().contains("committed 3 rows"));
    assert_eq!(span_ids(&catalog, &s3), ["a1", "a2", "b1"], "the reader sees the second export's rows alone");
    let metadata = catalog.metadata("mirror", "spans").unwrap();
    let mut operations: Vec<_> = metadata.snapshots().map(|s| (s.sequence_number(), s.summary().operation.clone())).collect();
    operations.sort_by_key(|(sequence, _)| *sequence);
    assert_eq!(operations.into_iter().map(|(_, op)| op).collect::<Vec<_>>(), [iceberg::spec::Operation::Append, iceberg::spec::Operation::Overwrite]);
    assert_eq!(catalog.live_files(&s3, "mirror", "spans").len(), 1);
    assert!(export().contains("unchanged"));
}
