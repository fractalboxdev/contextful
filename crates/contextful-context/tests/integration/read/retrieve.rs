//! `read.retrieve` and `read.rank` over the engine: arms per registered relation,
//! snippets, the resolved publication column, projected fields and the retrieval block.

use super::*;
use contextful_context::read::RetrieveRequest;
use contextful_core::store::bound_time::Bounds;
use serde_json::json;

fn ask(prefix: &str, query: &str) -> RetrieveRequest {
    RetrieveRequest::new(prefix, query, at("2030-02-01T00:00:00Z"))
}

fn ids(r: &Response, key: &str) -> Vec<String> {
    column(r, "_row").iter().map(|row| row[key].as_str().unwrap().to_string()).collect()
}

/// A snippet concatenates up to three text columns: label-priority columns — title, summary, description, thesis and kin — first, then prose-worthy columns in schema order.
// spec: read.retrieve.snippet@f249e729
#[test]
fn a_snippet_leads_with_label_columns() {
    let r = Reads::new();
    let s = r.session(&["research/notes"], Some(("research/notes", "acme")), None);
    let ranked = r.face.retrieve(&s, &ask("research/", "solar battery storage"), Bounds::default()).unwrap();
    assert_eq!(column(&ranked, "_snippet")[0], json!("Solar battery storage costs fall — Cell prices fell again."));
    let v = r.session(&["research/*"], None, Some("public-cloud:us-east-1"));
    let vendor = r.face.retrieve(&v, &ask("research/vendor", "feed"), Bounds::default()).unwrap();
    // Three text columns at most, in schema order after the label column.
    assert_eq!(column(&vendor, "_snippet"), [json!("Solar battery storage feed — quoted")]);
}

/// Content hashes, URLs, identifiers and instant-valued columns never qualify for a snippet.
// spec: read.retrieve.identifiers-never-snippet@ff991c2b
#[test]
fn identifier_columns_never_enter_a_snippet() {
    let r = Reads::new();
    let s = r.session(&["research/notes"], Some(("research/notes", "acme")), None);
    let ranked = r.face.retrieve(&s, &RetrieveRequest { min_score: Some(0), ..ask("research/", "solar battery storage") }, Bounds::default()).unwrap();
    let rows = column(&ranked, "_row");
    let n3 = rows.iter().position(|row| row["note_id"] == json!("n3")).unwrap();
    // n3's URL spells the whole question; its snippet carries none of it.
    assert!(rows[n3]["source_url"].as_str().unwrap().contains("solar-battery-storage"));
    assert_eq!(column(&ranked, "_score")[n3], json!(0));
    for snippet in column(&ranked, "_snippet") {
        let snippet = snippet.as_str().unwrap();
        assert!(!snippet.contains("https://") && !snippet.contains("2030-") && !snippet.contains("n1"), "{snippet}");
    }
}

/// The engine resolves each table's publication column itself, outside the caller's filter.
// spec: read.retrieve.engine-resolved-date@31afd195
#[test]
fn the_engine_resolves_the_publication_column() {
    let r = Reads::new();
    let s = r.session(&["research/*"], Some(("research/notes", "acme")), Some("on-prem:hq"));
    let request = RetrieveRequest { min_score: Some(0), ..ask("research/notes", "solar battery storage hiring grids") };
    let ranked = r.face.retrieve(&s, &request, Bounds::default()).unwrap();
    let basis: Vec<(String, String)> = column(&ranked, "_row")
        .iter()
        .zip(column(&ranked, "_date_basis"))
        .filter_map(|(row, b)| Some((row.get("note_id")?.as_str()?.to_string(), b.as_str()?.to_string())))
        .collect();
    assert!(basis.contains(&("n1".into(), "published_at".into())), "{basis:?}");
    assert!(basis.contains(&("n3".into(), "uncastable".into())), "{basis:?}");
    let quiet = r.face.retrieve(&s, &RetrieveRequest { min_score: Some(0), ..ask("research/visits", "") }, Bounds::default()).unwrap();
    assert!(column(&quiet, "_date_basis").iter().all(|b| b == "_ingested_at"), "{:?}", quiet.rows);
}

