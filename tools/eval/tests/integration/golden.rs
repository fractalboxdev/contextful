//! Golden-set growth: the three generators, human approval, promotion from adjudicated
//! labels, write-time redaction before commit, rotation, and the public-corpus fetch manifest.

use std::collections::BTreeMap;

use contextful_core::redaction::Rule;
use contextful_eval::case::{self, Case};
use contextful_eval::golden::{
    absent, chains, commit, compare_generations, promote, walks, Candidate, Edge, Entity, Fact, Generator, GoldenRedaction,
    LabelSource, NativeSet, OutcomeLabel, Rotation, StoreView,
};
use contextful_eval::metrics::RowRef;
use contextful_eval::public::{FetchManifest, PUBLIC_CORPUS_DIR};
use serde_json::json;
use sha2::{Digest, Sha256};

fn store() -> StoreView {
    let entity = |id: &str, name: &str| Entity { id: id.into(), name: name.into() };
    StoreView {
        entities: vec![entity("e1", "Ada Varga"), entity("e2", "Lisbon Archive"), entity("e3", "River Guild")],
        facts: vec![Fact { entity: "e1".into(), attribute: "role".into(), value: "archivist".into(), row: RowRef::new("people", "e1") }],
        edges: vec![
            Edge { from: "e1".into(), relation: "works at".into(), to: "e2".into(), row: RowRef::new("edges", "1") },
            Edge { from: "e2".into(), relation: "belongs to".into(), to: "e3".into(), row: RowRef::new("edges", "2") },
        ],
    }
}

fn case(line: serde_json::Value) -> Case {
    case::load(&line.to_string()).unwrap().remove(0)
}

fn rule(table: &str, column: &str, matcher: serde_json::Value, operation: &str, argument: Option<serde_json::Value>) -> Rule {
    let mut r = json!({ "table": table, "column": column, "match": matcher, "operation": operation });
    if let Some(a) = argument {
        r["argument"] = a;
    }
    serde_json::from_value(r).unwrap()
}

fn ssn_rule() -> Rule {
    rule("people", "notes", json!({ "pattern": "[0-9]{3}-[0-9]{2}-[0-9]{4}" }), "replace", Some(json!("ssn")))
}

/// Three generators draft candidate cases from the deployment's store — entity-to-fact-to-edge walks, composed edge
/// chains, and questions about absent entities — and a human approves each before commit.
// spec: assurance.baseline.generators@da0f0258
#[test]
fn three_generators_draft_candidates_and_only_approved_ones_commit() {
    let store = store();
    let walk = walks(&store, "corpus");
    assert_eq!(walk.len(), 1);
    assert_eq!(walk[0].generator, Generator::Walk);
    let w = &walk[0].case;
    assert!(w.question.contains("Ada Varga") && w.question.contains("works at"), "{}", w.question);
    assert_eq!(w.expected.artifacts, vec!["people#e1".to_string()]);
    assert_eq!(w.expected.edges, vec!["edges#1".to_string()]);
    assert_eq!(w.expected.entities, vec!["Ada Varga".to_string(), "Lisbon Archive".to_string()]);

    let chain = chains(&store, "corpus");
    assert_eq!(chain.len(), 1);
    assert_eq!(chain[0].generator, Generator::Chain);
    assert_eq!(chain[0].case.expected.edges, vec!["edges#1".to_string(), "edges#2".to_string()]);
    assert_eq!(chain[0].case.expected.answer.as_deref(), Some("River Guild"));

    // A decoy the store already names is no absent entity.
    let gone = absent(&store, &["Mira Okafor", "Ada Varga"], "corpus");
    assert_eq!(gone.len(), 1);
    assert_eq!(gone[0].generator, Generator::Absent);
    assert!(gone[0].case.question.contains("Mira Okafor"));
    assert!(gone[0].case.expected.must_abstain);

    // Generation is deterministic, and every draft's id is distinct.
    assert_eq!(walks(&store, "corpus"), walk);
    let drafts: Vec<Candidate> = walk.into_iter().chain(chain).chain(gone).collect();
    let ids: Vec<String> = drafts.iter().map(|c| c.case.id.clone()).collect();
    assert_eq!(ids.iter().collect::<std::collections::BTreeSet<_>>().len(), 3);

    // Only a candidate a reviewer approved commits; an empty reviewer approves nothing.
    let approvals = BTreeMap::from([(ids[0].clone(), "reviewer-a".to_string()), (ids[2].clone(), String::new())]);
    let committed = commit(drafts, &approvals, &GoldenRedaction::default()).unwrap();
    assert_eq!(committed.iter().map(|c| c.id.clone()).collect::<Vec<_>>(), vec![ids[0].clone()]);
    // A committed case round-trips through the one loader.
    let line = serde_json::to_string(&committed[0]).unwrap();
    assert_eq!(case::load(&line).unwrap()[0], committed[0]);
}

