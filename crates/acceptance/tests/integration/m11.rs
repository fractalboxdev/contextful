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
    p.write(
        "contextful.toml",
        "[[pipeline]]\nid = \"doc-text\"\ntables = [{ name = \"passages\", primary_key = [\"unit_ref\", \"cue_seq\"] }]\n\
         [pipeline.source]\nname = \"derive\"\n\
         config = { engine = \"text-reader\", source_table = \"documents\", media_column = \"path\", parent_id_column = \"doc_id\" }\n\n\
         [derive.text-reader]\ndriver = \"exec\"\n\n[derive.text-reader.engine]\ncommand = [\"sh\", \"engine.sh\", \"{input}\"]\noutput_format = \"srt\"\n",
    );
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
}