/// Reserved projected columns — modality, language, prompt hash, kind — render null for a table lacking them, and its rows stay in the union.
// spec: read.retrieve.reserved-columns-project-null@ec5bb1d4
#[test]
fn reserved_columns_project_null_for_a_table_lacking_them() {
    let r = Reads::new();
    let s = r.session(&["research/notes"], Some(("research/notes", "acme")), None);
    let ranked = r.face.retrieve(&s, &ask("research/", "solar battery storage"), Bounds::default()).unwrap();
    assert_eq!(ranked.rows.len(), 2);
    for c in ["_modality", "_lang", "_prompt_hash", "_kind"] {
        assert!(column(&ranked, c).iter().all(|v| v.is_null()), "{c}");
    }
}

/// Match counts ride outside the internals opt-in, reporting how many rows the ranker scored as matching in the same call.
// spec: read.respond.match-count@d3df9689
#[test]
fn the_block_reports_how_many_rows_matched() {
    let r = Reads::new();
    let s = r.session(&["research/notes"], Some(("research/notes", "acme")), None);
    let ranked = r.face.retrieve(&s, &ask("research/", "solar battery storage"), Bounds::default()).unwrap();
    let block = &ranked.blocks["contextful.retrieval"];
    assert_eq!(block["matched"], json!(2));
    assert_eq!(block["candidates_prefloor"], json!(3));
    assert_eq!(block["floor"], json!(2));
    assert!(!ranked.blocks.contains_key("contextful.internals"));
}

/// The lexical engine's own float score never crosses to a caller.
// spec: read.rank.internal-score-stays-internal@8a3741ac
#[test]
fn only_the_integer_score_crosses() {
    let r = Reads::new();
    let s = r.session(&["research/notes"], Some(("research/notes", "acme")), None);
    let ranked = r.face.retrieve(&s, &ask("research/", "solar battery storage"), Bounds::default()).unwrap();
    assert_eq!(column(&ranked, "_score"), [json!(3), json!(2)]);
    assert!(column(&ranked, "_vscore").iter().all(|v| v.is_null()));
    for row in &ranked.rows {
        for v in row {
            assert!(!v.is_f64(), "a float crossed: {row:?}");
        }
    }
}