/// Only an adjudicated outcome label promotes into a golden set; a self-rated row does not.
// spec: assurance.baseline.promotion-source@dcfcfc16
#[test]
fn only_an_adjudicated_label_promotes() {
    let labeled = |id: &str, source: LabelSource| OutcomeLabel {
        case: case(json!({ "id": id, "corpus": "c", "question": "Who runs the archive?", "expected": { "answer": "Ada" } })),
        source,
    };
    let labels = vec![
        labeled("a", LabelSource::Adjudicated { adjudicator: "reviewer-b".into() }),
        labeled("b", LabelSource::SelfRated),
        labeled("c", LabelSource::Adjudicated { adjudicator: " ".into() }),
    ];
    let promoted = promote(labels, &GoldenRedaction::default()).unwrap();
    assert_eq!(promoted.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), vec!["a"]);
}

/// A golden mined from a live query or promoted from a labeled outcome passes {{authority.redact.write-time}}
/// before commit; a case it cannot redact raises `GoldenRedactionFailed`.
// spec: assurance.baseline.generated-redaction@584f1f70
#[test]
fn a_golden_passes_write_time_redaction_or_refuses() {
    let redaction = GoldenRedaction::new(vec![ssn_rule()]).unwrap();
    let mined = case(json!({ "id": "m1", "corpus": "c", "question": "Whose number is 123-45-6789?",
        "expected": { "answer": "Ada, 123-45-6789", "artifacts": ["people#e1"] } }));
    let clean = redaction.redact(mined.clone()).unwrap();
    assert_eq!(clean.question, "Whose number is [REDACTED:ssn]?");
    assert_eq!(clean.expected.answer.as_deref(), Some("Ada, [REDACTED:ssn]"));

    // Commit and promotion both apply it.
    let label = OutcomeLabel { case: mined.clone(), source: LabelSource::Adjudicated { adjudicator: "r".into() } };
    assert_eq!(promote(vec![label], &redaction).unwrap()[0].question, clean.question);
    let draft = Candidate { generator: Generator::Walk, case: mined.clone() };
    let approvals = BTreeMap::from([("m1".to_string(), "r".to_string())]);
    assert_eq!(commit(vec![draft], &approvals, &redaction).unwrap()[0].question, clean.question);

    // A whole-value rule binds a table the golden cites: free text holds no span to remove.
    let whole = GoldenRedaction::new(vec![rule("people", "ssn", json!("whole"), "drop", None)]).unwrap();
    let e = whole.redact(mined.clone()).unwrap_err();
    assert_eq!(e.code(), "GoldenRedactionFailed");
    assert!(e.to_string().contains("m1"), "{e}");
    // A keyed substitute needs a pepper the harness never holds.
    let keyed = GoldenRedaction::new(vec![rule("people", "notes", json!({ "pattern": "[0-9]{3}-[0-9]{2}-[0-9]{4}" }), "hash", None)]).unwrap();
    assert_eq!(keyed.redact(mined.clone()).unwrap_err().code(), "GoldenRedactionFailed");
    // A question redacted to nothing leaves no case.
    let blank = GoldenRedaction::new(vec![rule("people", "notes", json!({ "pattern": ".+" }), "drop", None)]).unwrap();
    assert_eq!(blank.redact(mined.clone()).unwrap_err().code(), "GoldenRedactionFailed");
    let label = OutcomeLabel { case: mined, source: LabelSource::Adjudicated { adjudicator: "r".into() } };
    assert_eq!(promote(vec![label], &blank).unwrap_err().code(), "GoldenRedactionFailed");
    // A rule the writer would refuse refuses here too.
    let bad = rule("people", "notes", json!({ "pattern": "" }), "drop", None);
    assert_eq!(GoldenRedaction::new(vec![bad]).unwrap_err().code(), "GoldenRedactionFailed");
}

/// The native set rotates on a cadence, a sample of its truth is reviewed on each rotation, and a comparison across
/// generations resolves through the pinned run stamp.
// spec: assurance.baseline.rotation@2cad2879
#[test]
fn the_native_set_rotates_on_a_cadence_with_a_reviewed_sample() {
    let cases: Vec<Case> =
        (0..20).map(|i| case(json!({ "id": format!("n{i}"), "corpus": "c", "question": format!("q{i}") }))).collect();
    let set = NativeSet { generation: 3, rotated_at: 1_000, cases: cases[..10].to_vec() };
    let rotation = Rotation { cadence_secs: 86_400 * 30, review_sample: 4 };
    assert!(!rotation.due(&set, 1_000 + 86_400));
    assert!(rotation.due(&set, 1_000 + 86_400 * 30));

    let next = rotation.rotate(&set, cases[10..].to_vec(), 1_000 + 86_400 * 30, 5);
    assert_eq!(next.set.generation, 4);
    assert_eq!(next.set.rotated_at, 1_000 + 86_400 * 30);
    assert_eq!(next.review.len(), 4);
    assert!(next.review.iter().all(|id| next.set.cases.iter().any(|c| &c.id == id)));
    // One seed reproduces the sample; a set smaller than the sample is reviewed whole.
    assert_eq!(rotation.rotate(&set, cases[10..].to_vec(), 0, 5).review, next.review);
    assert_eq!(rotation.rotate(&set, cases[..2].to_vec(), 0, 5).review.len(), 2);

    // Reports from two generations compare only under one run stamp.
    let stamp = json!({ "k": 10, "tier": "judged", "model": "open-instruct-7b", "samples": 1 });
    let earlier = json!({ "run": stamp, "n_cases": 10 });
    assert!(compare_generations(&earlier, &json!({ "run": stamp, "n_cases": 10 })).is_ok());
    let other = json!({ "run": { "k": 10, "tier": "judged", "model": "other-judge", "samples": 1 } });
    assert_eq!(compare_generations(&earlier, &other).unwrap_err().code(), "BaselineRunStampMismatch");
    assert_eq!(compare_generations(&earlier, &json!({})).unwrap_err().code(), "BaselineRunStampMismatch");
}

