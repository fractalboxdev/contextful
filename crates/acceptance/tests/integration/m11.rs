//! Milestone 11 — the derive tier.
//!
//! Reach: a pipeline reads the words inside a landed document and fills them into the
//! parent row.

use contextful_acceptance::{bin, GitRepo};
use parquet::file::reader::{FileReader, SerializedFileReader};
use parquet::record::Field;
use std::collections::BTreeMap;
use std::process::Output;

const STORE: &str = ".contextful/context/research";

/// The operator's engine: prints a document's paragraphs as SubRip cues, one per
/// non-empty line, two seconds apart.
const ENGINE: &str = r#"awk 'NF { n++; printf "%d\n00:00:%02d,000 --> 00:00:%02d,500\n%s\n\n", n, 2*(n-1), 2*(n-1)+1, $0 }' "$1""#;

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Every row of the table's current file list, each as column name to text.
fn rows(p: &GitRepo, cf: &std::path::Path, table: &str) -> Vec<BTreeMap<String, String>> {
    let listed = ok(&p.run(cf, &["context", "files", table, "--project", "research"]));
    let mut out = Vec::new();
    for f in listed.lines() {
        let reader = SerializedFileReader::new(std::fs::File::open(p.root.join(STORE).join(f)).unwrap()).unwrap();
        for row in reader.get_row_iter(None).unwrap() {
            let mut m = BTreeMap::new();
            for (name, field) in row.unwrap().get_column_iter() {
                let text = match field {
                    Field::Str(s) => s.clone(),
                    Field::Long(n) => n.to_string(),
                    Field::Int(n) => n.to_string(),
                    Field::Null => continue,
                    other => other.to_string(),
                };
                m.insert(name.clone(), text);
            }
            out.push(m);
        }
    }
    out
}

#[test]
fn m11_derive() {
    let cf = bin("contextful");
    let p = GitRepo::init();
    p.write(&format!("{STORE}/config.toml"), "[node]\nid = \"ingest-a\"\n");
    p.write("docs/memo.txt", "Quarterly revenue rose.\n\nThe board approved the plan.\n");
    p.write("docs/brief.txt", "Filing deadline is Friday.\n");
    p.write("engine.sh", ENGINE);
    let manifest = |args: &str| {
        format!(
            "authoring_posture = \"per_request\"\n[[pipeline]]\nid = \"doc-text\"\ntables = [{{ name = \"passages\", primary_key = [\"unit_ref\", \"derivation_key\", \"cue_seq\"] }}]\n\
             [pipeline.source]\nname = \"derive\"\n\
             config = {{ engine = \"text-reader\", source_table = \"documents\", media_column = \"path\", parent_id_column = \"doc_id\" }}\n\n\
             [derive.text-reader]\ndriver = \"exec\"\n\n[derive.text-reader.engine]\ncommand = [\"sh\", \"engine.sh\", \"{{input}}\"{args}]\noutput_format = \"srt\"\n"
        )
    };
    p.write("contextful.toml", &manifest(""));
    p.write(
        "documents.jsonl",
        "{\"doc_id\":\"d1\",\"path\":\"docs/memo.txt\"}\n{\"doc_id\":\"d2\",\"path\":\"docs/brief.txt\"}\n{\"doc_id\":\"d3\",\"path\":\"docs/lost.txt\"}\n",
    );
    ok(&p.run(&cf, &["context", "land", "documents", "--project", "research", "--rows", "documents.jsonl", "--run-id", "load-1", "--site-id", "site", "--now", "2030-01-01T00:00:00Z"]));

    let fire = |run: &str, now: &str| p.run(&cf, &["pipeline", "run", "doc-text", "--project", "research", "--run-id", run, "--site-id", "site", "--now", now]);
    ok(&fire("derive-1", "2030-01-01T01:00:00Z"));

    // The words of each landed document fill in against its parent row, one passage per cue run.
    let derived = rows(&p, &cf, "doc_text_passages");
    let text_of = |doc: &str| -> Vec<String> {
        let mut rs: Vec<_> = derived.iter().filter(|r| r["unit_ref"] == doc && r["kind"] == "passage").collect();
        rs.sort_by_key(|r| r["cue_seq"].parse::<i64>().unwrap());
        rs.iter().map(|r| r["text"].clone()).collect()
    };
    assert_eq!(text_of("d1"), ["Quarterly revenue rose. The board approved the plan."]);
    assert_eq!(text_of("d2"), ["Filing deadline is Friday."]);
    let parents: Vec<String> = rows(&p, &cf, "documents").into_iter().map(|r| r["doc_id"].clone()).collect();
    for r in derived.iter().filter(|r| r["kind"] == "passage") {
        assert!(parents.contains(&r["unit_ref"]), "every passage names its parent row: {r:?}");
    }
    // A document the engine cannot read settles as a marker in the output table, never a row elsewhere.
    let marker = derived.iter().find(|r| r["unit_ref"] == "d3").unwrap();
    assert_eq!((marker["kind"].as_str(), marker["cue_seq"].as_str(), marker["unit_status"].as_str(), marker["attempts"].as_str()), ("marker", "-1", "failed", "1"));

    // The next tick recomputes the outstanding set: only the failed unit is attempted again.
    let out = ok(&fire("derive-2", "2030-01-01T02:00:00Z"));
    assert!(out.contains("success"), "{out}");
    ok(&p.run(&cf, &["context", "compact", "doc_text_passages", "--project", "research", "--now", "2030-01-01T03:00:00Z"]));
    let derived = rows(&p, &cf, "doc_text_passages");
    let markers: Vec<_> = derived.iter().filter(|r| r["unit_ref"] == "d3").collect();
    assert_eq!(markers.len(), 1, "one marker per unit: {markers:?}");
    assert_eq!(markers[0]["attempts"], "2");
    assert_eq!(derived.iter().filter(|r| r["unit_ref"] == "d1").count(), 1, "a derived unit is not derived twice");
    let first_key = derived.iter().find(|r| r["unit_ref"] == "d1").unwrap()["derivation_key"].clone();

    // A changed engine argument re-derives every unit on the next tick; once folded, each
    // re-derived unit answers from the new derivation alone.
    p.write("contextful.toml", &manifest(", \"--strict\""));
    ok(&fire("derive-3", "2030-01-01T04:00:00Z"));
    ok(&p.run(&cf, &["context", "compact", "doc_text_passages", "--project", "research", "--now", "2030-01-01T05:00:00Z"]));
    let derived = rows(&p, &cf, "doc_text_passages");
    for doc in ["d1", "d2"] {
        let rs: Vec<_> = derived.iter().filter(|r| r["unit_ref"] == doc).collect();
        assert_eq!(rs.len(), 1, "{doc}: the superseded passage is folded away: {rs:?}");
        assert_ne!(rs[0]["derivation_key"], first_key, "{doc}");
    }
    let d1 = derived.iter().find(|r| r["unit_ref"] == "d1").unwrap();
    assert_eq!(d1["text"], "Quarterly revenue rose. The board approved the plan.");
    let retried: Vec<_> = derived.iter().filter(|r| r["unit_ref"] == "d3" && r["derivation_key"] != markers[0]["derivation_key"]).collect();
    assert_eq!(retried.len(), 1, "the failing unit restarts its attempts under the new key: {retried:?}");
    assert_eq!(retried[0]["attempts"], "1");
}