/// The retrieval block is omitted from every non-ranked statement and from a build with no ranker, and an absent block differs from one reporting zero matches.
// spec: read.rank.absent-block@886f6327
#[test]
fn a_statement_carries_no_retrieval_block() {
    let r = Reads::new();
    let s = r.session(&["research/notes"], None, None);
    let statement = r.query(&s, r#"SELECT note_id FROM "research/notes""#).unwrap();
    assert!(!statement.blocks.contains_key("contextful.retrieval"));
    let nothing = r.face.retrieve(&s, &ask("research/", "zeppelin"), Bounds::default()).unwrap();
    assert_eq!(nothing.blocks["contextful.retrieval"]["matched"], json!(0));
    assert!(nothing.rows.is_empty());
}

/// Every step, zone exclusion included, completes before a ranked result is cut to its requested size; visible positions carry no trace of a withheld row.
// spec: authority.compose.before-the-cut@7710de0e
#[test]
fn restriction_completes_before_the_cut() {
    let r = Reads::new();
    // Globex's note and the vendor feed score as high as the best acme note.
    let s = r.session(&["research/*"], Some(("research/notes", "acme")), None);
    let one = r.face.retrieve(&s, &RetrieveRequest { limit: Some(1), ..ask("research/", "solar battery storage") }, Bounds::default()).unwrap();
    assert_eq!(ids(&one, "note_id"), ["n1"]);
    let two = r.face.retrieve(&s, &RetrieveRequest { limit: Some(2), ..ask("research/", "solar battery storage") }, Bounds::default()).unwrap();
    assert_eq!(ids(&two, "note_id"), ["n1", "n2"]);
    let block = &two.blocks["contextful.retrieval"];
    assert_eq!((block["returned"].clone(), block["candidates"].clone()), (json!(2), json!(2)));
}

/// The compiler emits one relation per granted table, bound to the table's bare name for the session's lifetime. Every retrieval arm reads through it.
// spec: authority.compose.registered-relation@4af661d3
#[test]
fn one_relation_per_granted_table_and_every_arm_reads_it() {
    let r = Reads::new();
    let s = r.session(&["research/notes", "research/vendor"], None, Some("public-cloud:us-east-1"));
    let names: Vec<&str> = s.relations().map(|rel| rel.name()).collect();
    assert_eq!(names, ["research/notes", "research/vendor"]);
    let ranked = r.face.retrieve(&s, &ask("research/", "solar battery storage"), Bounds::default()).unwrap();
    // The notes relation drops every row under this zone; the vendor arm answers.
    assert_eq!(column(&ranked, "_table"), [json!("research/vendor")]);
}

/// A table reached with no matching grant yields no rows.
// spec: authority.filter-rows.default-deny@5b48112f
#[test]
fn an_ungranted_table_yields_no_rows() {
    let r = Reads::new();
    let s = r.session(&["research/notes"], None, None);
    let ranked = r.face.retrieve(&s, &RetrieveRequest { limit: Some(50), ..ask("", "battery storage") }, Bounds::default()).unwrap();
    let tables = column(&ranked, "_table");
    assert!(!tables.is_empty());
    assert!(tables.iter().all(|t| t == "research/notes"), "{tables:?}");
}

/// Regression: a ranked read meets the least row ceiling a statement does — the table's
/// published limit and the grant's — with `truncated` set by the probe row, and a limit
/// past any ceiling reads no wider window.
#[test]
fn a_ranked_read_meets_the_row_ceiling() {
    let r = Reads::new();
    let s = r.session(&["research/notes"], None, None);
    let capped = r.face.retrieve(&s, &RetrieveRequest { limit: Some(50), min_score: Some(0), ..ask("research/notes", "solar battery storage") }, Bounds::default()).unwrap();
    assert_eq!((capped.rows.len(), capped.truncated), (3, true), "notes publish max_rows = 3");
    let mut grant = read(&["research/notes"], None);
    grant.max_rows = Some(1);
    let g = r.session_for(loop_subject("agent://research-loop"), vec![grant], None);
    let one = r.face.retrieve(&g, &ask("research/notes", "solar battery storage"), Bounds::default()).unwrap();
    assert_eq!((one.rows.len(), one.truncated), (1, true));
    let huge = r.face.retrieve(&s, &RetrieveRequest { limit: Some(u64::MAX), ..ask("research/", "solar") }, Bounds::default()).unwrap();
    assert!(huge.blocks["contextful.retrieval"]["window"].as_u64().unwrap() <= 8 * contextful_context::read::retrieve::MAX_LIMIT);
    let exact = r.face.retrieve(&s, &RetrieveRequest { limit: Some(2), ..ask("research/notes", "solar battery storage") }, Bounds::default()).unwrap();
    assert_eq!(exact.rows.len(), 2);
}

/// Two tables of one row set, `lab/plain` and `lab/indexed`, the second declaring a vector
/// sidecar; `extra` extends the indexed table's block.
const SIDECAR: &str = r#"
[[pipeline.tables]]
name = "lab/plain"
primary_key = ["passage_id"]

[[pipeline.tables]]
name = "lab/indexed"
primary_key = ["passage_id"]

[[pipeline.tables.indexes]]
kind = "vector"
column = "embedding"
model = "e5"
dim = 3
"#;

/// 300 passages folded into each table. `p000`, the oldest, points where the question
/// points and sits outside a limit-10 recency window; the five newest mention "battery".
fn sidecar_reads(extra: &str) -> Reads {
    use contextful_context::fold::fold;
    use contextful_core::store::reconcile::{ColumnType, FloatItem};
    let manifest = format!("{MANIFEST}{SIDECAR}{extra}");
    let mut r = Reads::with_manifest(&format!("{MANIFEST}{SIDECAR}"));
    let rows: Vec<serde_json::Map<String, Value>> = (0..300)
        .map(|i| {
            let (title, owner, embedding) = match i {
                0 => ("passage zero".to_string(), "agent://other", json!([0.0, 0.0, 1.0])),
                295.. => (format!("battery cell {i}"), "agent://research-loop", json!([1.0, (i % 10) as f64 / 10.0, 0.0])),
                _ => (format!("passage {i}"), "agent://research-loop", json!([1.0, (i % 10) as f64 / 10.0, 0.0])),
            };
            json!({"passage_id": format!("p{i:03}"), "title": title, "owner": owner, "embedding": embedding}).as_object().unwrap().clone()
        })
        .collect();
    let types: HashMap<String, ColumnType> = [("embedding".to_string(), ColumnType::FixedSizeList(FloatItem::Float32, 3))].into_iter().collect();
    for table in ["lab/plain", "lab/indexed"] {
        let decl = TableDecl::parse_pipeline(&manifest).unwrap().into_iter().find(|d| d.name == table).unwrap();
        let ctx = RunContext {
            node: NodeId::parse("ingest-a").unwrap(),
            injection: Injection { run_id: "run-0001".into(), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None, taint: None },
            committed_at: at("2030-01-10T00:00:00Z"),
        };
        land(&r.store, &decl, &Batch { rows: rows.clone(), types: types.clone() }, &ctx).unwrap();
        fold(&r.store, &decl, at("2030-01-11T00:00:00Z")).unwrap();
    }
    r.face = Face::open(r.store.clone(), &manifest, pepper()).unwrap();
    r
}

fn battery(table: &str) -> RetrieveRequest {
    RetrieveRequest { query_embedding: Some(vec![0.0, 0.0, 1.0]), ..ask(table, "battery") }
}

/// The current snapshot directory of `table` and the directory of its one sidecar.
fn sidecar_dirs(r: &Reads, table: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let (chain, _) = r.store.chain(table).unwrap();
    let dir = r.store.snapshot_dir(table, &chain[0].snapshot_id).unwrap();
    let path = chain[0].indexes[0]["path"].as_str().unwrap().to_string();
    (dir.clone(), dir.join(path))
}

/// Each sidecar arm, vector over a query embedding and full-text over the content tokens, adds its top results to the recency window, each re-joined by {{authority.compose.vector-arm}}. Per-row scores equal the exact path's.
// spec: read.retrieve.sidecar-generates-candidates@f6d3b339
#[test]
fn the_sidecar_adds_a_row_the_recency_window_misses_with_its_exact_scores() {
    let r = sidecar_reads("");
    let s = r.session(&["lab/*"], None, None);
    let exact = r.face.retrieve(&s, &battery("lab/plain"), Bounds::default()).unwrap();
    let accelerated = r.face.retrieve(&s, &battery("lab/indexed"), Bounds::default()).unwrap();
    assert!(!ids(&exact, "passage_id").contains(&"p000".to_string()), "{:?}", exact.rows);
    assert_eq!(ids(&accelerated, "passage_id")[0], "p000");
    assert_eq!(column(&accelerated, "_vscore")[0], json!(1.0));
    // Every row both paths return carries the same scores.
    let scored = |resp: &Response| -> Vec<(String, Value, Value)> {
        let mut v: Vec<(String, Value, Value)> = ids(resp, "passage_id")
            .into_iter()
            .zip(column(resp, "_score"))
            .zip(column(resp, "_vscore"))
            .map(|((id, s), v)| (id, s, v))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    };
    let shared: Vec<_> = scored(&accelerated).into_iter().filter(|(id, _, _)| id != "p000").collect();
    assert_eq!(shared, scored(&exact));
    assert!(accelerated.blocks["contextful.retrieval"]["candidates_prefloor"].as_u64().unwrap() > 200);
}

/// For one reader the accelerated arm returns a superset of the exact path's rows. Two readers with one query on one snapshot can recall different rows.
// spec: read.retrieve.gain-never-loss@5f8f34b1
#[test]
fn the_accelerated_arm_returns_every_exact_row_and_readers_can_differ() {
    let r = sidecar_reads("[pipeline.tables.policy.rows]\npredicate = \"owner = subject.agent\"\n");
    let s = r.session(&["lab/*"], None, None);
    let exact: std::collections::BTreeSet<String> = ids(&r.face.retrieve(&s, &battery("lab/plain"), Bounds::default()).unwrap(), "passage_id").into_iter().collect();
    assert_eq!(exact.len(), 5);
    let own = ids(&r.face.retrieve(&s, &battery("lab/indexed"), Bounds::default()).unwrap(), "passage_id");
    assert!(exact.iter().all(|id| own.contains(id)), "{exact:?} {own:?}");
    // `p000` belongs to another agent: the probe yields it and this reader never sees it,
    // while its owner recalls it from the same snapshot.
    assert!(!own.contains(&"p000".to_string()));
    let owner = r.session_for(loop_subject("agent://other"), vec![read(&["lab/*"], None)], None);
    let theirs = ids(&r.face.retrieve(&owner, &battery("lab/indexed"), Bounds::default()).unwrap(), "passage_id");
    assert_eq!(theirs, ["p000"]);
}

/// A sidecar yields candidate `id_column` values, not rows; they re-join through the enforced relation on that column before a top-K is final.
// spec: store.index.candidate-ids@0d53be8f
#[test]
fn sidecar_candidates_are_identifier_values_rejoined_through_the_relation() {
    let r = sidecar_reads("[pipeline.tables.policy.rows]\npredicate = \"owner = subject.agent\"\n");
    let s = r.session(&["lab/*"], None, None);
    let (column_name, candidates) = r.face.sidecar_candidates(&s, "lab/indexed", &[0.0, 0.0, 1.0], 10).unwrap();
    assert_eq!(column_name, "passage_id");
    assert_eq!(candidates[0], "p000");
    // A row policy is restriction context: the probe widens to 4 times 64 rows.
    assert_eq!(candidates.len(), 256);
    let ranked = r.face.retrieve(&s, &battery("lab/indexed"), Bounds::default()).unwrap();
    assert!(!ids(&ranked, "passage_id").contains(&"p000".to_string()));
}

/// Any sidecar precondition failure — no snapshot, no matching sidecar, a masked or zone-withheld identifier or indexed column, a classed text column, a dimension or manifest mismatch, an unreadable dump — falls back to the exact scan.
// spec: read.retrieve.sidecar-falls-back@d9f25051
#[test]
fn every_failed_precondition_falls_back_to_the_exact_scan() {
    use contextful_context::vector::Fallback;
    let r = sidecar_reads("");
    let s = r.session(&["lab/*", "research/*"], None, None);
    let q = [0.0, 0.0, 1.0];
    assert_eq!(r.face.sidecar_candidates(&s, "research/notes", &q, 10).err(), Some(Fallback::NoSnapshot));
    assert_eq!(r.face.sidecar_candidates(&s, "lab/plain", &q, 10).err(), Some(Fallback::NoSidecar));
    assert_eq!(r.face.sidecar_candidates(&s, "lab/indexed", &[0.0, 1.0], 10).err(), Some(Fallback::DimensionMismatch));
    let exact = ids(&r.face.retrieve(&s, &battery("lab/plain"), Bounds::default()).unwrap(), "passage_id");
    let falls_back = |r: &Reads, s: &Session| assert_eq!(ids(&r.face.retrieve(s, &battery("lab/indexed"), Bounds::default()).unwrap(), "passage_id"), exact);

    // A masked identifier or vector column.
    for mask in ["passage_id = { strategy = \"hash\" }", "embedding = { strategy = \"drop\" }"] {
        let m = sidecar_reads(&format!("[pipeline.tables.policy.columns]\n{mask}\n"));
        let ms = m.session(&["lab/*"], None, None);
        assert_eq!(m.face.sidecar_candidates(&ms, "lab/indexed", &q, 10).err(), Some(Fallback::Withheld), "{mask}");
    }
    // A zone the vector column is withheld from.
    let z = sidecar_reads("[pipeline.tables.policy.zone]\nallow = [\"on-prem:*\", \"public-cloud:*\"]\n[pipeline.tables.policy.columns]\nembedding = { class = \"phi\" }\n");
    let zs = z.session(&["lab/*"], None, Some("public-cloud:us-east-1"));
    assert_eq!(z.face.sidecar_candidates(&zs, "lab/indexed", &q, 10).err(), Some(Fallback::Withheld));

    // A sidecar manifest disagreeing with the snapshot's entry, then an unreadable graph.
    let (_, dir) = sidecar_dirs(&r, "lab/indexed");
    let own = dir.join("_manifest.json");
    let original = std::fs::read_to_string(&own).unwrap();
    std::fs::write(&own, original.replace("\"model\": \"e5\"", "\"model\": \"other\"")).unwrap();
    assert_eq!(r.face.sidecar_candidates(&s, "lab/indexed", &q, 10).err(), Some(Fallback::ManifestMismatch));
    falls_back(&r, &s);
    std::fs::write(&own, &original).unwrap();
    let graph = dir.join("graph.bin");
    let bytes = std::fs::read(&graph).unwrap();
    std::fs::write(&graph, &bytes[..bytes.len() / 2]).unwrap();
    assert_eq!(r.face.sidecar_candidates(&s, "lab/indexed", &q, 10).err(), Some(Fallback::Unreadable));
    falls_back(&r, &s);
    std::fs::remove_file(&graph).unwrap();
    assert_eq!(r.face.sidecar_candidates(&s, "lab/indexed", &q, 10).err(), Some(Fallback::Unreadable));
    falls_back(&r, &s);
    std::fs::write(&graph, &bytes).unwrap();
    assert!(r.face.sidecar_candidates(&s, "lab/indexed", &q, 10).is_ok());
}

/// A sidecar holding more than 64 MiB of stored vectors stays unloaded and the arm takes the exact scan.
// spec: read.retrieve.sidecar-size-cap@8e042b8e
#[test]
fn a_sidecar_past_64_mib_of_vectors_stays_unloaded() {
    use contextful_context::vector::Fallback;
    use contextful_core::read::rank::{sidecar_over_cap, SIDECAR_SIZE_CAP_BYTES};
    assert_eq!(SIDECAR_SIZE_CAP_BYTES, 64 * 1024 * 1024);
    assert!(!sidecar_over_cap(SIDECAR_SIZE_CAP_BYTES) && sidecar_over_cap(SIDECAR_SIZE_CAP_BYTES + 1));

    let r = sidecar_reads("");
    let s = r.session(&["lab/*"], None, None);
    let exact = ids(&r.face.retrieve(&s, &battery("lab/plain"), Bounds::default()).unwrap(), "passage_id");
    // Both manifests claim a graph one row past the cap at dim 3; the graph file is never read.
    let (snapshot, dir) = sidecar_dirs(&r, "lab/indexed");
    let over = SIDECAR_SIZE_CAP_BYTES / 12 + 1;
    for (path, entry) in [(snapshot.join("_manifest.json"), "/indexes/0"), (dir.join("_manifest.json"), "")] {
        let mut v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        *v.pointer_mut(&format!("{entry}/row_count")).unwrap() = json!(over);
        std::fs::write(&path, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    }
    std::fs::remove_file(dir.join("graph.bin")).unwrap();
    assert_eq!(r.face.sidecar_candidates(&s, "lab/indexed", &[0.0, 0.0, 1.0], 10).err(), Some(Fallback::OverCap));
    assert_eq!(ids(&r.face.retrieve(&s, &battery("lab/indexed"), Bounds::default()).unwrap(), "passage_id"), exact);
}