/// A public benchmark corpus enters through a fetch-manifest entry naming its URL, its dataset's terms and a SHA-256
/// digest, and is never committed; fetched bytes under another digest raise `PublicCorpusDigestMismatch`.
// spec: assurance.baseline.public-corpora@f2d79b71
#[test]
fn a_public_corpus_enters_through_its_content_hashed_manifest() {
    let bytes = b"{\"id\":\"q1\"}\n";
    let digest: String = Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect();
    let manifest = FetchManifest::parse(&format!(
        "[[corpus]]\nname = \"bench\"\nurl = \"https://example.org/bench.jsonl\"\nterms = \"CC-BY-NC-4.0\"\nsha256 = \"{digest}\"\n"
    ))
    .unwrap();
    let entry = manifest.entry("bench").unwrap();
    assert_eq!(entry.terms, "CC-BY-NC-4.0");
    assert!(entry.verify(bytes).is_ok());
    let e = entry.verify(b"tampered").unwrap_err();
    assert_eq!(e.code(), "PublicCorpusDigestMismatch");
    assert!(e.to_string().contains("bench"), "{e}");
    assert!(manifest.entry("other").is_none());
    let actual = digest;

    // An entry missing its terms or carrying a malformed digest does not load.
    for bad in [
        "[[corpus]]\nname = \"b\"\nurl = \"https://example.org/b\"\nterms = \"\"\nsha256 = \"00\"\n".to_string(),
        format!("[[corpus]]\nname = \"b\"\nurl = \"https://example.org/b\"\nterms = \"MIT\"\nsha256 = \"{}\"\n", "zz".repeat(32)),
        format!("[[corpus]]\nname = \"b\"\nurl = \"ftp://example.org/b\"\nterms = \"MIT\"\nsha256 = \"{actual}\"\n"),
    ] {
        assert!(FetchManifest::parse(&bad).is_err(), "{bad}");
    }

    // The fetch directory is ignored by version control, so a fetched corpus is never committed.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let ignore = std::fs::read_to_string(root.join(".gitignore")).unwrap();
    assert!(ignore.lines().any(|l| l.trim() == format!("/{PUBLIC_CORPUS_DIR}/")), "{PUBLIC_CORPUS_DIR}");
}

#[test]
fn a_store_view_reads_the_memory_shapes_columns() {
    use contextful_eval::golden::ShapeRows;
    let rows = |v: serde_json::Value| -> Vec<serde_json::Map<String, serde_json::Value>> {
        v.as_array().unwrap().iter().map(|r| r.as_object().unwrap().clone()).collect()
    };
    let entities = rows(json!([{ "entity_id": "e1", "name": "Ada Varga" }, { "entity_id": "e2", "name": "Lisbon Archive" }]));
    let facts = rows(json!([
        { "claim_id": "f1", "subject": "e1", "predicate": "role", "object": "archivist", "superseded_by": null },
        { "claim_id": "f0", "subject": "e1", "predicate": "role", "object": "clerk", "superseded_by": "f1" },
    ]));
    let edges = rows(json!([{ "edge_id": 7, "source_id": "e1", "rel_type": "works at", "target_id": "e2" }]));
    let key = |k: &str| vec![k.to_string()];
    let view = StoreView::from_memory(
        ShapeRows { table: "kg/entities", key: &key("entity_id"), rows: &entities },
        ShapeRows { table: "kg/facts", key: &key("claim_id"), rows: &facts },
        ShapeRows { table: "kg/edges", key: &[], rows: &edges },
    );
    assert_eq!(view.entities.len(), 2);
    // A retired claim is no fact.
    assert_eq!(view.facts, vec![Fact { entity: "e1".into(), attribute: "role".into(), value: "archivist".into(), row: RowRef::new("kg/facts", "f1") }]);
    // With no declared key the shape's id column keys the row.
    assert_eq!(view.edges[0].row, RowRef::new("kg/edges", "7"));
    assert_eq!(walks(&view, "c").len(), 1);
}
