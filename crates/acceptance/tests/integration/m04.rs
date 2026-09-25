//! Milestone 4 — ingest.
//!
//! Reach: a declared pipeline pulls from a real source, lands rows under a cursor, and
//! never holds a credential in plaintext.

use contextful_acceptance::http::{Response, Server};
use contextful_acceptance::{bin, GitRepo};
use parquet::file::reader::{FileReader, SerializedFileReader};
use parquet::record::Field;
use std::process::Output;
use std::sync::{Arc, Mutex};

const STORE: &str = ".contextful/context/research";
const MINT: &str = "mint-bootstrap-3f9a1c77e2";
const LEASED: &str = "lease-9f8e7d6c5b4a3921";
const AWS_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn refused(out: &Output, error: &str) {
    assert!(!out.status.success(), "expected {error}, got success: {}", String::from_utf8_lossy(&out.stdout));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains(error), "expected {error}, got: {stderr}");
}

/// `(id, note)` of every row in the landed parts of one run.
fn landed(p: &GitRepo, listed: &str, run: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for f in listed.lines().filter(|f| f.contains(&format!("/runs/{run}/"))) {
        let reader = SerializedFileReader::new(std::fs::File::open(p.root.join(STORE).join(f)).unwrap()).unwrap();
        for row in reader.get_row_iter(None).unwrap() {
            let row = row.unwrap();
            let (mut id, mut note) = (String::new(), String::new());
            for (name, field) in row.get_column_iter() {
                match (name.as_str(), field) {
                    ("id", Field::Str(s)) => id = s.clone(),
                    ("note", Field::Str(s)) => note = s.clone(),
                    _ => {}
                }
            }
            out.push((id, note));
        }
    }
    out.sort();
    out
}

#[test]
fn m04_ingest() {
    let cf = bin("contextful");
    // The vendor: rows by `updated_at`, served at or after `since`, one per page.
    let rows = Arc::new(Mutex::new(vec![
        ("f1", "2030-01-01T00:00:00Z", format!("rotated key {AWS_KEY} in the filing")),
        ("f2", "2030-01-01T00:05:00Z", "quarterly".to_string()),
        ("f3", "2030-01-01T00:05:00Z", "annual".to_string()),
    ]));
    let served = rows.clone();
    let vendor = Server::start(move |r| match r.path() {
        "/leases" if r.header("authorization") == Some(&format!("Bearer {MINT}")) => {
            Response::json(200, &format!("{{\"value\":\"{LEASED}\",\"expires_in\":600}}"))
        }
        "/leases" => Response::json(401, "{}"),
        "/v1/filings" if r.header("authorization") != Some(&format!("Bearer {LEASED}")) => Response::json(401, "{}"),
        "/v1/filings" => {
            let since = r.query("since").unwrap_or_default();
            let page: usize = r.query("page").and_then(|p| p.parse().ok()).unwrap_or(1);
            let rows = served.lock().unwrap();
            let due: Vec<_> = rows.iter().filter(|(_, at, _)| since.is_empty() || *at >= since.as_str()).collect();
            let body = match due.get(page - 1) {
                Some((id, at, note)) => format!("{{\"data\":[{{\"id\":\"{id}\",\"updated_at\":\"{at}\",\"note\":\"{note}\"}}]}}"),
                None => "{\"data\":[]}".to_string(),
            };
            Response::json(200, &body)
        }
        _ => Response::json(404, "{}"),
    });

    let p = GitRepo::init();
    p.write(&format!("{STORE}/config.toml"), "[node]\nid = \"ingest-a\"\n");
    let declaration = |headers: &str| {
        format!(
            "[[pipeline]]\nid = \"filings\"\nincremental = \"updated_at\"\n\n[pipeline.source]\nname = \"http\"\n\n\
             [pipeline.source.config]\nendpoint = \"{}\"\nformat = \"json\"\nrecords = \"/data\"\npage_param = \"page\"\nsince_param = \"since\"\n\n\
             [pipeline.source.config.headers]\n{headers}\n\n[[pipeline.tables]]\nname = \"records\"\nprimary_key = [\"id\"]\n",
            vendor.url("/v1/filings")
        )
    };
    p.write("contextful.toml", &declaration("Authorization = \"Bearer ${secret://vendor-token}\""));
    let env = [
        ("CONTEXTFUL_SECRETS_BACKEND", "lease,env"),
        ("CONTEXTFUL_LEASE_SCOPES", "vendor-token"),
        ("CONTEXTFUL_LEASE_PROVIDER_URL", &vendor.url("")),
        ("LEASE_MINT", MINT),
    ];
    let fire = |run: &str, now: &str| {
        p.run_env(&cf, &["pipeline", "run", "filings", "--project", "research", "--run-id", run, "--site-id", "site-a", "--now", now], &env)
    };

    // A declared pipeline pulls from the source through a leased credential.
    let out = ok(&fire("run-1", "2030-01-01T01:00:00Z"));
    assert!(out.contains("success"), "{out}");
    let listed = ok(&p.run(&cf, &["context", "files", "filings_records", "--project", "research"]));
    let first = landed(&p, &listed, "run-1");
    assert_eq!(first.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["f1", "f2", "f3"]);
    // The secret guard masked the credential the vendor's data carried before anything recorded it.
    assert_eq!(first[0].1, "rotated key [REDACTED:secret] in the filing");
    assert_eq!(vendor.received("/leases").len(), 1, "one mint for the run");

    // The next fire reads from the committed position, re-landing the boundary instant.
    rows.lock().unwrap().push(("f4", "2030-01-01T00:09:00Z", "amended".to_string()));
    ok(&fire("run-2", "2030-01-01T02:00:00Z"));
    let sinces: Vec<Option<String>> = vendor.received("/v1/filings").iter().map(|r| r.query("since")).collect();
    assert!(sinces.contains(&Some("2030-01-01T00:05:00Z".to_string())), "{sinces:?}");
    let listed = ok(&p.run(&cf, &["context", "files", "filings_records", "--project", "research"]));
    assert_eq!(landed(&p, &listed, "run-2").iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["f2", "f3", "f4"]);

    // No credential, leased, bootstrap or scraped, rests in any file the engine wrote.
    for secret in [LEASED, MINT, AWS_KEY] {
        assert!(p.files_containing(secret.as_bytes()).is_empty(), "{secret} in {:?}", p.files_containing(secret.as_bytes()));
    }

    // A credential written into the declaration refuses before any request leaves.
    let before = vendor.requests.lock().unwrap().len();
    p.write("contextful.toml", &declaration("Authorization = \"Bearer ghp_0123456789abcdefghij0123456789abcdef\""));
    refused(&fire("run-3", "2030-01-01T03:00:00Z"), "SecretMaterialInDeclaration");
    assert_eq!(vendor.requests.lock().unwrap().len(), before);
}