#[test]
fn m11_link_preview_fetches_once_and_lands_head_facts() {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut request = [0; 4096];
        let n = socket.read(&mut request).unwrap();
        assert!(String::from_utf8_lossy(&request[..n]).starts_with("GET /article?token=secret HTTP/1.1"));
        let body = b"<html><head><title>Research memo</title><meta name=\"description\" content=\"Findings and evidence\"></head></html>";
        write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
        socket.write_all(body).unwrap();
    });
    let cf = bin("contextful");
    let p = GitRepo::init();
    p.write(&format!("{STORE}/config.toml"), "[node]\nid = \"ingest-a\"\n");
    p.write("contextful.toml", "authoring_posture = \"per_request\"\n[[pipeline]]\nid = \"cards\"\ntables = [{ name = \"cards\", primary_key = [\"unit_ref\", \"derivation_key\", \"cue_seq\"] }]\n[pipeline.source]\nname = \"derive\"\nconfig = { task = \"link_preview\", engine = \"reader\", source_table = \"documents\", media_column = \"url\", parent_id_column = \"doc_id\" }\n[derive.reader]\ndriver = \"fetch\"\nallow_hosts = [\"localhost\"]\n");
    p.write("documents.jsonl", &format!("{{\"doc_id\":\"d1\",\"url\":\"http://localhost:{port}/article?token=secret\"}}\n"));
    ok(&p.run(&cf, &["context", "land", "documents", "--project", "research", "--rows", "documents.jsonl", "--run-id", "load-1", "--site-id", "site", "--now", "2030-01-01T00:00:00Z"]));
    let fire = |id: &str| p.run(&cf, &["pipeline", "run", "cards", "--project", "research", "--run-id", id, "--site-id", "site", "--now", "2030-01-01T01:00:00Z"]);
    ok(&fire("cards-1"));
    server.join().unwrap();
    let landed = rows(&p, &cf, "cards_cards");
    assert_eq!(landed.len(), 1, "{landed:?}");
    assert_eq!(landed[0]["title"], "Research memo");
    assert_eq!(landed[0]["description"], "Findings and evidence");
    assert_eq!(landed[0]["url"], format!("http://localhost:{port}/article"));
    ok(&fire("cards-2"));
    assert_eq!(rows(&p, &cf, "cards_cards").len(), 1);
}
